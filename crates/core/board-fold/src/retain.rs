//! Fold compaction: what a fold keeps of history older than a horizon, so a checkpoint holds the
//! live board and a bounded window rather than every event since the charter.
//!
//! [`Fold::compact`] keeps, whatever their age:
//!
//! - every task, as a tombstone once it is finished: its id, parent, needs, labels,
//!   terms, phase (with its outcome and worker), attempts, assignee, rank and claim cap, so a late
//!   write naming it is answered as a full replay answers it (`Exists` for a post of its id,
//!   `Terminal` for a claim or finish, a need that resolves the same way, a child under it);
//! - every keyed event id (`{by}#{key}`) and every other id that is not a minted `{writer}.{n}`,
//!   with its verdict, so a keyed retry is answered with the verdict it first got;
//! - the children, dependents and label indexes, access, mirror, messages, watches, steers and
//!   intents, untouched.
//!
//! It drops, for history before the horizon:
//!
//! - the spec text, replies and entries of a finished task ([`Task::finished`]: done, cancelled,
//!   or failed with no way back) none of whose entries is at or past the horizon, unless it is
//!   pinned: a need of an unfinished task, an ancestor of one, ever mirrored, holding steers, or a
//!   goal or report end;
//! - the verdicts of minted ids first folded before the horizon, kept as per-writer ranges of
//!   their counts, so a replayed row is still known (and skipped) and [`Fold::minted`] still
//!   counts past them; [`Fold::outcome`] answers one with [`Rejection::Forgotten`].
//!
//! The horizon never passes an entry a watcher has not indexed or still has to deliver.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    EventId, Fold, Rejection, Task, TaskId,
    sim::{Mutation, mutated},
};

/// What the fold remembers of an event it folded: the seq it first folded at and its verdict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub at: u64,
    pub verdict: Result<u64, Rejection>,
}

/// What one compaction dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compacted {
    /// The horizon it compacted below, after clamping to what watchers still need.
    pub horizon: u64,
    /// Tasks it made tombstones of.
    pub tasks: usize,
    /// Entries it dropped.
    pub entries: usize,
    /// Event verdicts it forgot.
    pub events: usize,
}

/// A minted `{writer}.{n}` id's writer and count; keyed and other ids are never minted ones.
fn minted(id: &EventId) -> Option<(&str, u64)> {
    let (writer, count) = id.0.rsplit_once('.')?;
    if writer.is_empty() || writer.contains('#') {
        return None;
    }
    let n = count.parse::<u64>().ok()?;
    (n.to_string() == count).then_some((writer, n))
}

/// Adds counts to sorted, disjoint inclusive ranges, merging neighbours.
fn merge(ranges: &mut Vec<(u64, u64)>, counts: Vec<u64>) {
    let mut all = std::mem::take(ranges);
    all.extend(counts.into_iter().map(|count| (count, count)));
    all.sort_unstable();
    for (first, last) in all {
        match ranges.last_mut() {
            Some(held) if first <= held.1.saturating_add(1) => held.1 = held.1.max(last),
            _ => ranges.push((first, last)),
        }
    }
}

impl Fold {
    pub(crate) fn saw(&mut self, id: &EventId, at: u64, verdict: Result<u64, Rejection>) {
        self.seen.insert(id.clone(), Seen { at, verdict });
    }

    /// Whether a compaction forgot this event's verdict.
    pub(crate) fn forgotten(&self, id: &EventId) -> bool {
        let Some((writer, count)) = minted(id) else {
            return false;
        };
        self.forgot.get(writer).is_some_and(|ranges| {
            let at = ranges.partition_point(|&(_, last)| last < count);
            ranges.get(at).is_some_and(|&(first, _)| first <= count)
        })
    }

    /// The highest count of this writer's ids a compaction forgot.
    pub(crate) fn forgot_upto(&self, writer: &str) -> u64 {
        self.forgot
            .get(writer)
            .and_then(|ranges| ranges.last())
            .map_or(0, |&(_, last)| last)
    }

    /// Drops the history before `horizon` that later folds can't need (see the module docs).
    /// Every later op is judged as a fold that kept it would judge it.
    pub fn compact(&mut self, horizon: u64) -> Compacted {
        let mut horizon = horizon.min(self.next);
        if let Some(entry) = self.entries.get(self.marks.indexed()) {
            horizon = horizon.min(entry.seq);
        }
        if let Some(floor) = self.marks.pending_floor() {
            horizon = horizon.min(floor);
        }
        let mut last: BTreeMap<&TaskId, u64> = BTreeMap::new();
        for entry in &self.entries {
            last.insert(&entry.task, entry.seq);
        }
        let mut pinned: BTreeSet<&TaskId> = self
            .mirror
            .pins()
            .chain(self.steers.pins())
            .chain(self.intents.pins())
            .collect();
        let mut climbed = BTreeSet::new();
        for task in self.tasks.values().filter(|task| !task.finished()) {
            pinned.extend(&task.spec.needs);
            let mut above = task.spec.parent.as_ref();
            while let Some(parent) = above.filter(|parent| climbed.insert(*parent)) {
                pinned.insert(parent);
                above = self
                    .tasks
                    .get(parent)
                    .and_then(|up| up.spec.parent.as_ref());
            }
        }
        let doomed: BTreeSet<TaskId> = self
            .tasks
            .iter()
            .filter(|(id, task)| {
                task.finished()
                    && !pinned.contains(id)
                    && last.get(id).is_none_or(|seq| *seq < horizon)
                    && (last.contains_key(id)
                        || !task.spec.spec.is_empty()
                        || !task.replies.is_empty())
            })
            .map(|(id, _)| id.clone())
            .collect();
        drop(last);
        for id in &doomed {
            if mutated(Mutation::CompactionDropsTombstones) {
                self.tasks.remove(id);
            } else if let Some(task) = self.tasks.get_mut(id) {
                task.spec.spec.clear();
                task.replies.clear();
            }
        }
        let indexed = self.marks.indexed();
        let before = self.entries.len();
        let mut index = 0;
        let mut kept_indexed = 0;
        self.entries.retain(|entry| {
            let keep = !doomed.contains(&entry.task);
            if keep && index < indexed {
                kept_indexed += 1;
            }
            index += 1;
            keep
        });
        self.marks.reindex(kept_indexed);
        self.posted = self.posted_order();
        let mut forget: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        let mut events = 0;
        self.seen.retain(|id, seen| match minted(id) {
            Some((writer, count)) if seen.at < horizon => {
                forget.entry(writer.to_owned()).or_default().push(count);
                events += 1;
                false
            }
            _ => true,
        });
        for (writer, counts) in forget {
            merge(self.forgot.entry(writer).or_default(), counts);
        }
        Compacted {
            horizon,
            tasks: doomed.len(),
            entries: before - self.entries.len(),
            events,
        }
    }

    /// Checks that this fold, compacted from `full`'s history, still holds everything `full`
    /// holds that a later op can read: the simulator's proof that compaction loses no answer.
    pub fn agrees(&self, full: &Self) -> Result<(), String> {
        if (self.next, self.now_ms) != (full.next, full.now_ms) {
            return Err(format!(
                "compacted at {}/{}ms, full at {}/{}ms",
                self.next, self.now_ms, full.next, full.now_ms
            ));
        }
        if self.tasks.len() != full.tasks.len() {
            return Err(format!(
                "compacted holds {} tasks, full {}",
                self.tasks.len(),
                full.tasks.len()
            ));
        }
        for (id, task) in &full.tasks {
            let Some(kept) = self.tasks.get(id) else {
                return Err(format!("compaction dropped task {id:?}"));
            };
            let mut stripped = task.clone();
            if kept.spec.spec.is_empty() {
                stripped.spec.spec.clear();
            }
            let tail = task.replies.len().saturating_sub(kept.replies.len());
            stripped.replies.drain(..tail);
            if &stripped != kept || (kept != task && !kept.finished()) {
                return Err(format!(
                    "compaction changed task {id:?}: {kept:?} for {task:?}"
                ));
            }
        }
        let mut held = self.entries.iter().peekable();
        for entry in &full.entries {
            if held.peek() == Some(&entry) {
                held.next();
            } else if !self.tasks.get(&entry.task).is_some_and(Task::finished) {
                return Err(format!(
                    "compaction dropped an entry of a live task: {entry:?}"
                ));
            }
        }
        if let Some(extra) = held.next() {
            return Err(format!("compacted holds an entry full doesn't: {extra:?}"));
        }
        if self.posted != self.posted_order() {
            return Err("compacted posted order is stale".to_owned());
        }
        for (id, seen) in &full.seen {
            match self.seen.get(id) {
                Some(kept) if kept == seen => {}
                None if self.forgotten(id) => {}
                kept => return Err(format!("event {id:?}: compacted {kept:?}, full {seen:?}")),
            }
        }
        let forgot = |fold: &Self| -> u64 {
            fold.forgot
                .values()
                .flatten()
                .map(|(first, last)| last - first + 1)
                .sum()
        };
        if self.seen.len() as u64 + forgot(self) != full.seen.len() as u64 + forgot(full) {
            return Err("compacted knows events full doesn't".to_owned());
        }
        let mut marks = self.marks.clone();
        marks.reindex(full.marks.indexed());
        let same = self.children == full.children
            && self.dependents == full.dependents
            && self.access == full.access
            && self.mirror == full.mirror
            && self.live == full.live
            && self.labelled == full.labelled
            && self.said == full.said
            && marks == full.marks
            && self.steers == full.steers
            && self.intents == full.intents;
        if !same {
            return Err("compaction changed state it keeps whole".to_owned());
        }
        Ok(())
    }
}
