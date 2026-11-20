use serde::{Deserialize, Serialize};

use crate::{
    Fold, Phase, Task, TaskId,
    sim::{Mutation, mutated},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefPolicy {
    pub tokens_max: u64,
    pub bytes_per_token: u64,
}

impl BriefPolicy {
    pub const fn tokens(&self, bytes: usize) -> u64 {
        (bytes as u64).div_ceil(self.bytes_per_token)
    }

    pub const fn bytes_max(&self) -> u64 {
        self.tokens_max.saturating_mul(self.bytes_per_token)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BriefError {
    UnknownTask,
    PolicyEmpty,
    OverBudget {
        tokens_required: u64,
        tokens_max: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    pub task: TaskId,
    pub text: String,
    pub tokens: u64,
    pub steers: Vec<u64>,
    pub omitted_bytes: u64,
}

struct Draft {
    text: String,
    bytes_max: usize,
    omitted_bytes: u64,
}

impl Draft {
    fn optional(&mut self, section: &str) {
        let remaining = self.bytes_max.saturating_sub(self.text.len());
        if section.len() <= remaining || mutated(Mutation::BriefOverBudget) {
            self.text.push_str(section);
            return;
        }
        let marker_bytes = omitted(section.len()).len();
        let Some(mut cut) = remaining.checked_sub(marker_bytes) else {
            self.omitted_bytes += section.len() as u64;
            return;
        };
        while !section.is_char_boundary(cut) {
            cut -= 1;
        }
        let dropped = section.len() - cut;
        self.text.push_str(&section[..cut]);
        self.text.push_str(&omitted(dropped));
        self.omitted_bytes += dropped as u64;
    }
}

fn omitted(bytes: usize) -> String {
    format!("\n[{bytes:020} bytes omitted]\n")
}

fn outcome(task: &Task) -> Option<(&'static str, &str)> {
    match &task.phase {
        Phase::Done { result, .. } => Some(("done", result)),
        Phase::Failed { error, .. } => Some(("failed", error)),
        Phase::Cancelled { reason } => Some(("cancelled", reason)),
        Phase::Open | Phase::Claimed { .. } | Phase::Blocked { .. } => None,
    }
}

impl Fold {
    pub fn brief(&self, id: &TaskId, policy: &BriefPolicy) -> Result<Brief, BriefError> {
        if policy.tokens_max == 0 || policy.bytes_per_token == 0 {
            return Err(BriefError::PolicyEmpty);
        }
        let task = self.task(id).ok_or(BriefError::UnknownTask)?;
        let (text, steers) = self.required(id, task);
        let tokens_required = policy.tokens(text.len());
        if tokens_required > policy.tokens_max {
            return Err(BriefError::OverBudget {
                tokens_required,
                tokens_max: policy.tokens_max,
            });
        }
        let mut draft = Draft {
            text,
            bytes_max: usize::try_from(policy.bytes_max()).unwrap_or(usize::MAX),
            omitted_bytes: 0,
        };
        if let Some(parent) = task
            .spec
            .parent
            .as_ref()
            .and_then(|parent| self.task(parent))
        {
            draft.optional(&format!(
                "\n# Parent goal {}\n{}\n",
                parent.spec.id.0, parent.spec.spec
            ));
            if mutated(Mutation::BriefLeaksParentHistory) {
                for (by, body) in &parent.replies {
                    draft.text.push_str(&format!("{by}: {body}\n"));
                }
            }
        }
        let prior = task.spec.needs.iter().chain(self.children(id));
        for (prior, task) in prior.filter_map(|prior| self.task(prior).map(|task| (prior, task))) {
            if let Some((kind, body)) = outcome(task) {
                draft.optional(&format!("\n# Prior result {} ({kind})\n{body}\n", prior.0));
            }
        }
        let tokens = policy.tokens(draft.text.len());
        assert!(tokens <= policy.tokens_max, "brief over its token budget");
        Ok(Brief {
            task: id.clone(),
            text: draft.text,
            tokens,
            steers,
            omitted_bytes: draft.omitted_bytes,
        })
    }

    fn required(&self, id: &TaskId, task: &Task) -> (String, Vec<u64>) {
        let mut text = format!("# Task {}\n{}\n", id.0, task.spec.spec);
        if !task.spec.labels.is_empty() {
            text.push_str("\n# Labels\n");
            for (key, value) in &task.spec.labels {
                text.push_str(&format!("{key}={value}\n"));
            }
        }
        if !task.spec.terms.is_default() {
            text.push_str(&format!("\n# Terms\n{:?}\n", task.spec.terms));
        }
        let pending: Vec<_> = self.steers(id).collect();
        let kept = if mutated(Mutation::BriefDropsSteer) {
            pending.len().saturating_sub(1)
        } else {
            pending.len()
        };
        let mut steers = Vec::with_capacity(kept);
        for steer in &pending[..kept] {
            text.push_str(&format!(
                "\n# Steer {} from {}\n{}\n",
                steer.seq, steer.by, steer.body
            ));
            steers.push(steer.seq);
        }
        (text, steers)
    }
}
