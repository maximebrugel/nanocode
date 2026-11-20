//! Fold checkpoints: a board saves its fold beside the log every [`CHECKPOINT_EVERY`] events and
//! reopens from the newest one, folding only the rows after it, so a reopen costs the tail
//! rather than the whole log.
//!
//! A checkpoint is the fold's compact snapshot after every row before its seq, under
//! [`CHECKPOINT_VERSION`], with a hash of the row just before its seq. A reopen trusts it only
//! when its version is this build's, it restores, its fold's next seq is its seq and that row is
//! still in the log as hashed; anything else replays the whole log. A checkpoint is only ever a
//! shortcut: losing one, failing to save one or failing to read one costs a full replay.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nanocode_sys::base64::STANDARD_NO_PAD;
use nanocode_turnlog::{LogHost, StoredCheckpoint};

use crate::{
    Board, BoardError, CHECKPOINT_VERSION, END, Fold,
    sim::{Mutation, mutated},
};

/// How many events a board folds past its last checkpoint before it saves another.
pub const CHECKPOINT_EVERY: u64 = 4096;

static EVERY: AtomicU64 = AtomicU64::new(CHECKPOINT_EVERY);

/// Sets how many events a board folds past its last checkpoint before it saves another, for
/// every board in the process; 0 saves none on its own.
pub fn checkpoint_every(events: u64) {
    EVERY.store(events, Ordering::Relaxed);
}

/// FNV-1a over a row's body: what a checkpoint holds of the row before its seq, so a log that
/// lost or rewrote that row no longer matches it.
fn row_hash(body: &str) -> u64 {
    body.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// How many of the newest events a checkpoint keeps whole: older history is compacted
/// ([`Fold::compact`]) before the fold is saved.
pub const RETAIN_EVENTS: u64 = CHECKPOINT_EVERY;

static RETAIN: AtomicU64 = AtomicU64::new(RETAIN_EVENTS);

/// Sets how many of the newest events every board's checkpoints keep whole.
pub fn retain_events(events: u64) {
    RETAIN.store(events, Ordering::Relaxed);
}

/// A checkpoint of a fold whose last row, the one before its next seq, has the body `last_body`,
/// compacted to the newest [`retain_events`] events.
pub fn encode(fold: &Fold, last_body: &str) -> StoredCheckpoint {
    encode_within(fold, last_body, RETAIN.load(Ordering::Relaxed))
}

/// [`encode`], keeping the newest `retain` events whole.
pub fn encode_within(fold: &Fold, last_body: &str, retain: u64) -> StoredCheckpoint {
    let mut fold = fold.clone();
    fold.compact(fold.next().saturating_sub(retain));
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

    /// Saves a checkpoint when the fold is [`CHECKPOINT_EVERY`] events past the last one. A
    /// failure only means the next reopen folds more.
    pub(crate) async fn keep_checkpoint(&mut self) {
        let every = EVERY.load(Ordering::Relaxed);
        if every > 0 && self.fold.next() >= self.checkpointed + every {
            let _ = self.checkpoint().await;
        }
    }
}
