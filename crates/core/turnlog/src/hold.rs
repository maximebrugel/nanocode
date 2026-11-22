use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Approved,
    Edited { input: Box<RawValue> },
    Denied { reason: Option<String> },
    Replied { message: String },
    Expired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Answered {
    Approved,
    Edited,
    Denied,
    Replied,
    Expired,
}

impl Answered {
    #[must_use]
    pub const fn runs(self) -> bool {
        matches!(self, Self::Approved | Self::Edited)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Edited => "edited",
            Self::Denied => "denied",
            Self::Replied => "replied",
            Self::Expired => "expired",
        }
    }
}

impl Answer {
    #[must_use]
    pub const fn answered(&self) -> Answered {
        match self {
            Self::Approved => Answered::Approved,
            Self::Edited { .. } => Answered::Edited,
            Self::Denied { .. } => Answered::Denied,
            Self::Replied { .. } => Answered::Replied,
            Self::Expired => Answered::Expired,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    Waiting {
        call_id: String,
        card: Box<RawValue>,
        input: Box<RawValue>,
        tool: String,
        turn: Option<String>,
    },
    Running {
        answered: Answered,
        call_id: String,
        input: Box<RawValue>,
        tool: String,
        turn: Option<String>,
    },
    Settled {
        answered: Answered,
        call_id: String,
        turn: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum Resumption {
    Run {
        call_id: String,
        input: Box<RawValue>,
        tool: String,
    },
    Answered {
        answer: Answer,
        call_id: String,
        tool: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HoldError {
    Unknown { key: String },
    Answered { key: String, answered: Answered },
    Taken { key: String, call_id: String },
    NotRunning { key: String },
}

impl std::fmt::Display for HoldError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { key } => write!(formatter, "no call is held under `{key}`"),
            Self::Answered { key, answered } => write!(
                formatter,
                "the call held under `{key}` was already answered: {}",
                answered.as_str()
            ),
            Self::Taken { key, call_id } => {
                write!(formatter, "`{key}` already holds call {call_id}")
            }
            Self::NotRunning { key } => {
                write!(formatter, "the call held under `{key}` is not running")
            }
        }
    }
}

impl std::error::Error for HoldError {}

pub struct HeldCall<'a> {
    pub call_id: &'a str,
    pub card: &'a RawValue,
    pub input: &'a RawValue,
    pub tool: &'a str,
    pub turn: Option<&'a str>,
}

impl Hold {
    #[must_use]
    pub fn call_id(&self) -> &str {
        match self {
            Self::Waiting { call_id, .. }
            | Self::Running { call_id, .. }
            | Self::Settled { call_id, .. } => call_id,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> &'static str {
        match self {
            Self::Waiting { .. } => "waiting",
            Self::Running { .. } => "running",
            Self::Settled { .. } => "settled",
        }
    }

    pub fn hold(
        existing: Option<&Self>,
        key: &str,
        call: &HeldCall<'_>,
    ) -> Result<Option<Self>, HoldError> {
        match existing {
            None => Ok(Some(Self::Waiting {
                call_id: call.call_id.to_owned(),
                card: call.card.to_owned(),
                input: call.input.to_owned(),
                tool: call.tool.to_owned(),
                turn: call.turn.map(str::to_owned),
            })),
            Some(Self::Waiting { call_id, .. }) if call_id == call.call_id => Ok(None),
            Some(
                Self::Running {
                    answered, call_id, ..
                }
                | Self::Settled {
                    answered, call_id, ..
                },
            ) if call_id == call.call_id => Err(HoldError::Answered {
                key: key.to_owned(),
                answered: *answered,
            }),
            Some(held) => Err(HoldError::Taken {
                key: key.to_owned(),
                call_id: held.call_id().to_owned(),
            }),
        }
    }

    pub fn resolve(
        existing: Option<&Self>,
        key: &str,
        answer: Answer,
    ) -> Result<(Self, Resumption), HoldError> {
        let Some(existing) = existing else {
            return Err(HoldError::Unknown {
                key: key.to_owned(),
            });
        };
        let Self::Waiting {
            call_id,
            input,
            tool,
            turn,
            ..
        } = existing
        else {
            let (Self::Running { answered, .. } | Self::Settled { answered, .. }) = existing else {
                unreachable!("a hold that is not waiting is running or settled");
            };
            return Err(HoldError::Answered {
                key: key.to_owned(),
                answered: *answered,
            });
        };
        let answered = answer.answered();
        if !answered.runs() {
            let settled = Self::Settled {
                answered,
                call_id: call_id.clone(),
                turn: turn.clone(),
            };
            let resumption = Resumption::Answered {
                answer,
                call_id: call_id.clone(),
                tool: tool.clone(),
            };
            return Ok((settled, resumption));
        }
        let input = match answer {
            Answer::Edited { input } => input,
            _ => input.clone(),
        };
        let running = Self::Running {
            answered,
            call_id: call_id.clone(),
            input: input.clone(),
            tool: tool.clone(),
            turn: turn.clone(),
        };
        let resumption = Resumption::Run {
            call_id: call_id.clone(),
            input,
            tool: tool.clone(),
        };
        Ok((running, resumption))
    }

    pub fn release(existing: Option<&Self>, key: &str) -> Result<Self, HoldError> {
        match existing {
            None => Err(HoldError::Unknown {
                key: key.to_owned(),
            }),
            Some(Self::Running {
                answered,
                call_id,
                turn,
                ..
            }) => Ok(Self::Settled {
                answered: *answered,
                call_id: call_id.clone(),
                turn: turn.clone(),
            }),
            Some(Self::Waiting { .. } | Self::Settled { .. }) => Err(HoldError::NotRunning {
                key: key.to_owned(),
            }),
        }
    }

    #[must_use]
    pub fn turn(&self) -> Option<&str> {
        match self {
            Self::Waiting { turn, .. } | Self::Running { turn, .. } | Self::Settled { turn, .. } => {
                turn.as_deref()
            }
        }
    }

    pub(crate) fn prunable(&self, retained: impl Fn(&str) -> bool) -> bool {
        let settled = matches!(self, Self::Settled { .. })
            || crate::sim::mutated(crate::sim::Mutation::HoldPrunesUnsettled);
        settled && self.turn().is_some_and(|turn| !retained(turn))
    }

    pub(crate) fn follows(&self, previous: Option<&Self>) -> bool {
        let Some(previous) = previous else {
            return true;
        };
        if let (Self::Settled { turn: Some(pruned), .. }, Self::Waiting { turn, .. }) =
            (previous, self)
            && previous.call_id() != self.call_id()
        {
            return turn.as_deref() != Some(pruned.as_str());
        }
        if previous.call_id() != self.call_id() || previous.turn() != self.turn() {
            return false;
        }
        match (previous, self) {
            (Self::Waiting { .. }, Self::Waiting { .. }) => true,
            (Self::Waiting { .. }, Self::Running { answered, .. }) => answered.runs(),
            (Self::Waiting { .. }, Self::Settled { answered, .. }) => !answered.runs(),
            (
                Self::Running {
                    answered: running, ..
                },
                Self::Settled { answered, .. },
            ) => running == answered,
            _ => false,
        }
    }
}
