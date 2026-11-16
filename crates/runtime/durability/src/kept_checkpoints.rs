use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use nanocode_sync::{Atom, Map};
use serde_json::Value;

use crate::{EncodedPayload, Hydrated, Previous};

#[derive(Clone)]
pub(crate) struct KeptCheckpoint {
    pub(crate) key: String,
    pub(crate) checkpoint: Value,
    pub(crate) payload: EncodedPayload,
    pub(crate) records: HashSet<String>,
    pub(crate) hydrated: Option<Hydrated>,
}

impl KeptCheckpoint {
    pub(crate) fn into_previous(self) -> Previous {
        Previous {
            checkpoint: self.checkpoint,
            hydrated: self.hydrated,
        }
    }
}

#[derive(Clone)]
enum Kept {
    Keeping(Map<Arc<str>, Arc<KeptCheckpoint>>),
    Forgotten,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChildPreload {
    pub kept: bool,
    pub unchanged: bool,
    pub whole: bool,
    pub reused: usize,
    pub decoded: usize,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChildReuse {
    pub exact: usize,
    pub advanced: usize,
    pub reused: usize,
    pub decoded: usize,
}

pub(crate) struct KeptCheckpoints {
    kept: Atom<Kept>,
    exact: AtomicUsize,
    advanced: AtomicUsize,
    reused: AtomicUsize,
    decoded: AtomicUsize,
}

impl KeptCheckpoints {
    pub(crate) fn new() -> Self {
        Self {
            kept: Atom::new(Kept::Keeping(Map::new())),
            exact: AtomicUsize::new(0),
            advanced: AtomicUsize::new(0),
            reused: AtomicUsize::new(0),
            decoded: AtomicUsize::new(0),
        }
    }

    pub(crate) fn forget(&self) {
        self.kept.set(Kept::Forgotten);
    }

    pub(crate) fn len(&self) -> usize {
        self.kept.read(|kept| match kept {
            Kept::Keeping(map) => map.len(),
            Kept::Forgotten => 0,
        })
    }

    pub(crate) fn key(&self, id: &str) -> Option<String> {
        self.kept.read(|kept| match kept {
            Kept::Keeping(map) => map.get(id).map(|kept| kept.key.clone()),
            Kept::Forgotten => None,
        })
    }

    pub(crate) fn take(&self, id: &str) -> Option<KeptCheckpoint> {
        self.kept
            .update(|kept| match kept {
                Kept::Keeping(map) => map.remove(id),
                Kept::Forgotten => None,
            })
            .map(|kept| Arc::try_unwrap(kept).unwrap_or_else(|kept| (*kept).clone()))
    }

    pub(crate) fn keep(&self, id: &str, checkpoint: KeptCheckpoint) -> bool {
        let checkpoint = Arc::new(checkpoint);
        self.kept.update(|kept| match kept {
            Kept::Keeping(map) => {
                map.insert(Arc::from(id), Arc::clone(&checkpoint));
                true
            }
            Kept::Forgotten => false,
        })
    }

    pub(crate) fn note_exact(&self) {
        self.exact.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn note_advanced(&self, reused: usize, decoded: usize) {
        self.advanced.fetch_add(1, Ordering::Relaxed);
        self.reused.fetch_add(reused, Ordering::Relaxed);
        self.decoded.fetch_add(decoded, Ordering::Relaxed);
    }

    pub(crate) fn reuse(&self) -> ChildReuse {
        ChildReuse {
            exact: self.exact.load(Ordering::Relaxed),
            advanced: self.advanced.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            decoded: self.decoded.load(Ordering::Relaxed),
        }
    }
}

pub(crate) fn value_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => 8,
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(value_bytes).sum(),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| key.len() + value_bytes(value))
            .sum(),
    }
}
