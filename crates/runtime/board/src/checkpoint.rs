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
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use nanocode_sys::base64::STANDARD_NO_PAD;
use nanocode_turnlog::{LogHost, StoredCheckpoint};

use crate::{
    Board, BoardError, CHECKPOINT_VERSION, END, Fold,
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
    encode_spread(fold, last_body, crate::Spread::for_fold(fold))
}

/// [`encode`] with the fold's sections encoded inline or on threads of their own.
pub fn encode_spread(fold: &Fold, last_body: &str, spread: crate::Spread) -> StoredCheckpoint {
    let snapshot = fold.snapshot_spread(spread);
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
    ///
    /// The save freezes the board's own fold, which costs one pointer per chunk of its
    /// copy-on-write collections, and encodes it off this thread: only the chunks written since
    /// the last save encode again, the rest reuse the encoding they cached. The board keeps
    /// folding meanwhile, copying a chunk the save still holds the first time it writes to it.
    pub(crate) fn keep_checkpoint(&mut self) {
        let every = every();
        let next = self.fold.next();
        let wait = if mutated(Mutation::CheckpointEveryFixed) {
            every
        } else {
            every.max(self.checkpointed)
        };
        if every == 0 || !self.checkpoints || next < self.checkpointed + wait {
            return;
        }
        if self.saver.unsupported.load(Ordering::Acquire) {
            self.checkpoints = false;
            return;
        }
        if self.saver.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        self.checkpointed = next;
        let saving = Saving(Arc::clone(&self.saver));
        spawn(save(
            Arc::clone(&self.host),
            self.scope.clone(),
            self.fold.clone(),
            saving,
        ));
    }
}

/// How many times its own encode time a save holds the encoder idle after it.
#[cfg(not(target_family = "wasm"))]
const ENCODE_REST: u32 = 3;

/// Holds the encoder idle for ENCODE_REST times `took`, on a native host outside the simulator.
#[cfg(not(target_family = "wasm"))]
fn rest(took: std::time::Duration) {
    if nanocode_platform::host().is_none() {
        std::thread::sleep(took * ENCODE_REST);
    }
}

#[cfg(target_family = "wasm")]
const fn rest(_took: std::time::Duration) {}

/// Held while a save encodes, so the process's boards encode one checkpoint at a time.
static ENCODING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What a board's saves share with it: whether one runs, and whether the host keeps none.
#[derive(Default)]
pub(crate) struct Saver {
    busy: AtomicBool,
    unsupported: AtomicBool,
}

/// A running save; dropping it lets the next one start.
struct Saving(Arc<Saver>);

impl Drop for Saving {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
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

/// Runs CPU-bound work (encoding the fold) on a blocking thread, so a large save never holds an
/// executor thread the committers need; inline under a simulated host.
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

/// Saves a frozen fold as the scope's checkpoint at its next seq, hashing the row before it.
async fn save(host: Arc<dyn LogHost>, scope: String, fold: Fold, saving: Saving) {
    let next = fold.next();
    if next == 0 {
        return;
    }
    let Ok(last) = host.scan(&scope, next - 1, next).await else {
        return;
    };
    let Some((_, body)) = last.into_iter().find(|(seq, _)| *seq == next - 1) else {
        return;
    };
    // One save encodes at a time in the process, on one thread, and the next waits
    // ENCODE_REST times as long as this one took: checkpoints use at most 1 / (1 + ENCODE_REST)
    // of one core, and the boards' commits keep the rest.
    let Some(mut saved) = off_thread(move || {
        let _one = ENCODING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let started = std::time::Instant::now();
        let saved = encode_spread(&fold, &body, crate::Spread::Inline);
        rest(started.elapsed());
        saved
    })
    .await
    else {
        return;
    };
    if mutated(Mutation::CheckpointAhead) {
        saved.seq += 1;
    }
    let Some(write) = host.save_checkpoint(&scope, &saved) else {
        saving.0.unsupported.store(true, Ordering::Release);
        return;
    };
    let _ = write.await;
    drop(saving);
}
