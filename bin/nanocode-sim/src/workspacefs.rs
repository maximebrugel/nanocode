use std::{
    cell::Cell,
    collections::BTreeMap,
    future::Future,
    io,
    num::NonZeroUsize,
    ops::Range,
    pin::pin,
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use nanocode_durability_core::StoreFuture;
use nanocode_shell::{FileSystem, Machine, Remove, Reply, Write};
use nanocode_workspace_fs::{
    Batch, Blob, Caps, Digest, Entry, Failure, Manifest, MemStore, Store, WorkspaceFs,
    sim::{self as fs_sim, Mutation as FsRule},
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const WORKSPACE: &str = "sim";
const STEPS_MIN: u64 = 40;
const STEPS_MAX: u64 = 160;
const FILE_BYTES_MIN: u64 = 8;
const FILE_BYTES_MAX: u64 = 96;
const WORKSPACE_FILES_MIN: u64 = 2;
const WORKSPACE_FILES_MAX: u64 = 6;
const PATHS_MIN: u64 = 4;
const PATHS_MAX: u64 = 12;
const OPS_PER_COMMIT: usize = 8;
const PATH_BYTES: usize = 32;
const CRASH_PPM: u64 = 80_000;
const CRASH_AFTER_PPM: u64 = 500_000;
const OVERSIZE_PPM: u64 = 150_000;
const RESUME_PPM: u64 = 60_000;
const BAD_DIGEST_PPM: u64 = 200_000;
const TICK_NANOS: u64 = 1_000;
const NAMES: [&str; 8] = ["a", "b", "d", "d/c", "d/e", "d/e/f", "g", "g/h"];
const MODES: [u32; 3] = [0o644, 0o600, 0o755];
const PLAN_NONE: u8 = 0;
const PLAN_BEFORE: u8 = 1;
const PLAN_AFTER: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceFsMutation {
    None,
    FileCapIgnored,
    LostUpdate,
    TornCommit,
    VisibleBeforeCommit,
}

impl WorkspaceFsMutation {
    const ALL: [Self; 5] = [
        Self::None,
        Self::FileCapIgnored,
        Self::LostUpdate,
        Self::TornCommit,
        Self::VisibleBeforeCommit,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FileCapIgnored => "file_cap_ignored",
            Self::LostUpdate => "lost_update",
            Self::TornCommit => "torn_commit",
            Self::VisibleBeforeCommit => "visible_before_commit",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .skip(1)
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> FsRule {
        match self {
            Self::None => FsRule::None,
            Self::FileCapIgnored => FsRule::FileCapIgnored,
            Self::LostUpdate => FsRule::LostUpdate,
            Self::TornCommit => FsRule::TornCommit,
            Self::VisibleBeforeCommit => FsRule::VisibleBeforeCommit,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceFsStats {
    pub seeds: u64,
    pub steps: u64,
    pub commits: u64,
    pub stale: u64,
    pub capped: u64,
    pub crashes_before: u64,
    pub crashes_after: u64,
    pub resumes: u64,
    pub uploads: u64,
    pub bad_digests: u64,
    pub max_version: u64,
}

impl WorkspaceFsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.steps += other.steps;
        self.commits += other.commits;
        self.stale += other.stale;
        self.capped += other.capped;
        self.crashes_before += other.crashes_before;
        self.crashes_after += other.crashes_after;
        self.resumes += other.resumes;
        self.uploads += other.uploads;
        self.bad_digests += other.bad_digests;
        if other.max_version > self.max_version {
            self.max_version = other.max_version;
        }
    }
}

struct Faulty {
    inner: MemStore,
    plan: AtomicU8,
    fired: AtomicU8,
}

impl Store for Faulty {
    fn load<'a>(&'a self, workspace: &'a str) -> StoreFuture<'a, Result<Manifest, Failure>> {
        self.inner.load(workspace)
    }

    fn commit<'a>(
        &'a self,
        workspace: &'a str,
        batch: &'a Batch,
    ) -> StoreFuture<'a, Result<u64, Failure>> {
        let plan = self.plan.swap(PLAN_NONE, Ordering::SeqCst);
        self.fired.store(plan, Ordering::SeqCst);
        Box::pin(async move {
            match plan {
                PLAN_BEFORE => Err(Failure::Backend("crashed before the commit".to_owned())),
                PLAN_AFTER => {
                    self.inner.commit(workspace, batch).await?;
                    Err(Failure::Backend("crashed after the commit".to_owned()))
                }
                _ => self.inner.commit(workspace, batch).await,
            }
        })
    }

    fn blob<'a>(&'a self, digest: &'a Digest) -> StoreFuture<'a, Result<Blob, Failure>> {
        self.inner.blob(digest)
    }
}

struct Clock(Cell<u64>);

impl Machine for Clock {
    fn now(&self) -> Duration {
        let now = self.0.get() + TICK_NANOS;
        self.0.set(now);
        Duration::from_nanos(now)
    }

    fn sleep(&self, _: Duration) -> std::pin::Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::ready(()))
    }

    fn random(&self, bytes: &mut [u8]) {
        bytes.fill(0);
    }
}

fn resolve<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("workspace calls over an in-memory store finish at once"),
    }
}

fn reply<T>(reply: Reply<T>) -> io::Result<T> {
    match reply {
        Reply::Now(result) => result,
        Reply::Later(future) => resolve(future),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expect {
    Any,
    TooLarge,
    BadDigest,
}

struct World {
    rng: Rng,
    caps: Caps,
    store: Arc<Faulty>,
    clock: Rc<dyn Machine>,
    writers: Vec<WorkspaceFs>,
    history: BTreeMap<u64, Manifest>,
    stats: WorkspaceFsStats,
}

impl World {
    fn head(&self) -> Manifest {
        resolve(self.store.load(WORKSPACE)).expect("the in-memory store loads")
    }

    fn open(&self) -> WorkspaceFs {
        let store: Arc<dyn Store> = self.store.clone();
        resolve(WorkspaceFs::open(
            store,
            WORKSPACE,
            self.caps,
            Rc::clone(&self.clock),
        ))
        .expect("a workspace opens over the in-memory store")
    }

    fn path(&mut self) -> String {
        format!("/{}", NAMES[self.rng.pick(NAMES.len())])
    }

    fn data(&mut self, oversize: bool) -> Vec<u8> {
        let size = if oversize {
            self.caps.file_bytes + 1 + self.rng.below(self.caps.file_bytes)
        } else {
            self.rng.below(self.caps.file_bytes + 1)
        };
        (0..size).map(|_| self.rng.below(256) as u8).collect()
    }

    fn step(&mut self) -> Result<(), String> {
        let at = self.rng.pick(self.writers.len());
        if self.rng.chance(CRASH_PPM) {
            let plan = if self.rng.chance(CRASH_AFTER_PPM) {
                PLAN_AFTER
            } else {
                PLAN_BEFORE
            };
            self.store.plan.store(plan, Ordering::SeqCst);
        }
        self.store.fired.store(PLAN_NONE, Ordering::SeqCst);
        let before = self.head();
        let fs = self.writers[at].clone();
        let mut written: Option<(String, Vec<u8>)> = None;
        let mut expect = Expect::Any;
        let kind = self.rng.below(10);
        let result = match kind {
            0..=2 => {
                let path = self.path();
                let oversize = self.rng.chance(OVERSIZE_PPM);
                if oversize {
                    expect = Expect::TooLarge;
                }
                let data = self.data(oversize);
                let how = match kind {
                    0 => Write::Replace,
                    1 => Write::Append,
                    _ => Write::New,
                };
                let mut whole = data.clone();
                if how == Write::Append {
                    if let Ok(held) = reply(fs.read(path.as_bytes(), u64::MAX)) {
                        whole = [&held[..], &data[..]].concat();
                    }
                    if whole.len() as u64 > self.caps.file_bytes {
                        expect = Expect::TooLarge;
                    }
                }
                written = Some((path.clone(), whole));
                reply(fs.write(path.as_bytes(), &data, how))
            }
            3 => {
                let path = self.path();
                reply(fs.mkdir(path.as_bytes(), MODES[self.rng.pick(MODES.len())]))
            }
            4 => {
                let path = self.path();
                let what = if self.rng.chance(500_000) {
                    Remove::File
                } else {
                    Remove::Dir
                };
                reply(fs.remove(path.as_bytes(), what))
            }
            5 => {
                let (from, to) = (self.path(), self.path());
                reply(fs.rename(from.as_bytes(), to.as_bytes()))
            }
            6 => {
                let (target, link) = (self.path(), self.path());
                if self.rng.chance(500_000) {
                    reply(fs.hard_link(target.as_bytes(), link.as_bytes()))
                } else {
                    reply(fs.symlink(&target.as_bytes()[1..], link.as_bytes()))
                }
            }
            7 => {
                let path = self.path();
                reply(fs.set_mode(path.as_bytes(), MODES[self.rng.pick(MODES.len())]))
            }
            8 => {
                let path = self.path();
                let mtime_ns = self.rng.below(1 << 40) as i64;
                reply(fs.set_mtime(path.as_bytes(), mtime_ns))
            }
            _ => {
                self.stats.uploads += 1;
                let path = self.path();
                let oversize = self.rng.chance(OVERSIZE_PPM);
                let data = self.data(oversize);
                let bad = self.rng.chance(BAD_DIGEST_PPM);
                let expected = if bad {
                    self.stats.bad_digests += 1;
                    expect = Expect::BadDigest;
                    Digest::of(&[&data[..], b"!"].concat())
                } else {
                    Digest::of(&data)
                };
                if oversize {
                    expect = Expect::TooLarge;
                }
                let chunk = 1 + self.rng.below(self.caps.file_bytes) as usize;
                let mut upload = fs.upload(path.as_bytes(), expected, MODES[0]);
                let pushed = data.chunks(chunk).try_for_each(|piece| upload.push(piece));
                if !bad {
                    written = Some((path, data));
                }
                pushed.and_then(|()| resolve(upload.finish()).map(drop))
            }
        };
        self.judge(at, &before, &result, expect, written.as_ref())
    }

    fn judge(
        &mut self,
        at: usize,
        before: &Manifest,
        result: &io::Result<()>,
        expect: Expect,
        written: Option<&(String, Vec<u8>)>,
    ) -> Result<(), String> {
        self.stats.steps += 1;
        let fired = self.store.fired.load(Ordering::SeqCst);
        match fired {
            PLAN_BEFORE => self.stats.crashes_before += 1,
            PLAN_AFTER => self.stats.crashes_after += 1,
            _ => {}
        }
        if let Err(error) = result {
            match error.kind() {
                io::ErrorKind::ResourceBusy => self.stats.stale += 1,
                io::ErrorKind::FileTooLarge | io::ErrorKind::StorageFull => {
                    self.stats.capped += 1;
                }
                _ => {}
            }
        }
        match (expect, result) {
            (Expect::TooLarge, Ok(())) => {
                return Err("a write over the file cap succeeded".to_owned());
            }
            (Expect::BadDigest, Ok(())) => {
                return Err("an upload with the wrong sha256 committed".to_owned());
            }
            _ => {}
        }
        let head = self.head();
        if head.version() == before.version() {
            if head != *before {
                return Err(format!(
                    "the store changed at version {} without a new version",
                    head.version()
                ));
            }
        } else if head.version() == before.version() + 1 {
            if result.is_err() && fired != PLAN_AFTER {
                return Err(format!(
                    "a failed change landed as version {}",
                    head.version()
                ));
            }
            if self.history.insert(head.version(), head.clone()).is_some() {
                return Err(format!("version {} was committed twice", head.version()));
            }
            self.stats.commits += 1;
        } else {
            return Err(format!(
                "the head moved from version {} to {}: commits did not linearize",
                before.version(),
                head.version()
            ));
        }
        self.stats.max_version = self.stats.max_version.max(head.version());
        self.check_caps(&head)?;
        self.check_blobs(&head)?;
        for (index, writer) in self.writers.iter().enumerate() {
            let shown = writer.manifest();
            match self.history.get(&shown.version()) {
                Some(held) if *held == shown => {}
                _ => {
                    return Err(format!(
                        "writer {index} shows version {} that the store never committed",
                        shown.version()
                    ));
                }
            }
        }
        if let Err(error) = result
            && error.kind() == io::ErrorKind::ResourceBusy
            && self.writers[at].version() != head.version()
        {
            return Err("a stale writer did not catch up with the head".to_owned());
        }
        if let (Ok(()), Some((path, bytes))) = (result, written) {
            let read = reply(self.writers[at].read(path.as_bytes(), u64::MAX))
                .map_err(|error| format!("{path} unreadable after its write: {error}"))?;
            if read[..] != bytes[..] {
                return Err(format!(
                    "{path} reads {} bytes after a write of {}",
                    read.len(),
                    bytes.len()
                ));
            }
        }
        if self.rng.chance(RESUME_PPM) {
            self.resume(at)?;
        }
        Ok(())
    }

    fn check_caps(&self, head: &Manifest) -> Result<(), String> {
        if head.bytes() > self.caps.workspace_bytes {
            return Err(format!(
                "the workspace holds {} bytes over its cap of {}",
                head.bytes(),
                self.caps.workspace_bytes
            ));
        }
        if head.entries().len() > self.caps.paths {
            return Err(format!(
                "the workspace holds {} paths over its cap of {}",
                head.entries().len(),
                self.caps.paths
            ));
        }
        for (path, entry) in head.entries() {
            if entry.size() > self.caps.file_bytes {
                return Err(format!(
                    "{path} holds {} bytes over the file cap of {}",
                    entry.size(),
                    self.caps.file_bytes
                ));
            }
        }
        Ok(())
    }

    fn check_blobs(&self, head: &Manifest) -> Result<(), String> {
        let live = head.digests().count();
        if self.store.inner.blob_count() != live {
            return Err(format!(
                "the store keeps {} blobs for {live} live digests",
                self.store.inner.blob_count()
            ));
        }
        for (path, entry) in head.entries() {
            if let Entry::File { digest, size, .. } = entry {
                let blob = resolve(self.store.blob(digest))
                    .map_err(|failure| format!("{path}: {failure}"))?;
                if Digest::of(blob.bytes()) != *digest || blob.bytes().len() as u64 != *size {
                    return Err(format!("{path}: its blob does not match its entry"));
                }
            }
        }
        Ok(())
    }

    fn resume(&mut self, at: usize) -> Result<(), String> {
        self.stats.resumes += 1;
        let reopened = self.open();
        let head = self.head();
        if reopened.manifest() != head {
            return Err(format!(
                "reopening rebuilt version {} instead of the head {}",
                reopened.version(),
                head.version()
            ));
        }
        for (path, entry) in head.entries() {
            if let Entry::File { digest, .. } = entry {
                let download = resolve(reopened.download(path.as_bytes()))
                    .map_err(|error| format!("{path} unreadable after a restart: {error}"))?;
                let chunk = NonZeroUsize::new(1 + self.rng.pick(4)).expect("a chunk holds a byte");
                let joined: Vec<u8> = download
                    .chunks(chunk)
                    .flat_map(|piece| piece.to_vec())
                    .collect();
                if Digest::of(&joined) != *digest || download.digest != *digest {
                    return Err(format!("{path} changed across a restart"));
                }
            }
        }
        self.writers[at] = reopened;
        Ok(())
    }

    fn digest(&self) -> u64 {
        let head = self.head();
        let mut hash = fnv(FNV_OFFSET, format!("{:?}", head.entries()).as_bytes());
        hash = fnv(hash, &head.version().to_le_bytes());
        fnv(hash, format!("{:?}", self.stats).as_bytes())
    }
}

pub struct WorkspaceFsOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: WorkspaceFsStats,
    shape: String,
}

fn simulate(seed: u64, mutation: WorkspaceFsMutation) -> WorkspaceFsOutcome {
    fs_sim::mutate(mutation.rule());
    let mut rng = Rng::derive(seed, "workspace-fs");
    let file_bytes = rng.range(FILE_BYTES_MIN, FILE_BYTES_MAX);
    let caps = Caps {
        file_bytes,
        workspace_bytes: file_bytes * rng.range(WORKSPACE_FILES_MIN, WORKSPACE_FILES_MAX),
        paths: rng.range(PATHS_MIN, PATHS_MAX) as usize,
        ops: OPS_PER_COMMIT,
        path_bytes: PATH_BYTES,
    };
    let steps = rng.range(STEPS_MIN, STEPS_MAX);
    let shape = format!(
        "steps={steps} file_bytes={} workspace_bytes={} paths={}",
        caps.file_bytes, caps.workspace_bytes, caps.paths
    );
    let store = Arc::new(Faulty {
        inner: MemStore::new(),
        plan: AtomicU8::new(PLAN_NONE),
        fired: AtomicU8::new(PLAN_NONE),
    });
    let clock: Rc<dyn Machine> = Rc::new(Clock(Cell::new(0)));
    let mut world = World {
        rng,
        caps,
        store,
        clock,
        writers: Vec::new(),
        history: BTreeMap::from([(0, Manifest::default())]),
        stats: WorkspaceFsStats {
            seeds: 1,
            ..WorkspaceFsStats::default()
        },
    };
    world.writers = vec![world.open(), world.open()];
    let mut wrong = None;
    for step in 0..steps {
        if let Err(message) = world.step() {
            wrong = Some(format!("step {step}: {message}"));
            break;
        }
    }
    if wrong.is_none() {
        for at in 0..world.writers.len() {
            if let Err(message) = world.resume(at) {
                wrong = Some(format!("final restart: {message}"));
                break;
            }
        }
    }
    fs_sim::mutate(FsRule::None);
    WorkspaceFsOutcome {
        digest: world.digest(),
        wrong,
        stats: world.stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: WorkspaceFsMutation) -> WorkspaceFsOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        fs_sim::mutate(FsRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        WorkspaceFsOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: WorkspaceFsStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: WorkspaceFsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut WorkspaceFsStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} stats={:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: WorkspaceFsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario workspace_fs --seed {seed}");
    if mutation != WorkspaceFsMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: WorkspaceFsMutation,
    totals: &WorkspaceFsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=workspace_fs mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        WorkspaceFsMutation::None if failed == 0 => ExitCode::SUCCESS,
        WorkspaceFsMutation::None => ExitCode::FAILURE,
        _ if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        _ => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
