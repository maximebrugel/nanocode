use crate::{
    answers::Answers,
    command::{Call, Commands, Session, Stream},
    file::{self, Device, bad_descriptor},
    fs::{FileSystem, Kind, Links, Remove, Reply, Stat, WholeFile, Write, join, normalize},
    machine::{Identity, Machine},
    shell::{Changes, Limit, Limits},
};
use nanocode_bytes::Bytes;
use psh_engine::{
    ControlHandle, HostFile,
    host::{
        Access, ChildFd, DirEntry, ExitStatus, ExtProc, ExtraMetadata, FileKind, HostFuture, HostInfo, KnownEntry, Metadata,
        OpenOptions, Rlimit, RunInShell, SIGKILL, Signal, SpawnSpec, Spawned,
    },
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    future::Future,
    io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
    time::Duration,
};

const PROGRAMS: &[u8] = b"/usr/bin/";
const SYMLINK_HOPS: u32 = 40;
const DEVICES: &[u8] = b"/dev";

pub(crate) struct Vfs {
    pub(crate) fs: RefCell<Rc<dyn FileSystem>>,
    pub(crate) commands: Rc<dyn Commands>,
    pub(crate) machine: Rc<dyn Machine>,
    pub(crate) identity: Identity,
    pub(crate) limits: Limits,
    pub(crate) strict: bool,
    pub(crate) operations: Cell<u64>,
    /// Counts the calls that may change a file's contents (writes, removes, renames),
    /// so an open file can tell whether its file changed since its own last write.
    pub(crate) writes: Cell<u64>,
    pub(crate) changes: RefCell<Changes>,
    pub(crate) missing: RefCell<Vec<Vec<u8>>>,
    pub(crate) refused: Cell<Option<Limit>>,
    pub(crate) answers: Option<Answers>,
    pub(crate) unanswered: RefCell<Option<String>>,
    pub(crate) control: OnceCell<ControlHandle>,
    pub(crate) spawned: Cell<u32>,
    pub(crate) without: Vec<Vec<u8>>,
    pub(crate) children: Cell<u64>,
    pub(crate) left: Cell<u64>,
    pub(crate) slice: Cell<u32>,
    pub(crate) utilities: &'static [psh_engine::util::UtilEntry],
    /// While a command Python started runs to completion inside one call, its
    /// sleeps are deadlines the run loop waits out itself (see
    /// [`crate::command::Session::run`]), since no executor drives timers then.
    pub(crate) timers: RefCell<Option<Vec<(Duration, Waker)>>>,
    /// How far such runs have skipped ahead of the machine's clock where the
    /// platform cannot wait in place (wasm): their sleeps end at once, in
    /// deadline order, and the shell's clock moves on by what they slept.
    pub(crate) skipped: Cell<Duration>,
}

impl Vfs {
    /// The machine's clock, plus what sleeps skipped where it cannot wait.
    pub(crate) fn now(&self) -> Duration {
        self.machine.now().saturating_add(self.skipped.get())
    }

    pub(crate) fn fs(&self) -> Rc<dyn FileSystem> {
        self.fs.borrow().clone()
    }

    pub(crate) fn refuse(&self, limit: Limit) {
        if self.refused.get().is_none() {
            self.refused.set(Some(limit));
        }
        if let Some(control) = self.control.get() {
            control.cancel();
        }
    }

    pub(crate) fn unanswer(&self, what: &[u8]) -> io::Error {
        self.unanswered
            .borrow_mut()
            .get_or_insert_with(|| String::from_utf8_lossy(what).into_owned());
        if let Some(control) = self.control.get() {
            control.cancel();
        }
        io::Error::other("this machine's answer is not known")
    }

    fn charge(&self) -> io::Result<()> {
        let used = self.operations.get() + 1;
        if used > self.limits.operations {
            self.refuse(Limit::Operations);
            return Err(io::Error::other("too many file operations"));
        }
        self.operations.set(used);
        Ok(())
    }

    fn change<T: 'static>(
        self: &Rc<Self>,
        reply: Reply<T>,
        record: impl FnOnce(&mut Changes) + 'static,
    ) -> Reply<T> {
        let vfs = self.clone();
        reply.map(move |result| {
            if result.is_ok() {
                record(&mut vfs.changes.borrow_mut());
            }
            result
        })
    }

    pub(crate) fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat> {
        match self.charge() {
            Ok(()) => self.fs().stat(path, follow),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn read(&self, path: &[u8]) -> Reply<Bytes> {
        let limit = self.limits.file;
        match self.charge() {
            Ok(()) => self.fs().read(path, limit).map(move |result| {
                result.and_then(|data| {
                    if data.len() as u64 > limit {
                        Err(io::ErrorKind::FileTooLarge.into())
                    } else {
                        Ok(data)
                    }
                })
            }),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn write(self: &Rc<Self>, path: &[u8], data: &[u8], how: Write) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                self.writes.set(self.writes.get() + 1);
                let reply = self.fs().write(path, data, how);
                if self.changes.borrow().written.contains(path) {
                    return reply;
                }
                let path = path.to_vec();
                self.change(reply, move |changes| changes.wrote(path))
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    /// Writes the leading files of `files` in one step where the file system has one
    /// (see [`FileSystem::write_files`]), as many as the operation and file limits let
    /// pass: how many it wrote. The caller writes the rest one by one, meeting a limit
    /// as a single write does.
    pub(crate) fn write_files(self: &Rc<Self>, files: &[WholeFile<'_>], dir_mode: u32) -> Reply<usize> {
        let left = self.limits.operations.saturating_sub(self.operations.get());
        let files = &files[..files
            .iter()
            .take(usize::try_from(left).unwrap_or(usize::MAX))
            .take_while(|file| file.data.len() as u64 <= self.limits.file)
            .count()];
        if files.is_empty() {
            return Reply::Now(Ok(0));
        }
        let paths: Vec<(Vec<u8>, bool)> = files
            .iter()
            .map(|file| (file.path.to_vec(), file.mode & 0o7777 != file::FILE_MODE))
            .collect();
        let vfs = self.clone();
        self.fs().write_files(files, dir_mode).map(move |result| {
            let wrote = result?;
            vfs.operations.set(vfs.operations.get() + wrote.files as u64);
            let mut changes = vfs.changes.borrow_mut();
            for dir in wrote.made {
                changes.removed.remove(&dir);
                changes.made.insert(dir);
            }
            for at in wrote.created {
                if let Some((path, true)) = paths.get(at) {
                    changes.modes.insert(path.clone());
                }
            }
            for (path, _) in paths.into_iter().take(wrote.files) {
                changes.wrote(path);
            }
            Ok(wrote.files)
        })
    }

    pub(crate) fn set_mode(self: &Rc<Self>, path: &[u8], mode: u32) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                let reply = self.fs().set_mode(path, mode);
                let path = path.to_vec();
                self.change(reply, move |changes| {
                    changes.modes.insert(path);
                })
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn mkdir(self: &Rc<Self>, path: Vec<u8>, mode: u32) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                let reply = self.fs().mkdir(&path, mode);
                self.change(reply, move |changes| {
                    changes.removed.remove(&path);
                    changes.made.insert(path);
                })
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn remove(self: &Rc<Self>, path: Vec<u8>, what: Remove) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                self.writes.set(self.writes.get() + 1);
                let reply = self.fs().remove(&path, what);
                self.change(reply, move |changes| changes.removed(path))
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn rename(self: &Rc<Self>, from: Vec<u8>, to: Vec<u8>) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                self.writes.set(self.writes.get() + 1);
                let reply = self.fs().rename(&from, &to);
                self.change(reply, move |changes| changes.moved(from, to))
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn symlink(self: &Rc<Self>, target: &[u8], link: Vec<u8>) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                let reply = self.fs().symlink(target, &link);
                self.change(reply, move |changes| {
                    changes.removed.remove(&link);
                    changes.linked.insert(link);
                })
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn hard_link(self: &Rc<Self>, existing: &[u8], link: Vec<u8>) -> Reply<()> {
        match self.charge() {
            Ok(()) => {
                self.writes.set(self.writes.get() + 1);
                let reply = self.fs().hard_link(existing, &link);
                self.change(reply, move |changes| changes.wrote(link))
            }
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn set_mtime(&self, path: &[u8], mtime_ns: i64) -> Reply<()> {
        match self.charge() {
            Ok(()) => self.fs().set_mtime(path, mtime_ns),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn lacks(&self, name: &[u8]) {
        self.missing.borrow_mut().push(name.to_vec());
        if self.strict {
            self.refuse(Limit::Missing);
        }
    }

    pub(crate) fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>> {
        match self.charge() {
            Ok(()) => self.fs().list(path),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn list_kinds(&self, path: &[u8]) -> Reply<Vec<(Vec<u8>, Option<Kind>)>> {
        match self.charge() {
            Ok(()) => self.fs().list_kinds(path),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    pub(crate) fn read_link(&self, path: &[u8]) -> Reply<Vec<u8>> {
        match self.charge() {
            Ok(()) => self.fs().read_link(path),
            Err(error) => Reply::Now(Err(error)),
        }
    }

    async fn canonical(self: Rc<Self>, path: Vec<u8>) -> io::Result<Vec<u8>> {
        let mut rest: Vec<Vec<u8>> = path
            .split(|&byte| byte == b'/')
            .filter(|part| !part.is_empty())
            .rev()
            .map(<[u8]>::to_vec)
            .collect();
        let mut found = Vec::new();
        let mut hops = 0;
        while let Some(part) = rest.pop() {
            let next = join(if found.is_empty() { b"/" } else { &found }, &part);
            if self.stat(&next, false).get().await?.kind != Kind::Symlink {
                found = next;
                continue;
            }
            hops += 1;
            if hops > SYMLINK_HOPS {
                return Err(io::Error::other("Too many levels of symbolic links"));
            }
            let target = self.read_link(&next).get().await?;
            let target = if target.starts_with(b"/") {
                normalize(&target)
            } else {
                normalize(&join(if found.is_empty() { b"/" } else { &found }, &target))
            };
            found.clear();
            rest.extend(
                target
                    .split(|&byte| byte == b'/')
                    .filter(|part| !part.is_empty())
                    .rev()
                    .map(<[u8]>::to_vec),
            );
        }
        if found.is_empty() {
            found.push(b'/');
        }
        Ok(found)
    }
}

fn inode(path: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in path {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The inode of a hard-linked file, the same through each of its names.
fn shared_inode(links: Links) -> u64 {
    inode(format!("\0link {}", links.group).as_bytes())
}

const fn file_kind(kind: Kind) -> FileKind {
    match kind {
        Kind::File => FileKind::File,
        Kind::Dir => FileKind::Dir,
        Kind::Symlink => FileKind::Symlink,
    }
}

fn metadata(path: &[u8], stat: Stat, links: Option<Links>, info: &HostInfo) -> Metadata {
    Metadata {
        kind: file_kind(stat.kind),
        size: stat.size,
        mode: stat.mode,
        mtime_ns: stat.mtime_ns,
        dev: 1,
        ino: links.map_or_else(|| inode(path), shared_inode),
        uid: info.uid,
        gid: info.groups.first().copied().unwrap_or(info.uid),
    }
}

fn device(path: &[u8]) -> Metadata {
    Metadata {
        kind: FileKind::CharDevice,
        size: 0,
        mode: 0o666,
        mtime_ns: 0,
        dev: 2,
        ino: inode(path),
        uid: 0,
        gid: 0,
    }
}

pub(crate) struct Host(pub(crate) Rc<Vfs>);

/// The limits a stock Linux machine gives a process, for the ones every such machine
/// shares (no core dumps by default, the rest unlimited), so `ulimit -f` and `ulimit -c`
/// work as there; the machine-dependent ones (open files, processes, stack) are absent.
/// What `ulimit` sets stays in the shell, as nothing here enforces it.
const fn sandbox_rlimit(resource: Rlimit) -> Option<(psh_engine::host::Limit, psh_engine::host::Limit)> {
    match resource {
        Rlimit::Core => Some((Some(0), None)),
        Rlimit::FileSize | Rlimit::Data | Rlimit::Rss | Rlimit::As | Rlimit::Cpu | Rlimit::Locks => {
            Some((None, None))
        }
        _ => None,
    }
}

impl psh_engine::Host for Host {
    fn open(&self, path: &[u8], opts: OpenOptions) -> HostFuture<io::Result<Rc<dyn HostFile>>> {
        let path = normalize(path);
        if let Some(device) = Device::at(&path, &self.0.machine) {
            return Box::pin(std::future::ready(Ok(Rc::new(device) as Rc<dyn HostFile>)));
        }
        Box::pin(file::open(self.0.clone(), path, opts))
    }

    fn write_files(
        &self,
        files: &[psh_engine::host::WholeFile<'_>],
        dir_mode: u32,
    ) -> HostFuture<io::Result<usize>> {
        let paths: Vec<Vec<u8>> = files
            .iter()
            .map(|file| normalize(file.path))
            .take_while(|path| Device::at(path, &self.0.machine).is_none())
            .collect();
        let files: Vec<WholeFile<'_>> = files
            .iter()
            .zip(&paths)
            .map(|(file, path)| WholeFile {
                path,
                data: file.data,
                mode: file.mode,
            })
            .collect();
        self.0.write_files(&files, dir_mode).boxed()
    }

    fn stat(&self, path: &[u8], follow: bool) -> HostFuture<io::Result<Metadata>> {
        let path = normalize(path);
        let vfs = self.0.clone();
        if Device::at(&path, &vfs.machine).is_some() {
            return Box::pin(std::future::ready(Ok(device(&path))));
        }
        self.0
            .stat(&path, follow)
            .map(move |result| match result {
                Ok(stat) => {
                    let links = vfs.fs().links(&path, follow);
                    Ok(metadata(&path, stat, links, &vfs.identity.info))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && path == DEVICES => {
                    Ok(metadata(
                        &path,
                        Stat {
                            kind: Kind::Dir,
                            size: 0,
                            mode: 0o755,
                            mtime_ns: 0,
                        },
                        None,
                        &vfs.identity.info,
                    ))
                }
                Err(error) => Err(error),
            })
            .boxed()
    }

    fn access(&self, path: &[u8], mode: Access) -> HostFuture<io::Result<bool>> {
        let path = normalize(path);
        if Device::at(&path, &self.0.machine).is_some() {
            return Box::pin(std::future::ready(Ok(mode != Access::Execute)));
        }
        self.0
            .stat(&path, true)
            .map(move |result| match result {
                Ok(stat) => Ok(match mode {
                    Access::Read | Access::Write => true,
                    Access::Execute => stat.kind == Kind::Dir || stat.mode & 0o111 != 0,
                }),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            })
            .boxed()
    }

    fn read_dir(&self, path: &[u8]) -> HostFuture<io::Result<Vec<DirEntry>>> {
        let path = normalize(path);
        let devices = path == DEVICES;
        self.0
            .list(&path)
            .map(move |result| {
                let mut names = match result {
                    Ok(names) => names,
                    Err(error) if devices && error.kind() == io::ErrorKind::NotFound => Vec::new(),
                    Err(error) => return Err(error),
                };
                if devices {
                    names.extend(Device::NAMES.iter().map(|name| name.to_vec()));
                    names.sort();
                    names.dedup();
                }
                Ok(names.into_iter().map(|name| DirEntry { name }).collect())
            })
            .boxed()
    }

    fn read_dir_kinds(&self, path: &[u8]) -> HostFuture<io::Result<Vec<KnownEntry>>> {
        let path = normalize(path);
        if path == DEVICES {
            // Devices are not in the file system's listing; read_dir adds them.
            let listed = self.read_dir(&path);
            return Box::pin(async move { Ok(listed.await?.into_iter().map(|e| (e, None)).collect()) });
        }
        self.0
            .list_kinds(&path)
            .map(|result| {
                Ok(result?
                    .into_iter()
                    .map(|(name, kind)| (DirEntry { name }, kind.map(file_kind)))
                    .collect())
            })
            .boxed()
    }

    fn canonicalize(&self, path: &[u8]) -> HostFuture<io::Result<Vec<u8>>> {
        Box::pin(self.0.clone().canonical(normalize(path)))
    }

    fn resolve(&self, name: &[u8], path: &[u8], cwd: &[u8]) -> HostFuture<Option<Vec<u8>>> {
        if self.0.commands.has(name) {
            return Box::pin(std::future::ready(Some(join(PROGRAMS, name))));
        }
        let dir = if path.starts_with(b"/") {
            path.to_vec()
        } else {
            join(cwd, path)
        };
        let found = normalize(&join(&dir, name));
        self.0
            .stat(&found, true)
            .map(move |result| match result {
                Ok(stat) if stat.kind == Kind::File && stat.mode & 0o111 != 0 => Ok(Some(found)),
                _ => Ok(None),
            })
            .boxed_or(None)
    }

    fn spawn(&self, spec: SpawnSpec) -> io::Result<Spawned> {
        let name = spec.program.strip_prefix(PROGRAMS).unwrap_or(&spec.program);
        let answered = self
            .0
            .answers
            .as_ref()
            .is_none_or(|answers| str::from_utf8(name).is_ok_and(|name| answers.runs(name)));
        if !answered {
            return Err(self.0.unanswer(name));
        }
        let known = spec
            .program
            .strip_prefix(PROGRAMS)
            .is_some_and(|name| self.0.commands.has(name))
            || self.0.commands.has(&spec.program);
        if !known {
            return Err(RunInShell::error());
        }
        let mut given: Vec<(u32, Rc<dyn HostFile>)> = Vec::new();
        let mut pipes = Vec::new();
        for (number, descriptor) in spec.fds {
            let theirs: Rc<dyn HostFile> = match descriptor {
                ChildFd::File(file) => file,
                ChildFd::Dup(other) => given
                    .iter()
                    .find(|(known, _)| *known == other)
                    .map(|(_, file)| file.clone())
                    .ok_or(io::ErrorKind::InvalidInput)?,
                ChildFd::Null | ChildFd::Closed => Rc::new(Device::Null),
                ChildFd::Pipe { input } => {
                    let (reader, writer) = file::pipe();
                    if input {
                        pipes.push((number, Rc::new(writer) as Rc<dyn HostFile>));
                        Rc::new(reader)
                    } else {
                        pipes.push((number, Rc::new(reader) as Rc<dyn HostFile>));
                        Rc::new(writer)
                    }
                }
            };
            given.push((number, theirs));
        }
        let stream = |number: u32| {
            Stream(
                given
                    .iter()
                    .find(|(known, _)| *known == number)
                    .map_or_else(
                        || Rc::new(Closed) as Rc<dyn HostFile>,
                        |(_, file)| file.clone(),
                    ),
            )
        };
        let call = Call {
            argv: spec.argv,
            env: spec.env,
            cwd: spec.cwd,
            stdin: stream(0),
            stdout: stream(1),
            stderr: stream(2),
            shell: Session(self.0.clone()),
        };
        let spawned = self.0.spawned.get() + 1;
        self.0.spawned.set(spawned);
        Ok(Spawned {
            proc: Box::new(Process {
                pid: self.0.identity.pid + spawned,
                run: Some(self.0.commands.run(call)),
                status: None,
                waiting: None,
            }),
            pipes,
        })
    }

    fn info(&self) -> HostInfo {
        self.0.identity.info.clone()
    }

    fn rlimit(&self, resource: Rlimit) -> Option<(psh_engine::host::Limit, psh_engine::host::Limit)> {
        sandbox_rlimit(resource)
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> HostFuture<io::Result<()>> {
        self.0.mkdir(normalize(path), mode).boxed()
    }

    fn remove_file(&self, path: &[u8]) -> HostFuture<io::Result<()>> {
        self.remove(path, Remove::File)
    }

    fn remove_dir(&self, path: &[u8]) -> HostFuture<io::Result<()>> {
        self.remove(path, Remove::Dir)
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> HostFuture<io::Result<()>> {
        self.0.rename(normalize(from), normalize(to)).boxed()
    }

    fn symlink(&self, target: &[u8], link: &[u8]) -> HostFuture<io::Result<()>> {
        self.0.symlink(target, normalize(link)).boxed()
    }

    fn hard_link(&self, existing: &[u8], link: &[u8]) -> HostFuture<io::Result<()>> {
        self.0.hard_link(&normalize(existing), normalize(link)).boxed()
    }

    fn stat_extra(
        &self,
        path: &[u8],
        follow: bool,
    ) -> HostFuture<io::Result<(Metadata, ExtraMetadata)>> {
        let stat = psh_engine::Host::stat(self, path, follow);
        let vfs = self.0.clone();
        let path = normalize(path);
        Box::pin(async move {
            let metadata = stat.await?;
            let mut extra = ExtraMetadata::derived(&metadata);
            if let Some(links) = vfs.fs().links(&path, follow) {
                extra.nlink = links.names;
            }
            Ok((metadata, extra))
        })
    }

    fn read_link(&self, path: &[u8]) -> HostFuture<io::Result<Vec<u8>>> {
        self.0.read_link(&normalize(path)).boxed()
    }

    fn set_mode(&self, path: &[u8], mode: u32) -> HostFuture<io::Result<()>> {
        self.0.set_mode(&normalize(path), mode).boxed()
    }

    fn set_times(
        &self,
        path: &[u8],
        _atime_ns: Option<i64>,
        mtime_ns: Option<i64>,
        _follow: bool,
    ) -> HostFuture<io::Result<()>> {
        let Some(mtime_ns) = mtime_ns else {
            return Box::pin(std::future::ready(Ok(())));
        };
        self.0.set_mtime(&normalize(path), mtime_ns).boxed()
    }

    fn user_name(&self, uid: u32) -> Option<Vec<u8>> {
        (uid == self.0.identity.info.uid).then(|| self.0.identity.user.clone())
    }

    fn group_name(&self, gid: u32) -> Option<Vec<u8>> {
        (self.0.identity.info.groups.first() == Some(&gid)).then(|| self.0.identity.group.clone())
    }

    fn utc_offset(&self, epoch_secs: i64) -> i32 {
        self.0.machine.utc_offset(epoch_secs)
    }

    fn now(&self) -> Duration {
        self.0.now()
    }

    fn sleep(&self, duration: Duration) -> HostFuture<()> {
        if self.0.timers.borrow().is_none() {
            return self.0.machine.sleep(duration);
        }
        let vfs = self.0.clone();
        let until = vfs.now().saturating_add(duration);
        Box::pin(std::future::poll_fn(move |cx| {
            if vfs.now() >= until {
                return Poll::Ready(());
            }
            if let Some(timers) = vfs.timers.borrow_mut().as_mut() {
                timers.push((until, cx.waker().clone()));
            }
            Poll::Pending
        }))
    }

    fn pid(&self) -> u32 {
        self.0.identity.pid
    }

    fn home_dir(&self, user: &[u8]) -> Option<Vec<u8>> {
        (user == self.0.identity.user).then(|| self.0.identity.home.clone())
    }

    fn not_found(&self, name: &[u8]) {
        self.0.lacks(name);
    }
}

impl Host {
    fn remove(&self, path: &[u8], what: Remove) -> HostFuture<io::Result<()>> {
        self.0.remove(normalize(path), what).boxed()
    }
}

trait BoxedOr<T> {
    fn boxed_or(self, fallback: T) -> HostFuture<T>;
}

impl<T: 'static> BoxedOr<T> for Reply<T> {
    fn boxed_or(self, fallback: T) -> HostFuture<T> {
        match self {
            Self::Now(result) => Box::pin(std::future::ready(result.unwrap_or(fallback))),
            Self::Later(pending) => Box::pin(async move { pending.await.unwrap_or(fallback) }),
        }
    }
}

struct Closed;

impl HostFile for Closed {
    fn poll_read(&self, _cx: &mut Context<'_>, _buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }

    fn poll_write(&self, _cx: &mut Context<'_>, _buf: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(bad_descriptor()))
    }
}

struct Process {
    pid: u32,
    run: Option<Pin<Box<dyn Future<Output = i32>>>>,
    status: Option<ExitStatus>,
    waiting: Option<Waker>,
}

impl ExtProc for Process {
    fn poll_wait(&mut self, cx: &mut Context<'_>) -> Poll<ExitStatus> {
        if let Some(status) = self.status {
            return Poll::Ready(status);
        }
        let Some(run) = self.run.as_mut() else {
            return Poll::Ready(ExitStatus::Signaled(SIGKILL));
        };
        match run.as_mut().poll(cx) {
            Poll::Ready(code) => {
                self.run = None;
                let status = ExitStatus::Exited(code);
                self.status = Some(status);
                Poll::Ready(status)
            }
            Poll::Pending => {
                self.waiting = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    fn signal(&mut self, sig: Signal) {
        if self.status.is_none() {
            self.run = None;
            self.status = Some(ExitStatus::Signaled(sig));
            if let Some(waker) = self.waiting.take() {
                waker.wake();
            }
        }
    }

    fn release(self: Box<Self>) {}

    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }
}
