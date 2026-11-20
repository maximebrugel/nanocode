use crate::{
    fs::{Kind, Reply, Write},
    host::Vfs,
    machine::Machine,
};
use nanocode_bytes::Bytes;
use psh_engine::{
    HostFile,
    host::{FileKind, OpenOptions},
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

pub(crate) const PIPE_BYTES: usize = 65_536;
pub(crate) const FILE_MODE: u32 = 0o644;

pub(crate) fn bad_descriptor() -> io::Error {
    io::Error::other("Bad file descriptor")
}

pub(crate) struct Source {
    vfs: Rc<Vfs>,
    data: RefCell<Option<Bytes>>,
    at: Cell<usize>,
}

impl Source {
    pub(crate) const fn new(vfs: Rc<Vfs>) -> Self {
        Self {
            vfs,
            data: RefCell::new(None),
            at: Cell::new(0),
        }
    }

    pub(crate) fn set(&self, data: Option<Bytes>) {
        *self.data.borrow_mut() = data;
        self.at.set(0);
    }

    pub(crate) fn unanswered(&self) -> bool {
        self.vfs.answers.is_some() && self.data.borrow().is_none()
    }
}

impl HostFile for Source {
    fn poll_read(&self, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let data = self.data.borrow();
        let Some(data) = data.as_ref() else {
            return Poll::Ready(if self.vfs.answers.is_some() {
                Err(self.vfs.unanswer(b"standard input"))
            } else {
                Ok(0)
            });
        };
        let at = self.at.get();
        let count = buf.len().min(data.len() - at);
        buf[..count].copy_from_slice(&data[at..at + count]);
        self.at.set(at + count);
        Poll::Ready(Ok(count))
    }

    fn poll_write(&self, _cx: &mut Context<'_>, _buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(if self.data.borrow().is_some() {
            FileKind::Fifo
        } else {
            FileKind::CharDevice
        })
    }
}

pub(crate) struct Capture {
    data: RefCell<Vec<u8>>,
    limit: u64,
    truncated: Cell<bool>,
}

impl Capture {
    pub(crate) const fn new(limit: u64) -> Self {
        Self {
            data: RefCell::new(Vec::new()),
            limit,
            truncated: Cell::new(false),
        }
    }

    pub(crate) fn take(&self) -> (Vec<u8>, bool) {
        (self.data.take(), self.truncated.replace(false))
    }
}

impl HostFile for Capture {
    fn poll_read(&self, _cx: &mut Context<'_>, _buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn poll_write(&self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut data = self.data.borrow_mut();
        let room = self.limit.saturating_sub(data.len() as u64);
        let keep = buf.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        data.extend_from_slice(&buf[..keep]);
        if keep < buf.len() {
            self.truncated.set(true);
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::Fifo)
    }
}

pub(crate) enum Device {
    Null,
    Zero,
    Random(Rc<dyn Machine>),
}

impl Device {
    pub(crate) fn at(path: &[u8], machine: &Rc<dyn Machine>) -> Option<Self> {
        match path {
            b"/dev/null" => Some(Self::Null),
            b"/dev/zero" => Some(Self::Zero),
            b"/dev/random" | b"/dev/urandom" => Some(Self::Random(machine.clone())),
            _ => None,
        }
    }

    pub(crate) const NAMES: [&'static [u8]; 4] = [b"null", b"random", b"urandom", b"zero"];
}

impl HostFile for Device {
    fn poll_read(&self, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        match self {
            Self::Null => Poll::Ready(Ok(0)),
            Self::Zero => {
                buf.fill(0);
                Poll::Ready(Ok(buf.len()))
            }
            Self::Random(machine) => {
                machine.random(buf);
                Poll::Ready(Ok(buf.len()))
            }
        }
    }

    fn poll_write(&self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::CharDevice)
    }
}

struct Reader {
    path: Vec<u8>,
    data: Bytes,
    at: Cell<usize>,
}

/// A virtual file's identity for `HostFile::identity`: its path stands for the inode.
fn identity(path: &[u8], size: u64) -> Option<(u64, u64, u64)> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    Some((u64::MAX, hasher.finish(), size))
}

impl HostFile for Reader {
    fn poll_read(&self, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let at = self.at.get();
        let count = buf.len().min(self.data.len() - at);
        buf[..count].copy_from_slice(&self.data[at..at + count]);
        self.at.set(at + count);
        Poll::Ready(Ok(count))
    }

    fn poll_write(&self, _cx: &mut Context<'_>, _buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::File)
    }

    fn identity(&self) -> Option<(u64, u64, u64)> {
        identity(&self.path, self.data.len() as u64)
    }
}

/// A directory opened for reading (`< dir`), as `open(2)` opens one: reading it fails.
struct Directory;

impl HostFile for Directory {
    fn poll_read(&self, _cx: &mut Context<'_>, _buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::ErrorKind::IsADirectory.into()))
    }

    fn poll_write(&self, _cx: &mut Context<'_>, _buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::Dir)
    }
}

type Pending = Pin<Box<dyn Future<Output = io::Result<()>>>>;

/// A file opened for writing only, with `>` or `>>`. Its writes are appends to the
/// file system while nothing changed the file since this file's own last write. A `>`
/// file keeps its own offset, as a descriptor does: once something else wrote,
/// truncated or replaced the file, its next write lands at that offset (zero-filling
/// past the end), not at the end. `>>` always writes at the end.
struct Appender {
    vfs: Rc<Vfs>,
    path: Vec<u8>,
    size: Cell<u64>,
    /// `>`: the offset of the next write. `None` for `>>`.
    at: Option<Cell<u64>>,
    /// `vfs.writes` after this file's last write (or its open).
    seen: Rc<Cell<u64>>,
    pending: RefCell<Option<(Pending, usize)>>,
}

/// Writes `data` at offset `at` of the file at `path`: an append when the file ends
/// there, else the file rewritten with `data` in place.
async fn write_at(
    vfs: Rc<Vfs>,
    path: Vec<u8>,
    data: Vec<u8>,
    at: u64,
    seen: Rc<Cell<u64>>,
) -> io::Result<()> {
    let reply = match vfs.stat(&path, true).get().await {
        Ok(stat) if stat.size != at => {
            let mut content = vfs.read(&path).get().await?.to_vec();
            let start = usize::try_from(at).map_err(|_| io::Error::from(io::ErrorKind::FileTooLarge))?;
            let end = start + data.len();
            if content.len() < end {
                content.resize(end, 0);
            }
            content[start..end].copy_from_slice(&data);
            vfs.write(&path, &content, Write::Replace)
        }
        Ok(_) => vfs.write(&path, &data, Write::Append),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            vfs.write(&path, &data, Write::Append)
        }
        Err(error) => return Err(error),
    };
    seen.set(vfs.writes.get());
    reply.get().await
}

impl HostFile for Appender {
    fn poll_read(&self, _cx: &mut Context<'_>, _buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn poll_write(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut pending = self.pending.borrow_mut();
        if pending.is_none() {
            let len = buf.len() as u64;
            let start = self.at.as_ref().map(Cell::get);
            let end = start.unwrap_or_else(|| self.size.get()) + len;
            if end > self.vfs.limits.file {
                return Poll::Ready(Err(io::ErrorKind::FileTooLarge.into()));
            }
            self.size.set(self.size.get().max(end));
            if let Some(at) = &self.at {
                at.set(end);
            }
            match start {
                Some(start) if self.vfs.writes.get() != self.seen.get() => {
                    let write = write_at(
                        self.vfs.clone(),
                        self.path.clone(),
                        buf.to_vec(),
                        start,
                        self.seen.clone(),
                    );
                    *pending = Some((Box::pin(write), buf.len()));
                }
                _ => {
                    let reply = self.vfs.write(&self.path, buf, Write::Append);
                    self.seen.set(self.vfs.writes.get());
                    match reply {
                        Reply::Now(result) => return Poll::Ready(result.map(|()| buf.len())),
                        Reply::Later(write) => *pending = Some((write, buf.len())),
                    }
                }
            }
        }
        let Some((write, count)) = pending.as_mut() else {
            return Poll::Ready(Err(bad_descriptor()));
        };
        let count = *count;
        match write.as_mut().poll(cx) {
            Poll::Ready(result) => {
                *pending = None;
                Poll::Ready(result.map(|()| count))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::File)
    }

    fn identity(&self) -> Option<(u64, u64, u64)> {
        identity(&self.path, self.size.get())
    }
}

struct Updater {
    vfs: Rc<Vfs>,
    path: Vec<u8>,
    data: RefCell<Vec<u8>>,
    at: Cell<usize>,
    append: bool,
    readable: bool,
    pending: RefCell<Option<(Pending, usize)>>,
}

impl HostFile for Updater {
    fn poll_read(&self, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        if !self.readable {
            return Poll::Ready(Err(bad_descriptor()));
        }
        let data = self.data.borrow();
        let at = self.at.get().min(data.len());
        let count = buf.len().min(data.len() - at);
        buf[..count].copy_from_slice(&data[at..at + count]);
        self.at.set(at + count);
        Poll::Ready(Ok(count))
    }

    fn poll_write(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut pending = self.pending.borrow_mut();
        if pending.is_none() {
            let mut data = self.data.borrow_mut();
            let at = if self.append {
                data.len()
            } else {
                self.at.get()
            };
            let end = at + buf.len();
            if end as u64 > self.vfs.limits.file {
                return Poll::Ready(Err(io::ErrorKind::FileTooLarge.into()));
            }
            if data.len() < end {
                data.resize(end, 0);
            }
            data[at..end].copy_from_slice(buf);
            self.at.set(end);
            match self.vfs.write(&self.path, &data, Write::Replace) {
                Reply::Now(result) => return Poll::Ready(result.map(|()| buf.len())),
                Reply::Later(write) => *pending = Some((write, buf.len())),
            }
        }
        let Some((write, count)) = pending.as_mut() else {
            return Poll::Ready(Err(bad_descriptor()));
        };
        let count = *count;
        match write.as_mut().poll(cx) {
            Poll::Ready(result) => {
                *pending = None;
                Poll::Ready(result.map(|()| count))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::File)
    }

    fn identity(&self) -> Option<(u64, u64, u64)> {
        identity(&self.path, self.data.borrow().len() as u64)
    }
}

pub(crate) async fn open(
    vfs: Rc<Vfs>,
    path: Vec<u8>,
    options: OpenOptions,
) -> io::Result<Rc<dyn HostFile>> {
    if !options.write && !options.append {
        let data = match vfs.read(&path).get().await {
            Ok(data) => data,
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                if matches!(vfs.stat(&path, true).get().await, Ok(stat) if stat.kind == Kind::Dir) {
                    return Ok(Rc::new(Directory));
                }
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        return Ok(Rc::new(Reader {
            path,
            data,
            at: Cell::new(0),
        }));
    }
    let sequential = !options.read && (options.truncate || options.append || options.create_new);
    let mode = options.mode.filter(|&mode| mode & 0o7777 != FILE_MODE);
    let known = !options.create || mode.is_some() || !sequential;
    let existing = if known {
        match vfs.stat(&path, true).get().await {
            Ok(stat) => Some(stat),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    if known && existing.is_none() && !options.create {
        return Err(io::ErrorKind::NotFound.into());
    }
    if existing.is_some() && options.create_new {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    let created = known && existing.is_none();
    let file: Rc<dyn HostFile> = if sequential {
        let how = if options.create_new {
            Write::New
        } else if options.truncate {
            Write::Replace
        } else {
            Write::Append
        };
        vfs.write(&path, &[], how).get().await?;
        let size = match existing {
            Some(stat) if !options.truncate => stat.size,
            _ => 0,
        };
        Rc::new(Appender {
            vfs: vfs.clone(),
            path: path.clone(),
            size: Cell::new(size),
            at: (!options.append).then(|| Cell::new(0)),
            seen: Rc::new(Cell::new(vfs.writes.get())),
            pending: RefCell::new(None),
        })
    } else {
        let data = if existing.is_some() && !options.truncate {
            vfs.read(&path).get().await?.to_vec()
        } else {
            vfs.write(&path, &[], Write::Replace).get().await?;
            Vec::new()
        };
        Rc::new(Updater {
            vfs: vfs.clone(),
            path: path.clone(),
            data: RefCell::new(data),
            at: Cell::new(0),
            append: options.append,
            readable: options.read,
            pending: RefCell::new(None),
        })
    };
    if let (true, Some(mode)) = (created, mode) {
        match vfs.set_mode(&path, mode & 0o7777).get().await {
            Err(error) if error.kind() != io::ErrorKind::Unsupported => return Err(error),
            _ => {}
        }
    }
    Ok(file)
}

struct Channel {
    data: VecDeque<u8>,
    reader: bool,
    writer: bool,
    reading: Option<Waker>,
    writing: Option<Waker>,
}

pub(crate) struct PipeEnd {
    channel: Rc<RefCell<Channel>>,
    reads: bool,
}

pub(crate) fn pipe() -> (PipeEnd, PipeEnd) {
    let channel = Rc::new(RefCell::new(Channel {
        data: VecDeque::new(),
        reader: true,
        writer: true,
        reading: None,
        writing: None,
    }));
    (
        PipeEnd {
            channel: channel.clone(),
            reads: true,
        },
        PipeEnd {
            channel,
            reads: false,
        },
    )
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        let mut channel = self.channel.borrow_mut();
        let waker = if self.reads {
            channel.reader = false;
            channel.writing.take()
        } else {
            channel.writer = false;
            channel.reading.take()
        };
        drop(channel);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl HostFile for PipeEnd {
    fn poll_read(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        if !self.reads {
            return Poll::Ready(Err(bad_descriptor()));
        }
        let mut channel = self.channel.borrow_mut();
        if channel.data.is_empty() {
            if !channel.writer {
                return Poll::Ready(Ok(0));
            }
            channel.reading = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = buf.len().min(channel.data.len());
        let (front, back) = channel.data.as_slices();
        let first = count.min(front.len());
        buf[..first].copy_from_slice(&front[..first]);
        buf[first..count].copy_from_slice(&back[..count - first]);
        channel.data.drain(..count);
        let waker = channel.writing.take();
        drop(channel);
        if let Some(waker) = waker {
            waker.wake();
        }
        Poll::Ready(Ok(count))
    }

    fn poll_write(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        if self.reads {
            return Poll::Ready(Err(bad_descriptor()));
        }
        let mut channel = self.channel.borrow_mut();
        if !channel.reader {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let room = PIPE_BYTES.saturating_sub(channel.data.len());
        if room == 0 {
            channel.writing = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = room.min(buf.len());
        channel.data.extend(&buf[..count]);
        let waker = channel.reading.take();
        drop(channel);
        if let Some(waker) = waker {
            waker.wake();
        }
        Poll::Ready(Ok(count))
    }

    fn kind(&self) -> Option<FileKind> {
        Some(FileKind::Fifo)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Config, Ended, Identity, Limits, Machine, MemFs, NoCommands, Script, Shell, Step};
    use std::{future::Future, pin::Pin, rc::Rc, time::Duration};

    struct Still;

    impl Machine for Still {
        fn now(&self) -> Duration {
            Duration::ZERO
        }

        fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
            Box::pin(std::future::ready(()))
        }

        fn random(&self, bytes: &mut [u8]) {
            bytes.fill(4);
        }
    }

    fn run(source: &str) -> (Vec<u8>, Vec<u8>) {
        let machine: Rc<dyn Machine> = Rc::new(Still);
        let fs = Rc::new(MemFs::new(machine.clone()));
        let mut shell = Shell::new(Config {
            fs,
            commands: Rc::new(NoCommands),
            machine,
            identity: Identity {
                info: psh_engine::host::HostInfo::default(),
                pid: 2,
                user: b"u".to_vec(),
                group: b"u".to_vec(),
                home: b"/".to_vec(),
            },
            limits: Limits {
                fuel: 10_000_000,
                output: 1 << 20,
                file: 1 << 20,
                operations: 100_000,
                depth: 100,
            },
            env: Vec::new(),
            cwd: b"/".to_vec(),
            without: Vec::new(),
            strict: false,
        });
        shell.start(Script {
            source: source.as_bytes().to_vec(),
            name: b"bash".to_vec(),
            args: Vec::new(),
            options: Vec::new(),
            stdin: None,
        });
        for _ in 0..10_000 {
            if let Step::Done(ran) = shell.step(10_000) {
                assert!(matches!(ran.ended, Ended::Exited(_)), "{:?}", ran.ended);
                return (ran.stdout, ran.stderr);
            }
        }
        panic!("the script did not finish");
    }

    fn out(source: &str) -> String {
        String::from_utf8(run(source).0).unwrap()
    }

    #[test]
    fn truncating_redirect_keeps_its_offset() {
        assert_eq!(out("( echo x >> out; cat out; echo x ) > out; cat out"), "x\n");
        assert_eq!(out("{ echo x >> out; cat out; echo x; } > out; cat out"), "x\n");
        assert_eq!(out("( echo x > out; cat out; echo x ) > out; cat out"), "x\n");
        assert_eq!(
            out("for x in x x x; do echo \"<$x>\"; echo x >> out; cat out; done > out; cat out"),
            "<x>\n<x>\n<x>\nx\n"
        );
        assert_eq!(
            out("f() { echo \"$#:$1\"; echo x > out; cat out; shift; echo \"$@\"; }; f x x > out; cat out"),
            "x\n\0\0x\n"
        );
    }

    #[test]
    fn tee_into_its_own_output() {
        assert_eq!(out("echo x | tee out > out; cat out"), "x\n");
        assert_eq!(out("echo x | tee out >> out; cat out"), "x\n");
        assert_eq!(out("echo x | tee out | cat > out; cat out"), "x\n");
    }

    #[test]
    fn appending_redirect_writes_at_the_end() {
        assert_eq!(out("{ echo a; echo b >> out; echo c; } >> out; cat out"), "a\nb\nc\n");
        assert_eq!(out("exec 3> out; echo abc >&3; echo z > out; echo d >&3; cat out"), "z\n\0\0d\n");
    }

    #[test]
    fn reading_a_directory() {
        assert_eq!(out("let n=1 < .; echo $n"), "1\n");
        let (stdout, stderr) = run("cat < .; echo $?");
        assert_eq!(String::from_utf8(stdout).unwrap(), "1\n");
        assert!(String::from_utf8_lossy(&stderr).contains("Is a directory"), "{stderr:?}");
    }
}
