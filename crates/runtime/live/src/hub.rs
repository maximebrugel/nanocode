use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nanocode_live_fold::{
    Access, Ack, Cursor, Encoded, Entry, ErrCode, Live, Resume, Seal, View, ViewKey, encode,
    snapshot_frame,
};
use nanocode_sync::Mailbox;
use serde::Serialize;
use serde_json::Value;

pub mod sim;

use sim::{Mutation, mutated};

pub trait ViewSet: Send + Sync + 'static {
    type Record: Send + Sync + 'static;
    type View: View<Record = Self::Record> + Send + 'static;

    fn open(&self, key: &ViewKey) -> Option<(u32, Self::View)>;
}

pub trait SealPolicy: Seal<Reader: Clone + Send + Sized + 'static> + Send + Sync + 'static {}

impl<S: Seal<Reader: Clone + Send + Sized + 'static> + Send + Sync + 'static> SealPolicy for S {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overloaded;

pub trait Outbound: Send + Sync + 'static {
    fn push(&self, frame: Frame) -> Result<(), Overloaded>;
}

pub trait Follow: Send + Sync + 'static {
    fn follow(&self, partition: u32, after: u64);

    fn unfollow(&self, partition: u32);

    fn backfill(&self, key: ViewKey, partition: u32, after: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Snap,
    Diff,
    Ack,
    Eph,
    Err,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Snap => "snap",
            Self::Diff => "diff",
            Self::Ack => "ack",
            Self::Eph => "eph",
            Self::Err => "err",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub id: Option<u32>,
    pub tail: Arc<str>,
}

impl Frame {
    pub fn write(&self, out: &mut String) {
        out.push_str("{\"t\":\"");
        out.push_str(self.kind.name());
        out.push('"');
        if let Some(id) = self.id {
            let _ = write!(out, ",\"id\":{id}");
        }
        out.push_str(&self.tail);
    }

    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.tail.len() + 24);
        self.write(&mut out);
        out
    }
}

fn tail<T: Serialize>(value: &T) -> Arc<str> {
    let text = encode(value);
    match text.strip_prefix('{') {
        Some("}") => Arc::from("}"),
        Some(fields) => Arc::from(format!(",{fields}")),
        None => Arc::from(text),
    }
}

fn shared(encoded: &Encoded) -> Arc<str> {
    Arc::from(encoded.tail())
}

#[derive(Serialize)]
struct EphBody<'a> {
    body: &'a Value,
}

#[derive(Serialize)]
struct ErrBody<'a> {
    code: ErrCode,
    message: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    pub ring: usize,
    pub linger_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ConnId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub batches: u64,
    pub folds: u64,
    pub encodes: u64,
    pub sends: u64,
    pub snapshots: u64,
    pub resumes: u64,
    pub drops: u64,
    pub views: u64,
    pub subscribers: u64,
}

#[derive(Default)]
struct Counters {
    batches: AtomicU64,
    folds: AtomicU64,
    encodes: AtomicU64,
    sends: AtomicU64,
    snapshots: AtomicU64,
    resumes: AtomicU64,
    drops: AtomicU64,
    views: AtomicU64,
    subscribers: AtomicU64,
}

impl Counters {
    fn add(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Stats {
        Stats {
            batches: self.batches.load(Ordering::Relaxed),
            folds: self.folds.load(Ordering::Relaxed),
            encodes: self.encodes.load(Ordering::Relaxed),
            sends: self.sends.load(Ordering::Relaxed),
            snapshots: self.snapshots.load(Ordering::Relaxed),
            resumes: self.resumes.load(Ordering::Relaxed),
            drops: self.drops.load(Ordering::Relaxed),
            views: self.views.load(Ordering::Relaxed),
            subscribers: self.subscribers.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Inspect {
    pub partition: u32,
    pub live: bool,
    pub head: Cursor,
    pub tip: Cursor,
    pub subscribers: usize,
    pub waiting: usize,
    pub ring: usize,
}

type Sub = (ConnId, u32);

#[derive(Clone, Copy)]
struct Waiter {
    conn: ConnId,
    id: u32,
    cursor: Option<Cursor>,
    access: Access,
}

impl Waiter {
    const fn sub(&self) -> Sub {
        (self.conn, self.id)
    }
}

enum Occupancy {
    Watched(BTreeMap<Sub, Access>),
    Idle { since: u64 },
}

enum Phase<W> {
    Building {
        view: W,
        at: Cursor,
        waiting: Vec<Waiter>,
    },
    Live {
        live: Box<Live<W>>,
        occupancy: Occupancy,
        ahead: Vec<Waiter>,
    },
}

struct Instance<W> {
    partition: u32,
    phase: Phase<W>,
}

struct Part {
    views: BTreeSet<ViewKey>,
    head: Cursor,
}

struct Conn {
    outbound: Arc<dyn Outbound>,
    subs: BTreeMap<u32, ViewKey>,
}

struct Core<V: ViewSet, S: SealPolicy> {
    views: Arc<V>,
    seal: Arc<S>,
    follow: Arc<dyn Follow>,
    policy: Policy,
    counters: Arc<Counters>,
    clock: u64,
    conns: BTreeMap<ConnId, Conn>,
    instances: BTreeMap<ViewKey, Instance<V::View>>,
    parts: BTreeMap<u32, Part>,
}

pub struct Hub<V: ViewSet, S: SealPolicy> {
    core: Mailbox<Core<V, S>>,
    counters: Arc<Counters>,
    conns: Arc<AtomicU64>,
}

impl<V: ViewSet, S: SealPolicy> Clone for Hub<V, S> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            counters: Arc::clone(&self.counters),
            conns: Arc::clone(&self.conns),
        }
    }
}

impl<V: ViewSet, S: SealPolicy> Hub<V, S> {
    pub fn new(views: Arc<V>, seal: Arc<S>, follow: Arc<dyn Follow>, policy: Policy) -> Self {
        let counters = Arc::new(Counters::default());
        let core = Core {
            views,
            seal,
            follow,
            policy,
            counters: Arc::clone(&counters),
            clock: 0,
            conns: BTreeMap::new(),
            instances: BTreeMap::new(),
            parts: BTreeMap::new(),
        };
        Self {
            core: Mailbox::new(core),
            counters,
            conns: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn stats(&self) -> Stats {
        self.counters.snapshot()
    }

    pub fn connect(&self, outbound: Arc<dyn Outbound>) -> ConnId {
        let conn = ConnId(self.conns.fetch_add(1, Ordering::Relaxed));
        self.core.send(move |core| {
            core.conns.insert(
                conn,
                Conn {
                    outbound,
                    subs: BTreeMap::new(),
                },
            );
        });
        conn
    }

    pub fn disconnect(&self, conn: ConnId) {
        self.core.send(move |core| core.disconnect(conn));
    }

    pub fn subscribe(
        &self,
        conn: ConnId,
        id: u32,
        key: ViewKey,
        cursor: Option<Cursor>,
        reader: S::Reader,
    ) {
        self.core
            .send(move |core| core.subscribe(conn, id, key, cursor, &reader));
    }

    pub fn unsubscribe(&self, conn: ConnId, id: u32) {
        self.core.send(move |core| core.unsubscribe(conn, id));
    }

    pub fn commit(&self, partition: u32, epoch: u64, batch: Vec<(u64, V::Record)>) {
        self.core
            .send(move |core| core.commit(partition, epoch, &batch));
    }

    pub fn backfilled(
        &self,
        key: ViewKey,
        epoch: u64,
        records: Vec<(u64, V::Record)>,
        exhausted: bool,
    ) {
        self.core
            .send(move |core| core.backfilled(&key, epoch, &records, exhausted));
    }

    pub fn ack(&self, conn: ConnId, ack: Ack) {
        self.core.send(move |core| core.ack(conn, &ack));
    }

    pub fn eph(&self, key: ViewKey, body: Value) {
        self.core.send(move |core| core.eph(&key, &body));
    }

    pub fn expire(&self, now_ms: u64) {
        self.core.send(move |core| core.expire(now_ms));
    }

    pub fn inspect(
        &self,
        key: ViewKey,
    ) -> impl Future<Output = Option<Inspect>> + Send + use<V, S> {
        self.core.call(move |core| core.inspect(&key))
    }

    pub fn needed(&self) -> impl Future<Output = Vec<u32>> + Send + use<V, S> {
        self.core.call(|core| core.parts.keys().copied().collect())
    }
}

impl<V: ViewSet, S: SealPolicy> Core<V, S> {
    fn gauge(&self) {
        let subscribers = self
            .conns
            .values()
            .map(|conn| conn.subs.len())
            .sum::<usize>();
        self.counters
            .views
            .store(self.instances.len() as u64, Ordering::Relaxed);
        self.counters
            .subscribers
            .store(subscribers as u64, Ordering::Relaxed);
    }

    fn push(&self, conn: ConnId, frame: Frame) -> bool {
        let Some(target) = self.conns.get(&conn) else {
            return true;
        };
        Counters::add(&self.counters.sends, 1);
        target.outbound.push(frame).is_ok()
    }

    fn refuse(&self, conn: ConnId, id: u32, code: ErrCode, message: &str) {
        let frame = Frame {
            kind: Kind::Err,
            id: Some(id),
            tail: tail(&ErrBody { code, message }),
        };
        let _ = self.push(conn, frame);
    }

    fn encoded(&self, entry: &Entry, access: Access) -> Option<Arc<str>> {
        Counters::add(&self.counters.encodes, 1);
        let access = match access {
            Access::Outline if mutated(Mutation::OutlineGetsFull) => Access::Full,
            access => access,
        };
        entry
            .frame(access, &*self.seal)
            .map(|encoded| shared(&encoded))
    }

    fn subscribe(
        &mut self,
        conn: ConnId,
        id: u32,
        key: ViewKey,
        cursor: Option<Cursor>,
        reader: &S::Reader,
    ) {
        if !self.conns.contains_key(&conn) {
            return;
        }
        self.unsubscribe(conn, id);
        let access = self.seal.access(&key, reader);
        if access == Access::Denied {
            self.refuse(
                conn,
                id,
                ErrCode::Denied,
                "this view is sealed for this reader",
            );
            return;
        }
        if !self.instances.contains_key(&key) {
            let Some((partition, view)) = self.views.open(&key) else {
                self.refuse(conn, id, ErrCode::UnknownView, &key.name);
                return;
            };
            self.instances.insert(
                key.clone(),
                Instance {
                    partition,
                    phase: Phase::Building {
                        view,
                        at: Cursor::new(partition, 0, 0),
                        waiting: Vec::new(),
                    },
                },
            );
            self.follow.backfill(key.clone(), partition, 0);
        }
        if let Some(target) = self.conns.get_mut(&conn) {
            target.subs.insert(id, key.clone());
        }
        let waiter = Waiter {
            conn,
            id,
            cursor,
            access,
        };
        let ready = match self.instances.get_mut(&key) {
            Some(Instance {
                phase: Phase::Building { waiting, .. },
                ..
            }) => {
                waiting.push(waiter);
                None
            }
            _ => Some(waiter),
        };
        if let Some(waiter) = ready {
            self.attach(&key, waiter);
        }
        self.gauge();
    }

    fn attach(&mut self, key: &ViewKey, waiter: Waiter) {
        let resume = match self.instances.get(key) {
            Some(Instance {
                phase: Phase::Live { live, .. },
                ..
            }) => live.resume(waiter.cursor.as_ref()),
            _ => return,
        };
        let frames = match resume {
            Resume::Advance => {
                self.advance(key);
                return self.attach(key, waiter);
            }
            Resume::Ahead => {
                if let Some(Instance {
                    phase: Phase::Live { ahead, .. },
                    ..
                }) = self.instances.get_mut(key)
                {
                    ahead.push(waiter);
                }
                return;
            }
            Resume::Diffs(entries) => {
                Counters::add(&self.counters.resumes, 1);
                entries
                    .iter()
                    .filter_map(|entry| self.encoded(entry, waiter.access))
                    .map(|tail| Frame {
                        kind: Kind::Diff,
                        id: Some(waiter.id),
                        tail,
                    })
                    .collect::<Vec<_>>()
            }
            Resume::Snapshot => {
                Counters::add(&self.counters.snapshots, 1);
                Counters::add(&self.counters.encodes, 1);
                let snapshot = match self.instances.get(key) {
                    Some(Instance {
                        phase: Phase::Live { live, .. },
                        ..
                    }) => live.snapshot(),
                    _ => return,
                };
                snapshot_frame(&snapshot, waiter.access, &*self.seal)
                    .map(|encoded| Frame {
                        kind: Kind::Snap,
                        id: Some(waiter.id),
                        tail: shared(&encoded),
                    })
                    .into_iter()
                    .collect()
            }
        };
        if let Some(Instance {
            phase: Phase::Live { occupancy, .. },
            ..
        }) = self.instances.get_mut(key)
        {
            match occupancy {
                Occupancy::Watched(subs) => {
                    subs.insert(waiter.sub(), waiter.access);
                }
                Occupancy::Idle { .. } => {
                    *occupancy =
                        Occupancy::Watched(BTreeMap::from([(waiter.sub(), waiter.access)]));
                }
            }
        }
        let refused = frames
            .into_iter()
            .any(|frame| !self.push(waiter.conn, frame));
        if refused {
            self.drop_subs(key, &[waiter.sub()]);
        }
    }

    fn watchers(&self, key: &ViewKey) -> Vec<(Sub, Access)> {
        match self.instances.get(key) {
            Some(Instance {
                phase:
                    Phase::Live {
                        occupancy: Occupancy::Watched(subs),
                        ..
                    },
                ..
            }) => subs.iter().map(|(sub, access)| (*sub, *access)).collect(),
            _ => Vec::new(),
        }
    }

    fn broadcast(&mut self, key: &ViewKey, entry: &Entry) {
        let subs = self.watchers(key);
        let mut tails: BTreeMap<Access, Option<Arc<str>>> = BTreeMap::new();
        let mut dropped = Vec::new();
        for ((conn, id), access) in subs {
            let tail = if mutated(Mutation::EncodePerSubscriber) {
                self.encoded(entry, access)
            } else {
                tails
                    .entry(access)
                    .or_insert_with(|| self.encoded(entry, access))
                    .clone()
            };
            let Some(tail) = tail else {
                continue;
            };
            let frame = Frame {
                kind: Kind::Diff,
                id: Some(id),
                tail,
            };
            if !self.push(conn, frame) {
                dropped.push((conn, id));
            }
        }
        self.drop_subs(key, &dropped);
    }

    fn advance(&mut self, key: &ViewKey) {
        let entry = match self.instances.get_mut(key) {
            Some(Instance {
                phase: Phase::Live { live, .. },
                ..
            }) => live.advance(),
            _ => None,
        };
        if let Some(entry) = entry {
            self.broadcast(key, &entry);
        }
    }

    fn drop_subs(&mut self, key: &ViewKey, dropped: &[Sub]) {
        if dropped.is_empty() {
            return;
        }
        Counters::add(&self.counters.drops, dropped.len() as u64);
        for &(conn, id) in dropped {
            if let Some(target) = self.conns.get_mut(&conn) {
                target.subs.remove(&id);
            }
            self.detach(key, (conn, id));
        }
        self.gauge();
    }

    fn detach(&mut self, key: &ViewKey, sub: Sub) {
        let clock = self.clock;
        match self.instances.get_mut(key) {
            Some(Instance {
                phase: Phase::Building { waiting, .. },
                ..
            }) => waiting.retain(|waiter| waiter.sub() != sub),
            Some(Instance {
                phase: Phase::Live {
                    occupancy, ahead, ..
                },
                ..
            }) => {
                ahead.retain(|waiter| waiter.sub() != sub);
                if let Occupancy::Watched(subs) = occupancy {
                    subs.remove(&sub);
                }
                let empty = match occupancy {
                    Occupancy::Watched(subs) => subs.is_empty(),
                    Occupancy::Idle { .. } => false,
                };
                if empty && ahead.is_empty() {
                    *occupancy = Occupancy::Idle { since: clock };
                }
            }
            None => {}
        }
    }

    fn unsubscribe(&mut self, conn: ConnId, id: u32) {
        let Some(key) = self
            .conns
            .get_mut(&conn)
            .and_then(|target| target.subs.remove(&id))
        else {
            return;
        };
        self.detach(&key, (conn, id));
        self.gauge();
    }

    fn disconnect(&mut self, conn: ConnId) {
        let Some(target) = self.conns.remove(&conn) else {
            return;
        };
        for (id, key) in target.subs {
            self.detach(&key, (conn, id));
        }
        self.gauge();
    }

    fn backfilled(
        &mut self,
        key: &ViewKey,
        epoch: u64,
        records: &[(u64, V::Record)],
        exhausted: bool,
    ) {
        let head = self
            .instances
            .get(key)
            .and_then(|instance| self.parts.get(&instance.partition))
            .map(|part| part.head.pos);
        let Some(Instance {
            partition,
            phase: Phase::Building { view, at, .. },
        }) = self.instances.get_mut(key)
        else {
            return;
        };
        let partition = *partition;
        let mut ops = Vec::new();
        for (pos, record) in records {
            if *pos > at.pos {
                view.apply(*pos, record, &mut ops);
                ops.clear();
                *at = Cursor::new(partition, epoch, *pos);
            }
        }
        if !records.is_empty() {
            Counters::add(&self.counters.folds, 1);
        }
        let reached = Cursor::new(partition, epoch, at.pos);
        let behind = head.is_some_and(|head| reached.pos < head);
        if !exhausted || (behind && !mutated(Mutation::LiveBeforeHead)) {
            self.follow.backfill(key.clone(), partition, reached.pos);
            return;
        }
        let Some(Instance {
            phase: Phase::Building { view, waiting, .. },
            ..
        }) = self.instances.remove(key)
        else {
            return;
        };
        let live = Live::new(key.clone(), view, reached, self.policy.ring);
        self.instances.insert(
            key.clone(),
            Instance {
                partition,
                phase: Phase::Live {
                    live: Box::new(live),
                    occupancy: Occupancy::Idle { since: self.clock },
                    ahead: Vec::new(),
                },
            },
        );
        if let Some(part) = self.parts.get_mut(&partition) {
            part.views.insert(key.clone());
        } else {
            self.parts.insert(
                partition,
                Part {
                    views: BTreeSet::from([key.clone()]),
                    head: reached,
                },
            );
            self.follow.follow(partition, reached.pos);
        }
        for waiter in waiting {
            self.attach(key, waiter);
        }
        self.gauge();
    }

    fn commit(&mut self, partition: u32, epoch: u64, batch: &[(u64, V::Record)]) {
        Counters::add(&self.counters.batches, 1);
        let Some(last) = batch.last().map(|(pos, _)| *pos) else {
            return;
        };
        let Some(part) = self.parts.get_mut(&partition) else {
            return;
        };
        part.head = Cursor::new(partition, epoch, last.max(part.head.pos));
        let keys = part.views.iter().cloned().collect::<Vec<_>>();
        for key in keys {
            self.fold(&key, partition, epoch, batch);
        }
    }

    fn fold(&mut self, key: &ViewKey, partition: u32, epoch: u64, batch: &[(u64, V::Record)]) {
        let (entry, ahead) = match self.instances.get_mut(key) {
            Some(Instance {
                phase: Phase::Live { live, ahead, .. },
                ..
            }) => {
                let mut folding = live.begin();
                let mut folded = false;
                for (pos, record) in batch {
                    folded |= folding.fold(Cursor::new(partition, epoch, *pos), record);
                }
                let entry = folding.finish();
                if folded {
                    Counters::add(&self.counters.folds, 1);
                }
                let head = live.head();
                let (ready, waiting) =
                    std::mem::take(ahead)
                        .into_iter()
                        .partition::<Vec<_>, _>(|waiter| {
                            waiter.cursor.is_none_or(|cursor| cursor.pos <= head.pos)
                        });
                *ahead = waiting;
                (entry, ready)
            }
            _ => return,
        };
        if let Some(entry) = entry {
            self.broadcast(key, &entry);
        }
        for waiter in ahead {
            self.attach(key, waiter);
        }
    }

    fn ack(&mut self, conn: ConnId, ack: &Ack) {
        let Some(target) = self.conns.get(&conn) else {
            return;
        };
        if let Ok(cursor) = ack.result
            && !mutated(Mutation::AckSkipsAdvance)
        {
            let keys = target.subs.values().cloned().collect::<BTreeSet<_>>();
            for key in keys {
                let behind = matches!(
                    self.instances.get(&key),
                    Some(Instance {
                        phase: Phase::Live { live, .. },
                        ..
                    }) if live.tip().pos < cursor.pos && live.head().pos >= cursor.pos
                );
                if behind {
                    self.advance(&key);
                }
            }
        }
        let _ = self.push(
            conn,
            Frame {
                kind: Kind::Ack,
                id: None,
                tail: tail(ack),
            },
        );
    }

    fn eph(&mut self, key: &ViewKey, body: &Value) {
        let subs = self.watchers(key);
        if subs.is_empty() {
            return;
        }
        let body = tail(&EphBody { body });
        Counters::add(&self.counters.encodes, 1);
        let dropped = subs
            .into_iter()
            .filter(|&((conn, id), _)| {
                !self.push(
                    conn,
                    Frame {
                        kind: Kind::Eph,
                        id: Some(id),
                        tail: Arc::clone(&body),
                    },
                )
            })
            .map(|(sub, _)| sub)
            .collect::<Vec<_>>();
        self.drop_subs(key, &dropped);
    }

    fn expire(&mut self, now: u64) {
        self.clock = self.clock.max(now);
        let linger = self.policy.linger_ms;
        let expired = self
            .instances
            .iter()
            .filter_map(|(key, instance)| match &instance.phase {
                Phase::Live {
                    occupancy: Occupancy::Idle { since },
                    ..
                } if self.clock.saturating_sub(*since) >= linger => {
                    Some((key.clone(), instance.partition))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (key, partition) in expired {
            self.instances.remove(&key);
            let empty = self.parts.get_mut(&partition).is_some_and(|part| {
                part.views.remove(&key);
                part.views.is_empty()
            });
            if empty {
                self.parts.remove(&partition);
                self.follow.unfollow(partition);
            }
        }
        self.gauge();
    }

    fn inspect(&self, key: &ViewKey) -> Option<Inspect> {
        let instance = self.instances.get(key)?;
        Some(match &instance.phase {
            Phase::Building { at, waiting, .. } => Inspect {
                partition: instance.partition,
                live: false,
                head: *at,
                tip: *at,
                subscribers: 0,
                waiting: waiting.len(),
                ring: 0,
            },
            Phase::Live {
                live,
                occupancy,
                ahead,
            } => Inspect {
                partition: instance.partition,
                live: true,
                head: live.head(),
                tip: live.tip(),
                subscribers: match occupancy {
                    Occupancy::Watched(subs) => subs.len(),
                    Occupancy::Idle { .. } => 0,
                },
                waiting: ahead.len(),
                ring: live.ring().len(),
            },
        })
    }
}
