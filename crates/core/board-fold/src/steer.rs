use std::collections::{BTreeMap, VecDeque};

pub use nanocode_types_core::{PromptInput, UserInput};
use serde::{Deserialize, Serialize};

use crate::{
    Change, ClaimId, Event, EventId, Fold, Rejection, TaskId,
    messages::TEXT_CHARS,
    sim::{Mutation, mutated},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum SteerOp {
    Send {
        task: TaskId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim: Option<ClaimId>,
        body: PromptInput,
    },
    Heed {
        task: TaskId,
        claim: ClaimId,
        upto: u64,
    },
}

impl SteerOp {
    pub const fn task(&self) -> &TaskId {
        match self {
            Self::Send { task, .. } | Self::Heed { task, .. } => task,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steer {
    pub seq: u64,
    pub by: String,
    pub at_ms: u64,
    pub to: Option<ClaimId>,
    pub body: PromptInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Unheeded { upto: u64 },
    NotPending { upto: u64 },
    Text(usize),
    Local,
    Bytes(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Steered {
    Sent {
        by: String,
        to: Option<ClaimId>,
        body: PromptInput,
    },
    Heeded {
        claim: ClaimId,
        upto: u64,
        count: u64,
    },
    Dropped {
        upto: u64,
        count: u64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steers {
    #[serde(with = "crate::pairs")]
    queues: BTreeMap<TaskId, VecDeque<Steer>>,
}

impl Steers {
    pub const fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }
}

impl Fold {
    pub fn steers(&self, task: &TaskId) -> impl Iterator<Item = &Steer> + '_ {
        self.steers.queues.get(task).into_iter().flatten()
    }

    pub fn steers_for(
        &self,
        task: &TaskId,
        claim: &ClaimId,
    ) -> Result<impl Iterator<Item = &Steer> + '_, Rejection> {
        self.holder(task, claim)?;
        let since = match self.seen.get(&EventId(claim.0.clone())) {
            Some(Ok(seq)) if mutated(Mutation::SteerLostOnMove) => *seq,
            _ => 0,
        };
        Ok(self.steers(task).filter(move |steer| steer.seq > since))
    }

    pub(crate) fn unheeded(&self, task: &TaskId) -> Result<(), Rejection> {
        match self.steers(task).last() {
            Some(steer) if !mutated(Mutation::FinishIgnoresSteers) => {
                Err(Rejection::Steer(Refusal::Unheeded { upto: steer.seq }))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn check_steer(&self, op: &SteerOp) -> Result<(), Rejection> {
        let task = self.tasks.get(op.task()).ok_or(Rejection::UnknownTask)?;
        match op {
            SteerOp::Send { .. } if task.finished() && !mutated(Mutation::SteerToTerminal) => {
                Err(Rejection::Terminal)
            }
            SteerOp::Send {
                task,
                claim: Some(claim),
                body,
            } => {
                if !mutated(Mutation::SteerByClaimFollows) {
                    self.holder(task, claim)?;
                }
                steer_body(body)
            }
            SteerOp::Send { body, .. } => steer_body(body),
            SteerOp::Heed { task, claim, upto } => {
                if !mutated(Mutation::HeedFromStaleClaim) {
                    self.holder(task, claim)?;
                }
                if self.steers(task).any(|steer| steer.seq == *upto) {
                    Ok(())
                } else {
                    Err(Rejection::Steer(Refusal::NotPending { upto: *upto }))
                }
            }
        }
    }

    pub(crate) fn apply_steer(&mut self, seq: u64, event: &Event, op: &SteerOp) {
        let id = op.task().clone();
        let change = match op {
            SteerOp::Send { claim, body, .. } => {
                self.steers
                    .queues
                    .entry(id.clone())
                    .or_default()
                    .push_back(Steer {
                        seq,
                        by: event.by.clone(),
                        at_ms: self.now_ms,
                        to: claim.clone(),
                        body: body.clone(),
                    });
                Steered::Sent {
                    by: event.by.clone(),
                    to: claim.clone(),
                    body: body.clone(),
                }
            }
            SteerOp::Heed { claim, upto, .. } => {
                let Some(queue) = self.steers.queues.get_mut(&id) else {
                    return;
                };
                let before = queue.len();
                if !mutated(Mutation::SteerRedeliveredAfterHeed) {
                    queue.retain(|steer| steer.seq > *upto);
                }
                let count = (before - queue.len()) as u64;
                if queue.is_empty() {
                    self.steers.queues.remove(&id);
                }
                Steered::Heeded {
                    claim: claim.clone(),
                    upto: *upto,
                    count,
                }
            }
        };
        self.record(seq, event, &id, Change::Steered(change));
    }
}

impl Fold {
    /// Cancelling a task drops the steers no holder heeded and records how many, so
    /// whoever watches it learns they were never acted on instead of waiting forever.
    pub(crate) fn drop_steers(&mut self, seq: u64, event: &Event, id: &TaskId) {
        if mutated(Mutation::SteerKeptOnCancel) {
            return;
        }
        let Some(queue) = self.steers.queues.remove(id) else {
            return;
        };
        let Some(upto) = queue.back().map(|steer| steer.seq) else {
            return;
        };
        let change = Steered::Dropped {
            upto,
            count: queue.len() as u64,
        };
        self.record(seq, event, id, Change::Steered(change));
    }
}

/// The most a steer may carry inline (text, URLs, file ids and file data together).
pub const STEER_BYTES: usize = 64 * 1024;

/// A steer is a member message, so it may hold the same blocks as one, except paths on
/// the sender's machine: the claim can move to another holder anywhere before it is read.
fn steer_body(body: &PromptInput) -> Result<(), Rejection> {
    let refuse = |refusal| Err(Rejection::Steer(refusal));
    if let PromptInput::Content(items) = body {
        if items.iter().any(is_local) && !mutated(Mutation::SteerLocalPath) {
            return refuse(Refusal::Local);
        }
        let bytes: usize = items.iter().map(inline_bytes).sum();
        if bytes > STEER_BYTES {
            return refuse(Refusal::Bytes(bytes));
        }
    }
    let chars = body.text_chars();
    if body.is_empty() || chars > TEXT_CHARS {
        return refuse(Refusal::Text(chars));
    }
    Ok(())
}

const fn is_local(item: &UserInput) -> bool {
    matches!(
        item,
        UserInput::LocalImage { .. } | UserInput::LocalAudio { .. }
    )
}

fn inline_bytes(item: &UserInput) -> usize {
    match item {
        UserInput::Text { text } => text.len(),
        UserInput::Image { image_url, .. } => image_url.len(),
        UserInput::ImageFile { file_id, .. } => file_id.len(),
        UserInput::Audio { audio_url } => audio_url.len(),
        UserInput::File {
            file_data,
            filename,
        } => file_data.len() + filename.as_ref().map_or(0, String::len),
        UserInput::LocalImage { path, .. } | UserInput::LocalAudio { path } => {
            path.as_os_str().len()
        }
    }
}

/// The steer as text for a brief: its text blocks in order, other blocks as a short tag.
#[must_use]
pub fn render(body: &PromptInput) -> String {
    let items = match body {
        PromptInput::Text(text) => return text.clone(),
        PromptInput::Content(items) => items,
    };
    let parts: Vec<String> = items
        .iter()
        .map(|item| match item {
            UserInput::Text { text } => text.clone(),
            UserInput::Image { image_url, .. } if !image_url.starts_with("data:") => {
                format!("[image {image_url}]")
            }
            UserInput::Image { .. } | UserInput::LocalImage { .. } => "[image]".to_owned(),
            UserInput::ImageFile { file_id, .. } => format!("[image {file_id}]"),
            UserInput::Audio { .. } | UserInput::LocalAudio { .. } => "[audio]".to_owned(),
            UserInput::File { filename, .. } => {
                format!("[file {}]", filename.as_deref().unwrap_or("attached"))
            }
        })
        .collect();
    parts.join("
")
}
