use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use nanocode_machine::Illegal;

use super::{
    Change, Event, EventId, Fold, Op, Outcome, Phase, Rejection, TaskId,
    access::Can,
    delivery::{Held, Plan, Signal, Slot, Stage},
    sim::{Mutation, mutated},
    stage::Act,
    tasks::Moved,
};

static ROUTED: TaskId = TaskId(String::new());

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Target {
    pub sync: String,
    pub id: String,
}

impl Target {
    pub fn new(sync: &str, id: impl Into<String>) -> Self {
        Self {
            sync: sync.to_owned(),
            id: id.into(),
        }
    }

    pub fn dm(sync: &str, user: &str) -> Self {
        Self::new(sync, format!("dm:{user}"))
    }

    pub fn is_dm(&self) -> bool {
        self.id.starts_with("dm:")
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.sync, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Join {
    Root { message: String },
    Thread,
    Channel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Always,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Settle {
    Done(String),
    Cancelled(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Control {
    Button {
        id: String,
        label: String,
    },
    Form {
        id: String,
        label: String,
        fields: Vec<String>,
    },
}

impl Control {
    pub fn id(&self) -> &str {
        match self {
            Self::Button { id, .. } | Self::Form { id, .. } => id,
        }
    }

    fn accepts(&self, values: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Button { .. } => values.is_empty(),
            Self::Form { fields, .. } => values.keys().all(|key| fields.contains(key)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Author {
    pub user: String,
    pub name: String,
    pub bot: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum SyncOp {
    Link {
        task: TaskId,
        target: Target,
        join: Join,
        mode: Mode,
    },
    Unlink {
        task: TaskId,
        target: Target,
    },
    Mode {
        task: TaskId,
        target: Target,
        mode: Mode,
    },
    Account {
        member: String,
        sync: String,
        user: String,
        linked: bool,
    },
    Import {
        thread: Target,
        message: String,
        author: Author,
        body: String,
        context: Option<String>,
        sub: Option<String>,
        marker: Option<EventId>,
    },
    Edit {
        thread: Target,
        message: String,
        body: String,
        version: u64,
    },
    Delete {
        thread: Target,
        message: String,
    },
    Reword {
        thread: Target,
        author: Author,
        title: String,
        body: Option<String>,
        version: u64,
    },
    React {
        thread: Target,
        message: String,
        author: Author,
        emoji: String,
    },
    Act {
        thread: Target,
        message: String,
        author: Author,
        control: String,
        values: BTreeMap<String, String>,
    },
    Answer {
        task: TaskId,
        body: String,
        reply_to: Option<EventId>,
        controls: Vec<Control>,
    },
    Revise {
        task: TaskId,
        message: EventId,
        body: String,
    },
    Retract {
        task: TaskId,
        message: EventId,
    },
    Label {
        task: TaskId,
        set: BTreeMap<String, String>,
        unset: BTreeSet<String>,
        own: Vec<String>,
        version: u64,
    },
    Settle {
        task: TaskId,
        outcome: Settle,
    },
    Attempt {
        task: TaskId,
        message: EventId,
        target: Target,
        revision: u64,
    },
    Synced {
        task: TaskId,
        message: EventId,
        target: Target,
        external: String,
        revision: u64,
        thread: Option<String>,
    },
    Removed {
        task: TaskId,
        message: EventId,
        target: Target,
    },
    Undelivered {
        task: TaskId,
        message: EventId,
        target: Target,
        error: String,
    },
    Throttled {
        task: TaskId,
        message: EventId,
        target: Target,
        revision: u64,
    },
}

impl SyncOp {
    pub const fn task(&self) -> Option<&TaskId> {
        match self {
            Self::Import { .. }
            | Self::Edit { .. }
            | Self::Delete { .. }
            | Self::React { .. }
            | Self::Act { .. }
            | Self::Reword { .. }
            | Self::Account { .. } => None,
            Self::Link { task, .. }
            | Self::Unlink { task, .. }
            | Self::Mode { task, .. }
            | Self::Answer { task, .. }
            | Self::Revise { task, .. }
            | Self::Retract { task, .. }
            | Self::Label { task, .. }
            | Self::Settle { task, .. }
            | Self::Attempt { task, .. }
            | Self::Synced { task, .. }
            | Self::Removed { task, .. }
            | Self::Undelivered { task, .. }
            | Self::Throttled { task, .. } => Some(task),
        }
    }

    pub const fn needs(&self) -> Can {
        match self {
            Self::Answer { .. } | Self::Revise { .. } | Self::Retract { .. } => Can::Post,
            _ => Can::Sync,
        }
    }

    const fn thread(&self) -> Option<(&Target, Option<&String>)> {
        match self {
            Self::Import { thread, .. } | Self::Reword { thread, .. } => Some((thread, None)),
            Self::Edit {
                thread, message, ..
            }
            | Self::Delete { thread, message }
            | Self::React {
                thread, message, ..
            }
            | Self::Act {
                thread, message, ..
            } => Some((thread, Some(message))),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Linked,
    NotLinked(Target),
    UnknownMessage(EventId),
    NotBoardMessage,
    NotPlatformRoot,
    Echo,
    Delivered,
    Stale,
    NoCopy,
    NoAttempt,
    UnknownReply(EventId),
    Illegal { from: Stage, to: Stage },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Synced {
    Linked {
        target: Target,
        mode: Mode,
    },
    Unlinked {
        target: Target,
    },
    Moded {
        target: Target,
        mode: Mode,
    },
    Account {
        member: String,
        sync: String,
        user: String,
        linked: bool,
    },
    Labelled {
        set: BTreeMap<String, String>,
        unset: BTreeSet<String>,
    },
    Edited {
        message: EventId,
    },
    Reworded {
        spec: String,
    },
    Deleted {
        message: EventId,
    },
    Delivered {
        message: EventId,
        target: Target,
        external: String,
    },
    Removed {
        message: EventId,
        target: Target,
    },
    Undelivered {
        message: EventId,
        target: Target,
        error: String,
    },
    Read {
        member: String,
        upto: u64,
    },
    Reacted {
        message: EventId,
        by: String,
        emoji: String,
    },
    Acted {
        message: EventId,
        by: String,
        control: String,
        values: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Linked {
    pub task: TaskId,
    pub join: Join,
    pub mode: Mode,
    pub seq: u64,
    pub since: u64,
    pub thread: Option<Target>,
    pub to: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Root,
    Reply,
    Final,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub thread: Target,
    pub message: String,
    pub user: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub task: TaskId,
    pub kind: Kind,
    pub seq: u64,
    pub author: String,
    pub body: String,
    pub origin: Option<Origin>,
    pub sub: Option<String>,
    pub context: Option<String>,
    pub reply_to: Option<EventId>,
    pub controls: Vec<Control>,
    pub mentions: Vec<String>,
    pub dms: Vec<Target>,
    pub revision: u64,
    pub version: u64,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copy {
    pub external: String,
    pub revision: u64,
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Post { starts: bool },
    Edit { external: String },
    Delete { external: String },
    Sweep,
    Orphan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub task: TaskId,
    pub message: EventId,
    pub kind: Kind,
    pub target: Target,
    pub to: Target,
    pub sub: Option<String>,
    pub author: String,
    pub body: String,
    pub controls: Vec<Control>,
    pub mentions: BTreeMap<String, Option<String>>,
    pub revision: u64,
    pub attempts: u64,
    pub throttled: u64,
    pub action: Action,
}

impl Delivery {
    #[must_use]
    pub fn plan(&self, limit: u64) -> Plan {
        Slot {
            attempts: self.attempts,
            throttled: self.throttled,
            ..Slot::default()
        }
        .plan(&self.action, limit)
    }
}

enum Write<'a> {
    Attempt { revision: u64 },
    Refund { revision: u64 },
    Receipt { external: &'a str, revision: u64 },
    Remove,
    GiveUp { error: &'a str },
}

impl Write<'_> {
    const fn signal(&self) -> Signal {
        match self {
            Self::Attempt { .. } => Signal::Attempt,
            Self::Refund { .. } => Signal::Refund,
            Self::Receipt { .. } => Signal::Receipt,
            Self::Remove => Signal::Remove,
            Self::GiveUp { .. } => Signal::GiveUp,
        }
    }
}

type Parked = (EventId, u64, SyncOp);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mirror {
    #[serde(with = "crate::pairs")]
    links: BTreeMap<Target, Linked>,
    ever: BTreeSet<(TaskId, Target)>,
    gone: BTreeSet<Target>,
    #[serde(with = "crate::pairs")]
    threads: BTreeMap<Target, Target>,
    #[serde(with = "crate::pairs")]
    messages: BTreeMap<EventId, Message>,
    #[serde(with = "crate::pairs")]
    by_task: BTreeMap<TaskId, Vec<EventId>>,
    #[serde(with = "crate::pairs")]
    status: BTreeMap<TaskId, EventId>,
    #[serde(with = "crate::pairs")]
    external: BTreeMap<(Target, String), EventId>,
    #[serde(with = "crate::pairs")]
    copies: BTreeMap<(EventId, Target), Copy>,
    #[serde(with = "crate::pairs")]
    attempts: BTreeMap<(EventId, Target, u64), u64>,
    #[serde(with = "crate::pairs")]
    dead: BTreeMap<(EventId, Target), String>,
    #[serde(with = "crate::pairs")]
    labels: BTreeMap<(TaskId, String), u64>,
    #[serde(with = "crate::pairs")]
    accounts: BTreeMap<(String, String), String>,
    #[serde(with = "crate::pairs")]
    members: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(with = "crate::pairs")]
    read: BTreeMap<(TaskId, String), u64>,
    #[serde(with = "crate::pairs")]
    parked: BTreeMap<(Target, Option<String>), Vec<Parked>>,
    #[serde(with = "crate::pairs")]
    owned: BTreeMap<(TaskId, String), u64>,
    worded: BTreeMap<TaskId, u64>,
    #[serde(with = "crate::pairs")]
    throttled: BTreeMap<(EventId, Target, u64), u64>,
}

pub fn spec(title: &str, body: Option<&str>) -> String {
    match body {
        Some(body) if !body.trim().is_empty() => format!("{title}\n\n{body}"),
        _ => title.to_owned(),
    }
}

pub fn mentions(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in body.split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':' | '(' | ')'))
    {
        let Some(name) = word.strip_prefix('@') else {
            continue;
        };
        let name = name.trim_end_matches(['.', '!', '?']);
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if valid && !found.iter().any(|seen: &String| seen == name) {
            found.push(name.to_owned());
        }
    }
    found
}

type Tally = BTreeMap<(EventId, Target, u64), u64>;

fn sum(tally: &Tally, id: &EventId, target: &Target) -> u64 {
    let low = (id.clone(), target.clone(), 0);
    let high = (id.clone(), target.clone(), u64::MAX);
    tally.range(low..=high).map(|(_, count)| *count).sum()
}

fn tally(tally: &Tally, id: &EventId, target: &Target, action: &Action, revision: u64) -> u64 {
    match action {
        Action::Post { .. } | Action::Sweep | Action::Orphan => sum(tally, id, target),
        Action::Edit { .. } | Action::Delete { .. } => tally
            .get(&(id.clone(), target.clone(), revision))
            .copied()
            .unwrap_or_default(),
    }
}

impl Mirror {
    pub const fn new() -> Self {
        Self {
            links: BTreeMap::new(),
            ever: BTreeSet::new(),
            gone: BTreeSet::new(),
            threads: BTreeMap::new(),
            messages: BTreeMap::new(),
            by_task: BTreeMap::new(),
            status: BTreeMap::new(),
            external: BTreeMap::new(),
            copies: BTreeMap::new(),
            attempts: BTreeMap::new(),
            dead: BTreeMap::new(),
            labels: BTreeMap::new(),
            accounts: BTreeMap::new(),
            members: BTreeMap::new(),
            read: BTreeMap::new(),
            parked: BTreeMap::new(),
            owned: BTreeMap::new(),
            worded: BTreeMap::new(),
            throttled: BTreeMap::new(),
        }
    }

    pub fn key(task: &TaskId, target: &Target, join: &Join) -> Target {
        match join {
            Join::Channel => Target::new(&target.sync, format!("{}#{}", target.id, task.0)),
            Join::Root { .. } | Join::Thread => target.clone(),
        }
    }

    pub fn link_of(&self, task: &TaskId, target: &Target) -> Option<Target> {
        let channel = Self::key(task, target, &Join::Channel);
        [target.clone(), channel].into_iter().find(|key| {
            self.links
                .get(key)
                .is_some_and(|linked| linked.task == *task)
        })
    }

    pub const fn links(&self) -> &BTreeMap<Target, Linked> {
        &self.links
    }

    pub const fn messages(&self) -> &BTreeMap<EventId, Message> {
        &self.messages
    }

    pub const fn copies(&self) -> &BTreeMap<(EventId, Target), Copy> {
        &self.copies
    }

    pub const fn dead(&self) -> &BTreeMap<(EventId, Target), String> {
        &self.dead
    }

    pub const fn read(&self) -> &BTreeMap<(TaskId, String), u64> {
        &self.read
    }

    pub fn thread(&self, task: &TaskId) -> Vec<&EventId> {
        self.by_task.get(task).into_iter().flatten().collect()
    }

    pub fn status_of(&self, task: &TaskId) -> Option<&EventId> {
        self.status.get(task)
    }

    pub fn parked(&self) -> usize {
        self.parked.values().map(Vec::len).sum()
    }

    pub fn member(&self, sync: &str, user: &str) -> Option<&String> {
        self.accounts.get(&(sync.to_owned(), user.to_owned()))
    }

    pub fn account(&self, member: &str, sync: &str) -> Option<&String> {
        self.members
            .get(member)
            .and_then(|accounts| accounts.get(sync))
    }

    pub fn find(&self, thread: &Target, message: &str) -> Option<&EventId> {
        let link = self.threads.get(thread)?;
        self.external.get(&(link.clone(), message.to_owned()))
    }

    pub fn routes(&self, thread: &Target) -> Option<&Target> {
        self.threads.get(thread)
    }

    pub fn task_of(&self, thread: &Target) -> Option<&TaskId> {
        self.threads.get(thread).map(|link| &self.links[link].task)
    }

    pub fn author(&self, sync: &str, author: &Author) -> String {
        match self.member(sync, &author.user) {
            Some(member) if !mutated(Mutation::IdentityIgnored) => member.clone(),
            _ => format!("{sync}:{}~{}", author.user, author.name),
        }
    }

    fn routable(&self, task: &TaskId, message: &Message, target: &Target) -> bool {
        self.ever.contains(&(task.clone(), target.clone())) || message.dms.contains(target)
    }

    fn message(&self, task: &TaskId, id: &EventId) -> Result<&Message, Rejection> {
        match self.messages.get(id) {
            Some(message) if message.task == *task => Ok(message),
            _ => Err(Rejection::Sync(Refusal::UnknownMessage(id.clone()))),
        }
    }

    fn delivery(
        &self,
        id: &EventId,
        message: &Message,
        target: &Target,
        to: Target,
        action: Action,
    ) -> Delivery {
        let mentions = message
            .mentions
            .iter()
            .map(|member| (member.clone(), self.account(member, &target.sync).cloned()))
            .collect();
        let sub = message
            .reply_to
            .as_ref()
            .and_then(|above| self.messages.get(above))
            .and_then(|above| {
                above
                    .origin
                    .as_ref()
                    .filter(|origin| self.threads.get(&origin.thread) == Some(target))
                    .and(above.sub.clone())
            });
        Delivery {
            task: message.task.clone(),
            message: id.clone(),
            kind: message.kind,
            target: target.clone(),
            to,
            sub,
            author: message.author.clone(),
            body: message.body.clone(),
            controls: message.controls.clone(),
            mentions,
            revision: message.revision,
            attempts: tally(&self.attempts, id, target, &action, message.revision),
            throttled: tally(&self.throttled, id, target, &action, message.revision),
            action,
        }
    }

    fn tries(&self, id: &EventId, target: &Target) -> u64 {
        sum(&self.attempts, id, target)
    }

    pub fn held(&self, id: &EventId, target: &Target) -> Option<&Copy> {
        self.copies.get(&(id.clone(), target.clone()))
    }

    pub fn slot(&self, id: &EventId, target: &Target) -> Slot {
        let key = (id.clone(), target.clone());
        Slot {
            attempts: sum(&self.attempts, id, target),
            throttled: sum(&self.throttled, id, target),
            copy: self.copies.get(&key).map(|copy| {
                if copy.removed {
                    Held::Removed
                } else {
                    Held::Live
                }
            }),
            dead: self.dead.contains_key(&key),
            orphan: self
                .messages
                .get(id)
                .is_some_and(|message| self.rootless(&message.task, target)),
        }
    }

    fn moves(&self, id: &EventId, target: &Target, signal: Signal) -> Result<(), Rejection> {
        self.slot(id, target)
            .on(signal)
            .map(drop)
            .map_err(|Illegal { from, to }| Rejection::Sync(Refusal::Illegal { from, to }))
    }

    fn step(&mut self, id: &EventId, target: &Target, write: Write<'_>) -> Stage {
        let next = self
            .slot(id, target)
            .on(write.signal())
            .expect("a checked delivery move is in the table");
        let key = (id.clone(), target.clone());
        match write {
            Write::Attempt { revision } if !mutated(Mutation::DeliveryAttemptUncounted) => {
                *self
                    .attempts
                    .entry((id.clone(), target.clone(), revision))
                    .or_default() += 1;
            }
            Write::Attempt { .. } => {}
            Write::Refund { revision } => {
                *self
                    .throttled
                    .entry((id.clone(), target.clone(), revision))
                    .or_default() += 1
            }
            Write::Receipt { external, revision } => match self.copies.get_mut(&key) {
                Some(copy) => copy.revision = revision,
                None => {
                    self.copies.insert(
                        key.clone(),
                        Copy {
                            external: external.to_owned(),
                            revision,
                            removed: false,
                        },
                    );
                    if !mutated(Mutation::DeliveryReceiptKeepsDead) {
                        self.dead.remove(&key);
                    }
                }
            },
            Write::Remove => {
                if let Some(copy) = self.copies.get_mut(&key) {
                    copy.removed = true;
                }
            }
            Write::GiveUp { error } => {
                self.dead.insert(key, error.to_owned());
            }
        }
        next.stage()
    }

    pub(crate) fn leak(&mut self, op: &Op) {
        if let Op::Sync {
            sync:
                SyncOp::Attempt {
                    message,
                    target,
                    revision,
                    ..
                }
                | SyncOp::Throttled {
                    message,
                    target,
                    revision,
                    ..
                },
        } = op
        {
            *self
                .attempts
                .entry((message.clone(), target.clone(), *revision))
                .or_default() += 1;
        }
    }

    fn rootless(&self, task: &TaskId, target: &Target) -> bool {
        self.by_task
            .get(task)
            .and_then(|ids| ids.first())
            .is_some_and(|root| {
                self.messages[root].kind == Kind::Root
                    && self.dead.contains_key(&(root.clone(), target.clone()))
            })
    }

    fn action(
        &self,
        id: &EventId,
        message: &Message,
        target: &Target,
        eligible: bool,
    ) -> Option<Action> {
        let copy = self
            .copies
            .get(&(id.clone(), target.clone()))
            .filter(|_| !mutated(Mutation::ReceiptsIgnored));
        match copy {
            Some(copy) if message.deleted => {
                (!copy.removed && !mutated(Mutation::DeletesNotSynced)).then(|| Action::Delete {
                    external: copy.external.clone(),
                })
            }
            Some(copy) if copy.removed || copy.revision >= message.revision => None,
            Some(copy) => Some(Action::Edit {
                external: copy.external.clone(),
            }),
            None if message.deleted || !eligible => {
                let dead = self.dead.contains_key(&(id.clone(), target.clone()));
                (!dead && self.tries(id, target) > 0).then_some(Action::Sweep)
            }
            None if self.dead.contains_key(&(id.clone(), target.clone()))
                && !mutated(Mutation::DeadLettersBlock) =>
            {
                None
            }
            None => Some(Action::Post { starts: false }),
        }
    }

    fn eligible(linked: &Linked, message: &Message) -> bool {
        let starts = matches!(linked.join, Join::Channel) && message.kind == Kind::Root;
        match (linked.mode, message.kind) {
            (_, Kind::Status) => linked.mode == Mode::Status,
            (Mode::Status, Kind::Final) => true,
            (Mode::Status, _) if !mutated(Mutation::StatusPostsAll) => starts,
            _ => starts || message.seq > linked.since,
        }
    }

    pub fn outbox(&self) -> Vec<Delivery> {
        let mut out = Vec::new();
        for (target, linked) in &self.links {
            let mut order = Vec::new();
            let mut status = self.status.get(&linked.task);
            for id in self.by_task.get(&linked.task).into_iter().flatten() {
                let message = &self.messages[id];
                if message.seq > linked.seq
                    && let Some(status) = status.take()
                {
                    order.push(status);
                }
                order.push(id);
            }
            order.extend(status);
            let mut blocked = false;
            for id in order {
                let message = &self.messages[id];
                let echo = message
                    .origin
                    .as_ref()
                    .is_some_and(|origin| self.threads.get(&origin.thread) == Some(target))
                    && !mutated(Mutation::DeliverToOrigin);
                if echo {
                    continue;
                }
                let to = match (&linked.join, &linked.thread) {
                    (Join::Channel, Some(thread)) => thread.clone(),
                    (Join::Channel, None) if message.kind == Kind::Root => linked.to.clone(),
                    (Join::Channel, None)
                        if self.rootless(&linked.task, target)
                            && !mutated(Mutation::RootDeadIgnored) =>
                    {
                        if let Some(Action::Post { .. }) =
                            self.action(id, message, target, Self::eligible(linked, message))
                        {
                            out.push(self.delivery(
                                id,
                                message,
                                target,
                                linked.to.clone(),
                                Action::Orphan,
                            ));
                        }
                        continue;
                    }
                    (Join::Channel, None) => {
                        blocked = true;
                        continue;
                    }
                    _ => target.clone(),
                };
                let Some(mut action) =
                    self.action(id, message, target, Self::eligible(linked, message))
                else {
                    continue;
                };
                if let Action::Post { starts } = &mut action {
                    if blocked && !mutated(Mutation::OrderIgnored) {
                        continue;
                    }
                    blocked = true;
                    *starts = to == linked.to && matches!(linked.join, Join::Channel);
                }
                out.push(self.delivery(id, message, target, to, action));
            }
        }
        for (id, message) in &self.messages {
            for target in &message.dms {
                if let Some(action) = self.action(id, message, target, true) {
                    out.push(self.delivery(id, message, target, target.clone(), action));
                }
            }
        }
        out
    }
}

fn status_text(change: &Change) -> Option<String> {
    Some(match change {
        Change::Posted => "open".to_owned(),
        Change::Claimed { worker, .. } => format!("claimed by {worker}"),
        Change::Released { note, .. } => format!("open again: {note}"),
        Change::Finished(Outcome::Done(result)) => format!("done: {result}"),
        Change::Finished(Outcome::Failed(error)) => format!("failed: {error}"),
        Change::Cancelled { reason } => format!("cancelled: {reason}"),
        Change::Tasks(Moved::Blocked { kind, reason, .. }) => {
            format!("blocked on {kind}: {reason}")
        }
        Change::Tasks(Moved::Unblocked | Moved::Reopened) => "open".to_owned(),
        _ => return None,
    })
}

impl Fold {
    pub const fn mirror(&self) -> &Mirror {
        &self.mirror
    }

    pub fn outbox(&self) -> Vec<Delivery> {
        self.mirror.outbox()
    }

    fn add(&mut self, id: EventId, message: Message) {
        self.mirror
            .by_task
            .entry(message.task.clone())
            .or_default()
            .push(id.clone());
        self.mirror.messages.insert(id, message);
    }

    fn fresh(&self, task: &TaskId, seq: u64, kind: Kind, author: String, body: String) -> Message {
        let named = if kind == Kind::Status {
            Vec::new()
        } else {
            mentions(&body)
        };
        let linked = self
            .mirror
            .links
            .values()
            .any(|linked| linked.task == *task);
        let dms = if linked || mutated(Mutation::MentionsNotDm) {
            Vec::new()
        } else {
            named
                .iter()
                .filter_map(|member| self.mirror.members.get(member)?.iter().next())
                .map(|(sync, user)| Target::dm(sync, user))
                .collect()
        };
        Message {
            task: task.clone(),
            kind,
            seq,
            author,
            body,
            origin: None,
            sub: None,
            context: None,
            reply_to: None,
            controls: Vec::new(),
            mentions: named,
            dms,
            revision: 0,
            version: 0,
            deleted: false,
        }
    }

    pub(crate) fn observe(&mut self, seq: u64, event: &Event, task: &TaskId, change: &Change) {
        if let Some(text) = status_text(change) {
            let id = EventId(format!("{}#status", task.0));
            match self.mirror.messages.get_mut(&id) {
                Some(status) if status.body != text => {
                    status.body = text;
                    status.revision += 1;
                }
                Some(_) => {}
                None => {
                    let status = self.fresh(task, seq, Kind::Status, event.by.clone(), text);
                    self.mirror.messages.insert(id.clone(), status);
                    self.mirror.status.insert(task.clone(), id);
                }
            }
        }
        let (id, kind, body) = match change {
            Change::Posted => (
                event.id.clone(),
                Kind::Root,
                self.tasks
                    .get(task)
                    .map(|task| task.spec.spec.clone())
                    .unwrap_or_default(),
            ),
            Change::Replied { body, .. } => (event.id.clone(), Kind::Reply, body.clone()),
            Change::Finished(_) | Change::Cancelled { .. } => (
                EventId(format!("{}#final:{}", event.id.0, task.0)),
                Kind::Final,
                status_text(change).unwrap_or_default(),
            ),
            _ => return,
        };
        if self.mirror.messages.contains_key(&id) {
            return;
        }
        let author = match change {
            Change::Replied { by, .. } => by.clone(),
            _ => event.by.clone(),
        };
        let message = self.fresh(task, seq, kind, author, body);
        self.add(id, message);
    }

    pub(crate) fn check_sync(&self, op: &SyncOp) -> Result<(), Rejection> {
        let refuse = |refusal| Err(Rejection::Sync(refusal));
        let known = |task: &TaskId| {
            self.tasks
                .contains_key(task)
                .then_some(())
                .ok_or(Rejection::UnknownTask)
        };
        let mirror = &self.mirror;
        let linked = |task: &TaskId, target: &Target| {
            mirror
                .link_of(task, target)
                .map(drop)
                .ok_or_else(|| Rejection::Sync(Refusal::NotLinked(target.clone())))
        };
        let routable = |task: &TaskId, message: &EventId, target: &Target| {
            let state = mirror.message(task, message)?;
            if mirror.routable(task, state, target) {
                Ok(state)
            } else {
                Err(Rejection::Sync(Refusal::NotLinked(target.clone())))
            }
        };
        match op {
            SyncOp::Link {
                task, target, join, ..
            } => {
                known(task)?;
                if mirror.links.contains_key(&Mirror::key(task, target, join)) {
                    return refuse(Refusal::Linked);
                }
                Ok(())
            }
            SyncOp::Unlink { task, target } | SyncOp::Mode { task, target, .. } => {
                linked(task, target)
            }
            SyncOp::Account { .. }
            | SyncOp::Import { .. }
            | SyncOp::Edit { .. }
            | SyncOp::Delete { .. }
            | SyncOp::React { .. }
            | SyncOp::Reword { .. }
            | SyncOp::Act { .. } => Ok(()),
            SyncOp::Answer { task, reply_to, .. } => {
                known(task)?;
                match reply_to {
                    Some(above) if mirror.message(task, above).is_err() => {
                        refuse(Refusal::UnknownReply(above.clone()))
                    }
                    _ => Ok(()),
                }
            }
            SyncOp::Revise { task, message, .. } | SyncOp::Retract { task, message } => {
                let message = mirror.message(task, message)?;
                let board =
                    matches!(message.kind, Kind::Root | Kind::Reply) && message.origin.is_none();
                if !board || message.deleted {
                    return refuse(Refusal::NotBoardMessage);
                }
                Ok(())
            }
            SyncOp::Label { task, .. } => known(task),
            SyncOp::Settle { task, .. } => {
                let state = self.tasks.get(task).ok_or(Rejection::UnknownTask)?;
                state.may(Act::Settle, self.now_ms)?;
                let owned = mirror
                    .links
                    .values()
                    .any(|linked| linked.task == *task && matches!(linked.join, Join::Root { .. }));
                if !owned {
                    return refuse(Refusal::NotPlatformRoot);
                }
                Ok(())
            }
            SyncOp::Attempt {
                task,
                message,
                target,
                revision,
            } => {
                let state = routable(task, message, target)?;
                let copy = mirror.copies.get(&(message.clone(), target.clone()));
                let done = if state.deleted {
                    copy.is_some_and(|copy| copy.removed)
                } else {
                    copy.is_some_and(|copy| copy.revision >= *revision)
                };
                if *revision > state.revision || done {
                    return refuse(Refusal::Delivered);
                }
                mirror.moves(message, target, Signal::Attempt)
            }
            SyncOp::Synced {
                task,
                message,
                target,
                external,
                revision,
                ..
            } => {
                let state = routable(task, message, target)?;
                let echo = state.origin.as_ref().is_some_and(|origin| {
                    mirror.threads.get(&origin.thread) == Some(target) || origin.thread == *target
                });
                if echo && !mutated(Mutation::DeliverToOrigin) {
                    return refuse(Refusal::Echo);
                }
                match mirror.copies.get(&(message.clone(), target.clone())) {
                    Some(copy) if copy.external != *external => refuse(Refusal::Delivered),
                    Some(copy) if copy.revision > *revision => refuse(Refusal::Stale),
                    _ if *revision > state.revision => refuse(Refusal::Stale),
                    _ => mirror.moves(message, target, Signal::Receipt),
                }
            }
            SyncOp::Removed {
                task,
                message,
                target,
            } => {
                routable(task, message, target)?;
                match mirror.copies.get(&(message.clone(), target.clone())) {
                    Some(_) => mirror.moves(message, target, Signal::Remove),
                    None => refuse(Refusal::NoCopy),
                }
            }
            SyncOp::Undelivered {
                task,
                message,
                target,
                ..
            } => {
                routable(task, message, target)?;
                if mirror
                    .copies
                    .contains_key(&(message.clone(), target.clone()))
                {
                    return refuse(Refusal::Delivered);
                }
                if mirror.tries(message, target) == 0 && !mirror.rootless(task, target) {
                    return refuse(Refusal::NoAttempt);
                }
                mirror.moves(message, target, Signal::GiveUp)
            }
            SyncOp::Throttled {
                task,
                message,
                target,
                revision,
            } => {
                routable(task, message, target)?;
                let slot = (message.clone(), target.clone(), *revision);
                let tried = mirror.attempts.get(&slot).copied().unwrap_or_default();
                if tried <= mirror.throttled.get(&slot).copied().unwrap_or_default() {
                    return refuse(Refusal::NoAttempt);
                }
                mirror.moves(message, target, Signal::Refund)
            }
        }
    }

    fn note(&mut self, seq: u64, event: &Event, task: &TaskId, change: Synced) {
        self.record(seq, event, task, Change::Synced(change));
    }

    fn located(&self, thread: &Target, message: Option<&String>) -> Result<Target, bool> {
        let Some(link) = self.mirror.threads.get(thread) else {
            return Err(!self.mirror.gone.contains(thread));
        };
        match message {
            Some(message)
                if !self
                    .mirror
                    .external
                    .contains_key(&(link.clone(), message.clone())) =>
            {
                Err(true)
            }
            _ => Ok(link.clone()),
        }
    }

    pub(crate) fn transition_sync(&mut self, seq: u64, event: &Event, op: &SyncOp) {
        if let Some((thread, message)) = op.thread()
            && let Err(wait) = self.located(thread, message)
        {
            if wait && !mutated(Mutation::ParkedDropped) {
                let key = (
                    thread.clone(),
                    message
                        .cloned()
                        .filter(|_| self.mirror.threads.contains_key(thread)),
                );
                self.mirror.parked.entry(key).or_default().push((
                    event.id.clone(),
                    event.at_ms,
                    op.clone(),
                ));
            }
            return;
        }
        match op {
            SyncOp::Link {
                task,
                target,
                join,
                mode,
            } => {
                let key = Mirror::key(task, target, join);
                let thread = (!matches!(join, Join::Channel)).then(|| target.clone());
                let since = if matches!(join, Join::Channel) {
                    0
                } else {
                    seq
                };
                let linked = Linked {
                    task: task.clone(),
                    join: join.clone(),
                    mode: *mode,
                    seq,
                    since,
                    thread: thread.clone(),
                    to: target.clone(),
                };
                self.mirror.links.insert(key.clone(), linked);
                self.mirror.ever.insert((task.clone(), key));
                if let Join::Root { message } = join
                    && let Some(root) = self
                        .mirror
                        .by_task
                        .get(task)
                        .and_then(|ids| ids.first())
                        .cloned()
                {
                    if let Some(state) = self.mirror.messages.get_mut(&root)
                        && state.origin.is_none()
                    {
                        state.origin = Some(Origin {
                            thread: target.clone(),
                            message: message.clone(),
                            user: String::new(),
                        });
                    }
                    self.mirror
                        .external
                        .insert((target.clone(), message.clone()), root);
                }
                self.note(
                    seq,
                    event,
                    task,
                    Synced::Linked {
                        target: target.clone(),
                        mode: *mode,
                    },
                );
                if let Some(thread) = thread {
                    self.thread_found(seq, thread, target.clone());
                }
            }
            SyncOp::Unlink { task, target } => {
                if !mutated(Mutation::UnlinkIgnored)
                    && let Some(key) = self.mirror.link_of(task, target)
                    && let Some(linked) = self.mirror.links.remove(&key)
                {
                    let thread = linked.thread.unwrap_or_else(|| key.clone());
                    self.mirror.threads.remove(&thread);
                    self.mirror.gone.insert(thread.clone());
                    self.mirror.gone.insert(key);
                    self.mirror
                        .parked
                        .retain(|(parked, _), _| *parked != thread);
                }
                self.note(
                    seq,
                    event,
                    task,
                    Synced::Unlinked {
                        target: target.clone(),
                    },
                );
            }
            SyncOp::Mode { task, target, mode } => {
                if mutated(Mutation::FollowIgnored) {
                    return;
                }
                let key = self
                    .mirror
                    .link_of(task, target)
                    .expect("a checked link exists");
                let linked = self
                    .mirror
                    .links
                    .get_mut(&key)
                    .expect("a checked link exists");
                if linked.mode == *mode {
                    return;
                }
                linked.mode = *mode;
                if *mode == Mode::Always {
                    linked.since = linked.since.max(seq);
                }
                self.note(
                    seq,
                    event,
                    task,
                    Synced::Moded {
                        target: key,
                        mode: *mode,
                    },
                );
            }
            SyncOp::Account {
                member,
                sync,
                user,
                linked,
            } => {
                let key = (sync.clone(), user.clone());
                if *linked {
                    self.mirror.accounts.insert(key, member.clone());
                    self.mirror
                        .members
                        .entry(member.clone())
                        .or_default()
                        .insert(sync.clone(), user.clone());
                } else if self.mirror.accounts.get(&key) == Some(member) {
                    self.mirror.accounts.remove(&key);
                    if let Some(accounts) = self.mirror.members.get_mut(member) {
                        accounts.remove(sync);
                    }
                } else {
                    return;
                }
                let account = Synced::Account {
                    member: member.clone(),
                    sync: sync.clone(),
                    user: user.clone(),
                    linked: *linked,
                };
                self.note(seq, event, &ROUTED, account);
            }
            SyncOp::Import { .. } => self.import(seq, event, op),
            SyncOp::Edit {
                thread,
                message,
                body,
                version,
            } => {
                let Some((id, task)) = self.imported(thread, message) else {
                    return;
                };
                let state = self
                    .mirror
                    .messages
                    .get_mut(&id)
                    .expect("an imported message is indexed");
                if state.deleted
                    || (*version <= state.version && !mutated(Mutation::EditsUnordered))
                {
                    return;
                }
                state.version = *version;
                if state.body == *body {
                    return;
                }
                state.body.clone_from(body);
                state.mentions = mentions(body);
                state.revision += 1;
                self.note(seq, event, &task, Synced::Edited { message: id });
            }
            SyncOp::Delete { thread, message } => {
                if let Some((id, task)) = self.imported(thread, message) {
                    self.delete(seq, event, &task, id);
                }
            }
            SyncOp::Reword {
                thread,
                author,
                title,
                body,
                version,
            } => self.reword(
                seq,
                event,
                thread,
                author,
                &spec(title, body.as_deref()),
                *version,
            ),
            SyncOp::React {
                thread,
                message,
                author,
                emoji,
            } => {
                let link = self.mirror.threads[thread].clone();
                let id = self.mirror.external[&(link.clone(), message.clone())].clone();
                let task = self.mirror.messages[&id].task.clone();
                let by = self.mirror.author(&link.sync, author);
                self.note(
                    seq,
                    event,
                    &task,
                    Synced::Reacted {
                        message: id.clone(),
                        by,
                        emoji: emoji.clone(),
                    },
                );
                self.mark_read(seq, event, &task, &link.sync, author, &id);
            }
            SyncOp::Act {
                thread,
                message,
                author,
                control,
                values,
            } => {
                let link = self.mirror.threads[thread].clone();
                let id = self.mirror.external[&(link.clone(), message.clone())].clone();
                let state = &self.mirror.messages[&id];
                let task = state.task.clone();
                let valid = state
                    .controls
                    .iter()
                    .any(|known| known.id() == control && known.accepts(values));
                if !valid || author.bot {
                    return;
                }
                let by = self.mirror.author(&link.sync, author);
                let acted = Synced::Acted {
                    message: id.clone(),
                    by,
                    control: control.clone(),
                    values: values.clone(),
                };
                self.note(seq, event, &task, acted);
                self.mark_read(seq, event, &task, &link.sync, author, &id);
            }
            SyncOp::Answer {
                task,
                body,
                reply_to,
                controls,
            } => {
                if let Some(state) = self.tasks.get_mut(task) {
                    state.replies.push((event.by.clone(), body.clone()));
                }
                self.record(
                    seq,
                    event,
                    task,
                    Change::Replied {
                        by: event.by.clone(),
                        body: body.clone(),
                    },
                );
                if let Some(state) = self.mirror.messages.get_mut(&event.id) {
                    state.reply_to.clone_from(reply_to);
                    state.controls.clone_from(controls);
                }
            }
            SyncOp::Revise {
                task,
                message,
                body,
            } => {
                let state = self
                    .mirror
                    .messages
                    .get_mut(message)
                    .expect("a checked message is indexed");
                if state.body == *body {
                    return;
                }
                state.body.clone_from(body);
                state.mentions = mentions(body);
                state.revision += 1;
                self.note(
                    seq,
                    event,
                    task,
                    Synced::Edited {
                        message: message.clone(),
                    },
                );
            }
            SyncOp::Retract { task, message } => self.delete(seq, event, task, message.clone()),
            SyncOp::Label { .. } => self.label(seq, event, op),
            SyncOp::Settle { task, outcome } => match outcome {
                Settle::Done(result) => {
                    let now = self.now_ms;
                    let done = Phase::Done {
                        result: result.clone(),
                        worker: None,
                    };
                    if !self.enter(task, done, None, now) {
                        return;
                    }
                    self.record(
                        seq,
                        event,
                        task,
                        Change::Finished(Outcome::Done(result.clone())),
                    );
                }
                Settle::Cancelled(reason) => self.cancel(seq, event, task, reason.clone()),
            },
            SyncOp::Attempt {
                message,
                target,
                revision,
                ..
            } => {
                self.mirror.step(
                    message,
                    target,
                    Write::Attempt {
                        revision: *revision,
                    },
                );
            }
            SyncOp::Synced { .. } => self.synced(seq, event, op),
            SyncOp::Removed {
                task,
                message,
                target,
            } => {
                self.mirror.step(message, target, Write::Remove);
                self.note(
                    seq,
                    event,
                    task,
                    Synced::Removed {
                        message: message.clone(),
                        target: target.clone(),
                    },
                );
            }
            SyncOp::Undelivered {
                task,
                message,
                target,
                error,
            } => {
                self.mirror.step(message, target, Write::GiveUp { error });
                let dead = Synced::Undelivered {
                    message: message.clone(),
                    target: target.clone(),
                    error: error.clone(),
                };
                self.note(seq, event, task, dead);
            }
            SyncOp::Throttled {
                message,
                target,
                revision,
                ..
            } => {
                self.mirror.step(
                    message,
                    target,
                    Write::Refund {
                        revision: *revision,
                    },
                );
            }
        }
    }

    fn mark_read(
        &mut self,
        seq: u64,
        event: &Event,
        task: &TaskId,
        sync: &str,
        author: &Author,
        at: &EventId,
    ) {
        if author.bot && !mutated(Mutation::BotMarksRead) {
            return;
        }
        let Some(member) = self.mirror.member(sync, &author.user).cloned() else {
            return;
        };
        let latest = self
            .mirror
            .by_task
            .get(task)
            .and_then(|ids| ids.last())
            .map_or(0, |id| self.mirror.messages[id].seq);
        let key = (task.clone(), member.clone());
        let held = self.mirror.read.get(&key).copied().unwrap_or_default();
        let upto = if mutated(Mutation::ReadRegresses) {
            self.mirror
                .messages
                .get(at)
                .map_or(latest, |message| message.seq)
        } else {
            held.max(latest)
        };
        if upto == held {
            return;
        }
        self.mirror.read.insert(key, upto);
        self.note(seq, event, task, Synced::Read { member, upto });
    }

    fn import(&mut self, seq: u64, event: &Event, op: &SyncOp) {
        let SyncOp::Import {
            thread,
            message,
            author,
            body,
            context,
            sub,
            marker,
        } = op
        else {
            return;
        };
        let link = self.mirror.threads[thread].clone();
        let task = self.mirror.links[&link].task.clone();
        let key = (link.clone(), message.clone());
        if self.mirror.external.contains_key(&key) && !mutated(Mutation::ImportsDuplicated) {
            return;
        }
        if author.bot && !mutated(Mutation::EchoImported) {
            let ours = marker.as_ref().filter(|marker| {
                self.mirror
                    .messages
                    .get(*marker)
                    .is_some_and(|state| state.task == task)
                    && !self
                        .mirror
                        .copies
                        .contains_key(&((*marker).clone(), link.clone()))
            });
            if let Some(marker) = ours.cloned() {
                self.mirror.step(
                    &marker,
                    &link,
                    Write::Receipt {
                        external: message,
                        revision: 0,
                    },
                );
                self.mirror.external.insert(key, marker.clone());
                let delivered = Synced::Delivered {
                    message: marker.clone(),
                    target: link.clone(),
                    external: message.clone(),
                };
                self.note(seq, event, &task, delivered);
                self.mark_read(seq, event, &task, &link.sync, author, &marker);
                self.unpark(seq, &(thread.clone(), Some(message.clone())));
            }
            return;
        }
        let by = self.mirror.author(&link.sync, author);
        let mut state = self.fresh(&task, seq, Kind::Reply, by.clone(), body.clone());
        state.origin = Some(Origin {
            thread: thread.clone(),
            message: message.clone(),
            user: author.user.clone(),
        });
        state.sub.clone_from(sub);
        state.context.clone_from(context);
        self.add(event.id.clone(), state);
        self.mirror.external.insert(key, event.id.clone());
        if let Some(state) = self.tasks.get_mut(&task) {
            state.replies.push((by.clone(), body.clone()));
        }
        self.record(
            seq,
            event,
            &task,
            Change::Replied {
                by,
                body: body.clone(),
            },
        );
        self.mark_read(seq, event, &task, &link.sync, author, &event.id);
        let follow =
            body.trim().eq_ignore_ascii_case("follow") && !mutated(Mutation::FollowIgnored);
        if follow
            && let Some(linked) = self.mirror.links.get_mut(&link)
            && linked.mode == Mode::Status
        {
            linked.mode = Mode::Always;
            linked.since = linked.since.max(seq);
            self.note(
                seq,
                event,
                &task,
                Synced::Moded {
                    target: link.clone(),
                    mode: Mode::Always,
                },
            );
        }
        self.unpark(seq, &(thread.clone(), Some(message.clone())));
    }

    fn synced(&mut self, seq: u64, event: &Event, op: &SyncOp) {
        let SyncOp::Synced {
            task,
            message,
            target,
            external,
            revision,
            thread,
        } = op
        else {
            return;
        };
        let key = (message.clone(), target.clone());
        let held = self.mirror.copies.get(&key).map(|copy| copy.revision);
        if held == Some(*revision) && !mutated(Mutation::DeliveryReceiptsRenote) {
            return;
        }
        self.mirror.step(
            message,
            target,
            Write::Receipt {
                external,
                revision: *revision,
            },
        );
        if held.is_none() {
            self.mirror
                .external
                .insert((target.clone(), external.clone()), message.clone());
        }
        let delivered = Synced::Delivered {
            message: message.clone(),
            target: target.clone(),
            external: external.clone(),
        };
        self.note(seq, event, task, delivered);
        let root = self
            .mirror
            .messages
            .get(message)
            .is_some_and(|state| state.kind == Kind::Root);
        if let Some(thread) = thread
            && root
            && let Some(linked) = self.mirror.links.get_mut(target)
            && matches!(linked.join, Join::Channel)
            && linked.thread.is_none()
        {
            let thread = Target::new(&target.sync, thread.clone());
            linked.thread = Some(thread.clone());
            self.thread_found(seq, thread, target.clone());
        }
        let routed = self
            .mirror
            .links
            .get(target)
            .and_then(|linked| linked.thread.clone());
        if let Some(thread) = routed {
            self.unpark(seq, &(thread, Some(external.clone())));
        }
    }

    fn reword(
        &mut self,
        seq: u64,
        event: &Event,
        thread: &Target,
        author: &Author,
        spec: &str,
        version: u64,
    ) {
        let linked = &self.mirror.links[&self.mirror.threads[thread]];
        let task = linked.task.clone();
        if author.bot
            || !matches!(linked.join, Join::Root { .. })
            || mutated(Mutation::RootEditsIgnored)
        {
            return;
        }
        let held = self.mirror.worded.entry(task.clone()).or_default();
        if version <= *held {
            return;
        }
        *held = version;
        let Some(state) = self.tasks.get_mut(&task) else {
            return;
        };
        if state.spec.spec == spec {
            return;
        }
        state.spec.spec = spec.to_owned();
        let root = self.mirror.by_task.get(&task).and_then(|ids| ids.first());
        if let Some(root) = root.and_then(|id| self.mirror.messages.get_mut(id))
            && root.kind == Kind::Root
        {
            spec.clone_into(&mut root.body);
            root.mentions = mentions(spec);
            root.revision += 1;
        }
        self.note(
            seq,
            event,
            &task,
            Synced::Reworded {
                spec: spec.to_owned(),
            },
        );
    }

    fn imported(&self, thread: &Target, message: &str) -> Option<(EventId, TaskId)> {
        let link = self.mirror.threads.get(thread)?;
        let id = self
            .mirror
            .external
            .get(&(link.clone(), message.to_owned()))?;
        let state = &self.mirror.messages[id];
        let origin = state
            .origin
            .as_ref()
            .is_some_and(|origin| origin.thread == *thread || origin.thread == *link);
        origin.then(|| (id.clone(), state.task.clone()))
    }

    fn delete(&mut self, seq: u64, event: &Event, task: &TaskId, id: EventId) {
        let Some(state) = self.mirror.messages.get_mut(&id) else {
            return;
        };
        if state.deleted {
            return;
        }
        state.deleted = true;
        self.note(seq, event, task, Synced::Deleted { message: id });
    }

    fn label(&mut self, seq: u64, event: &Event, op: &SyncOp) {
        let SyncOp::Label {
            task,
            set,
            unset,
            own,
            version,
        } = op
        else {
            return;
        };
        let Some(state) = self.tasks.get(task) else {
            return;
        };
        let owned = state
            .spec
            .labels
            .keys()
            .filter(|key| own.iter().any(|prefix| key.starts_with(prefix.as_str())))
            .cloned()
            .collect::<Vec<_>>();
        let stale = mutated(Mutation::StaleLabels);
        for prefix in own {
            let seen = self
                .mirror
                .owned
                .entry((task.clone(), prefix.clone()))
                .or_default();
            *seen = (*seen).max(*version);
        }
        let owned_map = &self.mirror.owned;
        let floor = |key: &str| {
            owned_map
                .range((task.clone(), String::new())..)
                .take_while(|((held, _), _)| held == task)
                .filter(|((_, prefix), _)| key.starts_with(prefix.as_str()))
                .map(|(_, seen)| *seen)
                .max()
                .unwrap_or_default()
        };
        let floors = set
            .keys()
            .map(|key| (key.clone(), floor(key)))
            .collect::<BTreeMap<_, _>>();
        let mut added = BTreeMap::new();
        let mut removed = BTreeSet::new();
        for (key, value) in set {
            let seen = self
                .mirror
                .labels
                .entry((task.clone(), key.clone()))
                .or_default();
            if (*version >= *seen && *version >= floors[key]) || stale {
                *seen = *version;
                if state.spec.labels.get(key) != Some(value) {
                    added.insert(key.clone(), value.clone());
                }
            }
        }
        for key in unset.iter().chain(&owned) {
            if set.contains_key(key) {
                continue;
            }
            let seen = self
                .mirror
                .labels
                .entry((task.clone(), key.clone()))
                .or_default();
            if *version >= *seen || stale {
                *seen = *version;
                if state.spec.labels.contains_key(key) {
                    removed.insert(key.clone());
                }
            }
        }
        if added.is_empty() && removed.is_empty() {
            return;
        }
        let labels = &mut self
            .tasks
            .get_mut(task)
            .expect("a labelled task exists")
            .spec
            .labels;
        for key in &removed {
            labels.remove(key);
        }
        labels.extend(added.clone());
        self.note(
            seq,
            event,
            task,
            Synced::Labelled {
                set: added,
                unset: removed,
            },
        );
    }

    fn thread_found(&mut self, seq: u64, thread: Target, link: Target) {
        self.mirror.gone.remove(&thread);
        self.mirror.threads.insert(thread.clone(), link);
        self.unpark(seq, &(thread, None));
    }

    fn unpark(&mut self, seq: u64, key: &(Target, Option<String>)) {
        let waiting = self.mirror.parked.remove(key).unwrap_or_default();
        for (id, at_ms, op) in waiting {
            let event = Event {
                id,
                by: String::new(),
                at_ms,
                op: Op::Sync { sync: op.clone() },
            };
            self.transition_sync(seq, &event, &op);
        }
    }
}
