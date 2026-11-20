//! In-memory pipes between virtual processes (ARCH.md "Pipes between vprocs").
//!
//! A bounded buffer with reader and writer counts. Reading an empty pipe waits while a
//! writer exists; writing a full pipe waits while a reader exists; writing without
//! readers is `BrokenPipe` (the vproc's SIGPIPE). Both sides are tasks of the same
//! scheduler, so waiting never blocks a thread.
//!
//! **Materialized pipes (D1068).** When an external is started on an end of a pipe
//! whose buffer is empty, the pipe becomes an OS pipe of the host (`Host::pipe`): the
//! external gets the OS end itself, so `a | b` of two externals moves no byte through
//! the engine. From then on vproc writes go to the OS write end (the buffer was empty,
//! so nothing overtakes), and vproc reads take what is left in the buffer, then read the
//! OS read end directly. The engine holds each OS end while it
//! holds a reader or writer of that side; the kernel counts the externals' copies.
//!
//! **A pipe that holds bytes (nanocode).** An external started on the read end of a
//! pipe whose buffer is not empty gets the OS read end too, as a child of bash gets the
//! pipe itself: it takes only what it reads, and the rest stays for the next reader
//! (the next `read` of a loop whose body ran the external). The buffered bytes become
//! the backlog: they go into the OS pipe in order, as far as it has room, whenever a
//! vproc reads or writes the pipe and while an external reads it (a [`Feed`], polled by
//! whoever waits for the external). While a backlog is left, vproc writes join it, so
//! nothing overtakes, and an external writer gets a pump. The engine keeps the OS write
//! end until the backlog is gone.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::host::{Host, HostFile};

/// The capacity of a pipe, as on Linux.
pub(crate) const PIPE_CAPACITY: usize = 64 * 1024;

pub(crate) const PIPE_BUF: usize = 4096;

struct Inner {
    buf: VecDeque<u8>,
    /// Once materialized: what is still to go into the OS pipe, behind what it holds.
    backlog: VecDeque<u8>,
    /// The [`Feed`]s of externals reading the OS read end.
    feeds: usize,
    readers: usize,
    writers: usize,
    read_wakers: Vec<Waker>,
    write_wakers: Vec<Waker>,
    /// A vproc writing a pipe that still holds bytes first lets the other tasks run
    /// once, as a reading process runs while the writer goes on: `head -3` has taken
    /// its lines and gone by the writer's next one, which meets the closed pipe
    /// (SIGPIPE) as with bash, rather than the writer filling the buffer before the
    /// reader runs. Set while the writer waits that turn.
    yielded: bool,
    /// Set when a program that bash forks (an in-process utility, `cat f` in
    /// `(cat f; echo x) | head -3`) starts or ends with bytes still in the pipe: in
    /// bash the child starts, and the shell waits for its exit, while the reader takes
    /// its lines and may exit. So the next write first lets the reader run until it
    /// took them or left (the write then meets the closed pipe, SIGPIPE), a bounded
    /// number of rounds in case the reader never reads.
    handoff: Option<u32>,
    /// The stages at a pipeline's start gate that look at this pipe ([`Watch`]):
    /// woken when a reader or writer starts waiting on it.
    watchers: Vec<Waker>,
    /// The OS pipe once materialized: (read end, write end), each dropped when the
    /// engine's last reader or writer goes.
    os: Option<(OsEnd, OsEnd)>,
}

type OsEnd = Option<Rc<dyn HostFile>>;

fn register(list: &mut Vec<Waker>, w: &Waker) {
    if !list.iter().any(|x| x.will_wake(w)) {
        list.push(w.clone());
    }
}

fn wake_all(list: &mut Vec<Waker>) {
    for w in list.drain(..) {
        w.wake();
    }
}

pub(crate) struct PipeReader(Rc<RefCell<Inner>>);
pub(crate) struct PipeWriter(Rc<RefCell<Inner>>);

pub(crate) fn pipe() -> (PipeReader, PipeWriter) {
    let inner = Rc::new(RefCell::new(Inner {
        buf: VecDeque::new(),
        backlog: VecDeque::new(),
        feeds: 0,
        readers: 1,
        writers: 1,
        read_wakers: Vec::new(),
        write_wakers: Vec::new(),
        yielded: false,
        handoff: None,
        watchers: Vec::new(),
        os: None,
    }));
    (PipeReader(inner.clone()), PipeWriter(inner))
}

/// A pipe that already holds `data` and has no writer: here-documents and
/// here-strings. The body is in memory anyway, so nothing is written before the
/// consumer runs (pitfall 2 of `research/execution-core.md`).
pub(crate) fn preloaded(data: &[u8]) -> PipeReader {
    PipeReader(Rc::new(RefCell::new(Inner {
        buf: data.to_vec().into(),
        backlog: VecDeque::new(),
        feeds: 0,
        readers: 1,
        writers: 0,
        read_wakers: Vec::new(),
        write_wakers: Vec::new(),
        yielded: false,
        handoff: None,
        watchers: Vec::new(),
        os: None,
    })))
}

/// The OS end for an external started on a pipe end: `reading` = the read end.
/// Materializes the pipe if the host has OS pipes: for a reader what the buffer holds
/// becomes the backlog; for a writer only when the buffer is empty. `None` means the
/// external needs a pump (its bytes must come behind buffered ones, bytes a `read` gave
/// back come before the OS pipe's, or no OS pipes).
fn os_end(inner: &Rc<RefCell<Inner>>, reading: bool, host: &dyn Host) -> Option<Rc<dyn HostFile>> {
    let mut p = inner.borrow_mut();
    if p.os.is_none() {
        if !reading && !p.buf.is_empty() {
            return None;
        }
        let (r, w) = host.pipe().ok()?;
        p.backlog = std::mem::take(&mut p.buf);
        let r = (p.readers > 0).then_some(r);
        let w = (p.writers > 0 || !p.backlog.is_empty()).then_some(w);
        p.os = Some((r, w));
        // Waiting vprocs poll the OS ends from now on.
        wake_all(&mut p.read_wakers);
        wake_all(&mut p.write_wakers);
    }
    if if reading {
        !p.buf.is_empty()
    } else {
        !p.backlog.is_empty()
    } {
        return None;
    }
    let (r, w) = p.os.as_ref()?;
    if reading { r.clone() } else { w.clone() }
}

/// Moves the backlog into the OS pipe as far as it has room. Once it is gone, a pipe
/// without writers lets go of the OS write end, so its readers see the end.
fn flush(inner: &Rc<RefCell<Inner>>, cx: &mut Context<'_>) {
    loop {
        let wrote = {
            let p = inner.borrow();
            if p.backlog.is_empty() {
                return;
            }
            let Some((_, Some(w))) = &p.os else { return };
            w.poll_write(cx, p.backlog.as_slices().0)
        };
        let mut p = inner.borrow_mut();
        match wrote {
            Poll::Pending => return,
            Poll::Ready(Ok(n)) if n > 0 => {
                p.backlog.drain(..n);
                wake_all(&mut p.write_wakers);
            }
            // Nobody reads the OS pipe any more.
            Poll::Ready(_) => p.backlog.clear(),
        }
        if p.backlog.is_empty() {
            release(&mut p);
            return;
        }
    }
}

/// The OS write end goes once no writer and no backlog needs it.
fn release(p: &mut Inner) {
    if p.writers == 0
        && p.backlog.is_empty()
        && let Some((_, w)) = &mut p.os
    {
        *w = None;
    }
}

/// The backlog has no reader left: no vproc holds the read end and no external's wait
/// moves it.
fn abandon(p: &mut Inner) {
    if p.readers == 0 && p.feeds == 0 {
        p.backlog.clear();
        release(p);
        wake_all(&mut p.write_wakers);
    }
}

/// Moves a materialized pipe's backlog into its OS pipe while an external reads it
/// (see the module's notes): polled by whoever waits for the external, dropped when it
/// is gone.
pub(crate) struct Feed(Rc<RefCell<Inner>>);

impl Feed {
    /// `Ready` once nothing is left to move.
    pub(crate) fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        flush(&self.0, cx);
        if self.0.borrow().backlog.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        let mut p = self.0.borrow_mut();
        p.feeds -= 1;
        abandon(&mut p);
    }
}

/// A look at a pipe that holds neither end: a stage at its pipeline's start gate
/// watches the pipes it reads and writes, to go on when the stage at the other end
/// waits on it before that stage's own gate (`echo x | echo "$(cat)"`).
pub(crate) struct Watch(Rc<RefCell<Inner>>);

impl Watch {
    /// Whether a vproc waits to read this (empty) pipe, or to write it (full);
    /// otherwise `cx` is woken when one starts to.
    pub(crate) fn poll_waited_on(&self, cx: &mut Context<'_>) -> bool {
        let mut p = self.0.borrow_mut();
        if (!p.read_wakers.is_empty() && p.buf.is_empty()) || !p.write_wakers.is_empty() {
            return true;
        }
        register(&mut p.watchers, cx.waker());
        false
    }
}

impl PipeReader {
    pub(crate) fn watch(&self) -> Watch {
        Watch(self.0.clone())
    }
}

impl PipeWriter {
    pub(crate) fn watch(&self) -> Watch {
        Watch(self.0.clone())
    }
}

impl PipeReader {
    /// See [`os_end`].
    pub(crate) fn os_end(&self, host: &dyn Host) -> Option<Rc<dyn HostFile>> {
        os_end(&self.0, true, host)
    }

    /// The feed an external given the OS read end needs: `None` without a backlog.
    pub(crate) fn feed(&self) -> Option<Feed> {
        let mut p = self.0.borrow_mut();
        if p.os.is_none() || p.backlog.is_empty() {
            return None;
        }
        p.feeds += 1;
        Some(Feed(self.0.clone()))
    }

    /// Whether the pipe is an OS pipe: bytes read from it cannot be given back in
    /// front of what an external reading the OS end takes next.
    pub(crate) fn is_os(&self) -> bool {
        self.0.borrow().os.is_some()
    }

    pub(crate) fn poll_read(
        &self,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut p = self.0.borrow_mut();
        if out.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if p.buf.is_empty()
            && let Some((Some(r), _)) = &p.os
        {
            // Nothing buffered: read the OS pipe straight into the caller's buffer
            // (no copy, no read-ahead to hold for a later pump), the backlog moved
            // into it first.
            let r = r.clone();
            drop(p);
            flush(&self.0, cx);
            return r.poll_read(cx, out);
        }
        if !p.buf.is_empty() {
            let n = out.len().min(p.buf.len());
            // The two runs of the ring, copied as slices rather than byte by byte.
            let (front, back) = p.buf.as_slices();
            let first = n.min(front.len());
            out[..first].copy_from_slice(&front[..first]);
            out[first..n].copy_from_slice(&back[..n - first]);
            p.buf.drain(..n);
            wake_all(&mut p.write_wakers);
            return Poll::Ready(Ok(n));
        }
        if p.writers == 0 {
            return Poll::Ready(Ok(0));
        }
        register(&mut p.read_wakers, cx.waker());
        wake_all(&mut p.watchers);
        Poll::Pending
    }
}

impl PipeReader {
    pub(crate) fn line_len(&self, delim: u8) -> Option<usize> {
        let p = self.0.borrow();
        let (front, back) = p.buf.as_slices();
        match front.iter().position(|&b| b == delim) {
            Some(i) => Some(i + 1),
            None => back
                .iter()
                .position(|&b| b == delim)
                .map(|i| front.len() + i + 1),
        }
        .or((!p.buf.is_empty()).then_some(p.buf.len()))
    }

    pub(crate) fn unread(&self, data: &[u8]) {
        let mut p = self.0.borrow_mut();
        for &b in data.iter().rev() {
            p.buf.push_front(b);
        }
    }
}

/// How many scheduler rounds a write after a forked program's output waits at most
/// for the reader to take it ([`Inner::handoff`]).
const HANDOFF_ROUNDS: u32 = 64;

impl PipeWriter {
    /// See [`os_end`].
    pub(crate) fn os_end(&self, host: &dyn Host) -> Option<Rc<dyn HostFile>> {
        os_end(&self.0, false, host)
    }

    /// A program that bash forks starts or ended writing this pipe: see
    /// [`Inner::handoff`].
    pub(crate) fn hand_off(&self) {
        let mut p = self.0.borrow_mut();
        if p.readers > 0 && p.os.is_none() && !p.buf.is_empty() {
            p.handoff = Some(0);
        }
    }

    pub(crate) fn poll_write(&self, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let p = self.0.borrow();
        if let Some((_, Some(w))) = &p.os {
            let w = w.clone();
            let behind = !p.backlog.is_empty();
            drop(p);
            if behind {
                flush(&self.0, cx);
                let mut p = self.0.borrow_mut();
                // Still a backlog: these bytes come behind it.
                if !p.backlog.is_empty() {
                    if data.is_empty() {
                        return Poll::Ready(Ok(0));
                    }
                    let space = PIPE_CAPACITY.saturating_sub(p.backlog.len());
                    if space == 0 {
                        register(&mut p.write_wakers, cx.waker());
                        return Poll::Pending;
                    }
                    let n = space.min(data.len());
                    p.backlog.extend(&data[..n]);
                    return Poll::Ready(Ok(n));
                }
            }
            return w.poll_write(cx, data);
        }
        drop(p);
        let mut p = self.0.borrow_mut();
        if p.readers == 0 {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let space = PIPE_CAPACITY.saturating_sub(p.buf.len());
        if space == 0 {
            register(&mut p.write_wakers, cx.waker());
            wake_all(&mut p.watchers);
            return Poll::Pending;
        }
        if let Some(rounds) = p.handoff {
            if p.buf.is_empty() || rounds >= HANDOFF_ROUNDS {
                p.handoff = None;
            } else {
                p.handoff = Some(rounds + 1);
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        if !p.buf.is_empty() && !std::mem::replace(&mut p.yielded, true) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        p.yielded = false;
        let n = space.min(data.len());
        p.buf.extend(&data[..n]);
        wake_all(&mut p.read_wakers);
        Poll::Ready(Ok(n))
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        let mut p = self.0.borrow_mut();
        p.readers -= 1;
        if p.readers == 0 {
            p.buf.clear();
            if let Some((r, _)) = &mut p.os {
                *r = None;
            }
            abandon(&mut p);
            wake_all(&mut p.write_wakers);
        }
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let mut p = self.0.borrow_mut();
        p.writers -= 1;
        if p.writers == 0 {
            release(&mut p);
            wake_all(&mut p.read_wakers);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::{Script, Shell, Step, testhost::TestHost};

    /// Runs `source` in `/w` (holding the file `a`) at every budget and latency.
    fn check(source: &str, expected: &[u8]) {
        for latency in [false, true] {
            for budget in [1, 1000] {
                let host = Rc::new(TestHost::new());
                host.set_latency(latency);
                host.fs.write_file("/w/a", b"");
                let mut shell = Shell::new(host.config());
                shell.start(Script::command(&format!("cd /w; {source}")));
                let status = loop {
                    match shell.step(budget) {
                        Step::Done(status) => break status,
                        Step::Yield | Step::Blocked => {}
                        Step::Idle => panic!("{source}: idle before completion"),
                    }
                };
                let at = format!("{source}: latency={latency}, budget={budget}");
                assert_eq!(status, 0, "{at}");
                assert_eq!(host.stderr(), b"", "{at}");
                assert_eq!(host.stdout(), expected, "{at}");
            }
        }
    }

    #[test]
    fn stages_open_their_redirections_before_any_command_runs() {
        // A compound's words are expanded after the gate.
        check(
            r#"{ echo *; } | { read x; echo "$x"; } > out; read y < out; echo "$y""#,
            b"a out\n",
        );
        check(
            r#"f() { echo *; }; f | { read x; echo "$x"; } > out; read y < out; echo "$y""#,
            b"a out\n",
        );
    }

    #[test]
    fn a_stage_that_reads_before_its_gate_does_not_hold_the_pipeline() {
        check(r#"echo hi | echo "<$(read l; echo "$l")>""#, b"<hi>\n");
        check(r#"echo hi | x=$(read l; echo "$l") ; echo done"#, b"done\n");
    }

    #[test]
    fn a_path_that_names_no_file_is_not_found() {
        check(
            "a/b/c.txt 2>/dev/null; echo $?; ./nope 2>/dev/null; echo $?",
            b"127\n127\n",
        );
    }

    #[test]
    fn a_writer_meets_the_pipe_its_reader_closed() {
        check(
            "set -o pipefail; for i in 1 2 3 4; do echo $i; done | { read a; echo $a; }; echo $?",
            b"1\n141\n",
        );
    }
}
