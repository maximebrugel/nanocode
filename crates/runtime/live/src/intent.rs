use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use nanocode_live_fold::{Ack, Cursor, Failure, Intent, ViewKey};
use nanocode_sync::Mailbox;

use crate::follow::{Feed, Hold};

pub mod sim;

use sim::{Mutation, mutated};

pub const FIRST_MID: u64 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    Applied(Cursor),
    Failed(Failure),
}

impl Settled {
    pub fn ack(self, client: String, mid: u64) -> Ack {
        Ack {
            client,
            mid,
            result: match self {
                Self::Applied(cursor) => Ok(cursor),
                Self::Failed(failure) => Err(failure),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Applied,
    Failed(Failure),
    Repeated(Settled),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub client: String,
    pub mid: u64,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    Refused(Failure),
    Unknown(String),
}

#[cfg(not(target_family = "wasm"))]
pub type Submitting<'a> = Pin<Box<dyn Future<Output = Result<(), Rejection>> + Send + 'a>>;

#[cfg(target_family = "wasm")]
pub type Submitting<'a> = Pin<Box<dyn Future<Output = Result<(), Rejection>> + 'a>>;

pub trait Applier: Send + Sync + 'static {
    type Record;

    fn submit<'a>(&'a self, intent: &'a Intent) -> Submitting<'a>;

    fn partition(&self, intent: &Intent) -> u32;

    fn outcome(&self, record: &Self::Record) -> Option<Outcome>;
}

pub trait Views: Send + Sync + 'static {
    type Conn: Clone + PartialEq + Send + 'static;
    type Record;

    fn commit(&self, partition: u32, epoch: u64, batch: Vec<(u64, Self::Record)>);

    fn ack(&self, conn: &Self::Conn, ack: Ack);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    Apply,
    Repeat(Settled),
}

#[derive(Clone, Debug, Default)]
struct Seen {
    floor: u64,
    below: BTreeMap<u32, Cursor>,
    failed: BTreeMap<u64, Failure>,
    above: BTreeMap<u64, Settled>,
}

impl Seen {
    fn settled(&self, partition: u32, mid: u64) -> Option<Settled> {
        if mid >= self.floor {
            return self.above.get(&mid).cloned();
        }
        match self.failed.get(&mid) {
            Some(failure) => Some(Settled::Failed(failure.clone())),
            None => self.below(partition).map(Settled::Applied),
        }
    }

    fn below(&self, partition: u32) -> Option<Cursor> {
        if mutated(Mutation::BelowAcrossPartitions) {
            return self.below.values().copied().max_by_key(|below| below.pos);
        }
        self.below.get(&partition).copied()
    }

    fn settle(&mut self, mid: u64, settled: Settled) {
        self.above.insert(mid, settled);
        while let Some(entry) = self.above.first_entry() {
            if *entry.key() != self.floor {
                break;
            }
            match entry.remove() {
                Settled::Applied(cursor) => {
                    let below = self.below.entry(cursor.partition).or_insert(cursor);
                    if cursor.pos > below.pos {
                        *below = cursor;
                    }
                }
                Settled::Failed(failure) => {
                    self.failed.insert(self.floor, failure);
                }
            }
            self.floor += 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fold {
    First(Settled),
    Repeat(Settled),
    Twice(Settled),
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    clients: BTreeMap<String, Seen>,
}

impl Ledger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn settled(&self, client: &str, partition: u32, mid: u64) -> Option<Settled> {
        if mutated(Mutation::RepeatsReapplied) {
            return None;
        }
        self.clients.get(client)?.settled(partition, mid)
    }

    pub fn admit(&self, partition: u32, intent: &Intent) -> Admit {
        match self.settled(&intent.client, partition, intent.mid) {
            Some(settled) => Admit::Repeat(settled),
            None => Admit::Apply,
        }
    }

    pub fn fold(&mut self, at: Cursor, outcome: Outcome) -> Fold {
        if let Some(settled) = self.settled(&outcome.client, at.partition, outcome.mid) {
            return match outcome.verdict {
                Verdict::Repeated(_) => Fold::Repeat(settled),
                Verdict::Applied | Verdict::Failed(_) => Fold::Twice(settled),
            };
        }
        let settled = match outcome.verdict {
            Verdict::Applied => Settled::Applied(at),
            Verdict::Failed(failure) => Settled::Failed(failure),
            Verdict::Repeated(settled) => settled,
        };
        self.clients
            .entry(outcome.client)
            .or_default()
            .settle(outcome.mid, settled.clone());
        Fold::First(settled)
    }

    pub fn clients(&self) -> usize {
        self.clients.len()
    }
}

struct Waiting<C> {
    intent: Intent,
    partition: u32,
    conns: Vec<C>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Hold(u32),
    Release(u32),
}

pub enum Receipt {
    Settled(Ack),
    Submit(Intent),
    Joined,
}

pub struct Due<C> {
    pub conn: C,
    pub ack: Ack,
}

pub struct Intake<C> {
    ledger: Ledger,
    waiting: BTreeMap<(String, u64), Waiting<C>>,
    holds: BTreeMap<u32, usize>,
    heads: BTreeMap<u32, u64>,
    moves: Vec<Move>,
    twice: u64,
}

impl<C> Default for Intake<C> {
    fn default() -> Self {
        Self {
            ledger: Ledger::new(),
            waiting: BTreeMap::new(),
            holds: BTreeMap::new(),
            heads: BTreeMap::new(),
            moves: Vec::new(),
            twice: 0,
        }
    }
}

impl<C: Clone + PartialEq> Intake<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub const fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn receive(&mut self, conn: C, partition: u32, intent: Intent) -> Receipt {
        if let Some(settled) = self.ledger.settled(&intent.client, partition, intent.mid) {
            return Receipt::Settled(settled.ack(intent.client, intent.mid));
        }
        let key = (intent.client.clone(), intent.mid);
        if let Some(waiting) = self.waiting.get_mut(&key) {
            if !waiting.conns.contains(&conn) {
                waiting.conns.push(conn);
            }
            return Receipt::Joined;
        }
        self.waiting.insert(
            key,
            Waiting {
                intent: intent.clone(),
                partition,
                conns: vec![conn],
            },
        );
        let holds = self.holds.entry(partition).or_insert(0);
        *holds += 1;
        if *holds == 1 {
            self.moves.push(Move::Hold(partition));
        }
        Receipt::Submit(intent)
    }

    pub fn submitted(
        &mut self,
        client: &str,
        mid: u64,
        result: Result<(), Rejection>,
    ) -> Vec<Due<C>> {
        match result {
            Ok(()) => Vec::new(),
            Err(Rejection::Unknown(message)) if mutated(Mutation::UnknownRefuses) => self.release(
                client,
                mid,
                &Settled::Failed(Failure {
                    code: "unknown".into(),
                    message,
                }),
            ),
            Err(Rejection::Unknown(_)) => Vec::new(),
            Err(Rejection::Refused(failure)) => {
                self.release(client, mid, &Settled::Failed(failure))
            }
        }
    }

    pub fn fold(&mut self, at: Cursor, outcome: Outcome) -> Vec<Due<C>> {
        let head = self.heads.entry(at.partition).or_insert(0);
        if at.pos <= *head && *head > 0 {
            return Vec::new();
        }
        *head = at.pos;
        let (client, mid) = (outcome.client.clone(), outcome.mid);
        match self.ledger.fold(at, outcome) {
            Fold::First(settled) | Fold::Repeat(settled) => self.release(&client, mid, &settled),
            Fold::Twice(settled) => {
                self.twice += 1;
                self.release(&client, mid, &settled)
            }
        }
    }

    pub const fn twice(&self) -> u64 {
        self.twice
    }

    pub fn close(&mut self, conn: &C) {
        let mut emptied = Vec::new();
        self.waiting.retain(|_, waiting| {
            waiting.conns.retain(|held| held != conn);
            if waiting.conns.is_empty() {
                emptied.push(waiting.partition);
            }
            !waiting.conns.is_empty()
        });
        for partition in emptied {
            self.unhold(partition);
        }
    }

    pub fn moves(&mut self) -> Vec<Move> {
        std::mem::take(&mut self.moves)
    }

    fn unhold(&mut self, partition: u32) {
        let Some(holds) = self.holds.get_mut(&partition) else {
            return;
        };
        *holds -= 1;
        if *holds == 0 {
            self.holds.remove(&partition);
            self.moves.push(Move::Release(partition));
        }
    }

    pub fn unsettled(&self) -> Vec<Intent> {
        self.waiting
            .values()
            .map(|waiting| waiting.intent.clone())
            .collect()
    }

    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    fn release(&mut self, client: &str, mid: u64, settled: &Settled) -> Vec<Due<C>> {
        let Some(waiting) = self.waiting.remove(&(client.to_owned(), mid)) else {
            return Vec::new();
        };
        self.unhold(waiting.partition);
        waiting
            .conns
            .into_iter()
            .map(|conn| Due {
                conn,
                ack: settled.clone().ack(client.to_owned(), mid),
            })
            .collect()
    }
}

pub struct Intents<A, V: Views> {
    applier: Arc<A>,
    views: Arc<V>,
    hold: Option<Arc<dyn Hold>>,
    intake: Mailbox<Intake<V::Conn>>,
}

impl<A, V: Views> Clone for Intents<A, V> {
    fn clone(&self) -> Self {
        Self {
            applier: Arc::clone(&self.applier),
            views: Arc::clone(&self.views),
            hold: self.hold.clone(),
            intake: self.intake.clone(),
        }
    }
}

impl<A: Applier, V: Views<Record = A::Record>> Intents<A, V> {
    pub fn new(applier: Arc<A>, views: Arc<V>) -> Self {
        Self {
            applier,
            views,
            hold: None,
            intake: Mailbox::new(Intake::new()),
        }
    }

    pub fn holding(mut self, hold: Arc<dyn Hold>) -> Self {
        self.hold = Some(hold);
        self
    }

    pub async fn receive(&self, conn: V::Conn, intent: Intent) {
        let held = conn.clone();
        let partition = self.applier.partition(&intent);
        let hold = self.hold.clone();
        match self
            .intake
            .call(move |intake| {
                let receipt = intake.receive(held, partition, intent);
                apply(hold.as_ref(), intake.moves());
                receipt
            })
            .await
        {
            Receipt::Settled(ack) => self.views.ack(&conn, ack),
            Receipt::Joined => {}
            Receipt::Submit(intent) => self.submit(intent).await,
        }
    }

    pub async fn resubmit(&self) {
        for intent in self.intake.call(|intake| intake.unsettled()).await {
            self.submit(intent).await;
        }
    }

    async fn submit(&self, intent: Intent) {
        let result = self.applier.submit(&intent).await;
        let views = Arc::clone(&self.views);
        let hold = self.hold.clone();
        self.intake.send(move |intake| {
            for due in intake.submitted(&intent.client, intent.mid, result) {
                views.ack(&due.conn, due.ack);
            }
            apply(hold.as_ref(), intake.moves());
        });
    }

    pub fn commit(&self, partition: u32, epoch: u64, batch: Vec<(u64, A::Record)>) {
        let outcomes = batch
            .iter()
            .filter_map(|(pos, record)| {
                self.applier
                    .outcome(record)
                    .map(|outcome| (Cursor::new(partition, epoch, *pos), outcome))
            })
            .collect::<Vec<_>>();
        if mutated(Mutation::AckBeforeCommit) {
            self.settle(outcomes);
            return self.views.commit(partition, epoch, batch);
        }
        self.views.commit(partition, epoch, batch);
        self.settle(outcomes);
    }

    pub fn close(&self, conn: V::Conn) {
        let hold = self.hold.clone();
        self.intake.send(move |intake| {
            intake.close(&conn);
            apply(hold.as_ref(), intake.moves());
        });
    }

    pub async fn waiting(&self) -> usize {
        self.intake.call(|intake| intake.waiting()).await
    }

    pub async fn twice(&self) -> u64 {
        self.intake.call(|intake| intake.twice()).await
    }

    pub async fn ledger(&self) -> Ledger {
        self.intake.call(|intake| intake.ledger().clone()).await
    }

    fn settle(&self, outcomes: Vec<(Cursor, Outcome)>) {
        if outcomes.is_empty() {
            return;
        }
        let views = Arc::clone(&self.views);
        let hold = self.hold.clone();
        self.intake.send(move |intake| {
            for (at, outcome) in outcomes {
                for due in intake.fold(at, outcome) {
                    views.ack(&due.conn, due.ack);
                }
            }
            apply(hold.as_ref(), intake.moves());
        });
    }
}

impl<A, V> Feed for Intents<A, V>
where
    A: Applier,
    V: Views<Record = A::Record> + Feed<Record = A::Record>,
{
    type Record = A::Record;

    fn commit(&self, partition: u32, epoch: u64, batch: Vec<(u64, A::Record)>) {
        Self::commit(self, partition, epoch, batch);
    }

    fn backfilled(
        &self,
        key: ViewKey,
        epoch: u64,
        records: Vec<(u64, A::Record)>,
        exhausted: bool,
    ) {
        self.views.backfilled(key, epoch, records, exhausted);
    }
}

fn apply(hold: Option<&Arc<dyn Hold>>, moves: Vec<Move>) {
    let Some(hold) = hold else {
        return;
    };
    if mutated(Mutation::HoldSkipped) {
        return;
    }
    for change in moves {
        match change {
            Move::Hold(partition) => hold.hold(partition),
            Move::Release(partition) => hold.release(partition),
        }
    }
}

pub const fn reconciles(ack: Cursor, view: Cursor) -> bool {
    ack.partition == view.partition && view.pos >= ack.pos
}

enum Pending<M> {
    Sent(M),
    Acked(M, Cursor),
}

impl<M> Pending<M> {
    fn into_mutation(self) -> M {
        match self {
            Self::Sent(mutation) | Self::Acked(mutation, _) => mutation,
        }
    }

    const fn mutation(&self) -> &M {
        match self {
            Self::Sent(mutation) | Self::Acked(mutation, _) => mutation,
        }
    }
}

pub struct Optimistic<M> {
    client: String,
    next: u64,
    pending: BTreeMap<u64, Pending<M>>,
}

impl<M> Optimistic<M> {
    pub fn new(client: impl Into<String>, next: u64) -> Self {
        Self {
            client: client.into(),
            next,
            pending: BTreeMap::new(),
        }
    }

    pub fn client(&self) -> &str {
        &self.client
    }

    pub const fn next(&self) -> u64 {
        self.next
    }

    pub fn send(&mut self, mutation: M) -> u64 {
        let mid = self.next;
        self.next += 1;
        self.pending.insert(mid, Pending::Sent(mutation));
        mid
    }

    pub fn ack(&mut self, ack: &Ack, view: Cursor) -> Option<(u64, M)> {
        if ack.client != self.client {
            return None;
        }
        let mutation = self.pending.remove(&ack.mid)?.into_mutation();
        match ack.result {
            Ok(cursor) if !reconciles(cursor, view) && !mutated(Mutation::DropOnAck) => {
                self.pending
                    .insert(ack.mid, Pending::Acked(mutation, cursor));
                None
            }
            Ok(_) | Err(_) => Some((ack.mid, mutation)),
        }
    }

    pub fn reached(&mut self, view: Cursor) -> Vec<(u64, M)> {
        let landed = self
            .pending
            .iter()
            .filter_map(|(mid, pending)| match pending {
                Pending::Acked(_, cursor) if reconciles(*cursor, view) => Some(*mid),
                Pending::Sent(_) | Pending::Acked(..) => None,
            })
            .collect::<Vec<_>>();
        landed
            .into_iter()
            .filter_map(|mid| Some((mid, self.pending.remove(&mid)?.into_mutation())))
            .collect()
    }

    pub fn rebase(&self) -> impl Iterator<Item = (u64, &M)> {
        self.pending
            .iter()
            .map(|(mid, pending)| (*mid, pending.mutation()))
    }

    /// Everything to send again on a new connection: unacked intents, and acked ones whose
    /// position no view has reached yet. The new node may have snapshotted a view below that
    /// position, and only the repeated ack makes it advance that view far enough to reconcile.
    pub fn resend(&self) -> impl Iterator<Item = (u64, &M)> {
        self.pending
            .iter()
            .map(|(mid, pending)| (*mid, pending.mutation()))
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}
