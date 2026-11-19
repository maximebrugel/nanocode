use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use crate::{
    Board, BoardError, EventId, Op,
    messages::Say,
    sim::{Mutation, mutated},
};

const REMEMBERED: usize = 64;

pub struct Write<'a> {
    pub id: &'a EventId,
    pub by: &'a str,
    pub op: &'a Op,
}

impl Write<'_> {
    pub fn text(&self) -> Option<&str> {
        match self.op {
            Op::Post { task } => Some(&task.spec),
            Op::Reply { body, .. } => Some(body),
            Op::Say {
                say: Say::Post { text, .. } | Say::Edit { text, .. },
            } => Some(text),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Patch {
        text: Option<String>,
        labels: Option<BTreeMap<String, String>>,
    },
    Drop(String),
}

pub type Middleware = Arc<dyn Fn(&Write<'_>) -> Verdict + Send + Sync>;

#[derive(Default)]
pub struct Chain {
    layers: Vec<Middleware>,
    vetted: VecDeque<(EventId, Result<Op, String>)>,
}

const fn text_mut(op: &mut Op) -> Option<&mut String> {
    match op {
        Op::Post { task } => Some(&mut task.spec),
        Op::Reply { body, .. } => Some(body),
        Op::Say {
            say: Say::Post { text, .. } | Say::Edit { text, .. },
        } => Some(text),
        _ => None,
    }
}

const fn labels_mut(op: &mut Op) -> Option<&mut BTreeMap<String, String>> {
    match op {
        Op::Post { task } => Some(&mut task.labels),
        Op::Say {
            say: Say::Post { labels, .. },
        } => Some(labels),
        _ => None,
    }
}

const fn emptied(op: &Op) -> bool {
    match op {
        Op::Say {
            say: Say::Post { text, files, .. },
        } => text.is_empty() && files.is_empty(),
        Op::Say {
            say: Say::Edit { text, .. },
        }
        | Op::Reply { body: text, .. } => text.is_empty(),
        _ => false,
    }
}

impl Chain {
    pub(crate) fn vet(&mut self, id: &EventId, by: &str, op: &Op) -> Result<Op, BoardError> {
        let mut probe = op.clone();
        if self.layers.is_empty() || text_mut(&mut probe).is_none() {
            return Ok(probe);
        }
        if !mutated(Mutation::MiddlewareEveryAttempt)
            && let Some((_, kept)) = self.vetted.iter().find(|(seen, _)| seen == id)
        {
            return kept.clone().map_err(BoardError::Dropped);
        }
        let kept = self.run(id, by, probe);
        if self.vetted.len() == REMEMBERED {
            self.vetted.pop_front();
        }
        self.vetted.push_back((id.clone(), kept.clone()));
        assert!(self.vetted.len() <= REMEMBERED, "the chain remembers a bounded window");
        kept.map_err(BoardError::Dropped)
    }

    fn run(&self, id: &EventId, by: &str, mut op: Op) -> Result<Op, String> {
        for layer in &self.layers {
            match layer(&Write { id, by, op: &op }) {
                Verdict::Pass => {}
                Verdict::Drop(why) => return Err(why),
                Verdict::Patch { text, labels } => {
                    if let Some(text) = text
                        && let Some(slot) = text_mut(&mut op)
                    {
                        *slot = text;
                    }
                    if let Some(labels) = labels
                        && let Some(slot) = labels_mut(&mut op)
                    {
                        *slot = labels;
                    }
                    if emptied(&op) {
                        return Err("the middleware emptied the write".to_owned());
                    }
                }
            }
        }
        Ok(op)
    }
}

impl Board {
    pub fn add_middleware(&mut self, layer: Middleware) {
        self.chain.layers.push(layer);
    }
}
