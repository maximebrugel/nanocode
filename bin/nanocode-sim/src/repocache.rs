use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    ops::Range,
    pin::{Pin, pin},
    process::ExitCode,
    rc::Rc,
    task::{Context, Poll, Waker},
    time::Duration,
};

use std::os::unix::{
    ffi::OsStrExt,
    fs::{DirBuilderExt, MetadataExt},
};

use nanocode_bytes::Bytes;
use nanocode_repo_cache::{
    Config, Error, Id, Kind, Object, Objects, Ref, Schedule, Store, haves, missing, open, pack,
    read_ref, receive, refs,
    sim::{self as cache_sim, Mutation as CacheRule},
    update_ref, wants,
};
use nanocode_shell::{FileSystem, Kind as FsKind, Machine, MemFs, Remove, Reply, Stat, Write};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const HOSTS: usize = 2;
const STEPS_MIN: u64 = 20;
const STEPS_MAX: u64 = 80;
const AGENTS_MAX: usize = 6;
const ROUND_MAX: usize = 4;
const INTERVAL_MIN: u64 = 1;
const INTERVAL_MAX: u64 = 5;
const TICKS_MAX: u64 = 3;
const PUT_FAIL_PPM: u64 = 250_000;
const READ_LIMIT: u64 = 4096;
const HAVES_LIMIT: usize = 1 << 16;
const TICK_NANOS: u64 = 1_000;
const MAIN: &[u8] = b"main";
const ORIGIN: &[u8] = b"origin";
const FILE_MODE: &[u8] = b"100644 f\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoCacheMutation {
    None,
    SharedHead,
    FetchMovesHeads,
    RefsBeforeObjects,
    ResendHeld,
    SharedLock,
}

impl RepoCacheMutation {
    const ALL: [Self; 6] = [
        Self::None,
        Self::SharedHead,
        Self::FetchMovesHeads,
        Self::RefsBeforeObjects,
        Self::ResendHeld,
        Self::SharedLock,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SharedHead => "shared_head",
            Self::FetchMovesHeads => "fetch_moves_heads",
            Self::RefsBeforeObjects => "refs_before_objects",
            Self::ResendHeld => "resend_held",
            Self::SharedLock => "shared_lock",
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

    const fn rule(self) -> CacheRule {
        match self {
            Self::None => CacheRule::None,
            Self::SharedHead => CacheRule::SharedHead,
            Self::FetchMovesHeads => CacheRule::FetchMovesHeads,
            Self::RefsBeforeObjects => CacheRule::RefsBeforeObjects,
            Self::ResendHeld => CacheRule::ResendHeld,
            Self::SharedLock => CacheRule::SharedLock,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RepoCacheStats {
    pub seeds: u64,
    pub worktrees: u64,
    pub commits: u64,
    pub concurrent_rounds: u64,
    pub fetches: u64,
    pub fetches_not_due: u64,
    pub transfers: u64,
    pub failed_puts: u64,
    pub objects_sent: u64,
    pub objects_without_haves: u64,
    pub refs_moved: u64,
}

impl RepoCacheStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.worktrees += other.worktrees;
        self.commits += other.commits;
        self.concurrent_rounds += other.concurrent_rounds;
        self.fetches += other.fetches;
        self.fetches_not_due += other.fetches_not_due;
        self.transfers += other.transfers;
        self.failed_puts += other.failed_puts;
        self.objects_sent += other.objects_sent;
        self.objects_without_haves += other.objects_without_haves;
        self.refs_moved += other.refs_moved;
    }
}

struct Clock(Cell<u64>);

impl Machine for Clock {
    fn now(&self) -> Duration {
        let now = self.0.get() + TICK_NANOS;
        self.0.set(now);
        Duration::from_nanos(now)
    }

    fn sleep(&self, _: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::ready(()))
    }

    fn random(&self, bytes: &mut [u8]) {
        bytes.fill(0);
    }
}

struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

struct Slow(Rc<MemFs>);

fn later<T: 'static>(fs: &Rc<MemFs>, op: impl FnOnce(&MemFs) -> Reply<T> + 'static) -> Reply<T> {
    let fs = Rc::clone(fs);
    Reply::Later(Box::pin(async move {
        YieldOnce(false).await;
        op(&fs).get().await
    }))
}

impl FileSystem for Slow {
    fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat> {
        let path = path.to_vec();
        later(&self.0, move |fs| fs.stat(&path, follow))
    }

    fn read(&self, path: &[u8], limit: u64) -> Reply<Bytes> {
        let path = path.to_vec();
        later(&self.0, move |fs| fs.read(&path, limit))
    }

    fn write(&self, path: &[u8], data: &[u8], how: Write) -> Reply<()> {
        let (path, data) = (path.to_vec(), data.to_vec());
        later(&self.0, move |fs| fs.write(&path, &data, how))
    }

    fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>> {
        let path = path.to_vec();
        later(&self.0, move |fs| fs.list(&path))
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> Reply<()> {
        let path = path.to_vec();
        later(&self.0, move |fs| fs.mkdir(&path, mode))
    }

    fn remove(&self, path: &[u8], what: Remove) -> Reply<()> {
        let path = path.to_vec();
        later(&self.0, move |fs| fs.remove(&path, what))
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> Reply<()> {
        let (from, to) = (from.to_vec(), to.to_vec());
        later(&self.0, move |fs| fs.rename(&from, &to))
    }
}

#[derive(Default)]
struct Mem {
    map: RefCell<BTreeMap<Id, Object>>,
    fail_next: Cell<bool>,
}

impl Objects for Mem {
    fn get(&self, id: &Id) -> Option<Object> {
        self.map.borrow().get(id).cloned()
    }

    fn has(&self, id: &Id) -> bool {
        self.map.borrow().contains_key(id)
    }

    fn put_all(&self, objects: Vec<Object>) -> Reply<()> {
        if self.fail_next.take() {
            return Reply::Now(Err(io::Error::other("injected put failure")));
        }
        let mut map = self.map.borrow_mut();
        for object in objects {
            map.insert(object.id(), object);
        }
        Reply::Now(Ok(()))
    }
}

fn drive<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
}

fn interleave<T>(rng: &mut Rng, futures: Vec<Pin<Box<dyn Future<Output = T> + '_>>>) -> Vec<T> {
    let mut context = Context::from_waker(Waker::noop());
    let mut slots: Vec<Option<Pin<Box<dyn Future<Output = T> + '_>>>> =
        futures.into_iter().map(Some).collect();
    let mut done: Vec<Option<T>> = slots.iter().map(|_| None).collect();
    while slots.iter().any(Option::is_some) {
        let live: Vec<usize> = (0..slots.len()).filter(|&at| slots[at].is_some()).collect();
        let at = live[rng.pick(live.len())];
        if let Some(future) = slots[at].as_mut()
            && let Poll::Ready(value) = future.as_mut().poll(&mut context)
        {
            done[at] = Some(value);
            slots[at] = None;
        }
    }
    done.into_iter().flatten().collect()
}

type Update<'a> = Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>>;

struct Agent {
    name: Vec<u8>,
    branch: Vec<u8>,
    dir: Vec<u8>,
    last: Id,
}

struct Host {
    fs: Rc<dyn FileSystem>,
    objects: Mem,
    store: Store,
    agents: Vec<Agent>,
    schedule: Schedule,
}

struct World {
    rng: Rng,
    now: Duration,
    tick: Duration,
    made: u64,
    origin: Mem,
    origin_tip: Id,
    hosts: Vec<Host>,
    stats: RepoCacheStats,
}

fn obj(kind: Kind, data: Vec<u8>) -> Object {
    Object {
        kind,
        data: Bytes::from(data),
    }
}

fn hex(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

impl World {
    fn commit(&mut self, parent: Option<Id>) -> (Id, Vec<Object>) {
        self.made += 1;
        let blob = obj(
            Kind::Blob,
            format!("file {} {}", self.made, self.rng.draw()).into_bytes(),
        );
        let tree = obj(Kind::Tree, [FILE_MODE, &blob.id().0].concat());
        let mut text = format!("tree {}\n", tree.id().hex());
        if let Some(parent) = parent {
            text.push_str(&format!("parent {}\n", parent.hex()));
        }
        text.push_str(&format!(
            "author a <a> {} +0000\n\ncommit {}\n",
            self.made, self.made
        ));
        let commit = obj(Kind::Commit, text.into_bytes());
        (commit.id(), vec![blob, tree, commit])
    }

    fn advance_origin(&mut self) {
        let (tip, objects) = self.commit(Some(self.origin_tip));
        drop(self.origin.put_all(objects));
        self.origin_tip = tip;
    }

    fn tracking(&self, host: usize) -> Result<Option<Id>, String> {
        let h = &self.hosts[host];
        drive(read_ref(&*h.fs, &h.store, &h.store.tracking(ORIGIN, MAIN)))
            .map_err(|error| error.to_string())
    }

    fn add_agent(&mut self, host: usize) -> Result<(), String> {
        if self.hosts[host].agents.len() >= AGENTS_MAX {
            return Ok(());
        }
        let Some(start) = self.tracking(host)? else {
            return Ok(());
        };
        let n = self.hosts[host].agents.len();
        let name = format!("a{n}").into_bytes();
        let branch = format!("h{host}-a{n}").into_bytes();
        let dir = format!("/h{host}/wt/a{n}").into_bytes();
        let h = &self.hosts[host];
        let made = drive(h.store.worktree(&*h.fs, &name, &dir, &branch, start))
            .map_err(|error| error.to_string())?;
        if made.dir != dir {
            return Err("worktree came back at another directory".to_owned());
        }
        self.hosts[host].agents.push(Agent {
            name,
            branch,
            dir,
            last: start,
        });
        self.stats.worktrees += 1;
        self.check_isolation(host)
    }

    fn check_isolation(&self, host: usize) -> Result<(), String> {
        let h = &self.hosts[host];
        let mut links = BTreeSet::new();
        for agent in &h.agents {
            let link = drive(
                h.fs.read(&[agent.dir.as_slice(), b"/.git"].concat(), READ_LIMIT)
                    .get(),
            )
            .map_err(|error| format!("{} .git: {error}", hex(&agent.name)))?;
            let git_dir = link
                .strip_prefix(b"gitdir: ")
                .and_then(|rest| rest.strip_suffix(b"\n"))
                .ok_or("a .git file names its gitdir")?
                .to_vec();
            if git_dir == h.store.root() || !links.insert(git_dir.clone()) {
                return Err(format!(
                    "worktree {} shares its gitdir, so its HEAD and index",
                    hex(&agent.name)
                ));
            }
            let head = drive(
                h.fs.read(&[git_dir.as_slice(), b"/HEAD"].concat(), READ_LIMIT)
                    .get(),
            )
            .map_err(|error| error.to_string())?;
            if head.as_ref() != [b"ref: ".as_slice(), &Store::head(&agent.branch), b"\n"].concat() {
                return Err(format!(
                    "worktree {} HEAD names another branch",
                    hex(&agent.name)
                ));
            }
            let back = drive(
                h.fs.read(&[git_dir.as_slice(), b"/gitdir"].concat(), READ_LIMIT)
                    .get(),
            )
            .map_err(|error| error.to_string())?;
            if back.as_ref() != [agent.dir.as_slice(), b"/.git\n"].concat() {
                return Err(format!(
                    "worktree {} gitdir does not point back",
                    hex(&agent.name)
                ));
            }
        }
        let listed = drive(h.store.worktrees(&*h.fs)).map_err(|error| error.to_string())?;
        if listed.len() != h.agents.len() {
            return Err(format!(
                "{} worktrees listed for {} agents",
                listed.len(),
                h.agents.len()
            ));
        }
        Ok(())
    }

    fn round(&mut self, host: usize) -> Result<(), String> {
        let count = self.hosts[host].agents.len();
        if count == 0 {
            return Ok(());
        }
        let size = self.rng.range(1, count.min(ROUND_MAX) as u64) as usize;
        let mut picked: Vec<usize> = (0..count).collect();
        for at in 0..size {
            let swap = at + self.rng.pick(count - at);
            picked.swap(at, swap);
        }
        picked.truncate(size);
        let mut planned = Vec::new();
        for &at in &picked {
            let last = self.hosts[host].agents[at].last;
            let (tip, objects) = self.commit(Some(last));
            drop(self.hosts[host].objects.put_all(objects));
            planned.push((
                at,
                Store::head(&self.hosts[host].agents[at].branch),
                last,
                tip,
            ));
        }
        let h = &self.hosts[host];
        let futures: Vec<Update<'_>> = planned
            .iter()
            .map(|(_, name, last, tip)| {
                Box::pin(update_ref(&*h.fs, &h.store, name, Some(*last), *tip)) as Update<'_>
            })
            .collect();
        let results = interleave(&mut self.rng, futures);
        if size > 1 {
            self.stats.concurrent_rounds += 1;
        }
        for ((at, name, _, tip), result) in planned.into_iter().zip(results) {
            result.map_err(|error| {
                format!(
                    "commit on {} lost to a commit on another branch: {error}",
                    hex(&name)
                )
            })?;
            self.hosts[host].agents[at].last = tip;
            self.stats.commits += 1;
        }
        self.check_heads(host)
    }

    fn check_heads(&self, host: usize) -> Result<(), String> {
        let h = &self.hosts[host];
        for agent in &h.agents {
            let now = drive(read_ref(&*h.fs, &h.store, &Store::head(&agent.branch)))
                .map_err(|error| error.to_string())?;
            if now != Some(agent.last) {
                return Err(format!(
                    "branch {} moved without its agent",
                    hex(&agent.branch)
                ));
            }
            reachable(&h.objects, agent.last)?;
        }
        Ok(())
    }

    fn snapshot(&self, host: usize) -> Result<(Vec<Ref>, usize), String> {
        let h = &self.hosts[host];
        let all = drive(refs(&*h.fs, &h.store, b"refs/")).map_err(|error| error.to_string())?;
        Ok((all, h.objects.map.borrow().len()))
    }

    fn sync(&mut self, from: Option<usize>, to: usize) -> Result<(), String> {
        let advertised = match from {
            None => vec![Ref {
                name: Store::head(MAIN),
                id: self.origin_tip,
            }],
            Some(source) => {
                let h = &self.hosts[source];
                drive(refs(&*h.fs, &h.store, b"refs/heads/")).map_err(|error| error.to_string())?
            }
        };
        let peer = from.map_or_else(
            || ORIGIN.to_vec(),
            |source| format!("h{source}").into_bytes(),
        );
        let (before, count) = self.snapshot(to)?;
        let tips: Vec<Id> = before.iter().map(|known| known.id).collect();
        let haves = haves(&self.hosts[to].objects, &tips, HAVES_LIMIT);
        let sender: &Mem = from.map_or(&self.origin, |source| &self.hosts[source].objects);
        let want = wants(&advertised, &self.hosts[to].objects);
        let sent = pack(sender, &want, &haves).map_err(|error| error.to_string())?;
        let naive = missing(sender, &want, &[])
            .map_err(|error| error.to_string())?
            .len();
        let resent = sent
            .iter()
            .filter(|object| self.hosts[to].objects.has(&object.id()))
            .count();
        if resent > 0 {
            return Err(format!(
                "transfer sent {resent} objects the receiver already held"
            ));
        }
        let fail = self.rng.chance(PUT_FAIL_PPM);
        self.hosts[to].objects.fail_next.set(fail);
        let h = &self.hosts[to];
        let landed = drive(receive(
            &*h.fs,
            &h.store,
            &h.objects,
            sent.clone(),
            &advertised,
            &peer,
        ));
        h.objects.fail_next.set(false);
        self.stats.objects_sent += sent.len() as u64;
        self.stats.objects_without_haves += naive as u64;
        let (after, now_count) = self.snapshot(to)?;
        let heads = |all: &[Ref]| -> Vec<Ref> {
            all.iter()
                .filter(|known| known.name.starts_with(b"refs/heads/"))
                .cloned()
                .collect()
        };
        if heads(&before) != heads(&after) {
            return Err("a fetch moved a branch a worktree has checked out".to_owned());
        }
        match landed {
            Ok(received) => {
                self.stats.refs_moved += received.refs as u64;
                if now_count != count + sent.len() {
                    return Err(format!(
                        "{} objects landed of {} sent",
                        now_count - count,
                        sent.len()
                    ));
                }
                for known in &advertised {
                    reachable(&self.hosts[to].objects, known.id)?;
                }
            }
            Err(error) => {
                if !fail {
                    return Err(format!("transfer failed without a fault: {error}"));
                }
                self.stats.failed_puts += 1;
                if before != after || count != now_count {
                    return Err("a failed transfer left refs or objects behind".to_owned());
                }
            }
        }
        for known in &after {
            reachable(&self.hosts[to].objects, known.id)
                .map_err(|error| format!("{}: {error}", hex(&known.name)))?;
        }
        self.check_heads(to)
    }

    fn fetch(&mut self, host: usize) -> Result<(), String> {
        if !self.hosts[host].schedule.due(self.now) {
            self.stats.fetches_not_due += 1;
            return Ok(());
        }
        self.hosts[host].schedule.ran(self.now);
        self.stats.fetches += 1;
        self.sync(None, host)
    }

    fn step(&mut self) -> Result<(), String> {
        self.now += self.tick * u32::try_from(self.rng.range(0, TICKS_MAX)).unwrap_or(1);
        let host = self.rng.pick(HOSTS);
        match self.rng.below(5) {
            0 => self.add_agent(host),
            1 => {
                self.advance_origin();
                Ok(())
            }
            2 => self.fetch(host),
            3 => {
                self.stats.transfers += 1;
                self.sync(Some(host), (host + 1) % HOSTS)
            }
            _ => self.round(host),
        }
    }

    fn digest(&self) -> u64 {
        let mut hash = fnv(FNV_OFFSET, format!("{:?}", self.stats).as_bytes());
        for host in &self.hosts {
            for agent in &host.agents {
                hash = fnv(hash, &agent.last.0);
            }
        }
        hash
    }
}

fn reachable(objects: &Mem, tip: Id) -> Result<(), String> {
    missing(objects, &[tip], &[])
        .map(|_| ())
        .map_err(|error| format!("held refs reach a missing object: {error}"))
}

pub struct RepoCacheOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: RepoCacheStats,
    shape: String,
}

fn simulate(seed: u64, mutation: RepoCacheMutation) -> RepoCacheOutcome {
    cache_sim::mutate(mutation.rule());
    let mut rng = Rng::derive(seed, "repo-cache");
    let steps = rng.range(STEPS_MIN, STEPS_MAX);
    let interval = rng.range(INTERVAL_MIN, INTERVAL_MAX);
    let shape = format!("steps={steps} fetch_interval={interval}");
    let tick = Duration::from_nanos(TICK_NANOS);
    let mut hosts = Vec::new();
    let mut wrong = None;
    for at in 0..HOSTS {
        let machine: Rc<dyn Machine> = Rc::new(Clock(Cell::new(0)));
        let fs: Rc<dyn FileSystem> = Rc::new(Slow(Rc::new(MemFs::new(machine))));
        let root = format!("/h{at}/store.git").into_bytes();
        let config = Config {
            head: MAIN,
            remote: ORIGIN,
            url: b"https://example.invalid/repo",
            alternates: &[],
            read_limit: READ_LIMIT,
        };
        match drive(open(&*fs, &root, &config)) {
            Ok(store) => hosts.push(Host {
                fs,
                objects: Mem::default(),
                store,
                agents: Vec::new(),
                schedule: Schedule::new(tick * u32::try_from(interval).unwrap_or(1)),
            }),
            Err(error) => wrong = Some(format!("open: {error}")),
        }
    }
    let mut world = World {
        rng,
        now: Duration::ZERO,
        tick,
        made: 0,
        origin: Mem::default(),
        origin_tip: Id([0; nanocode_repo_cache::ID_BYTES]),
        hosts,
        stats: RepoCacheStats {
            seeds: 1,
            ..RepoCacheStats::default()
        },
    };
    let (tip, objects) = world.commit(None);
    drop(world.origin.put_all(objects));
    world.origin_tip = tip;
    if wrong.is_none() {
        for at in 0..HOSTS {
            if let Err(message) = world.sync(None, at) {
                wrong = Some(format!("first fetch: {message}"));
            }
        }
    }
    for step in 0..steps {
        if wrong.is_some() {
            break;
        }
        if let Err(message) = world.step() {
            wrong = Some(format!("step {step}: {message}"));
        }
    }
    if wrong.is_none() {
        for at in 0..world.hosts.len() {
            if let Err(message) = world
                .check_isolation(at)
                .and_then(|()| world.check_heads(at))
            {
                wrong = Some(format!("final: {message}"));
            }
        }
    }
    cache_sim::mutate(CacheRule::None);
    RepoCacheOutcome {
        digest: world.digest(),
        wrong,
        stats: world.stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: RepoCacheMutation) -> RepoCacheOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        cache_sim::mutate(CacheRule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        RepoCacheOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: RepoCacheStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: RepoCacheMutation,
    twice: bool,
    verbose: bool,
    totals: &mut RepoCacheStats,
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

pub fn replay(seed: u64, mutation: RepoCacheMutation) -> String {
    let mut command = format!("nanocode-sim --scenario repo_cache --seed {seed}");
    if mutation != RepoCacheMutation::None {
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
    mutation: RepoCacheMutation,
    totals: &RepoCacheStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=repo_cache mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        RepoCacheMutation::None if failed == 0 => ExitCode::SUCCESS,
        RepoCacheMutation::None => ExitCode::FAILURE,
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

const DISK_READ_LIMIT: u64 = 1 << 26;
const DISK_HEAD: &[u8] = b"master";

struct Disk;

fn os(path: &[u8]) -> &std::path::Path {
    std::path::Path::new(std::ffi::OsStr::from_bytes(path))
}

fn stat_of(meta: &std::fs::Metadata) -> Stat {
    let kind = if meta.file_type().is_symlink() {
        FsKind::Symlink
    } else if meta.is_dir() {
        FsKind::Dir
    } else {
        FsKind::File
    };
    Stat {
        kind,
        size: meta.len(),
        mode: meta.mode(),
        mtime_ns: meta
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.mtime_nsec()),
    }
}

impl FileSystem for Disk {
    fn stat(&self, path: &[u8], follow: bool) -> Reply<Stat> {
        let meta = if follow {
            std::fs::metadata(os(path))
        } else {
            std::fs::symlink_metadata(os(path))
        };
        Reply::Now(meta.map(|meta| stat_of(&meta)))
    }

    fn read(&self, path: &[u8], limit: u64) -> Reply<Bytes> {
        Reply::Now(std::fs::read(os(path)).and_then(|data| {
            if data.len() as u64 > limit {
                Err(io::ErrorKind::FileTooLarge.into())
            } else {
                Ok(Bytes::from(data))
            }
        }))
    }

    fn write(&self, path: &[u8], data: &[u8], how: Write) -> Reply<()> {
        let mut options = std::fs::OpenOptions::new();
        match how {
            Write::Replace => options.write(true).create(true).truncate(true),
            Write::Append => options.append(true).create(true),
            Write::New => options.write(true).create_new(true),
        };
        Reply::Now(
            options
                .open(os(path))
                .and_then(|mut file| std::io::Write::write_all(&mut file, data)),
        )
    }

    fn list(&self, path: &[u8]) -> Reply<Vec<Vec<u8>>> {
        Reply::Now(std::fs::read_dir(os(path)).and_then(|entries| {
            let mut names = entries
                .map(|entry| entry.map(|entry| entry.file_name().as_bytes().to_vec()))
                .collect::<io::Result<Vec<_>>>()?;
            names.sort();
            Ok(names)
        }))
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> Reply<()> {
        Reply::Now(std::fs::DirBuilder::new().mode(mode).create(os(path)))
    }

    fn remove(&self, path: &[u8], what: Remove) -> Reply<()> {
        Reply::Now(match what {
            Remove::File => std::fs::remove_file(os(path)),
            Remove::Dir => std::fs::remove_dir(os(path)),
        })
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> Reply<()> {
        Reply::Now(std::fs::rename(os(from), os(to)))
    }
}

pub fn disk(args: &[String]) -> ExitCode {
    let [root, count, url] = args else {
        eprintln!("usage: nanocode-sim --repo-cache-disk DIR WORKTREES URL");
        return ExitCode::FAILURE;
    };
    let Ok(count) = count.parse::<usize>() else {
        eprintln!("WORKTREES is a count");
        return ExitCode::FAILURE;
    };
    let fs = Disk;
    let store_dir = format!("{root}/store.git").into_bytes();
    let config = Config {
        head: DISK_HEAD,
        remote: ORIGIN,
        url: url.as_bytes(),
        alternates: &[],
        read_limit: DISK_READ_LIMIT,
    };
    let made = drive(open(&fs, &store_dir, &config)).and_then(|store| {
        let start = drive(read_ref(&fs, &store, &store.tracking(ORIGIN, DISK_HEAD)))?;
        Ok((store, start))
    });
    let (store, start) = match made {
        Ok((store, Some(start))) => (store, start),
        Ok((_, None)) => {
            println!(
                "STORE opened at {root}/store.git with no {}; fetch it and run again",
                hex(DISK_HEAD)
            );
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            println!("FAIL open: {error}");
            return ExitCode::FAILURE;
        }
    };
    for n in 0..count {
        let name = format!("a{n}").into_bytes();
        let dir = format!("{root}/wt/a{n}").into_bytes();
        let branch = format!("agent/a{n}").into_bytes();
        if let Err(error) = drive(store.worktree(&fs, &name, &dir, &branch, start)) {
            println!("FAIL worktree a{n}: {error}");
            return ExitCode::FAILURE;
        }
    }
    match drive(store.worktrees(&fs)) {
        Ok(listed) => {
            for worktree in &listed {
                println!(
                    "WORKTREE {} {} {}",
                    hex(&worktree.name),
                    hex(&worktree.dir),
                    hex(&worktree.branch)
                );
            }
            println!("WORKTREES {} at {}", listed.len(), start.hex());
            if listed.len() >= count {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            println!("FAIL list: {error}");
            ExitCode::FAILURE
        }
    }
}
