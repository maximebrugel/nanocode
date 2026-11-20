//! The simulator's store: Postgres semantics in memory. One fence row per partition, a batch is
//! checked against the rows that existed before it (one statement snapshot), a duplicate key
//! inside a batch aborts the whole transaction (a unique violation), and faults land before the
//! effect (serialization failure, reconnect) or after it (lost acknowledgement).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

use nanocode_durability_core::{StoreError, StoreFuture};
use nanocode_partitions::{Partition, Row};

use super::{
    Append, Entry, PartStore, Rows, Verdict, lock,
    sim::{Mutation, mutated},
};
use crate::host::{Compaction, Extension};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// The transaction fails with no effect.
    Before,
    /// The transaction commits and its answer is lost.
    After,
}

pub type Faults = Arc<dyn Fn(&'static str) -> Fault + Send + Sync>;
pub type Sleep = Arc<dyn Fn() -> StoreFuture<'static, ()> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub pos: u64,
    /// The token the write carried.
    pub epoch: Option<u64>,
    /// The fence row's epoch when the write committed.
    pub fence: u64,
    pub body: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelStats {
    pub transactions: u64,
    pub unique_violations: u64,
    pub before: u64,
    pub after: u64,
}

#[derive(Default)]
struct Model {
    rows: BTreeMap<(Partition, Arc<str>, u64), Stored>,
    next_pos: u64,
    fences: HashMap<Partition, Row<String>>,
    cursors: HashMap<(Partition, String), u64>,
    stats: ModelStats,
}

impl Model {
    fn live(&self, part: Partition, epoch: Option<u64>) -> Result<u64, StoreError> {
        let fence = self
            .fences
            .get(&part)
            .ok_or_else(|| StoreError::Backend(format!("partition {} not prepared", part.0)))?;
        let held = match (fence, epoch) {
            (_, None) => true,
            (Row::Held { epoch: current, .. }, Some(epoch)) => *current == epoch,
            (Row::Free { .. }, Some(_)) => false,
        };
        if held || mutated(Mutation::Unfenced) {
            Ok(fence.epoch())
        } else {
            Err(StoreError::Fenced)
        }
    }

    fn snapshot(&self, part: Partition, scope: &str) -> u64 {
        let scope: Arc<str> = Arc::from(scope);
        self.rows
            .range((part, Arc::clone(&scope), 0)..=(part, scope, u64::MAX))
            .rev()
            .find(|(_, stored)| stored.body.starts_with('s'))
            .map_or(0, |((_, _, seq), _)| *seq)
    }

    fn floor(&self, part: Partition) -> u64 {
        if mutated(Mutation::PruneIgnoresCursors) {
            return u64::MAX;
        }
        self.cursors
            .iter()
            .filter(|((cursor, _), _)| *cursor == part)
            .map(|(_, pos)| *pos)
            .min()
            .unwrap_or(u64::MAX)
    }

    fn drop_range(&mut self, part: Partition, scope: &str, from: u64, to: u64) -> u64 {
        let floor = self.floor(part);
        let scope: Arc<str> = Arc::from(scope);
        let doomed: Vec<_> = self
            .rows
            .range((part, Arc::clone(&scope), from)..(part, scope, to))
            .filter(|(_, stored)| stored.pos <= floor)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &doomed {
            self.rows.remove(key);
        }
        doomed.len() as u64
    }
}

#[derive(Default)]
pub struct ModelParts {
    model: Mutex<Model>,
    faults: Option<Faults>,
    sleep: Option<Sleep>,
}

impl ModelParts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_faults(mut self, faults: Faults) -> Self {
        self.faults = Some(faults);
        self
    }

    #[must_use]
    pub fn with_sleep(mut self, sleep: Sleep) -> Self {
        self.sleep = Some(sleep);
        self
    }

    pub fn stats(&self) -> ModelStats {
        lock(&self.model).stats
    }

    /// Every row of the partition, by scope and seq.
    pub fn rows(&self, part: Partition) -> Vec<(Arc<str>, u64, Stored)> {
        lock(&self.model)
            .rows
            .iter()
            .filter(|((row_part, _, _), _)| *row_part == part)
            .map(|((_, scope, seq), stored)| (Arc::clone(scope), *seq, stored.clone()))
            .collect()
    }

    /// The position reader consumed the partition through (0 before its first cursor).
    pub fn cursor_of(&self, part: Partition, reader: &str) -> u64 {
        lock(&self.model)
            .cursors
            .get(&(part, reader.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    pub fn fence_row(&self, part: Partition) -> Option<Row<String>> {
        lock(&self.model).fences.get(&part).cloned()
    }

    async fn pause(&self) {
        if let Some(sleep) = &self.sleep {
            sleep().await;
        }
    }

    fn fault(&self, op: &'static str) -> Fault {
        self.faults
            .as_ref()
            .map_or(Fault::None, |faults| faults(op))
    }

    /// Runs one write transaction: a pause, the fault roll, the effect, a pause, the answer.
    async fn write<T>(
        &self,
        op: &'static str,
        effect: impl FnOnce(&mut Model) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.pause().await;
        let fault = self.fault(op);
        if fault == Fault::Before {
            lock(&self.model).stats.before += 1;
            return Err(StoreError::Backend(format!(
                "{op}: could not serialize access due to concurrent update"
            )));
        }
        let answer = {
            let mut model = lock(&self.model);
            model.stats.transactions += 1;
            effect(&mut model)
        };
        self.pause().await;
        if fault == Fault::After && answer.is_ok() {
            lock(&self.model).stats.after += 1;
            return Err(StoreError::Backend(format!(
                "{op}: connection reset after commit was sent"
            )));
        }
        answer
    }

    async fn read<T>(&self, effect: impl FnOnce(&Model) -> T) -> T {
        self.pause().await;
        let model = lock(&self.model);
        effect(&model)
    }
}

impl PartStore for ModelParts {
    fn prepare(&self, count: NonZeroU32) -> StoreFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            let mut model = lock(&self.model);
            for part in 0..count.get() {
                model
                    .fences
                    .entry(Partition(part))
                    .or_insert(Row::Free { epoch: 0 });
            }
            Ok(())
        })
    }

    fn commit<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        batch: &'a [Append],
    ) -> StoreFuture<'a, Result<Vec<Verdict>, StoreError>> {
        Box::pin(self.write("commit", move |model| {
            let fence = model.live(part, epoch)?;
            let mut verdicts = Vec::with_capacity(batch.len());
            let mut inserts = Vec::new();
            for append in batch {
                let clash = append.rows.iter().any(|(seq, _)| {
                    model
                        .rows
                        .contains_key(&(part, Arc::clone(&append.scope), *seq))
                });
                if clash && !mutated(Mutation::Overwrite) {
                    verdicts.push(Verdict::Conflict);
                    continue;
                }
                for (index, (seq, body)) in append.rows.iter().enumerate() {
                    if index > 0 && mutated(Mutation::Partial) {
                        break;
                    }
                    inserts.push(((part, Arc::clone(&append.scope), *seq), body.clone()));
                }
                verdicts.push(Verdict::Committed);
            }
            let mut keys = HashSet::new();
            if !inserts.iter().all(|(key, _)| keys.insert(key.clone())) {
                model.stats.unique_violations += 1;
                return Err(StoreError::Backend(
                    "duplicate key value violates unique constraint nanocode_plog_pkey".into(),
                ));
            }
            for (key, body) in inserts {
                model.next_pos += 1;
                let stored = Stored {
                    pos: model.next_pos,
                    epoch,
                    fence,
                    body,
                };
                model.rows.insert(key, stored);
            }
            Ok(verdicts)
        }))
    }

    fn scan<'a>(
        &'a self,
        part: Partition,
        scope: &'a str,
        from: u64,
        to: u64,
    ) -> StoreFuture<'a, Result<Rows, StoreError>> {
        Box::pin(async move {
            let scope: Arc<str> = Arc::from(scope);
            Ok(self
                .read(|model| {
                    model
                        .rows
                        .range((part, Arc::clone(&scope), from)..(part, Arc::clone(&scope), to))
                        .map(|((_, _, seq), stored)| (*seq, stored.body.clone()))
                        .collect()
                })
                .await)
        })
    }

    fn load<'a>(
        &'a self,
        part: Partition,
        scope: &'a str,
    ) -> StoreFuture<'a, Result<Rows, StoreError>> {
        Box::pin(async move {
            Ok(self
                .read(|model| {
                    let snapshot = model.snapshot(part, scope);
                    let from = if mutated(Mutation::SnapshotSkipped) && snapshot > 0 {
                        snapshot + 1
                    } else {
                        snapshot
                    };
                    let scope: Arc<str> = Arc::from(scope);
                    model
                        .rows
                        .range((part, Arc::clone(&scope), from)..=(part, scope, u64::MAX))
                        .map(|((_, _, seq), stored)| (*seq, stored.body.clone()))
                        .collect()
                })
                .await)
        })
    }

    fn compact<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        compaction: Compaction<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(self.write("compact", move |model| {
            model.live(part, epoch)?;
            for (seq, body) in compaction.archived {
                if let Some(stored) = model.rows.get_mut(&(part, Arc::from(scope), *seq)) {
                    stored.body.clone_from(body);
                }
            }
            for range in std::iter::once(&compaction.dropped).chain(compaction.superseded) {
                if !range.is_empty() {
                    model.drop_range(part, scope, range.start, range.end);
                }
            }
            Ok(())
        }))
    }

    fn extend<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        extension: Extension<'a>,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(self.write("extend", move |model| {
            let fence = model.live(part, epoch)?;
            let key = (part, Arc::<str>::from(scope), extension.row);
            let Some(mut stored) = model.rows.remove(&key) else {
                return Err(StoreError::Fenced);
            };
            let keep = stored.body.len().saturating_sub(extension.cut);
            if !stored.body.is_char_boundary(keep) {
                model.rows.insert(key, stored);
                return Err(StoreError::Backend(
                    "extension cut splits a character".into(),
                ));
            }
            stored.body.truncate(keep);
            stored.body.push_str(extension.tail);
            model.next_pos += 1;
            stored.pos = model.next_pos;
            stored.epoch = epoch;
            stored.fence = fence;
            model.rows.insert((part, key.1, extension.seq), stored);
            Ok(())
        }))
    }

    fn follow(
        &self,
        part: Partition,
        after: u64,
        limit: usize,
    ) -> StoreFuture<'_, Result<Vec<Entry>, StoreError>> {
        Box::pin(async move {
            Ok(self
                .read(|model| {
                    let mut entries: Vec<Entry> = model
                        .rows
                        .iter()
                        .filter(|((row_part, _, _), stored)| {
                            *row_part == part && stored.pos > after
                        })
                        .map(|((_, scope, seq), stored)| Entry {
                            pos: stored.pos,
                            scope: Arc::clone(scope),
                            seq: *seq,
                            body: stored.body.clone(),
                        })
                        .collect();
                    entries.sort_by_key(|entry| entry.pos);
                    entries.truncate(limit);
                    entries
                })
                .await)
        })
    }

    fn fence(&self, part: Partition) -> StoreFuture<'_, Result<Row<String>, StoreError>> {
        Box::pin(async move {
            self.read(|model| {
                model.fences.get(&part).cloned().ok_or_else(|| {
                    StoreError::Backend(format!("partition {} not prepared", part.0))
                })
            })
            .await
        })
    }

    fn swap_fence<'a>(
        &'a self,
        part: Partition,
        expected: &'a Row<String>,
        next: &'a Row<String>,
    ) -> StoreFuture<'a, Result<bool, StoreError>> {
        Box::pin(self.write("fence", move |model| {
            let Some(row) = model.fences.get_mut(&part) else {
                return Ok(false);
            };
            if row != expected {
                return Ok(false);
            }
            *row = next.clone();
            Ok(true)
        }))
    }

    fn cursor<'a>(
        &'a self,
        part: Partition,
        reader: &'a str,
        pos: u64,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(self.write("cursor", move |model| {
            let cursor = model
                .cursors
                .entry((part, reader.to_owned()))
                .or_insert(pos);
            *cursor = (*cursor).max(pos);
            Ok(())
        }))
    }

    fn prune<'a>(
        &'a self,
        part: Partition,
        epoch: Option<u64>,
        scope: &'a str,
        before: u64,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(self.write("prune", move |model| {
            model.live(part, epoch)?;
            let to = before.min(model.snapshot(part, scope));
            Ok(model.drop_range(part, scope, 0, to))
        }))
    }
}
