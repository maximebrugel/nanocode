//! Partitioned boards: tasks hash onto P partitions, each its own log scope with its own seq.
//!
//! A task lives in one partition for life: a root task in the partition of its routing key (its
//! thread's UUID as 16 raw bytes when it names one, else its own id), a child in its parent's.
//! Single-task events (claim, renew, finish, reply, relabel, rank) go to the task's partition, so
//! claims on tasks in different partitions never meet in one log, and claims on different tasks
//! in one partition share a group commit rather than racing for a seq. Grants and revokes go to
//! every partition. A move stays inside its partition. Reads fold each partition on its own and
//! merge the folds into one view of the whole board.

use std::{collections::BTreeMap, num::NonZeroU32, sync::Arc};

use nanocode_partitions::{Partition, partition_of};
use nanocode_sync::{mpsc, oneshot};
use nanocode_turnlog::LogHost;

use crate::{
    Applied, Board, BoardError, Fold, NewTask, Op, Task, TaskId, Write, access,
    sim::{Mutation, mutated},
    tasks::TaskOp,
};

/// The label a task names its thread with; its value is the thread's UUID.
pub const THREAD_LABEL: &str = "thread";

/// The scope of partition `index` of the board at `scope`: one partition keeps the bare scope.
pub fn scope_of(scope: &str, count: NonZeroU32, partition: Partition) -> String {
    if count.get() == 1 {
        scope.to_owned()
    } else {
        format!("{scope}/p{}", partition.0)
    }
}

/// A UUID's 16 raw bytes from its hyphenated or plain hex form.
pub fn uuid_bytes(text: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = text.bytes().filter(|byte| *byte != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, pair) in hex.chunks(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        bytes[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(bytes)
}

/// The key a root task hashes by: its thread's UUID bytes, else its id's bytes.
pub fn home_key(task: &NewTask) -> Vec<u8> {
    task.labels
        .get(THREAD_LABEL)
        .and_then(|thread| uuid_bytes(thread))
        .map_or_else(|| task.id.0.as_bytes().to_vec(), |bytes| bytes.to_vec())
}

/// Where one write goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    One(Partition),
    Every,
    Refused(String),
}

/// The folds of every partition, read together.
pub struct Merged<'a> {
    folds: Vec<&'a Fold>,
}

impl<'a> Merged<'a> {
    pub const fn new(folds: Vec<&'a Fold>) -> Self {
        Self { folds }
    }

    pub fn count(&self) -> NonZeroU32 {
        NonZeroU32::new(u32::try_from(self.folds.len()).unwrap_or(u32::MAX))
            .unwrap_or(NonZeroU32::MIN)
    }

    pub fn fold(&self, partition: Partition) -> Option<&'a Fold> {
        self.folds.get(partition.0 as usize).copied()
    }

    fn shown(&self) -> impl Iterator<Item = (Partition, &'a Fold)> + '_ {
        let skip = usize::from(mutated(Mutation::MergeSkipsPartition) && self.folds.len() > 1);
        self.folds
            .iter()
            .take(self.folds.len() - skip)
            .enumerate()
            .map(|(index, fold)| (Partition(index as u32), *fold))
    }

    /// The partition that holds this task, if any partition's fold has it.
    pub fn home(&self, id: &TaskId) -> Option<Partition> {
        self.shown()
            .find(|(_, fold)| fold.task(id).is_some())
            .map(|(partition, _)| partition)
    }

    pub fn task(&self, id: &TaskId) -> Option<&'a Task> {
        self.shown().find_map(|(_, fold)| fold.task(id))
    }

    /// Every task on the board, in id order.
    pub fn tasks(&self) -> BTreeMap<&'a TaskId, &'a Task> {
        self.shown()
            .flat_map(|(_, fold)| fold.tasks().iter())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.shown().map(|(_, fold)| fold.tasks().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Claimable tasks across partitions: ranked first by rank, then by when they were posted,
    /// then by partition and id, so the order doesn't depend on which partition answered first.
    pub fn ready(&self, at_ms: u64) -> Vec<(Partition, &'a TaskId)> {
        let mut ready: Vec<(u64, Partition, &'a TaskId)> = self
            .shown()
            .flat_map(|(partition, fold)| {
                fold.ready(at_ms).map(move |id| {
                    let rank = fold.task(id).and_then(|task| task.rank).unwrap_or(u64::MAX);
                    (rank, partition, id)
                })
            })
            .collect();
        ready.sort();
        ready
            .into_iter()
            .map(|(_, partition, id)| (partition, id))
            .collect()
    }

    /// Rows folded across every partition.
    pub fn rows(&self) -> u64 {
        self.shown().map(|(_, fold)| fold.next()).sum()
    }

    /// Where a write goes, given what the folds know about the tasks it names.
    pub fn route(&self, by: &str, op: &Op) -> Route {
        let count = self.count();
        let hash = |key: &[u8]| partition_of(key, count);
        let home = |id: &TaskId| self.home(id).unwrap_or_else(|| hash(id.0.as_bytes()));
        if mutated(Mutation::RouteIgnoresTask) {
            return Route::One(hash(by.as_bytes()));
        }
        match op {
            Op::Post { task } => Route::One(
                task.parent
                    .as_ref()
                    .map_or_else(|| hash(&home_key(task)), home),
            ),
            Op::Tasks(TaskOp::Batch { name, tasks }) => Route::One(
                tasks
                    .iter()
                    .find_map(|planned| match &planned.parent {
                        Some(crate::tasks::Need::Task(parent)) => Some(home(parent)),
                        _ => None,
                    })
                    .unwrap_or_else(|| hash(name.0.as_bytes())),
            ),
            Op::Move { task, parent } => {
                let from = home(task);
                match parent {
                    Some(parent) if home(parent) != from => Route::Refused(format!(
                        "{task} is in partition {} and {parent} in {}: a move stays in its partition",
                        from.0,
                        home(parent).0
                    )),
                    _ => Route::One(from),
                }
            }
            Op::Grant { .. } | Op::Revoke { .. } | Op::Charter {} => Route::Every,
            _ => Route::One(op.task().map_or(Partition(0), home)),
        }
    }
}

type Reply = oneshot::Sender<Result<Applied, BoardError>>;

/// A handle that queues writes for one partition's committer.
#[derive(Clone)]
pub struct Committer {
    queue: mpsc::UnboundedSender<(Write, Reply)>,
}

impl Committer {
    /// A committer for `board` and the loop that drives it: run the loop on one task, hand
    /// clones of the committer to every writer. Each pass takes every queued write, up to
    /// `batch`, and commits them as one group. The loop returns the board once every committer
    /// is dropped.
    pub fn start(
        board: Board,
        batch: usize,
    ) -> (Self, impl std::future::Future<Output = (Board, Stats)>) {
        let (queue, mut writes) = mpsc::unbounded_channel::<(Write, Reply)>();
        let drive = async move {
            let mut board = board;
            let mut stats = Stats::default();
            let mut taken = Vec::new();
            while writes.recv_many(&mut taken, batch.max(1)).await > 0 {
                let (group, replies): (Vec<Write>, Vec<Reply>) = taken.drain(..).unzip();
                stats.groups += 1;
                stats.writes += group.len() as u64;
                stats.largest = stats.largest.max(group.len() as u64);
                let (verdicts, rounds) = board.submit_group(group).await;
                stats.appends += rounds;
                for (reply, verdict) in replies.into_iter().zip(verdicts) {
                    if verdict.is_ok() {
                        stats.applied += 1;
                    }
                    let _ = reply.send(verdict);
                }
                let mut yielded = false;
                std::future::poll_fn(|cx| {
                    if yielded {
                        std::task::Poll::Ready(())
                    } else {
                        yielded = true;
                        cx.waker().wake_by_ref();
                        std::task::Poll::Pending
                    }
                })
                .await;
            }
            (board, stats)
        };
        (Self { queue }, drive)
    }

    pub async fn submit(&self, write: Write) -> Result<Applied, BoardError> {
        let (reply, verdict) = oneshot::channel();
        self.queue
            .send((write, reply))
            .map_err(|_| BoardError::Dropped("the partition's committer stopped".into()))?;
        verdict.await.unwrap_or_else(|_| {
            Err(BoardError::Dropped(
                "the partition's committer stopped".into(),
            ))
        })
    }
}

/// What one committer did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub groups: u64,
    pub writes: u64,
    pub applied: u64,
    pub appends: u64,
    pub largest: u64,
}

/// Opens the board for every partition of `scope` on one host.
pub async fn open(
    host: &Arc<dyn LogHost>,
    scope: &str,
    count: NonZeroU32,
    identity: &access::Identity,
) -> Result<Vec<Board>, BoardError> {
    let mut boards = Vec::with_capacity(count.get() as usize);
    for index in 0..count.get() {
        let scope = scope_of(scope, count, Partition(index));
        boards.push(Board::open(Arc::clone(host), &scope, identity.clone()).await?);
    }
    Ok(boards)
}
