//! Fold checkpoints: a board saves its fold beside the log as it grows and reopens from the
//! newest one, folding only the rows after it, so a reopen costs the tail rather than the whole
//! log. The save runs off the commit path, on a task that folds the log's rows into its own copy
//! of the fold, so a commit never waits on encoding or writing one.
//!
//! A checkpoint is the fold's compact snapshot after every row before its seq, under
//! [`CHECKPOINT_VERSION`], with a hash of the row just before its seq. A reopen trusts it only
//! when its version is this build's, it restores, its fold's next seq is its seq and that row is
//! still in the log as hashed; anything else replays the whole log. A checkpoint is only ever a
//! shortcut: losing one, failing to save one or failing to read one costs a full replay.
//!
//! Boards save none on their own unless [`checkpoint_every`] turns them on: saving them cost
//! half the board's commit throughput on the bench, so by default a reopen replays the log, and
//! only [`Board::checkpoint`] saves one.

use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocode_sync::Slot;
use nanocode_sys::base64::STANDARD_NO_PAD;
use nanocode_turnlog::{LogHost, StoredCheckpoint};

use crate::{
    Board, BoardError, CHECKPOINT_VERSION, END, Fold, Retain,
    sim::{Mutation, mutated},
};

/// A cadence to turn checkpoints on with, through [`checkpoint_every`]: the fewest events a
/// board folds past its last checkpoint before it saves another. Past that, it waits as many
/// events as the last checkpoint covered, so each save's encode is paid for by as many events
/// as it encodes and the total stays linear in the log.
pub const CHECKPOINT_EVERY: u64 = 4096;

/// Off by default: a board saves no checkpoint on its own until [`checkpoint_every`] sets a
/// cadence.
static EVERY: AtomicU64 = AtomicU64::new(0);

/// Sets the fewest events a board folds past its last checkpoint before it saves another, for
/// every board in the process; 0, the default, saves none on its own.
pub fn checkpoint_every(events: u64) {
    EVERY.store(events, Ordering::Relaxed);
}

thread_local! {
    static HERE: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Sets [`checkpoint_every`] for boards driven on this thread alone, or with `None` goes back to
/// the process's setting. A simulation runs each seed on one thread, several at once, and a
/// seed must not save checkpoints at another seed's cadence.
pub fn checkpoint_every_here(events: Option<u64>) {
    HERE.with(|here| here.set(events));
}

fn every() -> u64 {
    HERE.with(Cell::get)
        .unwrap_or_else(|| EVERY.load(Ordering::Relaxed))
}

/// FNV-1a over a row's body: what a checkpoint holds of the row before its seq, so a log that
/// lost or rewrote that row no longer matches it.
fn row_hash(body: &str) -> u64 {
    body.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A checkpoint of a fold whose last row, the one before its next seq, has the body `last_body`.
pub fn encode(fold: &Fold, last_body: &str) -> StoredCheckpoint {
    let snapshot = fold.snapshot();
    let mut body = format!("{:016x}:", row_hash(last_body));
    STANDARD_NO_PAD.encode_string(snapshot, &mut body);
    StoredCheckpoint {
        seq: fold.next(),
        version: CHECKPOINT_VERSION.to_owned(),
        body,
    }
}

/// Why a reopen did not use the scope's checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// The host keeps no checkpoints.
    Unsupported,
    /// The scope has none.
    Missing,
    /// Reading it failed.
    Unread(String),
    /// A build whose fold differs wrote it.
    Version(String),
    /// It does not decode or restore, or its fold's next seq is not its seq.
    Corrupt(String),
    /// The row before its seq is not in the log as it hashed it.
    Tail,
}

/// How a board came to its fold when it opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// From a fold its caller held, folding the rows after it.
    Resumed,
    /// From the checkpoint at this seq, folding the rows after it.
    Checkpoint { seq: u64, tail: u64 },
    /// From a full replay of the log, and why the checkpoint was not used.
    Replay(Fallback),
}

/// A fold restored from the scope's checkpoint and the rows after it, or why it can't be.
pub async fn reopen(
    host: &Arc<dyn LogHost>,
    scope: &str,
) -> Result<(Fold, Vec<(u64, String)>), Fallback> {
    let read = host.checkpoint(scope).ok_or(Fallback::Unsupported)?;
    let saved = read
        .await
        .map_err(|error| Fallback::Unread(error.to_string()))?
        .ok_or(Fallback::Missing)?;
    if saved.version != CHECKPOINT_VERSION && !mutated(Mutation::CheckpointVersionIgnored) {
        return Err(Fallback::Version(saved.version));
    }
    let corrupt = |detail: &str| Fallback::Corrupt(detail.to_owned());
    if saved.seq == 0 {
        return Err(corrupt("checkpoint at seq 0"));
    }
    let (hash, snapshot) = saved
        .body
        .split_once(':')
        .ok_or_else(|| corrupt("no row hash"))?;
    let hash = u64::from_str_radix(hash, 16).map_err(|error| corrupt(&error.to_string()))?;
    let snapshot = STANDARD_NO_PAD
        .decode(snapshot)
        .map_err(|error| corrupt(&error.to_string()))?;
    let fold = Fold::restore(&snapshot).map_err(|error| corrupt(&error.to_string()))?;
    if fold.next() != saved.seq {
        return Err(corrupt(&format!(
            "fold at seq {} under a checkpoint at {}",
            fold.next(),
            saved.seq
        )));
    }
    let mut rows = host
        .scan(scope, saved.seq - 1, END)
        .await
        .map_err(|error| Fallback::Unread(error.to_string()))?;
    let matches = rows
        .first()
        .is_some_and(|(seq, body)| *seq == saved.seq - 1 && row_hash(body) == hash);
    if !matches && !mutated(Mutation::CheckpointTailUnchecked) {
        return Err(Fallback::Tail);
    }
    rows.retain(|(seq, _)| *seq >= saved.seq);
    Ok((fold, rows))
}

pub(crate) fn adopt(mut reopened: Fold, retain: Option<&Retain>, keep: &[String]) -> Fold {
    if reopened.retain().is_none() {
        reopened.set_retain(retain.cloned());
    }
    if reopened.kept_labels().is_empty() {
        let keys: Vec<&str> = keep.iter().map(String::as_str).collect();
        reopened.keep_labels(&keys);
    }
    reopened
}

fn fresh(retain: Option<&Retain>, keep: &[String]) -> Fold {
    let mut fold = retain.cloned().map_or_else(Fold::new, Fold::retaining);
    let keys: Vec<&str> = keep.iter().map(String::as_str).collect();
    fold.keep_labels(&keys);
    fold
}

impl Board {
    /// How this board came to its fold when it opened: from a checkpoint, from a fold its
    /// caller held, or from a full replay and why.
    pub const fn opened(&self) -> &Opened {
        &self.opened
    }

    /// Saves the fold as the scope's checkpoint at its next seq. Returns whether it saved one:
    /// not when the fold is empty, the host keeps no checkpoints, or the row before the next
    /// seq is not in the log.
    pub async fn checkpoint(&mut self) -> Result<bool, BoardError> {
        let next = self.fold.next();
        if next == 0 || !self.checkpoints {
            return Ok(false);
        }
        let last = self.host.scan(&self.scope, next - 1, next).await?;
        let Some((_, body)) = last.first().filter(|(seq, _)| *seq == next - 1) else {
            return Ok(false);
        };
        let saved = encode(&self.fold, body);
        let Some(save) = self.host.save_checkpoint(&self.scope, &saved) else {
            self.checkpoints = false;
            return Ok(false);
        };
        save.await?;
        self.checkpointed = next;
        Ok(true)
    }

    /// Hands a save to the board's saver once the fold is [`checkpoint_every`] events (none by
    /// default), or as many as the last checkpoint covered, past it. Skipped while the last save
    /// still runs; a failed save only means the next reopen folds more.
    pub(crate) fn keep_checkpoint(&mut self) {
        let every = every();
        let next = self.fold.next();
        let wait = if mutated(Mutation::CheckpointEveryFixed) || self.fold.retain().is_some() {
            every
        } else {
            every.max(self.checkpointed)
        };
        if every == 0 || !self.checkpoints || next < self.checkpointed + wait {
            return;
        }
        let Some(shadow) = self.saver.take() else {
            return;
        };
        if let Shadow::Unsupported = shadow {
            let _ = self.saver.put(shadow);
            self.checkpoints = false;
            return;
        }
        self.checkpointed = next;
        let lent = Lent {
            home: Arc::clone(&self.saver),
            shadow,
        };
        let like = Like {
            retain: self.fold.retain().cloned(),
            keep: self.fold.kept_labels().to_vec(),
        };
        spawn(save(
            Arc::clone(&self.host),
            self.scope.clone(),
            lent,
            next,
            like,
        ));
    }
}

/// The saver's own fold, which only it folds, from the log's rows.
pub(crate) enum Shadow {
    /// Not built yet, or lost with a save that did not finish.
    Unbuilt,
    Folded(Fold),
    /// The host keeps no checkpoints.
    Unsupported,
}

struct Like {
    retain: Option<Retain>,
    keep: Vec<String>,
}

/// The shadow while a save holds it; dropping it gives the saver its shadow back.
struct Lent {
    home: Arc<Slot<Shadow>>,
    shadow: Shadow,
}

impl Drop for Lent {
    fn drop(&mut self) {
        let shadow = std::mem::replace(&mut self.shadow, Shadow::Unbuilt);
        let _ = self.home.put(shadow);
    }
}

#[cfg(not(target_family = "wasm"))]
fn spawn(save: impl Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = nanocode_platform::Handle::try_current() {
        runtime.spawn(save);
    }
}

#[cfg(target_family = "wasm")]
fn spawn(save: impl Future<Output = ()> + 'static) {
    nanocode_platform::spawn_local(save);
}

/// Runs CPU-bound work (folding rows, encoding the fold) on a blocking thread, so a large save
/// never holds an executor thread the committers need; inline under a simulated host.
#[cfg(not(target_family = "wasm"))]
async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    if nanocode_platform::host().is_some() {
        return Some(work());
    }
    nanocode_platform::spawn_blocking(work).await.ok()
}

#[cfg(target_family = "wasm")]
async fn off_thread<T: 'static>(work: impl FnOnce() -> T + 'static) -> Option<T> {
    Some(work())
}

/// Folds `rows` into `fold` and encodes it as the checkpoint at the seq after the last one.
fn fold_and_encode(
    mut fold: Fold,
    rows: Vec<(u64, String)>,
    mut last: Option<String>,
) -> Option<(Fold, Option<StoredCheckpoint>)> {
    for (seq, body) in rows {
        fold.apply(seq, &body).ok()?;
        last = Some(body);
    }
    let saved = last.map(|last| {
        let mut saved = encode(&fold, &last);
        if mutated(Mutation::CheckpointAhead) {
            saved.seq += 1;
        }
        saved
    });
    Some((fold, saved))
}

/// Folds the log's rows before `target` into the shadow, building it from the scope's checkpoint
/// or the whole log first, and saves it as the checkpoint at the seq it folded to.
async fn save(host: Arc<dyn LogHost>, scope: String, mut lent: Lent, target: u64, like: Like) {
    let (fold, mut rows) = match std::mem::replace(&mut lent.shadow, Shadow::Unbuilt) {
        Shadow::Folded(fold) => (fold, Vec::new()),
        Shadow::Unsupported => {
            lent.shadow = Shadow::Unsupported;
            return;
        }
        Shadow::Unbuilt => match reopen(&host, &scope).await {
            Ok((fold, mut rows)) => {
                rows.retain(|(seq, _)| *seq < target);
                (adopt(fold, like.retain.as_ref(), &like.keep), rows)
            }
            Err(Fallback::Unsupported) => {
                lent.shadow = Shadow::Unsupported;
                return;
            }
            Err(_) => (fresh(like.retain.as_ref(), &like.keep), Vec::new()),
        },
    };
    let from = rows.last().map_or(fold.next(), |(seq, _)| seq + 1);
    if from < target {
        let Ok(more) = host.scan(&scope, from, target).await else {
            if rows.is_empty() {
                lent.shadow = Shadow::Folded(fold);
            }
            return;
        };
        rows.extend(more);
    }
    if rows.is_empty() {
        lent.shadow = Shadow::Folded(fold);
        return;
    }
    let Some(Some((fold, saved))) = off_thread(move || fold_and_encode(fold, rows, None)).await
    else {
        return;
    };
    let Some(saved) = saved else {
        lent.shadow = Shadow::Folded(fold);
        return;
    };
    let Some(write) = host.save_checkpoint(&scope, &saved) else {
        lent.shadow = Shadow::Unsupported;
        return;
    };
    let _ = write.await;
    lent.shadow = Shadow::Folded(fold);
}
