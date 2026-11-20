use crate::{
    model::{Caps, Change, Digest, Entry, MODE_BITS, Manifest, Op, Refusal, parent},
    sim::{Mutation, mutated},
    store::{Batch, Blob, Failure, Store},
};
use nanocode_bytes::{Bytes, BytesMut};
use nanocode_digest::Sha256;
use nanocode_shell::{
    FileDownload, FileSystem, FileUpload, Kind, Links, Machine, Remove, Reply, SHA256_BYTES, Stat,
    WholeFile, Write, Wrote, normalize,
};
use nanocode_sync::Semaphore;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io,
    num::NonZeroUsize,
    rc::Rc,
    sync::Arc,
};

pub const SYMLINK_HOPS: u32 = 40;
const FILE_MODE: u32 = 0o644;
const ROOT_MODE: u32 = 0o755;
const LINK_MODE: u32 = 0o777;

struct Inner {
    store: Arc<dyn Store>,
    workspace: String,
    caps: Caps,
    machine: Rc<dyn Machine>,
    manifest: RefCell<Manifest>,
    blobs: RefCell<BTreeMap<Digest, Blob>>,
    /// The hash state over the bytes of the blob `write` made last, with its digest,
    /// so that appending to that blob hashes only the new bytes.
    tail: RefCell<Option<(Digest, Sha256)>>,
    gate: Semaphore,
}

#[derive(Clone)]
pub struct WorkspaceFs {
    inner: Rc<Inner>,
}

pub struct Upload {
    fs: WorkspaceFs,
    path: Vec<u8>,
    expected: Digest,
    mode: Option<u32>,
    hasher: Sha256,
    data: BytesMut,
    size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub digest: Digest,
    pub mode: u32,
    pub mtime_ns: i64,
    pub bytes: Bytes,
}

impl Download {
    pub fn chunks(&self, chunk: NonZeroUsize) -> impl Iterator<Item = Bytes> + '_ {
        let chunk = chunk.get();
        (0..self.bytes.len())
            .step_by(chunk)
            .map(move |at| self.bytes.slice(at..self.bytes.len().min(at + chunk)))
    }
}

fn invalid(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

fn missing<T>() -> io::Result<T> {
    Err(io::ErrorKind::NotFound.into())
}

fn key(path: &[u8]) -> io::Result<String> {
    let path = normalize(path);
    let text = std::str::from_utf8(&path).map_err(|_| invalid("workspace paths are UTF-8"))?;
    Ok(text.trim_start_matches('/').to_owned())
}

fn resolve(manifest: &Manifest, path: &str, follow: bool) -> io::Result<String> {
    if plain(manifest, path) {
        return Ok(path.to_owned());
    }
    resolve_links(manifest, path, follow)
}

/// Whether `path` already names itself: every part a plain name and no symlink on
/// the way, so resolving it walks no link and changes nothing.
fn plain(manifest: &Manifest, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    if !manifest.has_symlinks() {
        return path.split('/').all(|part| !matches!(part, "" | "." | ".."));
    }
    let mut end = 0;
    for part in path.split('/') {
        if matches!(part, "" | "." | "..") {
            return false;
        }
        end += part.len();
        if let Some(Entry::Symlink { .. }) = manifest.get(&path[..end]) {
            return false;
        }
        end += 1;
    }
    true
}

fn resolve_links(manifest: &Manifest, path: &str, follow: bool) -> io::Result<String> {
    let mut rest: Vec<String> = path
        .split('/')
        .filter(|part| !part.is_empty())
        .rev()
        .map(str::to_owned)
        .collect();
    let mut found: Vec<String> = Vec::new();
    let mut hops = 0;
    while let Some(part) = rest.pop() {
        match part.as_str() {
            "." => continue,
            ".." => {
                found.pop();
                continue;
            }
            _ => found.push(part),
        }
        let Some(Entry::Symlink { target, .. }) = manifest.get(&found.join("/")) else {
            continue;
        };
        if rest.is_empty() && !follow {
            break;
        }
        hops += 1;
        if hops > SYMLINK_HOPS {
            return Err(io::Error::other("Too many levels of symbolic links"));
        }
        found.pop();
        if target.starts_with('/') {
            found.clear();
        }
        rest.extend(
            target
                .split('/')
                .filter(|part| !part.is_empty())
                .rev()
                .map(str::to_owned),
        );
    }
    Ok(found.join("/"))
}

const fn stat_of(entry: &Entry) -> Stat {
    match entry {
        Entry::File {
            size,
            mode,
            mtime_ns,
            ..
        } => Stat {
            kind: Kind::File,
            size: *size,
            mode: *mode,
            mtime_ns: *mtime_ns,
        },
        Entry::Dir { mode, mtime_ns } => Stat {
            kind: Kind::Dir,
            size: 0,
            mode: *mode,
            mtime_ns: *mtime_ns,
        },
        Entry::Symlink { target, mtime_ns } => Stat {
            kind: Kind::Symlink,
            size: target.len() as u64,
            mode: LINK_MODE,
            mtime_ns: *mtime_ns,
        },
    }
}

fn dir_exists(manifest: &Manifest, path: &str) -> io::Result<()> {
    let Some(dir) = parent(path) else {
        return Ok(());
    };
    let dir = resolve(manifest, dir, true)?;
    if dir.is_empty() {
        return Ok(());
    }
    match manifest.get(&dir) {
        Some(Entry::Dir { .. }) => Ok(()),
        Some(_) => Err(io::ErrorKind::NotADirectory.into()),
        None => missing(),
    }
}

fn touch(ops: &mut Vec<Op>, path: &str, mtime_ns: i64) {
    if let Some(dir) = parent(path) {
        ops.push(Op::SetMtime {
            path: dir.to_owned(),
            mtime_ns,
        });
    }
}

/// What writing the leading files of a list in one commit takes: for each, the
/// directories it makes (outermost first) and the mode of the file already there.
struct Planned {
    files: Vec<(String, Vec<String>, Option<u32>)>,
}

/// The leading files whose writes one commit can take as their one-by-one writes
/// would end: no symlink on the way, no directory or other file where a directory or
/// the file goes, each path once, within the commit's operations. It stops at the
/// first file that is not so.
fn plan(manifest: &Manifest, caps: &Caps, paths: impl Iterator<Item = io::Result<String>>) -> Planned {
    let mut files = Vec::new();
    let mut ops = 0;
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut made: BTreeSet<String> = BTreeSet::new();
    for path in paths {
        let Ok(path) = path else { break };
        if path.is_empty() || written.contains(&path) || made.contains(&path) {
            break;
        }
        if !resolve(manifest, &path, true).is_ok_and(|found| found == path) {
            break;
        }
        let held = match manifest.get(&path) {
            None => None,
            Some(Entry::File { mode, .. }) => Some(*mode),
            Some(_) => break,
        };
        let mut missing = Vec::new();
        let mut at = parent(&path);
        let mut clear = true;
        while let Some(dir) = at {
            if made.contains(dir) {
                break;
            }
            if written.contains(dir) {
                clear = false;
                break;
            }
            match manifest.get(dir) {
                Some(Entry::Dir { .. }) => break,
                Some(_) => {
                    clear = false;
                    break;
                }
                None => missing.push(dir.to_owned()),
            }
            at = parent(dir);
        }
        let more = 2 * (missing.len() + 1);
        if !clear || ops + more > caps.ops {
            break;
        }
        ops += more;
        missing.reverse();
        made.extend(missing.iter().cloned());
        written.insert(path.clone());
        files.push((path, missing, held));
    }
    Planned { files }
}

impl WorkspaceFs {
    pub async fn open(
        store: Arc<dyn Store>,
        workspace: impl Into<String>,
        caps: Caps,
        machine: Rc<dyn Machine>,
    ) -> io::Result<Self> {
        let workspace = workspace.into();
        let manifest = store.load(&workspace).await?;
        Ok(Self {
            inner: Rc::new(Inner {
                store,
                workspace,
                caps,
                machine,
                manifest: RefCell::new(manifest),
                blobs: RefCell::new(BTreeMap::new()),
                tail: RefCell::new(None),
                gate: Semaphore::new(1),
            }),
        })
    }

    #[must_use]
    pub fn manifest(&self) -> Manifest {
        self.inner.manifest.borrow().clone()
    }

    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.manifest.borrow().version()
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.inner.workspace
    }

    #[must_use]
    pub fn caps(&self) -> Caps {
        self.inner.caps
    }

    pub async fn refresh(&self) -> io::Result<u64> {
        let inner = &self.inner;
        let _turn = inner.gate.acquire().await.map_err(io::Error::other)?;
        inner.reload().await
    }

    pub async fn commit(&self, ops: Vec<Op>, fresh: Vec<Blob>) -> io::Result<u64> {
        let inner = &self.inner;
        let _turn = inner.gate.acquire().await.map_err(io::Error::other)?;
        inner.commit(&ops, fresh).await
    }

    pub async fn bytes(&self, path: &[u8]) -> io::Result<Bytes> {
        let (_, bytes) = self.inner.file(path).await?;
        Ok(bytes)
    }

    #[must_use]
    pub fn upload(&self, path: &[u8], expected: Digest, mode: u32) -> Upload {
        Upload {
            fs: self.clone(),
            path: path.to_vec(),
            expected,
            mode: Some(mode),
            hasher: Sha256::new(),
            data: BytesMut::new(),
            size: 0,
        }
    }

    pub async fn download(&self, path: &[u8]) -> io::Result<Download> {
        let (entry, bytes) = self.inner.file(path).await?;
        let Entry::File {
            digest,
            mode,
            mtime_ns,
            ..
        } = entry
        else {
            unreachable!("file() answers files only");
        };
        Ok(Download {
            digest,
            mode,
            mtime_ns,
            bytes,
        })
    }
}

impl Upload {
    pub fn push(&mut self, chunk: &[u8]) -> io::Result<()> {
        let size = self.size + chunk.len() as u64;
        let max = self.fs.inner.caps.file_bytes;
        if size > max {
            return Err(Refusal::FileTooLarge {
                path: String::from_utf8_lossy(&self.path).into_owned(),
                size,
                max,
            }
            .into());
        }
        self.size = size;
        self.hasher.update(chunk);
        self.data.extend_from_slice(chunk);
        Ok(())
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest(self.hasher.finalize())
    }

    pub async fn finish(self) -> io::Result<u64> {
        let digest = self.digest();
        if digest != self.expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "uploaded bytes hash to {}, not the expected {}",
                    digest.hex(),
                    self.expected.hex()
                ),
            ));
        }
        let inner = &self.fs.inner;
        let _turn = inner.gate.acquire().await.map_err(io::Error::other)?;
        let path = key(&self.path)?;
        let now = inner.now();
        let ops = {
            let manifest = inner.manifest.borrow();
            let path = resolve(&manifest, &path, true)?;
            if path.is_empty() {
                return Err(io::ErrorKind::IsADirectory.into());
            }
            dir_exists(&manifest, &path)?;
            let mode = match (self.mode, manifest.get(&path)) {
                (_, Some(Entry::Dir { .. })) => return Err(io::ErrorKind::IsADirectory.into()),
                (Some(mode), _) => mode,
                (None, Some(Entry::File { mode, .. })) => *mode,
                (None, _) => FILE_MODE,
            };
            let mut ops = vec![Op::Put {
                path: path.clone(),
                digest,
                size: self.size,
                mode: mode & MODE_BITS,
                mtime_ns: now,
            }];
            touch(&mut ops, &path, now);
            ops
        };
        let blob = Blob::verified(digest, self.data.freeze())?;
        inner.commit(&ops, vec![blob]).await
    }
}

impl FileUpload for Upload {
    fn push(&mut self, chunk: &[u8]) -> io::Result<()> {
        Self::push(self, chunk)
    }

    fn sha256(&self) -> [u8; SHA256_BYTES] {
        self.digest().0
    }

    fn commit(self: Box<Self>) -> Reply<()> {
        Reply::Later(Box::pin(async move { self.finish().await.map(drop) }))
    }
}

impl Inner {
    fn now(&self) -> i64 {
        i64::try_from(self.machine.now().as_nanos()).unwrap_or(i64::MAX)
    }

    async fn reload(&self) -> io::Result<u64> {
        let manifest = self.store.load(&self.workspace).await?;
        let version = manifest.version();
        self.blobs
            .borrow_mut()
            .retain(|digest, _| manifest.holds(digest));
        *self.manifest.borrow_mut() = manifest;
        Ok(version)
    }

    async fn commit(&self, ops: &[Op], fresh: Vec<Blob>) -> io::Result<u64> {
        let change = {
            let manifest = self.manifest.borrow();
            manifest.apply(manifest.version(), ops, &self.caps)?
        };
        let fresh: BTreeMap<Digest, Blob> = fresh
            .into_iter()
            .map(|blob| (*blob.digest(), blob))
            .collect();
        let batch = {
            let cached = self.blobs.borrow();
            Batch::new(change, |digest| {
                fresh.get(digest).or_else(|| cached.get(digest)).cloned()
            })
            .map_err(Failure::Missing)?
        };
        match self.store.commit(&self.workspace, &batch).await {
            Ok(version) => {
                assert_eq!(
                    version,
                    batch.change().version,
                    "a store commits the next version"
                );
                {
                    let mut blobs = self.blobs.borrow_mut();
                    for blob in batch.blobs() {
                        blobs.insert(*blob.digest(), blob.clone());
                    }
                    for digest in &batch.change().dropped {
                        blobs.remove(digest);
                    }
                }
                self.advance(batch.change()).await;
                Ok(version)
            }
            Err(failure) => {
                if mutated(Mutation::VisibleBeforeCommit) {
                    self.advance(batch.change()).await;
                    return Err(failure.into());
                }
                if mutated(Mutation::BackendFailureAcked) && matches!(failure, Failure::Backend(_)) {
                    self.advance(batch.change()).await;
                    return Ok(batch.change().version);
                }
                if mutated(Mutation::StaleRetryOverwrites)
                    && matches!(failure, Failure::Refused(Refusal::Stale { .. }))
                {
                    self.reload().await?;
                    return Box::pin(self.commit(ops, batch.blobs().to_vec())).await;
                }
                if let Failure::Refused(Refusal::Stale { .. }) | Failure::Backend(_) = &failure {
                    let _ = self.reload().await;
                }
                Err(failure.into())
            }
        }
    }

    async fn advance(&self, change: &Change) {
        let current = self.manifest.borrow().version() == change.base;
        if current {
            self.manifest.borrow_mut().advance(change);
        } else {
            let _ = self.reload().await;
        }
    }

    async fn blob(&self, digest: &Digest) -> Result<Bytes, Failure> {
        if let Some(blob) = self.blobs.borrow().get(digest) {
            return Ok(blob.bytes().clone());
        }
        let blob = self.store.blob(digest).await?;
        if blob.digest() != digest {
            return Err(Failure::Missing(*digest));
        }
        let bytes = blob.bytes().clone();
        if self.manifest.borrow().holds(digest) {
            self.blobs.borrow_mut().insert(*digest, blob);
        }
        Ok(bytes)
    }

    fn lookup(&self, path: &[u8], follow: bool) -> io::Result<(String, Option<Entry>)> {
        let path = key(path)?;
        let manifest = self.manifest.borrow();
        let path = resolve(&manifest, &path, follow)?;
        let entry = manifest.get(&path).cloned();
        Ok((path, entry))
    }

    async fn file(&self, path: &[u8]) -> io::Result<(Entry, Bytes)> {
        match self.held(path).await? {
            (entry, Ok(bytes)) => Ok((entry, bytes)),
            (_, Err(Failure::Missing(_))) if !mutated(Mutation::StaleReadSkipsReload) => {
                self.reload().await?;
                let (entry, bytes) = self.held(path).await?;
                Ok((entry, bytes?))
            }
            (_, Err(failure)) => Err(failure.into()),
        }
    }

    async fn held(&self, path: &[u8]) -> io::Result<(Entry, Result<Bytes, Failure>)> {
        match self.lookup(path, true)? {
            (path, _) if path.is_empty() => Err(io::ErrorKind::IsADirectory.into()),
            (_, Some(entry @ Entry::File { digest, .. })) => {
                let bytes = self.blob(&digest).await;
                Ok((entry, bytes))
            }
            (_, Some(Entry::Dir { .. })) => Err(io::ErrorKind::IsADirectory.into()),
            (_, Some(Entry::Symlink { .. }) | None) => missing(),
        }
    }

    async fn write(&self, path: &[u8], data: &[u8], how: Write) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let (path, entry) = self.lookup(path, true)?;
        if path.is_empty() {
            return Err(io::ErrorKind::IsADirectory.into());
        }
        dir_exists(&self.manifest.borrow(), &path)?;
        let mut hasher = Sha256::new();
        let (bytes, mode) = match (entry, how) {
            (Some(Entry::Dir { .. }), _) => return Err(io::ErrorKind::IsADirectory.into()),
            (Some(Entry::Symlink { .. }), _) => return missing(),
            (Some(Entry::File { .. }), Write::New) => {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            (Some(Entry::File { mode, .. }), Write::Replace) => {
                hasher.update(data);
                (Bytes::copy_from_slice(data), mode)
            }
            (
                Some(Entry::File {
                    digest, mode, size, ..
                }),
                Write::Append,
            ) => {
                let grown = size + data.len() as u64;
                if grown > self.caps.file_bytes {
                    return Err(Refusal::FileTooLarge {
                        path,
                        size: grown,
                        max: self.caps.file_bytes,
                    }
                    .into());
                }
                let held = self.blob(&digest).await?;
                let bytes = match self.tail.borrow_mut().take() {
                    // The blob the last append wrote: carry its hash on, and grow its
                    // buffer in place.
                    Some((last, mut state)) if last == digest => {
                        state.update(data);
                        hasher = state;
                        held.appended(data)
                    }
                    _ => {
                        hasher.update(&held);
                        hasher.update(data);
                        let mut bytes = BytesMut::with_capacity(held.len() + data.len());
                        bytes.extend_from_slice(&held);
                        bytes.extend_from_slice(data);
                        bytes.freeze()
                    }
                };
                (bytes, mode)
            }
            (None, _) => {
                hasher.update(data);
                (Bytes::copy_from_slice(data), FILE_MODE)
            }
        };
        let blob = Blob::hashed(Digest(hasher.finalize()), bytes);
        *self.tail.borrow_mut() = Some((*blob.digest(), hasher));
        let now = self.now();
        let mut ops = vec![Op::Put {
            path: path.clone(),
            digest: *blob.digest(),
            size: blob.bytes().len() as u64,
            mode,
            mtime_ns: now,
        }];
        touch(&mut ops, &path, now);
        self.commit(&ops, vec![blob]).await.map(drop)
    }

    async fn write_files(&self, files: Vec<(String, Bytes, u32)>, dir_mode: u32) -> io::Result<Wrote> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let planned = plan(
            &self.manifest.borrow(),
            &self.caps,
            files.iter().map(|(path, _, _)| Ok(path.clone())),
        );
        if planned.files.is_empty() {
            return Ok(Wrote::default());
        }
        let now = self.now();
        let mut ops = Vec::new();
        let mut blobs = Vec::with_capacity(planned.files.len());
        let mut wrote = Wrote {
            files: planned.files.len(),
            ..Wrote::default()
        };
        for (at, ((path, dirs, held), (_, data, mode))) in planned.files.into_iter().zip(files).enumerate() {
            for dir in dirs {
                ops.push(Op::Mkdir {
                    path: dir.clone(),
                    mode: dir_mode & MODE_BITS,
                    mtime_ns: now,
                });
                touch(&mut ops, &dir, now);
                wrote.made.push(format!("/{dir}").into_bytes());
            }
            if held.is_none() {
                wrote.created.push(at);
            }
            let blob = Blob::new(data);
            ops.push(Op::Put {
                path: path.clone(),
                digest: *blob.digest(),
                size: blob.bytes().len() as u64,
                mode: held.unwrap_or(mode & MODE_BITS),
                mtime_ns: now,
            });
            touch(&mut ops, &path, now);
            blobs.push(blob);
        }
        self.commit(&ops, blobs).await?;
        Ok(wrote)
    }

    async fn mkdir(&self, path: &[u8], mode: u32) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let (path, entry) = self.lookup(path, false)?;
        if path.is_empty() || entry.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        dir_exists(&self.manifest.borrow(), &path)?;
        let now = self.now();
        let mut ops = vec![Op::Mkdir {
            path: path.clone(),
            mode: mode & MODE_BITS,
            mtime_ns: now,
        }];
        touch(&mut ops, &path, now);
        self.commit(&ops, Vec::new()).await.map(drop)
    }

    async fn remove(&self, path: &[u8], what: Remove) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let (path, entry) = self.lookup(path, false)?;
        if path.is_empty() {
            return Err(invalid("the workspace root cannot be removed"));
        }
        dir_exists(&self.manifest.borrow(), &path)?;
        match (entry, what) {
            (None, _) => return missing(),
            (Some(Entry::Dir { .. }), Remove::File) => {
                return Err(io::ErrorKind::IsADirectory.into());
            }
            (Some(Entry::File { .. } | Entry::Symlink { .. }), Remove::Dir) => {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            _ => {}
        }
        let mut ops = vec![Op::Remove {
            path: path.clone(),
            recursive: false,
        }];
        touch(&mut ops, &path, self.now());
        self.commit(&ops, Vec::new()).await.map(drop)
    }

    async fn rename(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let (from, entry) = self.lookup(from, false)?;
        let (to, _) = self.lookup(to, false)?;
        if from.is_empty() || to.is_empty() || to.starts_with(&format!("{from}/")) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if entry.is_none() {
            return missing();
        }
        dir_exists(&self.manifest.borrow(), &from)?;
        dir_exists(&self.manifest.borrow(), &to)?;
        let now = self.now();
        let mut ops = vec![Op::Move {
            from: from.clone(),
            to: to.clone(),
        }];
        touch(&mut ops, &from, now);
        if parent(&from) != parent(&to) {
            touch(&mut ops, &to, now);
        }
        self.commit(&ops, Vec::new()).await.map(drop)
    }

    async fn hard_link(&self, existing: &[u8], link: &[u8]) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let (from, entry) = self.lookup(existing, false)?;
        let (to, held) = self.lookup(link, false)?;
        match entry {
            _ if from.is_empty() => return Err(io::ErrorKind::PermissionDenied.into()),
            None => return missing(),
            Some(Entry::Dir { .. }) => return Err(io::ErrorKind::PermissionDenied.into()),
            Some(_) => {}
        }
        if to.is_empty() || held.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        dir_exists(&self.manifest.borrow(), &to)?;
        let mut ops = vec![Op::Link {
            from,
            to: to.clone(),
        }];
        touch(&mut ops, &to, self.now());
        self.commit(&ops, Vec::new()).await.map(drop)
    }

    async fn symlink(&self, target: &[u8], link: &[u8]) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        let target = std::str::from_utf8(target)
            .map_err(|_| invalid("workspace symlink targets are UTF-8"))?
            .to_owned();
        let (path, entry) = self.lookup(link, false)?;
        if path.is_empty() || entry.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        dir_exists(&self.manifest.borrow(), &path)?;
        let now = self.now();
        let mut ops = vec![Op::Symlink {
            path: path.clone(),
            target,
            mtime_ns: now,
        }];
        touch(&mut ops, &path, now);
        self.commit(&ops, Vec::new()).await.map(drop)
    }

    async fn set_mode(&self, path: &[u8], mode: u32) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        match self.lookup(path, true)? {
            (path, _) if path.is_empty() => Err(invalid("the workspace root keeps its mode")),
            (path, Some(Entry::File { .. } | Entry::Dir { .. })) => {
                let ops = [Op::SetMode {
                    path,
                    mode: mode & MODE_BITS,
                }];
                self.commit(&ops, Vec::new()).await.map(drop)
            }
            (_, Some(Entry::Symlink { .. }) | None) => missing(),
        }
    }

    async fn set_mtime(&self, path: &[u8], mtime_ns: i64) -> io::Result<()> {
        let _turn = self.gate.acquire().await.map_err(io::Error::other)?;
        match self.lookup(path, true)? {
            (path, _) if path.is_empty() => Err(invalid("the workspace root keeps its time")),
            (path, Some(_)) => {
                let ops = [Op::SetMtime { path, mtime_ns }];
                self.commit(&ops, Vec::new()).await.map(drop)
            }
            (_, None) => missing(),
        }
    }
}

fn later<T: 'static>(
    inner: &Rc<Inner>,
    run: impl AsyncFnOnce(&Inner) -> io::Result<T> + 'static,
) -> Reply<T> {
    let inner = Rc::clone(inner);
    Reply::Later(Box::pin(async move { run(&inner).await }))
}

impl FileSystem for WorkspaceFs {
    fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat> {
        Reply::Now(match self.inner.lookup(path, follow) {
            Ok((path, _)) if path.is_empty() => Ok(Stat {
                kind: Kind::Dir,
                size: 0,
                mode: ROOT_MODE,
                mtime_ns: 0,
            }),
            Ok((_, Some(entry))) => Ok(stat_of(&entry)),
            Ok((_, None)) => missing(),
            Err(error) => Err(error),
        })
    }

    fn read(&self, path: &[u8], limit: u64) -> Reply<Bytes> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            let (entry, bytes) = inner.file(&path).await?;
            if entry.size() > limit {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            Ok(bytes)
        })
    }

    fn write(&self, path: &[u8], data: &[u8], how: Write) -> Reply<()> {
        let (path, data) = (path.to_vec(), data.to_vec());
        later(&self.inner, async move |inner: &Inner| {
            inner.write(&path, &data, how).await
        })
    }

    fn write_files(&self, files: &[WholeFile<'_>], dir_mode: u32) -> Reply<Wrote> {
        let take = plan(
            &self.inner.manifest.borrow(),
            &self.inner.caps,
            files.iter().map(|file| key(file.path)),
        )
        .files
        .len();
        if take == 0 {
            return Reply::Now(Ok(Wrote::default()));
        }
        let mut held = Vec::with_capacity(take);
        for file in &files[..take] {
            match key(file.path) {
                Ok(path) => held.push((path, Bytes::copy_from_slice(file.data), file.mode)),
                Err(error) => return Reply::Now(Err(error)),
            }
        }
        later(&self.inner, async move |inner: &Inner| {
            inner.write_files(held, dir_mode).await
        })
    }

    fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>> {
        Reply::Now(self.inner.lookup(path, true).and_then(|(path, entry)| {
            match entry {
                Some(Entry::Dir { .. }) => {}
                None if path.is_empty() => {}
                Some(_) => return Err(io::ErrorKind::NotADirectory.into()),
                None => return missing(),
            }
            Ok(self
                .inner
                .manifest
                .borrow()
                .children(&path)
                .map(|name| name.as_bytes().to_vec())
                .collect())
        }))
    }

    fn list_kinds(&self, path: &[u8]) -> Reply<Vec<(Vec<u8>, Option<Kind>)>> {
        Reply::Now(self.inner.lookup(path, true).and_then(|(path, entry)| {
            match entry {
                Some(Entry::Dir { .. }) => {}
                None if path.is_empty() => {}
                Some(_) => return Err(io::ErrorKind::NotADirectory.into()),
                None => return missing(),
            }
            // The directory resolved, so each child's own entry is what lstat finds.
            Ok(self
                .inner
                .manifest
                .borrow()
                .child_entries(&path)
                .into_iter()
                .map(|(name, entry)| (name.as_bytes().to_vec(), Some(stat_of(entry).kind)))
                .collect())
        }))
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> Reply<()> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            inner.mkdir(&path, mode).await
        })
    }

    fn remove(&self, path: &[u8], what: Remove) -> Reply<()> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            inner.remove(&path, what).await
        })
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> Reply<()> {
        let (from, to) = (from.to_vec(), to.to_vec());
        later(&self.inner, async move |inner: &Inner| {
            inner.rename(&from, &to).await
        })
    }

    fn symlink(&self, target: &[u8], link: &[u8]) -> Reply<()> {
        let (target, link) = (target.to_vec(), link.to_vec());
        later(&self.inner, async move |inner: &Inner| {
            inner.symlink(&target, &link).await
        })
    }

    fn hard_link(&self, existing: &[u8], link: &[u8]) -> Reply<()> {
        let (existing, link) = (existing.to_vec(), link.to_vec());
        later(&self.inner, async move |inner: &Inner| {
            inner.hard_link(&existing, &link).await
        })
    }

    fn links(&self, path: &[u8], follow: bool) -> Option<Links> {
        let (path, entry) = self.inner.lookup(path, follow).ok()?;
        let group = entry?.link()?;
        Some(Links {
            group,
            names: self.inner.manifest.borrow().names(&path),
        })
    }

    fn read_link(&self, path: &[u8]) -> Reply<Vec<u8>> {
        Reply::Now(match self.inner.lookup(path, false) {
            Ok((_, Some(Entry::Symlink { target, .. }))) => Ok(target.into_bytes()),
            Ok((_, Some(_))) => Err(io::ErrorKind::InvalidInput.into()),
            Ok((path, None)) if path.is_empty() => Err(io::ErrorKind::InvalidInput.into()),
            Ok((_, None)) => missing(),
            Err(error) => Err(error),
        })
    }

    fn set_mode(&self, path: &[u8], mode: u32) -> Reply<()> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            inner.set_mode(&path, mode).await
        })
    }

    fn set_mtime(&self, path: &[u8], mtime_ns: i64) -> Reply<()> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            inner.set_mtime(&path, mtime_ns).await
        })
    }

    fn upload(&self, path: &[u8], sha256: [u8; SHA256_BYTES]) -> Reply<Box<dyn FileUpload>> {
        let mut upload = Self::upload(self, path, Digest(sha256), FILE_MODE);
        upload.mode = None;
        Reply::Now(Ok(Box::new(upload)))
    }

    fn download(&self, path: &[u8], limit: u64) -> Reply<FileDownload> {
        let path = path.to_vec();
        later(&self.inner, async move |inner: &Inner| {
            let (entry, bytes) = inner.file(&path).await?;
            let Entry::File { digest, size, .. } = entry else {
                unreachable!("file() answers files only");
            };
            if size > limit {
                return Err(io::ErrorKind::FileTooLarge.into());
            }
            Ok(FileDownload {
                bytes,
                sha256: digest.0,
            })
        })
    }

    fn answers_now(&self) -> bool {
        mutated(Mutation::AnswersNow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemStore;
    use std::{
        cell::Cell,
        future::Future,
        pin::{Pin, pin},
        task::{Context, Poll, Waker},
        time::Duration,
    };

    struct Ticks(Cell<u64>);

    impl Machine for Ticks {
        fn now(&self) -> Duration {
            self.0.set(self.0.get() + 1);
            Duration::from_micros(self.0.get())
        }

        fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
            Box::pin(std::future::ready(()))
        }

        fn random(&self, bytes: &mut [u8]) {
            bytes.fill(7);
        }
    }

    fn resolve<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the in-memory store answers at once"),
        }
    }

    fn reply<T: 'static>(reply: Reply<T>) -> io::Result<T> {
        resolve(reply.get())
    }

    fn workspace(file_bytes: u64) -> WorkspaceFs {
        let store: Arc<dyn Store> = Arc::new(MemStore::new());
        let caps = Caps {
            file_bytes,
            workspace_bytes: 1 << 20,
            paths: 64,
            ops: 64,
            path_bytes: 64,
        };
        resolve(WorkspaceFs::open(
            store,
            "test",
            caps,
            Rc::new(Ticks(Cell::new(0))),
        ))
        .expect("a workspace opens")
    }

    fn file(path: &'static str, data: &'static str, mode: u32) -> WholeFile<'static> {
        WholeFile {
            path: path.as_bytes(),
            data: data.as_bytes(),
            mode,
        }
    }

    #[test]
    fn lists_children_with_their_kinds_past_each_subtree() {
        let fs = workspace(64);
        for path in ["/d/b/x/y", "/d/b-c", "/d/b.c", "/d/b0", "/d/a/z"] {
            reply(fs.write_files(&[file(path, "1", 0o644)], 0o755)).expect("a file");
        }
        reply(fs.symlink(b"b", b"/d/l")).expect("a link");
        let want: Vec<(Vec<u8>, Option<Kind>)> = [
            ("a", Kind::Dir),
            ("b", Kind::Dir),
            ("b-c", Kind::File),
            ("b.c", Kind::File),
            ("b0", Kind::File),
            ("l", Kind::Symlink),
        ]
        .into_iter()
        .map(|(name, kind)| (name.as_bytes().to_vec(), Some(kind)))
        .collect();
        assert_eq!(reply(fs.list_kinds(b"/d")).expect("a listing"), want);
        let names: Vec<Vec<u8>> = want.into_iter().map(|(name, _)| name).collect();
        assert_eq!(reply(fs.list(b"/d")).expect("names"), names);
        assert_eq!(reply(fs.list_kinds(b"/d/l")).expect("through the link").len(), 1);
        assert_eq!(reply(fs.list(b"/")).expect("the root"), vec![b"d".to_vec()]);
        assert!(reply(fs.list_kinds(b"/d/b0")).is_err());
    }

    #[test]
    fn write_files_makes_parents_and_files_in_one_commit() {
        let fs = workspace(64);
        reply(fs.write(b"/old", b"x", Write::Replace)).expect("a file");
        reply(fs.set_mode(b"/old", 0o600)).expect("its mode");
        let version = fs.version();
        let wrote = reply(fs.write_files(
            &[
                file("/a/b/c", "one", 0o644),
                file("/a/d", "two", 0o755),
                file("/old", "three", 0o755),
            ],
            0o750,
        ))
        .expect("the files");
        assert_eq!(fs.version(), version + 1);
        assert_eq!(wrote.files, 3);
        assert_eq!(wrote.made, vec![b"/a".to_vec(), b"/a/b".to_vec()]);
        assert_eq!(wrote.created, vec![0, 1]);
        let stat = |path: &[u8]| reply(fs.stat(path, false)).expect("a stat");
        assert_eq!(stat(b"/a").kind, Kind::Dir);
        assert_eq!(stat(b"/a").mode & MODE_BITS, 0o750);
        assert_eq!(stat(b"/a/b").mode & MODE_BITS, 0o750);
        assert_eq!(stat(b"/a/d").mode & MODE_BITS, 0o755);
        assert_eq!(stat(b"/old").mode & MODE_BITS, 0o600);
        assert_eq!(&reply(fs.read(b"/a/b/c", 64)).expect("c")[..], b"one");
        assert_eq!(&reply(fs.read(b"/old", 64)).expect("old")[..], b"three");
    }

    #[test]
    fn write_files_stops_before_a_file_it_cannot_take() {
        let fs = workspace(64);
        reply(fs.mkdir(b"/dir", 0o755)).expect("a directory");
        reply(fs.write(b"/plain", b"x", Write::Replace)).expect("a file");
        reply(fs.symlink(b"dir", b"/link")).expect("a symlink");
        let cases: [(&[WholeFile<'static>], usize); 6] = [
            (&[file("/x", "1", 0o644), file("/dir", "2", 0o644)], 1),
            (&[file("/x", "1", 0o644), file("/plain/y", "2", 0o644)], 1),
            (&[file("/x", "1", 0o644), file("/link/y", "2", 0o644)], 1),
            (&[file("/x", "1", 0o644), file("/link", "2", 0o644)], 1),
            (&[file("/x", "1", 0o644), file("/x", "2", 0o644)], 1),
            (&[file("/n/m", "1", 0o644), file("/n", "2", 0o644)], 1),
        ];
        for (files, took) in cases {
            let wrote = reply(fs.write_files(files, 0o755)).expect("the leading files");
            assert_eq!(wrote.files, took);
            assert_eq!(&reply(fs.read(files[0].path, 64)).expect("written")[..], files[0].data);
        }
        assert_eq!(stat_kind(&fs, b"/dir"), Kind::Dir);
        assert_eq!(&reply(fs.read(b"/plain", 64)).expect("plain")[..], b"x");
    }

    fn stat_kind(fs: &WorkspaceFs, path: &[u8]) -> Kind {
        reply(fs.stat(path, false)).expect("a stat").kind
    }

    #[test]
    fn write_files_over_a_cap_writes_none() {
        let fs = workspace(4);
        let version = fs.version();
        let refused = reply(fs.write_files(
            &[file("/a/small", "1", 0o644), file("/big", "12345", 0o644)],
            0o755,
        ));
        assert!(refused.is_err());
        assert_eq!(fs.version(), version);
        assert!(reply(fs.stat(b"/a", false)).is_err());
    }

    #[test]
    fn appends_hash_to_the_digest_of_the_whole_file() {
        let fs = workspace(64);
        let digest = |fs: &WorkspaceFs, path: &str| match fs.manifest().get(path) {
            Some(Entry::File { digest, .. }) => *digest,
            other => panic!("{path} is not a file: {other:?}"),
        };
        reply(fs.write(b"/a", b"one", Write::Replace)).expect("a");
        reply(fs.write(b"/b", b"two", Write::Replace)).expect("b");
        for chunk in [&b"-x"[..], b"-yy", b"", b"-zzz"] {
            reply(fs.write(b"/a", chunk, Write::Append)).expect("append a");
            reply(fs.write(b"/a", chunk, Write::Append)).expect("append a again");
            reply(fs.write(b"/b", chunk, Write::Append)).expect("append b");
        }
        reply(fs.write(b"/c", b"new", Write::Append)).expect("append makes c");
        reply(fs.write(b"/c", b"!", Write::Append)).expect("append c");
        for (path, want) in [
            ("a", &b"one-x-x-yy-yy-zzz-zzz"[..]),
            ("b", b"two-x-yy-zzz"),
            ("c", b"new!"),
        ] {
            assert_eq!(digest(&fs, path), Digest::of(want));
            let read = reply(fs.read(format!("/{path}").as_bytes(), 64)).expect("read");
            assert_eq!(&read[..], want);
        }
    }

    #[test]
    fn hard_links_share_one_file_until_each_name_goes() {
        let fs = workspace(64);
        reply(fs.write(b"/a", b"one", Write::Replace)).expect("a file");
        reply(fs.write(b"/other", b"one", Write::Replace)).expect("a twin");
        assert_eq!(fs.links(b"/a", true), None);
        reply(fs.hard_link(b"/a", b"/b")).expect("a hard link");
        let group = fs.links(b"/a", true).expect("linked").group;
        assert_eq!(fs.links(b"/a", true), Some(Links { group, names: 2 }));
        assert_eq!(fs.links(b"/b", true), Some(Links { group, names: 2 }));
        assert_eq!(fs.links(b"/other", true), None);
        reply(fs.write(b"/b", b"two", Write::Append)).expect("a write through b");
        reply(fs.set_mode(b"/a", 0o600)).expect("a mode through a");
        assert_eq!(&reply(fs.read(b"/a", 64)).expect("a")[..], b"onetwo");
        assert_eq!(reply(fs.stat(b"/b", false)).expect("b").mode & MODE_BITS, 0o600);
        assert_eq!(
            reply(fs.hard_link(b"/a", b"/other")).map_err(|error| error.kind()),
            Err(io::ErrorKind::AlreadyExists)
        );
        reply(fs.mkdir(b"/dir", 0o755)).expect("a directory");
        assert!(reply(fs.hard_link(b"/dir", b"/d2")).is_err());
        reply(fs.hard_link(b"/b", b"/dir/c")).expect("a third name");
        assert_eq!(fs.links(b"/a", true).map(|links| links.names), Some(3));
        reply(fs.remove(b"/a", Remove::File)).expect("unlink a");
        assert_eq!(fs.links(b"/b", true).map(|links| links.names), Some(2));
        assert_eq!(&reply(fs.read(b"/dir/c", 64)).expect("c")[..], b"onetwo");
        reply(fs.rename(b"/dir/c", b"/c")).expect("a rename keeps the link");
        reply(fs.write(b"/c", b"new", Write::Replace)).expect("a write through c");
        assert_eq!(&reply(fs.read(b"/b", 64)).expect("b")[..], b"new");
        let reopened = resolve(WorkspaceFs::open(
            Arc::clone(&fs.inner.store),
            "test",
            fs.caps(),
            Rc::new(Ticks(Cell::new(100))),
        ))
        .expect("a reopen");
        assert_eq!(reopened.manifest(), fs.manifest());
        assert_eq!(reopened.links(b"/c", true), Some(Links { group, names: 2 }));
        reply(fs.remove(b"/b", Remove::File)).expect("unlink b");
        assert_eq!(fs.links(b"/c", true).map(|links| links.names), Some(1));
        assert_eq!(&reply(fs.read(b"/c", 64)).expect("c")[..], b"new");
    }
}
