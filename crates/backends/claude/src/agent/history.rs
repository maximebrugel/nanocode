use crate::{ClaudeError, ContentBlock, Encoded, Message, MessageDigests, WireMessage};
use nanocode_durability::EncodedPayload;
use nanocode_sync::Atom;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::{
    ops::Deref,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Clone)]
pub(super) struct HistoryMessage(Arc<Entry>, u64);

static WIRE_IDS: AtomicU64 = AtomicU64::new(0);

fn wire_id() -> u64 {
    WIRE_IDS.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
struct Kept {
    json: Arc<str>,
    record: EncodedPayload,
}

impl Kept {
    fn encode(message: &Message) -> serde_json::Result<Self> {
        let json = Arc::<str>::from(serde_json::to_string(message)?);
        Ok(Self {
            record: EncodedPayload::from_json(Arc::clone(&json)),
            json,
        })
    }
}

struct Entry {
    message: Message,
    wire: OnceLock<Box<str>>,
    digests: OnceLock<MessageDigests>,
    images: OnceLock<usize>,
    companions: OnceLock<bool>,
    record: Atom<Option<Kept>>,
    results: OnceLock<Box<[(usize, u64)]>>,
    cleared: OnceLock<HistoryMessage>,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            message: self.message.clone(),
            wire: OnceLock::new(),
            digests: OnceLock::new(),
            images: OnceLock::new(),
            companions: OnceLock::new(),
            record: Atom::new(None),
            results: OnceLock::new(),
            cleared: OnceLock::new(),
        }
    }
}

impl HistoryMessage {
    pub(super) fn make_mut(&mut self) -> &mut Message {
        self.1 = wire_id();
        let entry = Arc::make_mut(&mut self.0);
        entry.wire = OnceLock::new();
        entry.digests = OnceLock::new();
        entry.images = OnceLock::new();
        entry.companions = OnceLock::new();
        entry.record = Atom::new(None);
        entry.results = OnceLock::new();
        entry.cleared = OnceLock::new();
        &mut entry.message
    }

    pub(super) fn cleared(&self, clear: impl FnOnce(&mut Message)) -> Self {
        self.0
            .cleared
            .get_or_init(|| {
                let mut message = self.0.message.clone();
                clear(&mut message);
                Self::from(message)
            })
            .clone()
    }

    pub(super) fn image_count(&self, count: impl FnOnce(&Self) -> usize) -> usize {
        if let Some(images) = self.0.images.get() {
            return *images;
        }
        let images = count(self);
        *self.0.images.get_or_init(|| images)
    }

    pub(super) fn result_tokens(
        &self,
        measure: impl Fn(&crate::ContentBlock) -> Option<u64>,
    ) -> &[(usize, u64)] {
        self.0.results.get_or_init(|| {
            self.0
                .message
                .content
                .iter()
                .enumerate()
                .filter_map(|(block, content)| measure(content).map(|tokens| (block, tokens)))
                .collect()
        })
    }

    pub(super) fn has_companions(&self, check: impl FnOnce(&Message) -> bool) -> bool {
        *self.0.companions.get_or_init(|| check(&self.0.message))
    }

    pub(super) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn into_message(self) -> Message {
        Arc::try_unwrap(self.0).map_or_else(|entry| entry.message.clone(), |entry| entry.message)
    }

    pub(super) fn recorded(message: Message, json: Arc<str>, record: EncodedPayload) -> Self {
        let recorded = Self::from(message);
        recorded.0.record.set(Some(Kept { json, record }));
        recorded
    }

    pub(super) fn with_json<R>(&self, read: impl FnOnce(&str) -> R) -> serde_json::Result<R> {
        match &*self.0.record.get() {
            Some(kept) => Ok(read(&kept.json)),
            None => Ok(read(&serde_json::to_string(&self.0.message)?)),
        }
    }

    pub(super) fn record(&self) -> serde_json::Result<EncodedPayload> {
        if let Some(kept) = &*self.0.record.get() {
            return Ok(kept.record.clone());
        }
        let encoded = Kept::encode(&self.0.message)?;
        Ok(self
            .0
            .record
            .update(|kept| kept.get_or_insert_with(|| encoded.clone()).record.clone()))
    }

    pub(super) fn committed(&self) {
        self.0.record.set(None);
    }
}

impl From<Message> for HistoryMessage {
    fn from(message: Message) -> Self {
        Self(
            Arc::new(Entry {
                message,
                wire: OnceLock::new(),
                digests: OnceLock::new(),
                images: OnceLock::new(),
                companions: OnceLock::new(),
                record: Atom::new(None),
                results: OnceLock::new(),
                cleared: OnceLock::new(),
            }),
            wire_id(),
        )
    }
}

impl Deref for HistoryMessage {
    type Target = Message;

    fn deref(&self) -> &Message {
        &self.0.message
    }
}

impl PartialEq for HistoryMessage {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || self.0.message == other.0.message
    }
}

impl Serialize for HistoryMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &*self.0.record.get() {
            Some(kept) => serde_json::from_str::<&RawValue>(&kept.json)
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer),
            None => self.0.message.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for HistoryMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Message::deserialize(deserializer).map(Self::from)
    }
}

impl WireMessage for HistoryMessage {
    fn message(&self) -> &Message {
        &self.0.message
    }

    fn wire_id(&self) -> Option<u64> {
        Some(self.1)
    }

    fn encoded<'a>(
        &'a self,
        subscription: bool,
        encode: &dyn Fn(&Message) -> Result<String, ClaudeError>,
    ) -> Result<Encoded<'a>, ClaudeError> {
        if subscription {
            let wire = match self.0.wire.get() {
                Some(wire) => wire,
                None => {
                    let encoded = encode(&self.0.message)?.into_boxed_str();
                    self.0.wire.get_or_init(|| encoded)
                }
            };
            return Ok(Encoded::Borrowed(wire));
        }
        match &*self.0.record.get() {
            Some(kept) => Ok(Encoded::Shared(Arc::clone(&kept.json))),
            None => encode(&self.0.message).map(Encoded::Owned),
        }
    }

    fn digests(
        &self,
        encode: &dyn Fn(&Message) -> Result<String, ClaudeError>,
    ) -> Result<MessageDigests, ClaudeError> {
        if let Some(digests) = self.0.digests.get() {
            return Ok(*digests);
        }
        let digests = MessageDigests::of(&self.encoded(false, encode)?);
        Ok(*self.0.digests.get_or_init(|| digests))
    }
}

#[derive(Clone)]
pub(super) struct CodeRound(Arc<Round>);

struct Round {
    value: serde_json::Value,
    record: OnceLock<EncodedPayload>,
    bytes: OnceLock<usize>,
}

impl CodeRound {
    pub(super) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn record(&self) -> serde_json::Result<EncodedPayload> {
        if let Some(record) = self.0.record.get() {
            return Ok(record.clone());
        }
        let record = EncodedPayload::from_json(serde_json::to_string(&self.0.value)?.into());
        Ok(self.0.record.get_or_init(|| record).clone())
    }

    pub(super) fn bytes(&self) -> usize {
        *self
            .0
            .bytes
            .get_or_init(|| super::bounded_json_len(&self.0.value, usize::MAX))
    }
}

impl From<serde_json::Value> for CodeRound {
    fn from(value: serde_json::Value) -> Self {
        Self(Arc::new(Round {
            value,
            record: OnceLock::new(),
            bytes: OnceLock::new(),
        }))
    }
}

impl Deref for CodeRound {
    type Target = serde_json::Value;

    fn deref(&self) -> &serde_json::Value {
        &self.0.value
    }
}

impl PartialEq for CodeRound {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.value == other.0.value
    }
}

impl Serialize for CodeRound {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CodeRound {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde_json::Value::deserialize(deserializer).map(Self::from)
    }
}

static ADMITTED_EPOCH: AtomicU64 = AtomicU64::new(1);

pub(super) struct AdmittedIds {
    order: nanocode_sync::Vector<Arc<str>>,
    positions: Option<nanocode_sync::Map<Arc<str>, usize>>,
    epoch: u64,
}

impl Default for AdmittedIds {
    fn default() -> Self {
        Self {
            order: nanocode_sync::Vector::new(),
            positions: None,
            epoch: ADMITTED_EPOCH.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl Clone for AdmittedIds {
    fn clone(&self) -> Self {
        Self {
            order: self.order.clone(),
            positions: self.positions.clone(),
            epoch: ADMITTED_EPOCH.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl PartialEq for AdmittedIds {
    fn eq(&self, other: &Self) -> bool {
        self.order.len() == other.order.len() && self.order.iter().all(|id| other.contains(id))
    }
}

impl AdmittedIds {
    pub(super) fn contains(&self, id: &str) -> bool {
        self.position(id).is_some()
    }

    pub(super) fn position(&self, id: &str) -> Option<usize> {
        self.positions.as_ref()?.get(id).copied()
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub(super) const fn len(&self) -> usize {
        self.order.len()
    }

    pub(super) const fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(super) fn since(&self, seen: usize) -> impl Iterator<Item = &Arc<str>> {
        (seen..self.order.len()).map(|at| &self.order[at])
    }

    pub(super) fn insert(&mut self, id: Arc<str>) {
        if !self.contains(&id) {
            self.positions
                .get_or_insert_default()
                .insert(Arc::clone(&id), self.order.len());
            self.order.push_back(id);
        }
    }
}

impl Extend<String> for AdmittedIds {
    fn extend<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        for id in ids {
            self.insert(id.into());
        }
    }
}

impl Extend<Arc<str>> for AdmittedIds {
    fn extend<I: IntoIterator<Item = Arc<str>>>(&mut self, ids: I) {
        for id in ids {
            self.insert(id);
        }
    }
}

impl IntoIterator for AdmittedIds {
    type Item = Arc<str>;
    type IntoIter = <nanocode_sync::Vector<Arc<str>> as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.order.into_iter()
    }
}

impl Serialize for AdmittedIds {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(&self.order)
    }
}

impl<'de> Deserialize<'de> for AdmittedIds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut ids = Self::default();
        ids.extend(Vec::<String>::deserialize(deserializer)?);
        Ok(ids)
    }
}

pub(super) fn media_placeholders(messages: &mut [HistoryMessage]) {
    let placeholder = |kind: &str| match kind {
        "image" => Some("[image]"),
        "document" => Some("[document]"),
        _ => None,
    };
    let media = |block: &ContentBlock| match block {
        ContentBlock::Image { .. } | ContentBlock::Document { .. } => true,
        ContentBlock::ToolResult {
            content: crate::ToolResultContent::Blocks(blocks),
            ..
        } => blocks
            .iter()
            .any(|block| block["type"].as_str().and_then(placeholder).is_some()),
        _ => false,
    };
    for message in messages {
        if !message.content.iter().any(media) {
            continue;
        }
        for block in &mut message.make_mut().content {
            match block {
                ContentBlock::Image { .. } => *block = ContentBlock::text("[image]"),
                ContentBlock::Document { .. } => *block = ContentBlock::text("[document]"),
                ContentBlock::ToolResult {
                    content: crate::ToolResultContent::Blocks(blocks),
                    ..
                } => {
                    for block in blocks {
                        if let Some(text) = block["type"].as_str().and_then(placeholder) {
                            *block = serde_json::json!({"type": "text", "text": text});
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

pub(super) fn strip_thinking(messages: &mut Vec<HistoryMessage>) {
    messages.retain_mut(|message| {
        if message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
            )
        }) {
            message.make_mut().content.retain(|block| {
                !matches!(
                    block,
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                )
            });
        }
        !message.content.is_empty() || message.is_effort_update()
    });
}
