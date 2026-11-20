//! Crash, resume and two-writer races for the runtimes that write through the
//! workspace's file system: a psh script, a Python program (run by psh) and a
//! Code Mode cell whose tool calls write files.
//!
//! Crash: each seed runs one program twice over a `WorkspaceFs` on an
//! in-memory store. The reference run records what the store holds after every
//! commit. The crashed run dies at one commit, before or after the store takes
//! it, and from then on the store answers nothing, as a dead process would. The
//! workspace reopened from the store must hold exactly the state before or
//! after that commit, with every file readable whole and each file's content
//! one it held between two of the program's operations; the program must have
//! reported a failure; and the view the dead process kept must not show a
//! commit the store refused. The same runtime then resumes over the reopened
//! workspace, reads every file and writes a summary, which must match what the
//! store held and survive one more restart.
//!
//! Race: two writers in different runtimes append unique tokens to the same
//! files, over one shared `WorkspaceFs` or one each over the same store, with
//! their steps and the store's commits interleaved by the seed. Every
//! acknowledged token must be in the store once, in its writer's order, and no
//! file may hold a torn token.
//!
//! Crash points: each seed also kills one job at one of its commits and
//! resumes it: an append written in pieces through one open file, a git
//! checkout between two branches, or a rename between two appends. The
//! appended file must hold its first content followed by whole pieces, and
//! appending the rest must give all of them once; the rename must leave the
//! files as they were between two operations, never the moved file twice or
//! nowhere, and running the rest must finish it; a forced checkout after the
//! crash must leave the branch's tree with nothing to commit.
//!
//! Many files: one job writes, moves and removes many files at once, from a
//! Python program's write-back or a cell's node:fs; every file must hold its
//! old content or its whole new one and a moved file must never be under
//! neither name. Push: git push dies while it writes the remote-tracking
//! refs; each must hold its old commit or the pushed one, pushing again must
//! leave the server on every pushed commit, and fetching then leaves every
//! ref on it. Atomic
//! write: a Python program replaces files through a temporary file and
//! os.replace; every target must hold its old content or its whole new one,
//! with no temporary file left, and running it again must finish it.
//!
//! Some canaries bend the job's file system instead of the store: a whole-file
//! write that removes the old file first, one that empties it first, and a
//! remote-tracking ref written in two pieces, and (only in the many-file
//! job) a move that removes the old path before writing the new one, and
//! (only in the atomic-write job) a write-back that removes each target
//! before moving a temporary copy over it.

use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    future::Future,
    io::ErrorKind,
    ops::Range,
    pin::Pin,
    process::ExitCode,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use nanocode_code_mode_engine::{CellTools, EmbeddedHost, Interrupt, RuntimeEvent};
use nanocode_durability_core::StoreFuture;
use nanocode_shell::{
    Commands, Config as ShellConfig, Credentials, Ended, Fetch, FileDownload,
    FileSystem, FileUpload, HostInfo, Identity, Kind, Limits, Links, Machine, NoCommands, Remove,
    Reply, SHA256_BYTES, Script, Shell, Stat, WholeFile, Write, Wrote,
};
use nanocode_sys::base64::STANDARD;
use nanocode_workspace_fs::{
    Batch, Blob, Caps, Digest, Failure, Manifest, MemStore, Store, WorkspaceFs,
    sim::{self as fs_sim, Mutation as FsRule},
};
use serde_json::{Value, json};

use crate::{
    gitremote::Origin,
    rng::{FNV_OFFSET, Rng, fnv},
};

const ROOT: &str = "/w";
const KEY: &str = "crash";
const FILES: [&str; 3] = ["a.txt", "b.txt", "d/c.txt"];
const RACE_FILES: [&str; 2] = ["r1.txt", "r2.txt"];
const SUMMARY: &str = "sum.txt";
const PIECES: [&str; 5] = ["a", "b", "ab", "\n", "c"];
const SLICE: u32 = 4096;
const DEPTH: u32 = 64;
const PYTHON: nanocode_python::Budget = nanocode_python::Budget {
    steps: 1 << 24,
    time: Duration::from_secs(60),
    memory: None,
    sleep: Duration::ZERO,
    depth: DEPTH as usize,
};
const CAPS: Caps = Caps {
    file_bytes: 1 << 16,
    workspace_bytes: 1 << 20,
    paths: 256,
    ops: 256,
    path_bytes: 256,
};
const OPS_MIN: u64 = 2;
const OPS_MAX: u64 = 10;
const TOKENS_MIN: u64 = 2;
const TOKENS_MAX: u64 = 8;
const YIELD_PPM: u64 = 400_000;
const HALF_PPM: u64 = 500_000;
const POLLS_MAX: u64 = 1 << 22;
const PLAN_NONE: u8 = 0;
const PLAN_BEFORE: u8 = 1;
const PLAN_AFTER: u8 = 2;
const RUNTIMES: [Runtime; 3] = [Runtime::Psh, Runtime::Python, Runtime::Cell];
const PIECES_MIN: u64 = 2;
const PIECES_MAX: u64 = 7;
/// The files a many-file job starts from, moves to, and writes.
const OLD: [&str; 5] = ["o0.txt", "o1.txt", "d/o2.txt", "d/o3.txt", "d/o4.txt"];
const MOVED: [&str; 2] = ["mv/m0.txt", "d/m1.txt"];
const NEW: [&str; 12] = [
    "n/f0.txt", "n/f1.txt", "n/m/f2.txt", "n/f3.txt", "n/m/f4.txt", "n/m/k/f5.txt",
    "f6.txt", "n/f7.txt", "d/f8.txt", "n/m/f9.txt", "n/m/k/f10.txt", "n/f11.txt",
];
const MANY_MIN: u64 = 6;
const MANY_MAX: u64 = 12;
const REPO: &str = "g";
const REPO_PATHS: [&str; 4] = ["a.txt", "b.txt", "d/c.txt", "e.txt"];
const TRACKED_PPM: u64 = 700_000;
const PUSH_REPO: &str = "p";
const BRANCHES: [&str; 3] = ["t0", "t1", "t2"];
const FRESH: &str = "t3";
/// The files an atomic-write job starts from (the last is new, in a new
/// directory).
const ATOMIC: [&str; 4] = ["d/a.json", "d/b.json", "c.json", "d/n/e.json"];
const GIT_ENV: &str = "export GIT_AUTHOR_NAME=Agent GIT_AUTHOR_EMAIL=agent@sim GIT_COMMITTER_NAME=Agent GIT_COMMITTER_EMAIL=agent@sim";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceCrashMutation {
    None,
    TornCommit,
    BackendFailureAcked,
    VisibleBeforeCommit,
    LostUpdate,
    StaleRetryOverwrites,
    ReplaceRemovesFirst,
    ReplaceEmptiesFirst,
    TrackingRefTorn,
    ManyMoveRemovesFirst,
    AtomicReplaceRemovesTarget,
}

impl WorkspaceCrashMutation {
    const ALL: [Self; 11] = [
        Self::None,
        Self::TornCommit,
        Self::BackendFailureAcked,
        Self::VisibleBeforeCommit,
        Self::LostUpdate,
        Self::StaleRetryOverwrites,
        Self::ReplaceRemovesFirst,
        Self::ReplaceEmptiesFirst,
        Self::TrackingRefTorn,
        Self::ManyMoveRemovesFirst,
        Self::AtomicReplaceRemovesTarget,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TornCommit => "torn_commit",
            Self::BackendFailureAcked => "backend_failure_acked",
            Self::VisibleBeforeCommit => "visible_before_commit",
            Self::LostUpdate => "lost_update",
            Self::StaleRetryOverwrites => "stale_retry_overwrites",
            Self::ReplaceRemovesFirst => "replace_removes_first",
            Self::ReplaceEmptiesFirst => "replace_empties_first",
            Self::TrackingRefTorn => "tracking_ref_torn",
            Self::ManyMoveRemovesFirst => "many_move_removes_first",
            Self::AtomicReplaceRemovesTarget => "atomic_replace_removes_target",
        }
    }

    pub fn names() -> String {
        Self::ALL[1..]
            .iter()
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> FsRule {
        match self {
            Self::None => FsRule::None,
            Self::TornCommit => FsRule::TornCommit,
            Self::BackendFailureAcked => FsRule::BackendFailureAcked,
            Self::VisibleBeforeCommit => FsRule::VisibleBeforeCommit,
            Self::LostUpdate => FsRule::LostUpdate,
            Self::StaleRetryOverwrites => FsRule::StaleRetryOverwrites,
            Self::ReplaceRemovesFirst
            | Self::ReplaceEmptiesFirst
            | Self::TrackingRefTorn
            | Self::ManyMoveRemovesFirst
            | Self::AtomicReplaceRemovesTarget => FsRule::None,
        }
    }

    /// How the mutation bends the file system a crash point's job writes
    /// through, if it does.
    const fn bend(self) -> Bend {
        match self {
            Self::ReplaceRemovesFirst => Bend::RemovesFirst,
            Self::ReplaceEmptiesFirst => Bend::EmptiesFirst,
            Self::TrackingRefTorn => Bend::TornTrackingRef,
            Self::ManyMoveRemovesFirst => Bend::MoveRemovesFirst,
            Self::AtomicReplaceRemovesTarget => Bend::ReplaceRemovesTarget,
            _ => Bend::None,
        }
    }
}

/// A canary in the file system a crash point's job writes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bend {
    None,
    /// A whole-file write over an existing file removes it first.
    RemovesFirst,
    /// A whole-file write over an existing file empties it first.
    EmptiesFirst,
    /// A whole-file write of a remote-tracking ref writes it in two pieces.
    TornTrackingRef,
    /// The many-file job moves a file by removing the old path first, then
    /// writing the new one in pieces.
    MoveRemovesFirst,
    /// The atomic-write job's write-back replays the program's pattern
    /// without its atomicity: it writes a temporary file, removes the target,
    /// then moves the temporary file over it.
    ReplaceRemovesTarget,
}

/// How many pieces a bent move writes its new path in.
const MOVE_PIECES: usize = 4;

thread_local! {
    static BEND: Cell<Bend> = const { Cell::new(Bend::None) };
}

/// The job's file system, bent by the seed's canary when it has one. A
/// bent one takes no many-file step, so every write goes one at a time.
fn bent(files: Rc<dyn FileSystem>, label: &str) -> Rc<dyn FileSystem> {
    match BEND.with(Cell::get) {
        Bend::None => files,
        // Only the many-file job's moves are bent, so no other point sees it.
        Bend::MoveRemovesFirst if !label.starts_with("many") => files,
        Bend::ReplaceRemovesTarget if label != "atomic" => files,
        bend => Rc::new(Bent(files, bend)),
    }
}

struct Bent(Rc<dyn FileSystem>, Bend);

impl FileSystem for Bent {
    fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat> {
        self.0.stat(path, follow)
    }

    fn read(&self, path: &[u8], limit: u64) -> Reply<nanocode_bytes::Bytes> {
        self.0.read(path, limit)
    }

    fn write(&self, path: &[u8], data: &[u8], how: Write) -> Reply<()> {
        let bends = how == Write::Replace
            && match self.1 {
                Bend::None | Bend::MoveRemovesFirst => false,
                Bend::TornTrackingRef => path
                    .windows(b"/.git/refs/remotes/".len())
                    .any(|part| part == b"/.git/refs/remotes/"),
                Bend::RemovesFirst | Bend::EmptiesFirst | Bend::ReplaceRemovesTarget => true,
            };
        if !bends {
            return self.0.write(path, data, how);
        }
        let (files, bend, path, data) = (self.0.clone(), self.1, path.to_vec(), data.to_vec());
        Reply::Later(Box::pin(async move {
            if bend == Bend::TornTrackingRef {
                let half = data.len() / 2;
                files.write(&path, &data[..half], Write::Replace).get().await?;
                return files.write(&path, &data[half..], Write::Append).get().await;
            }
            let old = files
                .stat(&path, false)
                .get()
                .await
                .is_ok_and(|stat| stat.kind == Kind::File);
            if old && bend == Bend::ReplaceRemovesTarget {
                let mut temporary = path.clone();
                temporary.extend_from_slice(b".tmp-canary");
                files.write(&temporary, &data, Write::Replace).get().await?;
                files.remove(&path, Remove::File).get().await?;
                return files.rename(&temporary, &path).get().await;
            }
            if old && bend == Bend::RemovesFirst {
                files.remove(&path, Remove::File).get().await?;
            } else if old {
                files.write(&path, b"", Write::Replace).get().await?;
            }
            files.write(&path, &data, Write::Replace).get().await
        }))
    }

    fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>> {
        self.0.list(path)
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> Reply<()> {
        self.0.mkdir(path, mode)
    }

    fn remove(&self, path: &[u8], what: Remove) -> Reply<()> {
        self.0.remove(path, what)
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> Reply<()> {
        if self.1 != Bend::MoveRemovesFirst {
            return self.0.rename(from, to);
        }
        let (files, from, to) = (self.0.clone(), from.to_vec(), to.to_vec());
        Reply::Later(Box::pin(async move {
            let stat = files.stat(&from, false).get().await?;
            if stat.kind != Kind::File {
                return files.rename(&from, &to).get().await;
            }
            let data = files.read(&from, u64::MAX).get().await?;
            files.remove(&from, Remove::File).get().await?;
            files.write(&to, b"", Write::Replace).get().await?;
            for piece in data.chunks(data.len().div_ceil(MOVE_PIECES).max(1)) {
                files.write(&to, piece, Write::Append).get().await?;
            }
            Ok(())
        }))
    }

    fn symlink(&self, target: &[u8], link: &[u8]) -> Reply<()> {
        self.0.symlink(target, link)
    }

    fn read_link(&self, path: &[u8]) -> Reply<Vec<u8>> {
        self.0.read_link(path)
    }

    fn hard_link(&self, existing: &[u8], link: &[u8]) -> Reply<()> {
        self.0.hard_link(existing, link)
    }

    fn links(&self, path: &[u8], follow: bool) -> Option<Links> {
        self.0.links(path, follow)
    }

    fn set_mode(&self, path: &[u8], mode: u32) -> Reply<()> {
        self.0.set_mode(path, mode)
    }

    fn set_mtime(&self, path: &[u8], mtime_ns: i64) -> Reply<()> {
        self.0.set_mtime(path, mtime_ns)
    }

    fn upload(&self, path: &[u8], sha256: [u8; SHA256_BYTES]) -> Reply<Box<dyn FileUpload>> {
        self.0.upload(path, sha256)
    }

    fn download(&self, path: &[u8], limit: u64) -> Reply<FileDownload> {
        self.0.download(path, limit)
    }

    fn write_files(&self, files: &[WholeFile<'_>], dir_mode: u32) -> Reply<Wrote> {
        let _ = (files, dir_mode);
        Reply::Now(Ok(Wrote::default()))
    }

    fn answers_now(&self) -> bool {
        self.0.answers_now()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WorkspaceCrashStats {
    pub seeds: u64,
    pub commits: u64,
    pub crashes_before: u64,
    pub crashes_after: u64,
    pub resumed: u64,
    pub races_shared: u64,
    pub races_split: u64,
    pub tokens: u64,
    pub acked: u64,
    pub yields: u64,
    pub points_append: u64,
    pub points_checkout: u64,
    pub points_rename: u64,
    pub points_many: u64,
    pub points_push: u64,
    pub points_atomic: u64,
}

impl WorkspaceCrashStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.commits += other.commits;
        self.crashes_before += other.crashes_before;
        self.crashes_after += other.crashes_after;
        self.resumed += other.resumed;
        self.races_shared += other.races_shared;
        self.races_split += other.races_split;
        self.tokens += other.tokens;
        self.acked += other.acked;
        self.yields += other.yields;
        self.points_append += other.points_append;
        self.points_checkout += other.points_checkout;
        self.points_rename += other.points_rename;
        self.points_many += other.points_many;
        self.points_push += other.points_push;
        self.points_atomic += other.points_atomic;
    }
}

pub struct WorkspaceCrashOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: WorkspaceCrashStats,
    shape: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Runtime {
    Psh,
    Python,
    Cell,
}

impl Runtime {
    const fn name(self) -> &'static str {
        match self {
            Self::Psh => "psh",
            Self::Python => "python",
            Self::Cell => "cell",
        }
    }
}

/// What the store holds: each path's file digest, or None for a directory.
type State = BTreeMap<String, Option<Digest>>;

/// Each file's content, by its workspace key.
type Contents = BTreeMap<String, Vec<u8>>;

fn state(manifest: &Manifest) -> State {
    manifest
        .entries()
        .iter()
        .map(|(path, entry)| (path.clone(), entry.digest()))
        .collect()
}

fn shown_state(state: &State) -> String {
    let mut shown = String::new();
    for (path, digest) in state {
        let _ = match digest {
            Some(digest) => write!(shown, "{path}={} ", &digest.hex()[..8]),
            None => write!(shown, "{path}/ "),
        };
    }
    shown
}

/// A store that dies at one planned commit, before or after the store takes
/// it, and answers nothing after that; that yields at random before commits
/// so writers interleave; and that records what it holds after every commit.
struct Dying {
    inner: Arc<MemStore>,
    crash_at: AtomicU64,
    plan: AtomicU8,
    fired: AtomicU8,
    dead: AtomicBool,
    commits: AtomicU64,
    yields: AtomicU64,
    rng: Mutex<Option<Rng>>,
    states: Mutex<Vec<State>>,
}

impl Dying {
    fn new(inner: Arc<MemStore>, rng: Option<Rng>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            crash_at: AtomicU64::new(0),
            plan: AtomicU8::new(PLAN_NONE),
            fired: AtomicU8::new(PLAN_NONE),
            dead: AtomicBool::new(false),
            commits: AtomicU64::new(0),
            yields: AtomicU64::new(0),
            rng: Mutex::new(rng),
            states: Mutex::new(Vec::new()),
        })
    }

    fn yields(&self) -> bool {
        let mut rng = self.rng.lock().expect("the yield rng is not poisoned");
        let yields = rng.as_mut().is_some_and(|rng| rng.chance(YIELD_PPM));
        if yields {
            self.yields.fetch_add(1, Ordering::SeqCst);
        }
        yields
    }

    fn gone() -> Failure {
        Failure::Backend("the process is gone".to_owned())
    }

    fn plan(&self, at: u64, plan: u8) {
        self.crash_at
            .store(self.commits.load(Ordering::SeqCst) + at, Ordering::SeqCst);
        self.plan.store(plan, Ordering::SeqCst);
    }

    fn states(&self) -> Vec<State> {
        self.states
            .lock()
            .expect("the recorded states are not poisoned")
            .clone()
    }
}

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            self.0 = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Poll::Ready(())
    }
}

impl Store for Dying {
    fn load<'a>(&'a self, workspace: &'a str) -> StoreFuture<'a, Result<Manifest, Failure>> {
        if self.dead.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Err(Self::gone())));
        }
        self.inner.load(workspace)
    }

    fn commit<'a>(
        &'a self,
        workspace: &'a str,
        batch: &'a Batch,
    ) -> StoreFuture<'a, Result<u64, Failure>> {
        let pause = self.yields();
        Box::pin(async move {
            YieldOnce(pause).await;
            if self.dead.load(Ordering::SeqCst) {
                return Err(Self::gone());
            }
            let commit = self.commits.fetch_add(1, Ordering::SeqCst) + 1;
            let plan = match commit == self.crash_at.load(Ordering::SeqCst) {
                true => self.plan.load(Ordering::SeqCst),
                false => PLAN_NONE,
            };
            if plan == PLAN_BEFORE {
                self.dead.store(true, Ordering::SeqCst);
                self.fired.store(plan, Ordering::SeqCst);
                return Err(Failure::Backend(
                    "the process died before the commit".to_owned(),
                ));
            }
            let committed = self.inner.commit(workspace, batch).await;
            let held = self.inner.load(workspace).await?;
            self.states
                .lock()
                .expect("the recorded states are not poisoned")
                .push(state(&held));
            if plan == PLAN_AFTER {
                self.dead.store(true, Ordering::SeqCst);
                self.fired.store(plan, Ordering::SeqCst);
                committed?;
                return Err(Failure::Backend(
                    "the process died after the commit".to_owned(),
                ));
            }
            committed
        })
    }

    fn blob<'a>(&'a self, digest: &'a Digest) -> StoreFuture<'a, Result<Blob, Failure>> {
        if self.dead.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Err(Self::gone())));
        }
        self.inner.blob(digest)
    }
}

struct Clock {
    ticks: Cell<u64>,
}

impl Machine for Clock {
    fn now(&self) -> Duration {
        let ticks = self.ticks.get() + 1;
        self.ticks.set(ticks);
        Duration::from_millis(ticks)
    }

    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::pending())
    }

    fn random(&self, bytes: &mut [u8]) {
        let ticks = self.ticks.get() + 1;
        self.ticks.set(ticks);
        let mut rng = Rng::derive(ticks, "workspace_crash_random");
        for byte in bytes {
            *byte = rng.draw() as u8;
        }
    }
}

fn clock() -> Rc<dyn Machine> {
    Rc::new(Clock {
        ticks: Cell::new(0),
    })
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Polls every future, in an order the seed picks, until all are done.
fn drive<T>(mut futures: Vec<Boxed<'_, T>>, rng: &mut Rng) -> Result<Vec<T>, String> {
    let mut cx = Context::from_waker(Waker::noop());
    let mut done: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    let mut polls = 0_u64;
    loop {
        let live: Vec<usize> = (0..futures.len()).filter(|&i| done[i].is_none()).collect();
        if live.is_empty() {
            return Ok(done.into_iter().flatten().collect());
        }
        let at = live[rng.pick(live.len())];
        if let Poll::Ready(value) = futures[at].as_mut().poll(&mut cx) {
            done[at] = Some(value);
        }
        polls += 1;
        if polls > POLLS_MAX {
            return Err(format!("the writers made no progress in {POLLS_MAX} polls"));
        }
    }
}

fn one<'a, T: 'a>(future: impl Future<Output = T> + 'a, rng: &mut Rng) -> Result<T, String> {
    let mut done = drive(vec![Box::pin(future) as Boxed<'a, T>], rng)?;
    Ok(done.remove(0))
}

async fn open(store: Arc<Dying>) -> Result<WorkspaceFs, String> {
    let store: Arc<dyn Store> = store;
    WorkspaceFs::open(store, KEY, CAPS, clock())
        .await
        .map_err(|error| format!("open: {error}"))
}

fn absolute(path: &str) -> String {
    format!("{ROOT}/{path}")
}

fn keyed(path: &str) -> String {
    format!("{}/{path}", &ROOT[1..])
}

fn words(rng: &mut Rng, most: u64) -> String {
    (0..rng.range(1, most + 1))
        .map(|_| PIECES[rng.pick(PIECES.len())])
        .collect()
}

fn quoted(content: &str) -> String {
    format!("'{}'", content.replace('\'', "'\\''"))
}

#[derive(Clone, Debug)]
enum FileOp {
    Put(&'static str, String),
    Append(&'static str, String),
    Remove(&'static str),
    Rename(&'static str, &'static str),
}

/// What one run of a program said: whether it reported success, what it
/// acknowledged, and what it printed.
#[derive(Debug, Default)]
struct Said {
    ok: bool,
    acked: Vec<String>,
    output: String,
}

async fn psh(files: Rc<dyn FileSystem>, script: String) -> Said {
    psh_on(files, script, None).await
}

/// Runs script in psh, reaching a git server over the network when given one.
async fn psh_on(
    files: Rc<dyn FileSystem>,
    script: String,
    network: Option<(Rc<dyn Fetch>, Rc<dyn Credentials>)>,
) -> Said {
    let commands: Rc<dyn Commands> =
        Rc::new(nanocode_python::Command::new(PYTHON, Rc::new(NoCommands)));
    let mut shell = Shell::new(ShellConfig {
        fs: files,
        commands,
        machine: clock(),
        identity: Identity {
            info: HostInfo::default(),
            pid: 1,
            user: b"user".to_vec(),
            group: b"user".to_vec(),
            home: ROOT.as_bytes().to_vec(),
        },
        limits: Limits {
            fuel: u64::MAX,
            output: u64::MAX,
            file: u64::MAX,
            operations: u64::MAX,
            depth: DEPTH,
        },
        env: Vec::new(),
        cwd: ROOT.as_bytes().to_vec(),
        without: Vec::new(),
        strict: false,
    });
    if let Some((fetch, credentials)) = network {
        shell.set_fetch(fetch);
        shell.set_credentials(credentials);
    }
    let ran = shell
        .run(
            Script {
                source: script.into_bytes(),
                name: b"bash".to_vec(),
                args: Vec::new(),
                options: Vec::new(),
                stdin: None,
            },
            SLICE,
        )
        .await;
    let stdout = String::from_utf8_lossy(&ran.stdout).into_owned();
    let acked = stdout
        .lines()
        .filter_map(|line| line.strip_prefix('+'))
        .flat_map(|line| line.split(' ').map(str::to_owned))
        .collect();
    Said {
        ok: ran.ended == Ended::Exited(0),
        acked,
        output: format!(
            "{:?} {stdout} {}",
            ran.ended,
            String::from_utf8_lossy(&ran.stderr)
        ),
    }
}

fn psh_line(op: &FileOp) -> String {
    match op {
        FileOp::Put(path, content) => {
            format!("printf '%s' {} > {}", quoted(content), absolute(path))
        }
        FileOp::Append(path, content) => {
            format!("printf '%s' {} >> {}", quoted(content), absolute(path))
        }
        FileOp::Remove(path) => format!("rm -f {}", absolute(path)),
        FileOp::Rename(from, to) => format!("mv {} {}", absolute(from), absolute(to)),
    }
}

fn python_line(op: &FileOp) -> String {
    match op {
        FileOp::Put(path, content) => format!("open({:?}, 'w').write({content:?})", absolute(path)),
        FileOp::Append(path, content) => {
            format!("open({:?}, 'a').write({content:?})", absolute(path))
        }
        FileOp::Remove(path) => {
            let path = absolute(path);
            format!("os.path.exists({path:?}) and os.remove({path:?})")
        }
        FileOp::Rename(from, to) => format!("os.rename({:?}, {:?})", absolute(from), absolute(to)),
    }
}

fn python_program(ops: &[FileOp]) -> String {
    let mut program = "import os\n".to_owned();
    for op in ops {
        program.push_str(&python_line(op));
        program.push('\n');
    }
    format!("python3 -c {}", quoted(&program))
}

fn cell_input(op: &FileOp) -> Value {
    match op {
        FileOp::Put(path, content) => json!({"op": "put", "path": path, "data": content}),
        FileOp::Append(path, content) => json!({"op": "append", "path": path, "data": content}),
        FileOp::Remove(path) => json!({"op": "rm", "path": path}),
        FileOp::Rename(from, to) => json!({"op": "mv", "path": from, "to": to}),
    }
}

/// Answers one of the cell's tool calls over the file system.
async fn answer(files: &Rc<dyn FileSystem>, input: &Value) -> Result<Value, String> {
    let field = |name: &str| input.get(name).and_then(Value::as_str).unwrap_or_default();
    let path = absolute(field("path"));
    let path = path.as_bytes();
    let data = field("data").as_bytes();
    let done = match field("op") {
        "put" => files.write(path, data, Write::Replace).get().await,
        "append" => files.write(path, data, Write::Append).get().await,
        "mv" => files.rename(path, absolute(field("to")).as_bytes()).get().await,
        "rm" => match files.stat(path, false).get().await {
            Ok(_) => files.remove(path, Remove::File).get().await,
            Err(_) => Ok(()),
        },
        "read" => {
            return match files.read(path, u64::MAX).get().await {
                Ok(bytes) => Ok(json!({"data": String::from_utf8_lossy(&bytes)})),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(json!({"data": ""}))
                }
                Err(error) => Err(error.to_string()),
            };
        }
        other => return Err(format!("no op {other}")),
    };
    done.map(|()| json!({"ok": true}))
        .map_err(|error| error.to_string())
}

/// Makes path and every directory above it that is missing.
async fn mkdir_all(files: &Rc<dyn FileSystem>, path: &[u8]) -> std::io::Result<()> {
    let mut at = Vec::with_capacity(path.len());
    for part in path.split(|&byte| byte == b'/').filter(|part| !part.is_empty()) {
        at.push(b'/');
        at.extend_from_slice(part);
        match files.stat(&at, true).get().await {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                files.mkdir(&at, 0o755).get().await?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn under(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

/// Removes path and everything under it; a missing path is fine.
async fn remove_all(files: &Rc<dyn FileSystem>, path: &[u8]) -> std::io::Result<()> {
    let mut stack = vec![(path.to_vec(), false)];
    while let Some((at, listed)) = stack.pop() {
        let stat = match files.stat(&at, false).get().await {
            Ok(stat) => stat,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if stat.kind != Kind::Dir {
            files.remove(&at, Remove::File).get().await?;
        } else if listed {
            files.remove(&at, Remove::Dir).get().await?;
        } else {
            let names = files.list(&at).get().await?;
            stack.push((at.clone(), true));
            stack.extend(names.iter().map(|name| (under(&at, name), false)));
        }
    }
    Ok(())
}

/// Answers one call a cell's node:fs makes to its connected workspace (the
/// protocol node:internal/workspace documents) over files, as a host's
/// synchronous provider does.
async fn bridge(files: &Rc<dyn FileSystem>, input: &Value) -> Result<Value, String> {
    let field = |name: &str| input.get(name).and_then(Value::as_str).unwrap_or_default();
    let failed = |error: std::io::Error| error.to_string();
    let path = field("path").as_bytes();
    match field("op") {
        "info" => Ok(json!({ "cwd": ROOT, "env": {} })),
        "read" => match files.stat(path, true).get().await {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(json!({ "kind": "none" })),
            Err(error) => Err(failed(error)),
            Ok(stat) if stat.kind == Kind::Dir => Ok(json!({ "kind": "dir" })),
            Ok(_) => {
                let bytes = files.read(path, u64::MAX).get().await.map_err(failed)?;
                Ok(json!({ "kind": "file", "data": STANDARD.encode(&bytes) }))
            }
        },
        "list" => {
            let mut entries = Vec::new();
            for name in files.list(path).get().await.map_err(failed)? {
                let stat = files.stat(&under(path, &name), true).get().await.map_err(failed)?;
                let kind = if stat.kind == Kind::Dir { "dir" } else { "file" };
                entries.push(json!([String::from_utf8_lossy(&name), kind]));
            }
            Ok(json!({ "entries": entries }))
        }
        "write" => {
            let data = STANDARD.decode(field("data")).map_err(|error| error.to_string())?;
            files.write(path, &data, Write::Replace).get().await.map_err(failed)?;
            Ok(json!({}))
        }
        "mkdir" => mkdir_all(files, path).await.map(|()| json!({})).map_err(failed),
        "remove" => remove_all(files, path).await.map(|()| json!({})).map_err(failed),
        "rename" => files
            .rename(field("from").as_bytes(), field("to").as_bytes())
            .get()
            .await
            .map(|()| json!({}))
            .map_err(failed),
        other => Err(format!("no workspace op {other}")),
    }
}

/// Runs code as a Code Mode cell whose tools.w calls write through files;
/// each call's ack field names what it acknowledges when it succeeds.
async fn cell(files: Rc<dyn FileSystem>, code: String) -> Said {
    cell_with(files, code, None).await
}

/// The prelude that connects a cell's node:fs to the workspace through the
/// synchronous host call BRIDGE, as a host connects it.
const BRIDGED: &str = "globalThis.__nanocodeNodeWorkspace = (request) => callSync(\"ws\", request);";

/// Runs code as a Code Mode cell, with prelude when given: its tools.w calls go
/// to answer and its node:fs workspace calls (tool ws) to bridge, both over files.
async fn cell_with(files: Rc<dyn FileSystem>, code: String, prelude: Option<&str>) -> Said {
    let tools = CellTools::encode(&[json!({ "kind": "function", "name": "w", "tool_name": "w" })])
        .expect("the cell tool encodes");
    let mut said = Said::default();
    let mut host = match EmbeddedHost::spawn() {
        Ok(host) => host,
        Err(error) => {
            said.output = format!("spawn: {error}");
            return said;
        }
    };
    if let Err(error) = host.start_cell(
        1,
        &code,
        HashMap::new(),
        Arc::new(tools),
        prelude.map(Arc::from),
        None,
        Interrupt::new(),
    ) {
        said.output = format!("start_cell: {error}");
        return said;
    }
    let mut failed = false;
    loop {
        match host.read_event().await {
            Ok(RuntimeEvent::ToolCall { id, name, input, .. }) => {
                let answered = if name == "ws" {
                    bridge(&files, &input).await
                } else {
                    answer(&files, &input).await
                };
                let ok = answered.is_ok();
                if let (true, Some(ack)) = (ok, input.get("ack").and_then(Value::as_str)) {
                    said.acked.push(ack.to_owned());
                }
                failed |= !ok;
                let body = answered.unwrap_or_else(|error| json!({ "error": error }));
                let _ = writeln!(said.output, "{body}");
                if let Err(error) = host.send_tool_result(1, id, body.to_string(), ok) {
                    let _ = writeln!(said.output, "send_tool_result: {error}");
                    return said;
                }
            }
            Ok(RuntimeEvent::Done { stored, .. }) => {
                if let Some(sum) = stored.get("sum") {
                    said.output.push_str(sum.get());
                }
                said.ok = !failed;
                return said;
            }
            Ok(RuntimeEvent::Error { message, .. }) => {
                let _ = writeln!(said.output, "error: {message}");
                return said;
            }
            Ok(_) => {}
            Err(error) => {
                let _ = writeln!(said.output, "read_event: {error}");
                return said;
            }
        }
    }
}

fn cell_program(inputs: &[Value]) -> String {
    format!(
        "const OPS = {}; for (const op of OPS) {{ try {{ await tools.w(op); }} catch (e) {{}} }}",
        Value::Array(inputs.to_vec())
    )
}

/// Runs ops once in runtime over files.
async fn program(runtime: Runtime, files: Rc<dyn FileSystem>, ops: Vec<FileOp>) -> Said {
    match runtime {
        Runtime::Psh => {
            let mut script = vec!["set -e".to_owned()];
            script.extend(ops.iter().map(psh_line));
            psh(files, script.join("\n")).await
        }
        Runtime::Python => psh(files, python_program(&ops)).await,
        Runtime::Cell => {
            let inputs: Vec<Value> = ops.iter().map(cell_input).collect();
            cell(files, cell_program(&inputs)).await
        }
    }
}

/// Reads every file in FILES and writes them, each followed by |, to the
/// summary file, in runtime.
async fn summarize(runtime: Runtime, files: Rc<dyn FileSystem>) -> Said {
    let paths: Vec<String> = FILES.iter().map(|path| absolute(path)).collect();
    let summary = absolute(SUMMARY);
    match runtime {
        Runtime::Psh => {
            let script = format!(
                "set -e\nfor f in {}; do if [ -f \"$f\" ]; then cat \"$f\"; fi; printf '|'; done > {summary}",
                paths.join(" ")
            );
            psh(files, script).await
        }
        Runtime::Python => {
            let program = format!(
                "import os\ns = ''\nfor p in {paths:?}:\n    s += open(p).read() if os.path.exists(p) else ''\n    s += '|'\nopen({summary:?}, 'w').write(s)\n"
            );
            psh(files, format!("python3 -c {}", quoted(&program))).await
        }
        Runtime::Cell => {
            let code = format!(
                "let s = ''; for (const path of {}) {{ const v = await tools.w({{ op: 'read', path }}); const o = typeof v === 'string' ? JSON.parse(v) : v; s += o.data + '|'; }} await tools.w({{ op: 'put', path: {SUMMARY:?}, data: s }}); store('sum', s);",
                json!(FILES)
            );
            cell(files, code).await
        }
    }
}

async fn setup(files: WorkspaceFs, initial: Vec<(&'static str, String)>) -> Result<(), String> {
    let at = |what: &str, error: std::io::Error| format!("setup {what}: {error}");
    for dir in [ROOT.to_owned(), absolute("d")] {
        files
            .mkdir(dir.as_bytes(), 0o755)
            .get()
            .await
            .map_err(|error| at(&dir, error))?;
    }
    for (path, content) in initial {
        files
            .write(absolute(path).as_bytes(), content.as_bytes(), Write::Replace)
            .get()
            .await
            .map_err(|error| at(path, error))?;
    }
    Ok(())
}

/// Reads every file the workspace holds, so a blob the store lost shows.
async fn readable(files: WorkspaceFs) -> Result<Contents, String> {
    let mut held = BTreeMap::new();
    for (path, entry) in files.manifest().entries().iter() {
        if entry.digest().is_none() {
            continue;
        }
        let bytes = files
            .bytes(format!("/{path}").as_bytes())
            .await
            .map_err(|error| format!("{path} cannot be read whole: {error}"))?;
        held.insert(path.clone(), bytes.to_vec());
    }
    Ok(held)
}

fn fold(digest: &mut u64, contents: &Contents) {
    for (path, bytes) in contents {
        *digest = fnv(*digest, path.as_bytes());
        *digest = fnv(*digest, bytes);
    }
}

type Plan = (Vec<(&'static str, String)>, Vec<FileOp>);

fn gen_ops(rng: &mut Rng) -> Plan {
    let mut initial = Vec::new();
    for path in FILES {
        if rng.chance(HALF_PPM) {
            initial.push((path, words(rng, 4)));
        }
    }
    let ops = (0..rng.range(OPS_MIN, OPS_MAX))
        .map(|_| {
            let path = FILES[rng.pick(FILES.len())];
            match rng.pick(5) {
                0 | 1 => FileOp::Put(path, words(rng, 5)),
                2 | 3 => FileOp::Append(path, words(rng, 5)),
                _ => FileOp::Remove(path),
            }
        })
        .collect();
    (initial, ops)
}

/// What each file held between two operations: after the setup, then after
/// each operation in turn.
fn prefixes((initial, ops): &Plan) -> Vec<Contents> {
    let mut held: Contents = initial
        .iter()
        .map(|(path, content)| (keyed(path), content.clone().into_bytes()))
        .collect();
    let mut all = vec![held.clone()];
    for op in ops {
        match op {
            FileOp::Put(path, content) => {
                held.insert(keyed(path), content.clone().into_bytes());
            }
            FileOp::Append(path, content) => {
                held.entry(keyed(path))
                    .or_default()
                    .extend_from_slice(content.as_bytes());
            }
            FileOp::Remove(path) => {
                held.remove(&keyed(path));
            }
            FileOp::Rename(from, to) => {
                if let Some(bytes) = held.remove(&keyed(from)) {
                    held.insert(keyed(to), bytes);
                }
            }
        }
        all.push(held.clone());
    }
    all
}

/// Whether every file holds what it held between two operations; a psh or
/// Python write truncates before it writes, as a POSIX one does, so a file
/// those runtimes write may also be empty.
fn whole(runtime: Runtime, held: &Contents, prefixes: &[Contents]) -> Result<(), String> {
    if runtime == Runtime::Cell {
        return match prefixes.iter().any(|prefix| prefix == held) {
            true => Ok(()),
            false => Err(format!(
                "the store holds {held:?}, which the cell never held between two calls"
            )),
        };
    }
    for path in FILES.map(keyed) {
        let now = held.get(&path);
        let seen = prefixes.iter().any(|prefix| prefix.get(&path) == now)
            || now.is_some_and(Vec::is_empty);
        if !seen {
            return Err(format!(
                "{path} holds {:?}, which it never held between two operations",
                now.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            ));
        }
    }
    Ok(())
}

fn crash(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let runtime = RUNTIMES[rng.pick(RUNTIMES.len())];
    let plan = gen_ops(rng);
    let (initial, ops) = plan.clone();

    let reference = Dying::new(Arc::new(MemStore::new()), None);
    let files = one(open(reference.clone()), rng)??;
    one(setup(files.clone(), initial.clone()), rng)??;
    let base = reference.states().len();
    let said = one(program(runtime, Rc::new(files.clone()), ops.clone()), rng)?;
    if !said.ok {
        return Err(format!(
            "{} {ops:?} failed without a crash: {}",
            runtime.name(),
            said.output
        ));
    }
    drop(files);
    let states = reference.states().split_off(base - 1);
    let commits = states.len() as u64 - 1;
    stats.commits += commits;
    let _ = write!(shape, " crash={} commits={commits}", runtime.name());
    if commits == 0 {
        return Ok(());
    }

    let at = rng.range(1, commits);
    let planned = match rng.pick(2) {
        0 => PLAN_BEFORE,
        _ => PLAN_AFTER,
    };
    let _ = write!(shape, " at={at} plan={planned}");
    let inner = Arc::new(MemStore::new());
    let dying = Dying::new(inner.clone(), None);
    let files = one(open(dying.clone()), rng)??;
    one(setup(files.clone(), initial), rng)??;
    dying.plan(at, planned);
    let said = one(program(runtime, Rc::new(files.clone()), ops), rng)?;
    let fired = dying.fired.load(Ordering::SeqCst);
    if fired != planned {
        return Err(format!(
            "the planned crash at commit {at} of {commits} did not fire ({fired})"
        ));
    }
    let (before, after) = (&states[at as usize - 1], &states[at as usize]);
    let expected = match planned {
        PLAN_BEFORE => {
            stats.crashes_before += 1;
            before
        }
        _ => {
            stats.crashes_after += 1;
            after
        }
    };
    let when = format!("{} died at commit {at} of {commits}", runtime.name());
    if said.ok {
        return Err(format!("{when}, yet it reported success: {}", said.output));
    }
    let live = state(&files.manifest());
    let honest = live == *before || (planned == PLAN_AFTER && live == *after);
    if !honest {
        return Err(format!(
            "{when}: the dead process's view {} shows a commit the store did not take (before {}, after {})",
            shown_state(&live),
            shown_state(before),
            shown_state(after)
        ));
    }
    drop(files);

    let files = one(open(Dying::new(inner.clone(), None)), rng)??;
    let held = state(&files.manifest());
    if held != *expected {
        return Err(format!(
            "{when}: the store holds {}, not {}",
            shown_state(&held),
            shown_state(expected)
        ));
    }
    let contents = one(readable(files.clone()), rng)?.map_err(|error| format!("{when}: {error}"))?;
    fold(digest, &contents);
    whole(runtime, &contents, &prefixes(&plan)).map_err(|error| format!("{when}: {error}"))?;
    let mut summary = String::new();
    for path in FILES {
        if let Some(bytes) = contents.get(&keyed(path)) {
            summary.push_str(&String::from_utf8_lossy(bytes));
        }
        summary.push('|');
    }

    let resumed = one(summarize(runtime, Rc::new(files.clone())), rng)?;
    if !resumed.ok {
        return Err(format!(
            "{when}: resuming over the reopened workspace failed: {}",
            resumed.output
        ));
    }
    drop(files);
    stats.resumed += 1;
    let files = one(open(Dying::new(inner, None)), rng)??;
    let mut again = one(readable(files), rng)?.map_err(|error| format!("{when}: after resuming: {error}"))?;
    fold(digest, &again);
    let wrote = again.remove(&keyed(SUMMARY));
    if wrote.as_deref() != Some(summary.as_bytes()) {
        return Err(format!(
            "{when}: the resumed {} wrote the summary {:?}, not {summary:?}",
            runtime.name(),
            wrote.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        ));
    }
    if again != contents {
        return Err(format!(
            "{when}: resuming changed files it only read: {contents:?} then {again:?}"
        ));
    }
    Ok(())
}

struct Writer {
    runtime: Runtime,
    name: char,
    tokens: Vec<(&'static str, String)>,
}

impl Writer {
    fn run(&self, files: Rc<dyn FileSystem>) -> Boxed<'static, Said> {
        match self.runtime {
            Runtime::Psh => {
                let lines: Vec<String> = self
                    .tokens
                    .iter()
                    .map(|(path, token)| {
                        format!(
                            "printf '%s' '{token};' >> {} && echo +{token}",
                            absolute(path)
                        )
                    })
                    .collect();
                Box::pin(psh(files, lines.join("\n")))
            }
            Runtime::Python => {
                let lines: Vec<String> = self
                    .tokens
                    .chunks(2)
                    .map(|chunk| {
                        let ops: Vec<FileOp> = chunk
                            .iter()
                            .map(|(path, token)| FileOp::Append(path, format!("{token};")))
                            .collect();
                        let acked: Vec<&str> =
                            chunk.iter().map(|(_, token)| token.as_str()).collect();
                        format!("{} && echo +{}", python_program(&ops), acked.join(" "))
                    })
                    .collect();
                Box::pin(psh(files, lines.join("\n")))
            }
            Runtime::Cell => {
                let inputs: Vec<Value> = self
                    .tokens
                    .iter()
                    .map(|(path, token)| {
                        json!({"op": "append", "path": path, "data": format!("{token};"), "ack": token})
                    })
                    .collect();
                Box::pin(cell(files, cell_program(&inputs)))
            }
        }
    }

    /// Whether a token it wrote but did not acknowledge may still be stored:
    /// one Python run writes its files one by one, so a run that failed may
    /// have stored some of them.
    const fn partial(&self) -> bool {
        matches!(self.runtime, Runtime::Python)
    }
}

fn race(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let first = rng.pick(RUNTIMES.len());
    let second = (first + 1 + rng.pick(RUNTIMES.len() - 1)) % RUNTIMES.len();
    let shared = rng.chance(HALF_PPM);
    let mut writers = Vec::new();
    for (runtime, name) in [(first, 'A'), (second, 'B')] {
        let mut tokens = Vec::new();
        for index in 0..rng.range(TOKENS_MIN, TOKENS_MAX) {
            tokens.push((
                RACE_FILES[rng.pick(RACE_FILES.len())],
                format!("{name}{index}"),
            ));
        }
        writers.push(Writer {
            runtime: RUNTIMES[runtime],
            name,
            tokens,
        });
    }
    let _ = write!(
        shape,
        " race={}+{} {}",
        writers[0].runtime.name(),
        writers[1].runtime.name(),
        if shared { "shared" } else { "split" }
    );
    let inner = Arc::new(MemStore::new());
    let stores: Vec<Arc<Dying>> = ["a", "b"]
        .into_iter()
        .map(|label| Dying::new(inner.clone(), Some(Rng::derive(rng.draw(), label))))
        .collect();
    let made = one(open(stores[0].clone()), rng)??;
    one(setup(made.clone(), Vec::new()), rng)??;
    for path in RACE_FILES {
        let wrote = made.write(absolute(path).as_bytes(), b"", Write::Replace).get();
        one(wrote, rng)?.map_err(|error| format!("setup {path}: {error}"))?;
    }
    let views: Vec<WorkspaceFs> = match shared {
        true => {
            stats.races_shared += 1;
            vec![made.clone(), made]
        }
        false => {
            stats.races_split += 1;
            drop(made);
            let mut views = Vec::new();
            for store in &stores {
                views.push(one(open(store.clone()), rng)??);
            }
            views
        }
    };
    let runs: Vec<Boxed<'static, Said>> = writers
        .iter()
        .zip(&views)
        .map(|(writer, view)| writer.run(Rc::new(view.clone())))
        .collect();
    let said = drive(runs, rng)?;
    drop(views);
    stats.yields += stores
        .iter()
        .map(|store| store.yields.load(Ordering::SeqCst))
        .sum::<u64>();

    let files = one(open(Dying::new(inner, None)), rng)??;
    let held = one(readable(files), rng)??;
    fold(digest, &held);
    let mut found: BTreeMap<String, (&'static str, usize)> = BTreeMap::new();
    for path in RACE_FILES {
        let bytes = held.get(&keyed(path)).cloned().unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !text.is_empty() && !text.ends_with(';') {
            return Err(format!("{path} holds a torn token: {text:?}"));
        }
        for (at, token) in text.split(';').filter(|token| !token.is_empty()).enumerate() {
            if found.insert(token.to_owned(), (path, at)).is_some() {
                return Err(format!("{path} holds {token} twice: {text:?}"));
            }
        }
    }
    for (writer, said) in writers.iter().zip(&said) {
        stats.tokens += writer.tokens.len() as u64;
        stats.acked += said.acked.len() as u64;
        let acked: BTreeSet<&str> = said.acked.iter().map(String::as_str).collect();
        let mut last: BTreeMap<&str, usize> = BTreeMap::new();
        for (path, token) in &writer.tokens {
            match (acked.contains(token.as_str()), found.remove(token.as_str())) {
                (true, None) => {
                    return Err(format!(
                        "writer {} ({}) acknowledged {token} in {path}, which the store lost: {held:?}",
                        writer.name,
                        writer.runtime.name()
                    ));
                }
                (false, Some(_)) if !writer.partial() => {
                    return Err(format!(
                        "writer {} ({}) failed to write {token}, yet {path} holds it",
                        writer.name,
                        writer.runtime.name()
                    ));
                }
                (_, Some((held_in, at))) => {
                    if held_in != *path {
                        return Err(format!("{token} was written to {path} but sits in {held_in}"));
                    }
                    if last.get(path).is_some_and(|&previous| previous > at) {
                        return Err(format!(
                            "writer {}'s tokens in {path} are out of order: {held:?}",
                            writer.name
                        ));
                    }
                    last.insert(path, at);
                }
                (false, None) => {}
            }
        }
    }
    if let Some(token) = found.keys().next() {
        return Err(format!("the store holds {token}, which no writer wrote"));
    }
    Ok(())
}

type SetupFn = Box<dyn Fn(WorkspaceFs) -> Boxed<'static, Result<(), String>>>;
type JobFn = Box<dyn Fn(Rc<dyn FileSystem>) -> Boxed<'static, Said>>;

/// What a crash point left: the store the dead process wrote to, how it
/// died, and every file the reopened workspace holds.
struct Died {
    inner: Arc<MemStore>,
    when: String,
    contents: Contents,
}

/// Runs setup and then job to completion over one store, then again over a
/// store that dies at one of the job's commits, before or after the store
/// takes it. Checks what every crash must leave: the job reported a failure,
/// the dead process's view shows no commit the store refused, and the
/// reopened store holds exactly the state before or after that commit, every
/// file readable whole. None when the job made no commit.
fn die_during(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
    label: &str,
    setup: &SetupFn,
    job: &JobFn,
) -> Result<Option<Died>, String> {
    let reference = Dying::new(Arc::new(MemStore::new()), None);
    let files = one(open(reference.clone()), rng)??;
    one(setup(files.clone()), rng)??;
    let base = reference.states().len();
    let view = bent(Rc::new(files.clone()), label);
    let said = one(job(view), rng)?;
    if !said.ok {
        return Err(format!("{label} failed without a crash: {}", said.output));
    }
    drop(files);
    let states = reference.states().split_off(base - 1);
    let commits = states.len() as u64 - 1;
    stats.commits += commits;
    let _ = write!(shape, " point={label} commits={commits}");
    if commits == 0 {
        return Ok(None);
    }

    let at = rng.range(1, commits);
    let planned = match rng.pick(2) {
        0 => PLAN_BEFORE,
        _ => PLAN_AFTER,
    };
    let _ = write!(shape, " at={at} plan={planned}");
    let inner = Arc::new(MemStore::new());
    let dying = Dying::new(inner.clone(), None);
    let files = one(open(dying.clone()), rng)??;
    one(setup(files.clone()), rng)??;
    dying.plan(at, planned);
    let view = bent(Rc::new(files.clone()), label);
    let said = one(job(view), rng)?;
    let fired = dying.fired.load(Ordering::SeqCst);
    if fired != planned {
        return Err(format!(
            "{label}: the planned crash at commit {at} of {commits} did not fire ({fired})"
        ));
    }
    let (before, after) = (&states[at as usize - 1], &states[at as usize]);
    let expected = match planned {
        PLAN_BEFORE => {
            stats.crashes_before += 1;
            before
        }
        _ => {
            stats.crashes_after += 1;
            after
        }
    };
    let when = format!("{label} died at commit {at} of {commits}");
    if said.ok {
        return Err(format!("{when}, yet it reported success: {}", said.output));
    }
    let live = state(&files.manifest());
    if live != *before && !(planned == PLAN_AFTER && live == *after) {
        return Err(format!(
            "{when}: the dead process's view {} shows a commit the store did not take (before {}, after {})",
            shown_state(&live),
            shown_state(before),
            shown_state(after)
        ));
    }
    drop(files);
    let files = one(open(Dying::new(inner.clone(), None)), rng)??;
    let held = state(&files.manifest());
    if held != *expected {
        return Err(format!(
            "{when}: the store holds {}, not {}",
            shown_state(&held),
            shown_state(expected)
        ));
    }
    let contents = one(readable(files), rng)?.map_err(|error| format!("{when}: {error}"))?;
    fold(digest, &contents);
    Ok(Some(Died {
        inner,
        when,
        contents,
    }))
}

/// Reopens the store a crash point wrote to and reads every file again.
fn reopened(inner: &Arc<MemStore>, rng: &mut Rng, digest: &mut u64) -> Result<Contents, String> {
    let files = one(open(Dying::new(inner.clone(), None)), rng)??;
    let contents = one(readable(files), rng)??;
    fold(digest, &contents);
    Ok(contents)
}

fn lossy(bytes: Option<&Vec<u8>>) -> Option<String> {
    bytes.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

/// Appends pieces to path in runtime through one open file: one psh
/// redirection around every printf, one Python file object flushed after each
/// write, or one cell call per piece.
async fn appending(
    runtime: Runtime,
    files: Rc<dyn FileSystem>,
    path: &'static str,
    pieces: Vec<String>,
) -> Said {
    match runtime {
        Runtime::Psh => {
            let writes: Vec<String> = pieces
                .iter()
                .map(|piece| format!("printf '%s' {}", quoted(piece)))
                .collect();
            let script = format!("set -e\n{{ {}; }} >> {}", writes.join("; "), absolute(path));
            psh(files, script).await
        }
        Runtime::Python => {
            let program = format!(
                "f = open({:?}, 'a')\nfor s in {pieces:?}:\n    f.write(s)\n    f.flush()\nf.close()\n",
                absolute(path)
            );
            psh(files, format!("python3 -c {}", quoted(&program))).await
        }
        Runtime::Cell => {
            let ops = pieces
                .into_iter()
                .map(|piece| FileOp::Append(path, piece))
                .collect();
            program(Runtime::Cell, files, ops).await
        }
    }
}

/// A crash in the middle of an append written in pieces: the file holds its
/// first content and then whole pieces, and appending the rest gives every
/// piece once.
fn mid_append(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let runtime = RUNTIMES[rng.pick(RUNTIMES.len())];
    let path = FILES[rng.pick(FILES.len())];
    let first = words(rng, 4);
    let pieces: Vec<String> = (0..rng.range(PIECES_MIN, PIECES_MAX))
        .map(|at| format!("<{at}{}>", words(rng, 3)))
        .collect();
    let initial = vec![(path, first.clone())];
    let setup_fn: SetupFn = Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
        Box::pin(setup(files, initial.clone()))
    });
    let all = pieces.clone();
    let job: JobFn = Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
        Box::pin(appending(runtime, files, path, all.clone()))
    });
    let label = format!("append:{}", runtime.name());
    let Some(died) = die_during(rng, stats, digest, shape, &label, &setup_fn, &job)? else {
        return Ok(());
    };
    stats.points_append += 1;
    let when = died.when;
    let key = keyed(path);
    let filled = |count: usize| [first.as_bytes(), pieces[..count].concat().as_bytes()].concat();
    let held = died.contents.get(&key);
    let landed = (0..=pieces.len())
        .find(|&count| held == Some(&filled(count)))
        .ok_or_else(|| {
            format!(
                "{when}: {path} holds {:?}, not {first:?} followed by whole pieces of {pieces:?}",
                lossy(held)
            )
        })?;
    if died.contents.len() != 1 {
        return Err(format!(
            "{when}: the store holds files the append never wrote: {:?}",
            died.contents.keys().collect::<Vec<_>>()
        ));
    }
    let _ = write!(shape, " landed={landed}");
    if landed < pieces.len() {
        let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
        let rest = pieces[landed..].to_vec();
        let resumed = one(appending(runtime, Rc::new(files), path, rest), rng)?;
        if !resumed.ok {
            return Err(format!(
                "{when}: appending the rest failed: {}",
                resumed.output
            ));
        }
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    let want = filled(pieces.len());
    if again.get(&key) != Some(&want) {
        return Err(format!(
            "{when}: after resuming at piece {landed}, {path} holds {:?}, not {:?}",
            lossy(again.get(&key)),
            String::from_utf8_lossy(&want)
        ));
    }
    Ok(())
}

/// A crash around a rename between two appends: the store holds the files as
/// they were between two operations (the moved file once, under one name),
/// and running the rest of the operations finishes the job.
fn rename_across(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let runtime = RUNTIMES[rng.pick(RUNTIMES.len())];
    let mut order = FILES.to_vec();
    let from = order.remove(rng.pick(order.len()));
    let to = order.remove(rng.pick(order.len()));
    let side = order[0];
    let mut initial = vec![(from, words(rng, 4)), (side, words(rng, 4))];
    if rng.chance(HALF_PPM) {
        initial.push((to, words(rng, 4)));
    }
    let ops = vec![
        FileOp::Append(side, format!("<{}>", words(rng, 3))),
        FileOp::Rename(from, to),
        FileOp::Append(side, format!("<{}>", words(rng, 3))),
    ];
    let between = prefixes(&(initial.clone(), ops.clone()));
    let setup_fn: SetupFn = Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
        Box::pin(setup(files, initial.clone()))
    });
    let planned = ops.clone();
    let job: JobFn = Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
        Box::pin(program(runtime, files, planned.clone()))
    });
    let label = format!("rename:{}", runtime.name());
    let Some(died) = die_during(rng, stats, digest, shape, &label, &setup_fn, &job)? else {
        return Ok(());
    };
    stats.points_rename += 1;
    let when = died.when;
    let rest = match runtime {
        Runtime::Python => python_rest(&when, &died.contents, &between, &ops)?,
        _ => {
            let done = between
                .iter()
                .position(|held| *held == died.contents)
                .ok_or_else(|| {
                    format!(
                        "{when}: the store holds {:?}, which the files never held between two operations of {ops:?}",
                        died.contents
                    )
                })?;
            ops[done..].to_vec()
        }
    };
    let done = ops.len() - rest.len();
    let _ = write!(shape, " done={done}");
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let resumed = one(program(runtime, Rc::new(files), rest), rng)?;
    if !resumed.ok {
        return Err(format!(
            "{when}: running the rest after operation {done} failed: {}",
            resumed.output
        ));
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    let want = &between[between.len() - 1];
    if again != *want {
        return Err(format!(
            "{when}: after running the rest from operation {done}, the store holds {again:?}, not {want:?}"
        ));
    }
    Ok(())
}

/// What is left of a rename job after a Python run died. Python writes back
/// file by file when it ends, so each file holds what it held before the run
/// or after it, and the moved file may sit in both places but never in
/// neither. The rest is the side file's appends when they are missing and the
/// rename when its source is still there.
fn python_rest(
    when: &str,
    held: &Contents,
    between: &[Contents],
    ops: &[FileOp],
) -> Result<Vec<FileOp>, String> {
    let (start, end) = (&between[0], &between[between.len() - 1]);
    let paths: BTreeSet<&String> = start.keys().chain(end.keys()).chain(held.keys()).collect();
    for path in paths {
        let now = held.get(path);
        if now != start.get(path) && now != end.get(path) {
            return Err(format!(
                "{when}: {path} holds {:?}, neither what it held before the run nor after it",
                lossy(now)
            ));
        }
    }
    let (from, to, side) = match ops {
        [FileOp::Append(side, _), FileOp::Rename(from, to), FileOp::Append(_, _)] => (*from, *to, *side),
        _ => return Err(format!("{when}: not a rename job: {ops:?}")),
    };
    let moved = start.get(&keyed(from));
    if held.get(&keyed(from)).is_none() && held.get(&keyed(to)) != moved {
        return Err(format!(
            "{when}: {from} was moved to {to} but the store holds it in neither: {held:?}"
        ));
    }
    let mut rest = Vec::new();
    let key = keyed(side);
    if held.get(&key) != end.get(&key) {
        let (before, after) = (start.get(&key).map_or(0, Vec::len), &end[&key]);
        rest.push(FileOp::Append(
            side,
            String::from_utf8_lossy(&after[before..]).into_owned(),
        ));
    }
    if held.contains_key(&keyed(from)) {
        rest.push(FileOp::Rename(from, to));
    }
    Ok(rest)
}

/// A tree for the checkout's branches: each path's content, absent paths
/// untracked.
type Tree = BTreeMap<&'static str, String>;

fn writes(tree: &Tree) -> String {
    tree.iter()
        .map(|(path, content)| format!("printf '%s' {} > {path}\n", quoted(content)))
        .collect()
}

/// A crash during a git checkout's writes, from branch one to branch two: a
/// forced checkout of two over what the crash left must leave HEAD on two,
/// two's tree in the work tree, and nothing to commit.
fn checkout(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let mut first = Tree::new();
    for (at, path) in REPO_PATHS.into_iter().enumerate() {
        if at == 0 || rng.chance(TRACKED_PPM) {
            first.insert(path, words(rng, 4));
        }
    }
    let mut second = first.clone();
    for path in REPO_PATHS {
        match rng.pick(4) {
            0 => {}
            1 => {
                second.insert(path, format!("<two {}>", words(rng, 4)));
            }
            2 => {
                second.remove(path);
            }
            _ => second.entry(path).or_default().push_str("<two>"),
        }
    }
    if second == first || second.is_empty() {
        second.insert(REPO_PATHS[0], "<two>".to_owned());
    }
    let removed: String = first
        .keys()
        .filter(|path| !second.contains_key(*path))
        .map(|path| format!("rm -f {path}\n"))
        .collect();
    let repo = absolute(REPO);
    let script = format!(
        "set -e\n{GIT_ENV}\nmkdir -p {repo}/d\ncd {repo}\ngit init -q\n{}git add -A\ngit commit -qm one\ngit branch one\ngit checkout -q -b two\n{removed}{}git add -A\ngit commit -qm two\ngit checkout -q one\n",
        writes(&first),
        writes(&second)
    );
    let setup_fn: SetupFn = Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
        let script = script.clone();
        Box::pin(async move {
            setup(files.clone(), Vec::new()).await?;
            let said = psh(Rc::new(files), script).await;
            match said.ok {
                true => Ok(()),
                false => Err(format!("the repository's setup failed: {}", said.output)),
            }
        })
    });
    let switch = format!("set -e\n{GIT_ENV}\ncd {repo}\ngit checkout -q two\n");
    let job: JobFn = Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
        Box::pin(psh(files, switch.clone()))
    });
    let Some(died) = die_during(rng, stats, digest, shape, "checkout", &setup_fn, &job)? else {
        return Ok(());
    };
    stats.points_checkout += 1;
    let when = died.when;
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let resume = format!(
        "set -e\n{GIT_ENV}\ncd {repo}\nrm -f .git/index.lock .git/HEAD.lock\ngit checkout -q -f two\nhead=$(cat .git/HEAD)\nstatus=$(git status --porcelain)\necho \"head=$head status=$status\"\n[ \"$head\" = 'ref: refs/heads/two' ]\n[ -z \"$status\" ]\n"
    );
    let resumed = one(psh(Rc::new(files), resume), rng)?;
    if !resumed.ok {
        return Err(format!(
            "{when}: a forced checkout of two over what the crash left failed: {}",
            resumed.output
        ));
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    let prefix = keyed(&format!("{REPO}/"));
    let git = keyed(&format!("{REPO}/.git/"));
    let tree: BTreeMap<String, String> = again
        .iter()
        .filter(|(path, _)| path.starts_with(&prefix) && !path.starts_with(&git))
        .map(|(path, bytes)| {
            (
                path[prefix.len()..].to_owned(),
                String::from_utf8_lossy(bytes).into_owned(),
            )
        })
        .collect();
    let want: BTreeMap<String, String> = second
        .iter()
        .map(|(path, content)| ((*path).to_owned(), content.clone()))
        .collect();
    if tree != want {
        return Err(format!(
            "{when}: after a forced checkout of two the work tree holds {tree:?}, not {want:?}"
        ));
    }
    Ok(())
}

/// One job that writes many files at once: whole files (some new, in new
/// directories, some replacing old ones), old files moved and old files removed.
#[derive(Clone)]
struct ManyFiles {
    initial: Vec<(&'static str, String)>,
    writes: Vec<(&'static str, String)>,
    moves: Vec<(&'static str, &'static str)>,
    removes: Vec<&'static str>,
}

#[derive(Clone, Copy)]
enum Author {
    /// A Python program, whose writes reach the workspace together when it exits.
    Python,
    /// A Code Mode cell writing through node:fs, connected to the workspace.
    Bridge,
}

impl Author {
    const fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Bridge => "bridge",
        }
    }
}

fn parent(path: &str) -> String {
    path.rsplit_once('/').map_or_else(|| ROOT.to_owned(), |(dir, _)| dir.to_owned())
}

impl ManyFiles {
    fn new(rng: &mut Rng) -> Self {
        let initial: Vec<(&'static str, String)> = OLD.iter().map(|&path| (path, words(rng, 4))).collect();
        let mut free = OLD.to_vec();
        let mut take = |rng: &mut Rng| free.remove(rng.pick(free.len()));
        let replaced: Vec<&'static str> = (0..rng.range(1, 2)).map(|_| take(rng)).collect();
        let moves = (0..rng.range(1, 2)).map(|at| (take(rng), MOVED[at as usize])).collect();
        let removes = if rng.chance(HALF_PPM) { vec![take(rng)] } else { Vec::new() };
        let fresh = rng.range(MANY_MIN, MANY_MAX) as usize - replaced.len();
        let writes = replaced
            .into_iter()
            .chain(NEW[..fresh].iter().copied())
            .enumerate()
            .map(|(at, path)| (path, format!("<w{at}{}>", words(rng, 3))))
            .collect();
        Self {
            initial,
            writes,
            moves,
            removes,
        }
    }

    fn initial(&self, path: &str) -> Option<&String> {
        self.initial.iter().find(|(at, _)| *at == path).map(|(_, content)| content)
    }

    /// What each path the job touches may hold after a crash part of the way
    /// through it, and what it holds once the job finished.
    fn allowed(&self, path: &str) -> (Vec<Option<&String>>, Option<&String>) {
        if let Some((_, content)) = self.writes.iter().find(|(at, _)| *at == path) {
            return (vec![self.initial(path), Some(content)], Some(content));
        }
        if let Some(&(from, _)) = self.moves.iter().find(|(_, to)| *to == path) {
            return (vec![None, self.initial(from)], self.initial(from));
        }
        if self.moves.iter().any(|(from, _)| *from == path) || self.removes.contains(&path) {
            return (vec![self.initial(path), None], None);
        }
        (vec![self.initial(path)], self.initial(path))
    }

    fn paths(&self) -> BTreeSet<&'static str> {
        let mut paths: BTreeSet<&'static str> = OLD.iter().copied().collect();
        paths.extend(self.writes.iter().map(|(path, _)| *path));
        paths.extend(self.moves.iter().map(|(_, to)| *to));
        paths
    }

    /// The job, written so running it again after a crash finishes it.
    fn code(&self, writer: Author) -> String {
        let writes: Vec<(String, String, String)> = self
            .writes
            .iter()
            .map(|(path, content)| (absolute(path), parent(&absolute(path)), content.clone()))
            .collect();
        let moves: Vec<(String, String, String)> = self
            .moves
            .iter()
            .map(|(from, to)| (absolute(from), absolute(to), parent(&absolute(to))))
            .collect();
        let removes: Vec<String> = self.removes.iter().map(|path| absolute(path)).collect();
        match writer {
            Author::Python => {
                let program = format!(
                    "import os\nfor p, d, s in {writes:?}:\n    os.makedirs(d, exist_ok=True)\n    open(p, 'w').write(s)\nfor a, b, d in {moves:?}:\n    if os.path.exists(a):\n        os.makedirs(d, exist_ok=True)\n        os.rename(a, b)\nfor p in {removes:?}:\n    if os.path.exists(p):\n        os.remove(p)\n"
                );
                format!("python3 -c {}", quoted(&program))
            }
            Author::Bridge => format!(
                "const fs = require(\"node:fs\");\nfor (const [p, d, s] of {}) {{ fs.mkdirSync(d, {{ recursive: true }}); fs.writeFileSync(p, s); }}\nfor (const [a, b, d] of {}) if (fs.existsSync(a)) {{ fs.mkdirSync(d, {{ recursive: true }}); fs.renameSync(a, b); }}\nfor (const p of {}) if (fs.existsSync(p)) fs.unlinkSync(p);\n",
                json!(writes),
                json!(moves),
                json!(removes)
            ),
        }
    }
}

async fn write_many(writer: Author, files: Rc<dyn FileSystem>, code: String) -> Said {
    match writer {
        Author::Python => psh(files, code).await,
        Author::Bridge => cell_with(files, code, Some(BRIDGED)).await,
    }
}

/// A crash part of the way through one job's writes to many files, from a
/// Python program's write-back or a cell's node:fs: every file holds its old
/// content or its whole new one, a moved file is under one of its names or
/// both but never neither, and running the job again finishes it.
fn many(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let writer = [Author::Python, Author::Bridge][rng.pick(2)];
    let job = ManyFiles::new(rng);
    let code = job.code(writer);
    let initial = job.initial.clone();
    let setup_fn: SetupFn = Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
        Box::pin(setup(files, initial.clone()))
    });
    let ran = code.clone();
    let job_fn: JobFn = Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
        Box::pin(write_many(writer, files, ran.clone()))
    });
    let label = format!("many:{}", writer.name());
    let _ = write!(shape, " files={}", job.writes.len());
    let Some(died) = die_during(rng, stats, digest, shape, &label, &setup_fn, &job_fn)? else {
        return Ok(());
    };
    stats.points_many += 1;
    let when = died.when;
    let paths = job.paths();
    if let Some(stray) = died.contents.keys().find(|key| !paths.iter().any(|path| keyed(path) == **key)) {
        return Err(format!("{when}: the store holds {stray}, which the job never wrote"));
    }
    for path in &paths {
        let held = died.contents.get(&keyed(path)).map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        let (may, _) = job.allowed(path);
        if !may.iter().any(|content| content.cloned() == held) {
            return Err(format!("{when}: {path} holds {held:?}, not one of {may:?}"));
        }
    }
    if let Some((from, to)) = job
        .moves
        .iter()
        .find(|(from, to)| !died.contents.contains_key(&keyed(from)) && !died.contents.contains_key(&keyed(to)))
    {
        return Err(format!("{when}: moving {from} to {to} left it under neither name"));
    }
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let resumed = one(write_many(writer, Rc::new(files), code), rng)?;
    if !resumed.ok {
        return Err(format!("{when}: running the job again failed: {}", resumed.output));
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    for path in &paths {
        let held = again.get(&keyed(path)).map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        let (_, done) = job.allowed(path);
        if held.as_ref() != done {
            return Err(format!("{when}: after running the job again, {path} holds {held:?}, not {done:?}"));
        }
    }
    if again.len() != paths.iter().filter(|path| job.allowed(path).1.is_some()).count() {
        return Err(format!(
            "{when}: after running the job again the store holds {:?}",
            again.keys().collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// A crash while git push writes the remote-tracking refs, after the server
/// took the push: each ref must hold its old commit or the pushed one,
/// pushing again must leave the server on every pushed commit (and each ref
/// on its old commit or the pushed one), and fetching then must leave every
/// ref on its branch's commit.
fn push(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let kept = &BRANCHES[..rng.range(1, BRANCHES.len() as u64) as usize];
    let mut moved: Vec<&'static str> = kept.iter().copied().filter(|_| rng.chance(HALF_PPM)).collect();
    let fresh = rng.chance(HALF_PPM);
    if moved.is_empty() && !fresh {
        moved.push(kept[0]);
    }
    let mut pushed: Vec<&'static str> = kept.to_vec();
    if fresh {
        pushed.push(FRESH);
    }
    let named = pushed.join(" ");
    let repo = absolute(PUSH_REPO);
    let mut script = format!(
        "set -e\n{GIT_ENV}\nmkdir -p {repo}\ncd {repo}\ngit init -q\nprintf '%s' {} > f.txt\ngit add -A\ngit commit -qm base\ngit checkout -q -b {}\n",
        quoted(&words(rng, 4)),
        kept[0]
    );
    for branch in &kept[1..] {
        let _ = writeln!(script, "git branch {branch}");
    }
    let _ = writeln!(
        script,
        "git remote add origin {}\ngit push -q origin {}",
        Origin::url(),
        kept.join(" ")
    );
    for branch in &moved {
        let _ = writeln!(
            script,
            "git checkout -q {branch}\nprintf '%s' {} > {branch}.txt\ngit add -A\ngit commit -qm {branch}",
            quoted(&words(rng, 4))
        );
    }
    if fresh {
        let _ = writeln!(script, "git branch {FRESH}");
    }
    let seed = rng.below(u64::MAX);
    let origin: Rc<std::cell::RefCell<Option<Origin>>> = Rc::default();
    // Each branch's remote-tracking ref before the push (None for a new
    // branch) and the commit the push sends, both as their files hold them.
    let refs: Rc<std::cell::RefCell<BTreeMap<&'static str, (Option<String>, String)>>> = Rc::default();
    let network = {
        let origin = origin.clone();
        move || {
            let origin = origin.borrow();
            let origin = origin.as_ref().expect("setup made the server");
            (origin.fetch(), origin.credentials())
        }
    };
    let setup_fn: SetupFn = {
        let (origin, refs, network, pushed) = (origin.clone(), refs.clone(), network.clone(), pushed.clone());
        Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
            *origin.borrow_mut() = Some(Origin::new(seed));
            let (script, refs, network, pushed) = (script.clone(), refs.clone(), network(), pushed.clone());
            Box::pin(async move {
                setup(files.clone(), Vec::new()).await?;
                let said = psh_on(Rc::new(files.clone()), script, Some(network)).await;
                if !said.ok {
                    return Err(format!("the pushing repository's setup failed: {}", said.output));
                }
                let mut held = BTreeMap::new();
                for branch in pushed {
                    let read = |path: String| {
                        let files = files.clone();
                        async move {
                            let path = absolute(&format!("{PUSH_REPO}/.git/{path}"));
                            files
                                .read(path.as_bytes(), u64::MAX)
                                .get()
                                .await
                                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        }
                    };
                    let old = read(format!("refs/remotes/origin/{branch}")).await.ok();
                    let new = read(format!("refs/heads/{branch}"))
                        .await
                        .map_err(|error| format!("setup left no loose ref for {branch}: {error}"))?;
                    held.insert(branch, (old, new));
                }
                *refs.borrow_mut() = held;
                Ok(())
            })
        })
    };
    let job = format!("set -e\ncd {repo}\ngit push -q origin {named}\n");
    let job_fn: JobFn = {
        let network = network.clone();
        Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
            Box::pin(psh_on(files, job.clone(), Some(network())))
        })
    };
    let _ = write!(shape, " pushed={}", pushed.len());
    let Some(died) = die_during(rng, stats, digest, shape, "push", &setup_fn, &job_fn)? else {
        return Ok(());
    };
    stats.points_push += 1;
    let when = died.when;
    let refs = refs.borrow().clone();
    let tracking = |branch: &str| keyed(&format!("{PUSH_REPO}/.git/refs/remotes/origin/{branch}"));
    for (branch, (old, new)) in &refs {
        let held = lossy(died.contents.get(&tracking(branch)));
        if held != *old && held.as_ref() != Some(new) {
            return Err(format!(
                "{when}: the remote-tracking ref of {branch} holds {held:?}, not {old:?} or {new:?}"
            ));
        }
    }
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let again = format!(
        "set -e\ncd {repo}\nrm -f .git/*.lock .git/refs/remotes/origin/*.lock\ngit push -q origin {named}\n"
    );
    let resumed = one(psh_on(Rc::new(files), again, Some(network())), rng)?;
    if !resumed.ok {
        return Err(format!("{when}: pushing again failed: {}", resumed.output));
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    let heads = origin.borrow().as_ref().map(Origin::heads).unwrap_or_default();
    for (branch, (old, new)) in &refs {
        let held = lossy(again.get(&tracking(branch)));
        if held != *old && held.as_ref() != Some(new) {
            return Err(format!(
                "{when}: after pushing again the remote-tracking ref of {branch} holds {held:?}, not {old:?} or {new:?}"
            ));
        }
        if heads.get(*branch).map(String::as_str) != Some(new.trim_end()) {
            return Err(format!(
                "{when}: after pushing again the server holds {branch} at {:?}, not {new:?}",
                heads.get(*branch)
            ));
        }
    }
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let fetch = format!("set -e\ncd {repo}\ngit fetch -q origin\n");
    let fetched = one(psh_on(Rc::new(files), fetch, Some(network())), rng)?;
    if !fetched.ok {
        return Err(format!("{when}: fetching after pushing again failed: {}", fetched.output));
    }
    let after = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    for (branch, (_, new)) in &refs {
        let held = lossy(after.get(&tracking(branch)));
        if held.as_ref() != Some(new) {
            return Err(format!(
                "{when}: after fetching the remote-tracking ref of {branch} holds {held:?}, not {new:?}"
            ));
        }
    }
    Ok(())
}

/// A crash during a Python program's atomic writes: each target written to a
/// temporary file in its directory, then moved over it with os.replace. Every
/// target holds its old content or its whole new one, no temporary file is
/// left, and running the program again finishes it.
fn atomic(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    let initial: Vec<(&'static str, String)> = ATOMIC[..ATOMIC.len() - 1]
        .iter()
        .map(|&path| (path, words(rng, 4)))
        .collect();
    let mut targets: Vec<(&'static str, String)> = Vec::new();
    for path in ATOMIC {
        if rng.chance(HALF_PPM) {
            targets.push((path, format!("<a{}{}>", targets.len(), words(rng, 3))));
        }
    }
    if targets.is_empty() {
        targets.push((ATOMIC[0], format!("<a{}>", words(rng, 3))));
    }
    let listed: Vec<(String, String)> = targets
        .iter()
        .map(|(path, content)| (absolute(path), content.clone()))
        .collect();
    let program = format!(
        "import os, tempfile\nfor p, s in {listed:?}:\n    d = os.path.dirname(p)\n    os.makedirs(d, exist_ok=True)\n    fd, t = tempfile.mkstemp(dir=d, prefix='.tmp-')\n    with os.fdopen(fd, 'w') as f:\n        f.write(s)\n    os.replace(t, p)\n"
    );
    let code = format!("python3 -c {}", quoted(&program));
    let setup_fn: SetupFn = {
        let initial = initial.clone();
        Box::new(move |files: WorkspaceFs| -> Boxed<'static, Result<(), String>> {
            Box::pin(setup(files, initial.clone()))
        })
    };
    let job_fn: JobFn = {
        let code = code.clone();
        Box::new(move |files: Rc<dyn FileSystem>| -> Boxed<'static, Said> {
            Box::pin(psh(files, code.clone()))
        })
    };
    let _ = write!(shape, " targets={}", targets.len());
    let Some(died) = die_during(rng, stats, digest, shape, "atomic", &setup_fn, &job_fn)? else {
        return Ok(());
    };
    stats.points_atomic += 1;
    let when = died.when;
    let old = |path: &str| initial.iter().find(|(at, _)| *at == path).map(|(_, content)| content.clone());
    let new = |path: &str| targets.iter().find(|(at, _)| *at == path).map(|(_, content)| content.clone());
    if let Some(stray) = died.contents.keys().find(|key| !ATOMIC.iter().any(|path| keyed(path) == **key)) {
        return Err(format!("{when}: the store holds {stray}, which the program never meant to leave"));
    }
    for path in ATOMIC {
        let held = lossy(died.contents.get(&keyed(path)));
        if held != old(path) && (held.is_none() || held != new(path)) {
            return Err(format!(
                "{when}: {path} holds {held:?}, not {:?} or {:?}",
                old(path),
                new(path)
            ));
        }
    }
    let files = one(open(Dying::new(died.inner.clone(), None)), rng)??;
    let resumed = one(psh(Rc::new(files), code), rng)?;
    if !resumed.ok {
        return Err(format!("{when}: running the program again failed: {}", resumed.output));
    }
    stats.resumed += 1;
    let again = reopened(&died.inner, rng, digest).map_err(|error| format!("{when}: {error}"))?;
    let mut want = 0;
    for path in ATOMIC {
        let held = lossy(again.get(&keyed(path)));
        let done = new(path).or_else(|| old(path));
        want += usize::from(done.is_some());
        if held != done {
            return Err(format!("{when}: after running it again {path} holds {held:?}, not {done:?}"));
        }
    }
    if again.len() != want {
        return Err(format!(
            "{when}: after running it again the store holds {:?}",
            again.keys().collect::<Vec<_>>()
        ));
    }
    Ok(())
}

/// One crash point per seed: mid-append, mid-checkout or across a rename.
fn points(
    rng: &mut Rng,
    stats: &mut WorkspaceCrashStats,
    digest: &mut u64,
    shape: &mut String,
) -> Result<(), String> {
    match rng.pick(3) {
        0 => mid_append(rng, stats, digest, shape),
        1 => checkout(rng, stats, digest, shape),
        _ => rename_across(rng, stats, digest, shape),
    }
}

fn simulate(seed: u64, mutation: WorkspaceCrashMutation) -> WorkspaceCrashOutcome {
    let mut rng = Rng::derive(seed, "workspace_crash");
    let mut stats = WorkspaceCrashStats {
        seeds: 1,
        ..WorkspaceCrashStats::default()
    };
    let mut digest = FNV_OFFSET;
    let mut shape = String::new();
    crate::host::enter(seed, &crate::executor::Clock::new());
    fs_sim::mutate(mutation.rule());
    BEND.with(|bend| bend.set(mutation.bend()));
    let wrong = crash(&mut rng, &mut stats, &mut digest, &mut shape)
        .and_then(|()| race(&mut rng, &mut stats, &mut digest, &mut shape))
        .and_then(|()| {
            let mut rng = Rng::derive(seed, "workspace_crash_points");
            points(&mut rng, &mut stats, &mut digest, &mut shape)
        })
        .and_then(|()| {
            let mut rng = Rng::derive(seed, "workspace_crash_many");
            many(&mut rng, &mut stats, &mut digest, &mut shape)
        })
        .and_then(|()| {
            let mut rng = Rng::derive(seed, "workspace_crash_push");
            push(&mut rng, &mut stats, &mut digest, &mut shape)
        })
        .and_then(|()| {
            let mut rng = Rng::derive(seed, "workspace_crash_atomic");
            atomic(&mut rng, &mut stats, &mut digest, &mut shape)
        })
        .err();
    fs_sim::mutate(FsRule::None);
    BEND.with(|bend| bend.set(Bend::None));
    digest = fnv(digest, shape.as_bytes());
    WorkspaceCrashOutcome {
        digest,
        wrong,
        stats,
        shape: shape.trim().to_owned(),
    }
}

fn run_seed(seed: u64, mutation: WorkspaceCrashMutation) -> WorkspaceCrashOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        fs_sim::mutate(FsRule::None);
        BEND.with(|bend| bend.set(Bend::None));
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        WorkspaceCrashOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: WorkspaceCrashStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: WorkspaceCrashMutation,
    twice: bool,
    verbose: bool,
    totals: &mut WorkspaceCrashStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!("seed {seed} digest {:016x} {}", first.digest, first.shape);
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: WorkspaceCrashMutation) -> String {
    let mut command = format!("nanocode-sim --scenario workspace_crash --seed {seed}");
    if mutation != WorkspaceCrashMutation::None {
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
    mutation: WorkspaceCrashMutation,
    totals: &WorkspaceCrashStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=workspace_crash mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        WorkspaceCrashMutation::None if failed == 0 => ExitCode::SUCCESS,
        WorkspaceCrashMutation::None => ExitCode::FAILURE,
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
