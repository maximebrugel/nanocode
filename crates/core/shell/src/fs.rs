use nanocode_bytes::Bytes;
use std::{future::Future, io, pin::Pin};

pub enum Reply<T> {
    Now(io::Result<T>),
    Later(Pin<Box<dyn Future<Output = io::Result<T>>>>),
}

impl<T: 'static> Reply<T> {
    pub async fn get(self) -> io::Result<T> {
        match self {
            Self::Now(result) => result,
            Self::Later(pending) => pending.await,
        }
    }

    pub fn map<U: 'static>(
        self,
        then: impl FnOnce(io::Result<T>) -> io::Result<U> + 'static,
    ) -> Reply<U> {
        match self {
            Self::Now(result) => Reply::Now(then(result)),
            Self::Later(pending) => Reply::Later(Box::pin(async move { then(pending.await) })),
        }
    }

    pub fn boxed(self) -> Pin<Box<dyn Future<Output = io::Result<T>>>> {
        match self {
            Self::Now(result) => Box::pin(std::future::ready(result)),
            Self::Later(pending) => pending,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub kind: Kind,
    pub size: u64,
    pub mode: u32,
    pub mtime_ns: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write {
    Replace,
    Append,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Remove {
    File,
    Dir,
}

pub const SHA256_BYTES: usize = 32;

pub struct FileDownload {
    pub bytes: Bytes,
    pub sha256: [u8; SHA256_BYTES],
}

pub trait FileUpload {
    fn push(&mut self, chunk: &[u8]) -> io::Result<()>;
    fn sha256(&self) -> [u8; SHA256_BYTES];
    fn commit(self: Box<Self>) -> Reply<()>;
}

/// A file [`FileSystem::write_files`] leaves holding `data`.
#[derive(Clone, Copy)]
pub struct WholeFile<'a> {
    pub path: &'a [u8],
    pub data: &'a [u8],
    /// Permission bits the file gets when this write makes it; one already there keeps
    /// its own.
    pub mode: u32,
}

/// The names one hard-linked file shares (see [`FileSystem::links`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Links {
    /// Identifies the file across its names, for its inode number.
    pub group: u64,
    /// How many names it has.
    pub names: u64,
}

/// What [`FileSystem::write_files`] did.
#[derive(Default)]
pub struct Wrote {
    /// How many of the leading files it wrote.
    pub files: usize,
    /// The directories it made, as their paths.
    pub made: Vec<Vec<u8>>,
    /// The files it made, by their place in the list.
    pub created: Vec<usize>,
}

pub trait FileSystem {
    fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat>;
    fn read(&self, path: &[u8], limit: u64) -> Reply<Bytes>;
    fn write(&self, path: &[u8], data: &[u8], how: Write) -> Reply<()>;
    fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>>;

    /// The names in the directory at `path`, each with its kind where the listing
    /// knows it (what `stat` without following says), so a walk needs no `stat` per
    /// name. The default knows no kind.
    fn list_kinds(&self, path: &[u8]) -> Reply<Vec<(Vec<u8>, Option<Kind>)>> {
        self.list(path)
            .map(|names| Ok(names?.into_iter().map(|name| (name, None)).collect()))
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> Reply<()>;
    fn remove(&self, path: &[u8], what: Remove) -> Reply<()>;
    fn rename(&self, from: &[u8], to: &[u8]) -> Reply<()>;

    fn symlink(&self, target: &[u8], link: &[u8]) -> Reply<()> {
        let _ = (target, link);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    fn read_link(&self, path: &[u8]) -> Reply<Vec<u8>> {
        let _ = path;
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    /// Makes `link` another name of the file `existing` (not following a symlink
    /// there), so a change through either shows through both.
    fn hard_link(&self, existing: &[u8], link: &[u8]) -> Reply<()> {
        let _ = (existing, link);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    /// The names the file at `path` shares with others through hard links, or
    /// `None` when it has only its own: its inode then comes from its path.
    fn links(&self, path: &[u8], follow: bool) -> Option<Links> {
        let _ = (path, follow);
        None
    }

    fn set_mode(&self, path: &[u8], mode: u32) -> Reply<()> {
        let _ = (path, mode);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    fn set_mtime(&self, path: &[u8], mtime_ns: i64) -> Reply<()> {
        let _ = (path, mtime_ns);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    fn upload(&self, path: &[u8], sha256: [u8; SHA256_BYTES]) -> Reply<Box<dyn FileUpload>> {
        let _ = (path, sha256);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    fn download(&self, path: &[u8], limit: u64) -> Reply<FileDownload> {
        let _ = (path, limit);
        Reply::Now(Err(io::ErrorKind::Unsupported.into()))
    }

    /// Writes the leading files of `files` in one step, each as `write` with
    /// [`Write::Replace`] would after `mkdir` made its missing directories with
    /// `dir_mode`, and a file it makes with the file's mode. It stops before a file the
    /// step cannot take, such as one behind a symlink; an error means it wrote none.
    /// A file system without such a step writes none.
    fn write_files(&self, files: &[WholeFile<'_>], dir_mode: u32) -> Reply<Wrote> {
        let _ = (files, dir_mode);
        Reply::Now(Ok(Wrote::default()))
    }

    fn answers_now(&self) -> bool {
        true
    }
}

pub fn normalize(path: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(path.len() + 1);
    for part in path.split(|&byte| byte == b'/') {
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

pub fn parent(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&byte| byte == b'/') {
        Some(0) | None => b"/",
        Some(cut) => &path[..cut],
    }
}

pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = Vec::with_capacity(dir.len() + 1 + name.len());
    path.extend_from_slice(dir);
    if !dir.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}
