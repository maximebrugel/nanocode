use std::{cell::RefCell, io, rc::Rc, time::Duration};

use nanocode_platform::{Instant, UNIX_EPOCH};
use rustpython_vm::{PyResult, VirtualMachine, builtins::PyBaseExceptionRef, vm::limit::Limiter};

use crate::{Changes, Files, Held, Limit, Limits, Run, Stdio, Stop};

const SLICE: u64 = 1 << 12;

pub(crate) use _host::module_def;

pub struct State {
    files: Rc<dyn Files>,
    stdio: Stdio,
    read: usize,
    written: u64,
    cwd: Vec<u8>,
    env: Vec<(Vec<u8>, Vec<u8>)>,
    limits: Limits,
    stop: Stop,
    slept: Duration,
    started: Instant,
    steps: u64,
    slice: u64,
    held: Option<i64>,
    credit: u64,
    importing: (u32, i64),
    grace: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Which stream each held write went to, in order, as (fd, length) runs, so a
    /// caller can replay stdout and stderr interleaved as the program wrote them.
    pub order: Vec<(u8, usize)>,
    pub changes: Changes,
    pub refused: Option<Limit>,
    pub cancelled: bool,
    /// The status os._exit ended the run with: like a cancel, nothing after it runs.
    pub exited: Option<i32>,
    pub held_at_refusal: Held,
}

impl State {
    pub fn new(files: Rc<dyn Files>, run: Run) -> Self {
        Self {
            files,
            stdio: run.stdio,
            read: 0,
            written: 0,
            cwd: run.cwd,
            env: run.env,
            limits: run.limits,
            stop: run.stop,
            slept: Duration::ZERO,
            started: Instant::now(),
            steps: 0,
            slice: 0,
            held: held(),
            credit: run.limits.memory.unwrap_or(0),
            importing: (0, 0),
            grace: false,
            stdout: Vec::new(),
            stderr: Vec::new(),
            order: Vec::new(),
            changes: Changes::default(),
            refused: None,
            cancelled: false,
            exited: None,
            held_at_refusal: Held::default(),
        }
    }

    fn take(&mut self, size: Option<usize>) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        match &self.stdio {
            Stdio::Held(bytes) => {
                let rest = bytes.len() - self.read;
                let take = size.map_or(rest, |size| size.min(rest));
                let data = bytes[self.read..self.read + take].to_vec();
                self.read += take;
                Ok(data)
            }
            Stdio::Process => {
                let mut stdin = io::stdin().lock();
                let mut data = Vec::new();
                match size {
                    None => {
                        stdin.read_to_end(&mut data)?;
                    }
                    Some(size) => {
                        data.resize(size, 0);
                        let read = stdin.read(&mut data)?;
                        data.truncate(read);
                    }
                }
                Ok(data)
            }
        }
    }

    fn put(&mut self, fd: u8, data: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        self.written = self
            .written
            .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
        match (&self.stdio, fd) {
            (Stdio::Held(_), _) => {
                let fd = if fd == 2 { 2 } else { 1 };
                if fd == 2 {
                    self.stderr.extend_from_slice(data);
                } else {
                    self.stdout.extend_from_slice(data);
                }
                match self.order.last_mut() {
                    Some((last, len)) if *last == fd => *len += data.len(),
                    _ if data.is_empty() => {}
                    _ => self.order.push((fd, data.len())),
                }
            }
            (Stdio::Process, 2) => {
                let mut stderr = io::stderr().lock();
                stderr.write_all(data)?;
                stderr.flush()?;
            }
            (Stdio::Process, _) => {
                let mut stdout = io::stdout().lock();
                stdout.write_all(data)?;
                stdout.flush()?;
            }
        }
        Ok(())
    }

    pub const fn spent(&self, left: u64) -> u64 {
        self.steps.saturating_add(self.slice.saturating_sub(left))
    }

    const fn stopped(&self) -> bool {
        self.refused.is_some() || self.cancelled || self.exited.is_some()
    }

    fn used(&self) -> Option<u64> {
        let grown = held()?.saturating_sub(self.held?);
        Some(u64::try_from(grown).unwrap_or(0))
    }

    fn over(&mut self, more: u64) -> bool {
        let room = if self.importing.0 > 0 { self.credit } else { 0 };
        match (self.limits.memory, self.used()) {
            (Some(limit), Some(used)) if used.saturating_add(more) > limit.saturating_add(room) => {
                self.held_at_refusal = Held {
                    bytes: used,
                    asked: more,
                };
                true
            }
            _ => false,
        }
    }

    fn import(&mut self, on: bool) {
        let Some(now) = held() else { return };
        let (depth, from) = self.importing;
        let room = isize::try_from(self.credit).unwrap_or(isize::MAX);
        match (on, depth) {
            (true, 0) => {
                self.importing = (1, now);
                rustpython_vm::vm::limit::raise_memory_ceiling(room);
            }
            (true, _) => self.importing.0 = depth + 1,
            (false, 0) => {}
            (false, 1) => {
                self.importing = (0, 0);
                let kept = u64::try_from(now.saturating_sub(from))
                    .unwrap_or(0)
                    .min(self.credit);
                self.credit -= kept;
                self.held = self
                    .held
                    .map(|held| held.saturating_add(i64::try_from(kept).unwrap_or(0)));
                let kept = isize::try_from(kept).unwrap_or(isize::MAX);
                rustpython_vm::vm::limit::raise_memory_ceiling(kept.saturating_sub(room));
            }
            (false, _) => self.importing.0 = depth - 1,
        }
    }

    fn refuel(&mut self) -> Option<u64> {
        self.steps = self.steps.saturating_add(std::mem::take(&mut self.slice));
        if self.stopped() {
            return std::mem::take(&mut self.grace).then_some(SLICE);
        }
        if (self.stop)() {
            self.cancelled = true;
            return None;
        }
        let left = self.limits.steps.saturating_sub(self.steps);
        self.refused = if left == 0 {
            Some(Limit::Steps)
        } else if self.started.elapsed() >= self.limits.time {
            Some(Limit::Time)
        } else if self.over(0) {
            Some(Limit::Memory)
        } else {
            None
        };
        if self.refused.is_some() {
            return None;
        }
        self.slice = left.min(SLICE);
        Some(self.slice)
    }
}

pub fn held() -> Option<i64> {
    let held = rustpython_vm::vm::limit::held()?;
    i64::try_from(held).ok()
}

fn halt(vm: &VirtualMachine) -> PyBaseExceptionRef {
    let stop = vm.new_exception_empty(vm.ctx.exceptions.system_exit.to_owned());
    drop(vm.stopped.replace(Some(stop.clone())));
    vm.fuel.set(0);
    stop
}

pub struct Budget;

impl Limiter for Budget {
    fn refuel(&self, vm: &VirtualMachine) -> PyResult<u64> {
        with(|state| Ok(state.refuel()))
            .ok()
            .flatten()
            .ok_or_else(|| halt(vm))
    }

    fn reserve(&self, vm: &VirtualMachine, bytes: usize) -> PyResult<()> {
        let refused = with(|state| {
            let over = !state.stopped() && state.over(bytes as u64);
            if over {
                state.refused = Some(Limit::Memory);
            }
            Ok(over || state.stopped())
        });
        match refused {
            Ok(false) => Ok(()),
            _ => Err(halt(vm)),
        }
    }
}

pub struct Shut;

impl Limiter for Shut {
    fn refuel(&self, vm: &VirtualMachine) -> PyResult<u64> {
        Err(halt(vm))
    }

    fn reserve(&self, vm: &VirtualMachine, _: usize) -> PyResult<()> {
        Err(halt(vm))
    }
}

pub fn ending() -> bool {
    rustpython_vm::vm::limit::lift_memory_ceiling();
    with(|state| {
        state.grace = state.stopped();
        Ok(state.grace)
    })
    .unwrap_or(true)
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

pub struct Entered;

impl Entered {
    pub fn leave(self) -> State {
        STATE
            .with_borrow_mut(Option::take)
            .expect("a run leaves the state it entered")
    }
}

impl Drop for Entered {
    fn drop(&mut self) {
        STATE.with_borrow_mut(|state| *state = None);
    }
}

pub fn enter(state: State) -> Entered {
    STATE.with_borrow_mut(|slot| *slot = Some(state));
    Entered
}

fn with<T>(run: impl FnOnce(&mut State) -> io::Result<T>) -> io::Result<T> {
    STATE.with_borrow_mut(|state| match state {
        Some(state) => run(state),
        None => Err(io::Error::other("no Python run is active")),
    })
}

pub fn memory() -> Option<usize> {
    with(|state| Ok(state.limits.memory))
        .ok()
        .flatten()
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
}

pub fn input() -> Vec<u8> {
    with(|state| state.take(None)).unwrap_or_default()
}

pub fn source(path: &str) -> io::Result<Vec<u8>> {
    with(|state| {
        let path = absolute(&state.cwd, path.as_bytes());
        state.files.read(&path, state.limits.file)
    })
}

pub fn say(text: &str) {
    let _ = with(|state| state.put(1, text.as_bytes()));
}

pub fn print(text: &str) {
    let _ = with(|state| state.put(2, text.as_bytes()));
}

pub fn text(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::AlreadyExists => "File exists".to_owned(),
        io::ErrorKind::NotADirectory => "Not a directory".to_owned(),
        io::ErrorKind::IsADirectory => "Is a directory".to_owned(),
        io::ErrorKind::DirectoryNotEmpty => "Directory not empty".to_owned(),
        io::ErrorKind::FileTooLarge => "File too large".to_owned(),
        io::ErrorKind::ReadOnlyFilesystem => "Read-only file system".to_owned(),
        _ => error.to_string(),
    }
}

fn absolute(cwd: &[u8], path: &[u8]) -> Vec<u8> {
    let separator = |byte: &u8| *byte == b'/' || (cfg!(windows) && *byte == b'\\');
    let drive = cfg!(windows)
        && matches!(path, [letter, b':', next, ..] if letter.is_ascii_alphabetic() && separator(next));
    let mut joined = Vec::new();
    if drive {
        joined.push(b'/');
    } else if !path.first().is_some_and(separator) {
        joined.extend_from_slice(cwd);
        joined.push(b'/');
    }
    joined.extend_from_slice(path);
    let mut out = Vec::new();
    for part in joined.split(separator) {
        match part {
            b"" | b"." => {}
            b".." => {
                let cut = out.iter().rposition(|&byte| byte == b'/').unwrap_or(0);
                out.truncate(cut);
            }
            _ => {
                out.push(b'/');
                out.extend_from_slice(part);
            }
        }
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

#[cfg(not(target_family = "wasm"))]
fn wait(duration: Duration, stop: &Stop) -> io::Result<bool> {
    const POLL: Duration = Duration::from_millis(20);
    let until = Instant::now() + duration;
    loop {
        if stop() {
            return Ok(false);
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(true);
        }
        std::thread::sleep(left.min(POLL));
    }
}

#[cfg(target_family = "wasm")]
fn wait(_: Duration, _: &Stop) -> io::Result<bool> {
    Err(io::ErrorKind::Unsupported.into())
}

#[rustpython_vm::pymodule]
mod _host {
    use super::{Limit, State, UNIX_EPOCH, halt, text, wait, with};
    use crate::{Child, Kind, Remove, Write};
    use rustpython_vm::{
        PyObjectRef, PyResult, VirtualMachine,
        builtins::{PyBaseExceptionRef, PyBytesRef},
        function::ArgBytesLike,
    };
    use std::{io, time::Duration};

    fn failure(vm: &VirtualMachine, error: &io::Error, path: Option<&[u8]>) -> PyBaseExceptionRef {
        let exceptions = &vm.ctx.exceptions;
        let (kind, name, code) = match error.kind() {
            io::ErrorKind::NotFound => (exceptions.file_not_found_error, "ENOENT", 2),
            io::ErrorKind::PermissionDenied => (exceptions.permission_error, "EACCES", 13),
            io::ErrorKind::AlreadyExists => (exceptions.file_exists_error, "EEXIST", 17),
            io::ErrorKind::NotADirectory => (exceptions.not_a_directory_error, "ENOTDIR", 20),
            io::ErrorKind::IsADirectory => (exceptions.is_a_directory_error, "EISDIR", 21),
            io::ErrorKind::DirectoryNotEmpty => (exceptions.os_error, "ENOTEMPTY", 39),
            io::ErrorKind::FileTooLarge => (exceptions.os_error, "EFBIG", 27),
            io::ErrorKind::ReadOnlyFilesystem => (exceptions.os_error, "EROFS", 30),
            io::ErrorKind::InvalidInput => (exceptions.os_error, "EINVAL", 22),
            io::ErrorKind::Unsupported => (exceptions.os_error, "ENOTSUP", 95),
            io::ErrorKind::ResourceBusy => (exceptions.os_error, "EBUSY", 16),
            io::ErrorKind::CrossesDevices => (exceptions.os_error, "EXDEV", 18),
            _ => (exceptions.os_error, "EIO", 5),
        };
        let code = vm
            .import("errno", 0)
            .and_then(|errno| errno.get_attr(name, vm))
            .unwrap_or_else(|_| vm.ctx.new_int(code).into());
        let mut args: Vec<PyObjectRef> = vec![code, vm.ctx.new_str(text(error)).into()];
        if let Some(path) = path {
            let path = String::from_utf8_lossy(path).into_owned();
            args.push(vm.ctx.new_str(path).into());
        }
        vm.new_exception(kind.to_owned(), args)
    }

    fn state<T>(vm: &VirtualMachine, run: impl FnOnce(&mut State) -> io::Result<T>) -> PyResult<T> {
        with(run).map_err(|error| failure(vm, &error, None))
    }

    fn files<T>(
        vm: &VirtualMachine,
        path: &[u8],
        run: impl FnOnce(&mut State) -> io::Result<T>,
    ) -> PyResult<T> {
        with(run).map_err(|error| failure(vm, &error, Some(path)))
    }

    #[pyfunction]
    fn env(vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        let env = state(vm, |state| Ok(state.env.clone()))?;
        let pairs = env
            .into_iter()
            .map(|(name, value)| {
                let pair = vec![
                    vm.ctx.new_bytes(name).into(),
                    vm.ctx.new_bytes(value).into(),
                ];
                vm.ctx.new_tuple(pair).into()
            })
            .collect();
        Ok(vm.ctx.new_list(pairs).into())
    }

    #[pyfunction]
    fn cwd(vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        state(vm, |state| Ok(state.cwd.clone())).map(|cwd| vm.ctx.new_bytes(cwd))
    }

    #[pyfunction]
    fn region(on: bool) -> bool {
        rustpython_vm::vm::region::set(on)
    }

    #[pyfunction]
    fn importing(on: bool) {
        let _ = with(|state| {
            state.import(on);
            Ok(())
        });
    }

    #[pyfunction]
    fn file_limit(vm: &VirtualMachine) -> PyResult<u64> {
        state(vm, |state| Ok(state.limits.file))
    }

    fn found(path: &[u8], follow: bool, vm: &VirtualMachine) -> PyResult<(u8, u64, u32, i64)> {
        let site = |error: io::Error| match crate::sitedata::stat(path) {
            Some(stat) if error.kind() == io::ErrorKind::NotFound => Ok(stat),
            _ => Err(error),
        };
        let found = crate::sitedata::stat(path).filter(|stat| stat.kind == Kind::File);
        let stat = match found {
            Some(stat) => Ok(stat),
            None if follow => files(vm, path, |state| state.files.stat(path).or_else(site)),
            None => files(vm, path, |state| state.files.lstat(path).or_else(site)),
        };
        stat.map(|stat| {
            let kind = match stat.kind {
                Kind::File => 0,
                Kind::Dir => 1,
                Kind::Symlink => 2,
            };
            (kind, stat.size, stat.mode, stat.mtime_ns)
        })
    }

    #[pyfunction]
    fn stat(path: ArgBytesLike, vm: &VirtualMachine) -> PyResult<(u8, u64, u32, i64)> {
        found(&path.borrow_buf(), true, vm)
    }

    #[pyfunction]
    fn lstat(path: ArgBytesLike, vm: &VirtualMachine) -> PyResult<(u8, u64, u32, i64)> {
        found(&path.borrow_buf(), false, vm)
    }

    #[pyfunction]
    fn symlink(target: ArgBytesLike, link: ArgBytesLike, vm: &VirtualMachine) -> PyResult<()> {
        let target = target.borrow_buf();
        let link = link.borrow_buf();
        files(vm, &link, |state| {
            state.files.symlink(&target, &link)?;
            state.changes.removed.remove(&*link);
            state.changes.written.insert(link.to_vec());
            Ok(())
        })
    }

    #[pyfunction]
    fn link(existing: ArgBytesLike, link: ArgBytesLike, vm: &VirtualMachine) -> PyResult<()> {
        let existing = existing.borrow_buf();
        let link = link.borrow_buf();
        files(vm, &link, |state| {
            state.files.hard_link(&existing, &link)?;
            state.changes.removed.remove(&*link);
            state.changes.written.insert(link.to_vec());
            Ok(())
        })
    }

    #[pyfunction]
    fn links(path: ArgBytesLike, follow: bool, vm: &VirtualMachine) -> PyResult<Option<(u64, u64)>> {
        let path = path.borrow_buf();
        state(vm, |state| Ok(state.files.links(&path, follow)))
    }

    #[pyfunction]
    fn readlink(path: ArgBytesLike, vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        let path = path.borrow_buf();
        let target = files(vm, &path, |state| state.files.read_link(&path))?;
        Ok(vm.ctx.new_bytes(target))
    }

    #[pyfunction]
    fn read(path: ArgBytesLike, vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        let path = path.borrow_buf();
        files(vm, &path, |state| {
            let data = match crate::sitedata::read(&path) {
                Some(data) => data?,
                None => state.files.read(&path, state.limits.file)?,
            };
            if data.len() as u64 > state.limits.file {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            Ok(data)
        })
        .map(|data| vm.ctx.new_bytes(data))
    }

    #[pyfunction]
    fn read_at(
        path: ArgBytesLike,
        offset: u64,
        len: usize,
        vm: &VirtualMachine,
    ) -> PyResult<PyBytesRef> {
        let path = path.borrow_buf();
        files(vm, &path, |state| match crate::sitedata::read(&path) {
            Some(data) => {
                let data = data?;
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(data.len());
                let end = start.saturating_add(len).min(data.len());
                Ok(data[start..end].to_vec())
            }
            None => state.files.read_at(&path, offset, len, state.limits.file),
        })
        .map(|data| vm.ctx.new_bytes(data))
    }

    #[pyfunction]
    fn ranged(vm: &VirtualMachine) -> PyResult<bool> {
        files(vm, b"", |state| Ok(state.files.ranged()))
    }

    #[pyfunction]
    fn write(path: ArgBytesLike, data: ArgBytesLike, how: u8, vm: &VirtualMachine) -> PyResult<()> {
        let path = path.borrow_buf();
        let data = data.borrow_buf();
        files(vm, &path, |state| {
            if data.len() as u64 > state.limits.file {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            let how = match how {
                0 => Write::Replace,
                1 => Write::Append,
                _ => Write::New,
            };
            state.files.write(&path, &data, how)?;
            state.changes.removed.remove(&*path);
            state.changes.written.insert(path.to_vec());
            Ok(())
        })
    }

    #[pyfunction]
    fn list(path: ArgBytesLike, vm: &VirtualMachine) -> PyResult<PyObjectRef> {
        let path = path.borrow_buf();
        let names = files(vm, &path, |state| {
            match (state.files.list(&path), crate::sitedata::list(&path)) {
                (Ok(names), Some(site)) => {
                    let mut names: std::collections::BTreeSet<Vec<u8>> =
                        names.into_iter().collect();
                    names.extend(site);
                    Ok(names.into_iter().collect())
                }
                (Err(error), Some(site)) if error.kind() == io::ErrorKind::NotFound => {
                    Ok(site.into_iter().collect())
                }
                (names, _) => names,
            }
        })?;
        let names = names
            .into_iter()
            .map(|name| vm.ctx.new_bytes(name).into())
            .collect();
        Ok(vm.ctx.new_list(names).into())
    }

    #[pyfunction]
    fn mkdir(path: ArgBytesLike, mode: u32, vm: &VirtualMachine) -> PyResult<()> {
        let path = path.borrow_buf();
        files(vm, &path, |state| {
            if crate::sitedata::stat(&path).is_some() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            state.files.mkdir(&path, mode)?;
            state.changes.removed.remove(&*path);
            state.changes.made.insert(path.to_vec());
            Ok(())
        })
    }

    #[pyfunction]
    fn remove(path: ArgBytesLike, dir: bool, vm: &VirtualMachine) -> PyResult<()> {
        let path = path.borrow_buf();
        files(vm, &path, |state| {
            let what = if dir { Remove::Dir } else { Remove::File };
            state.files.remove(&path, what)?;
            state.changes.written.remove(&*path);
            state.changes.made.remove(&*path);
            state.changes.removed.insert(path.to_vec());
            Ok(())
        })
    }

    #[pyfunction]
    fn chmod(path: ArgBytesLike, mode: u32, vm: &VirtualMachine) -> PyResult<()> {
        let path = path.borrow_buf();
        files(vm, &path, |state| match state.files.set_mode(&path, mode) {
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                state.files.stat(&path).map(|_| ())
            }
            done => done,
        })
    }

    #[pyfunction]
    fn utime(path: ArgBytesLike, mtime_ns: i64, vm: &VirtualMachine) -> PyResult<()> {
        let path = path.borrow_buf();
        files(vm, &path, |state| match state.files.set_mtime(&path, mtime_ns) {
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                state.files.stat(&path).map(|_| ())
            }
            done => done,
        })
    }

    #[pyfunction]
    fn rename(from: ArgBytesLike, to: ArgBytesLike, vm: &VirtualMachine) -> PyResult<()> {
        let from = from.borrow_buf();
        let to = to.borrow_buf();
        files(vm, &from, |state| {
            state.files.rename(&from, &to)?;
            state.changes.moved.push((from.to_vec(), to.to_vec()));
            Ok(())
        })
    }

    /// os._exit: ends the run at once with status. The VM stops as it does at a
    /// refused budget, so no except, finally, __exit__ or atexit runs after it.
    #[pyfunction]
    fn exit(status: i32, vm: &VirtualMachine) -> PyResult<()> {
        state(vm, |state| {
            state.exited.get_or_insert(status);
            Ok(())
        })?;
        Err(halt(vm))
    }

    #[pyfunction]
    fn halted(vm: &VirtualMachine) -> bool {
        vm.stopped.borrow().is_some()
    }

    #[pyfunction]
    fn spawns(vm: &VirtualMachine) -> PyResult<bool> {
        state(vm, |state| Ok(state.files.spawns()))
    }

    #[pyfunction]
    fn spawn(
        argv: Vec<PyBytesRef>,
        env: Vec<PyBytesRef>,
        cwd: PyBytesRef,
        stdin: ArgBytesLike,
        merged: bool,
        vm: &VirtualMachine,
    ) -> PyResult<(i32, PyBytesRef, PyBytesRef)> {
        let argv: Vec<Vec<u8>> = argv.iter().map(|word| word.as_bytes().to_vec()).collect();
        let name = argv.first().cloned().unwrap_or_default();
        let env = env
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[name, value]| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
            .collect();
        let child = Child {
            argv,
            env,
            cwd: cwd.as_bytes().to_vec(),
            stdin: stdin.borrow_buf().to_vec(),
            merged,
        };
        let host = state(vm, |state| Ok(state.files.clone()))?;
        let exited = host
            .spawn(child)
            .map_err(|error| failure(vm, &error, Some(&name)))?;
        Ok((
            exited.status,
            vm.ctx.new_bytes(exited.stdout),
            vm.ctx.new_bytes(exited.stderr),
        ))
    }

    #[pyfunction]
    fn input(size: isize, vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        state(vm, |state| state.take(usize::try_from(size).ok())).map(|data| vm.ctx.new_bytes(data))
    }

    #[pyfunction]
    fn output(fd: u8, data: ArgBytesLike, vm: &VirtualMachine) -> PyResult<usize> {
        let data = data.borrow_buf();
        let kept = state(vm, |state| {
            let used = state
                .written
                .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
            if used > state.limits.output {
                state.refused.get_or_insert(Limit::Output);
                return Ok(false);
            }
            state.put(fd, &data)?;
            Ok(true)
        })?;
        if kept { Ok(data.len()) } else { Err(halt(vm)) }
    }

    #[pyfunction]
    fn random(size: usize, vm: &VirtualMachine) -> PyResult<PyBytesRef> {
        vm.reserve(size)?;
        let mut bytes = vec![0; size];
        nanocode_platform::fill_random(&mut bytes).map_err(|error| failure(vm, &error, None))?;
        Ok(vm.ctx.new_bytes(bytes))
    }

    #[pyfunction]
    fn now() -> f64 {
        nanocode_platform::system_time()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64())
    }

    #[pyfunction]
    fn monotonic(vm: &VirtualMachine) -> PyResult<f64> {
        state(vm, |state| Ok(state.started.elapsed().as_secs_f64()))
    }

    #[pyfunction]
    fn sleep(seconds: f64, vm: &VirtualMachine) -> PyResult<()> {
        let duration = Duration::try_from_secs_f64(seconds)
            .map_err(|_| vm.new_value_error("sleep length must be non-negative"))?;
        let slept = state(vm, |state| {
            let slept = state.slept.saturating_add(duration);
            let left = state.limits.time.saturating_sub(state.started.elapsed());
            let waited = if slept > state.limits.sleep {
                Err(io::ErrorKind::TimedOut.into())
            } else {
                wait(duration.min(left), &state.stop)
            };
            match waited {
                Err(_) => {
                    state.refused.get_or_insert(Limit::Sleep);
                }
                Ok(false) => state.cancelled = true,
                Ok(true) if duration > left => {
                    state.refused.get_or_insert(Limit::Time);
                }
                Ok(true) => {}
            }
            state.slept = slept;
            Ok(!state.stopped())
        })?;
        if slept { Ok(()) } else { Err(halt(vm)) }
    }
}
