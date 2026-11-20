use crate::{
    ClaudeError, ContentBlock, Encoded, Message, MessageDigests, MessageLayout, MessageView,
    OutputConfig, Role, WireMessage,
};
use nanocode_durability::EncodedPayload;
use nanocode_sync::{Atom, SwapOption};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::{
    ops::{Deref, DerefMut},
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
    fn of(json: Arc<str>) -> Self {
        Self {
            record: EncodedPayload::from_json(Arc::clone(&json)),
            json,
        }
    }

    fn encode(message: &Message) -> serde_json::Result<Self> {
        Ok(Self::of(serde_json::to_string(message)?.into()))
    }
}

struct Entry {
    typed: SwapOption<Message>,
    kept: Atom<Option<Kept>>,
    role: Role,
    output: Option<OutputConfig>,
    same_wire: OnceLock<bool>,
    layout: OnceLock<MessageLayout>,
    digests: OnceLock<MessageDigests>,
    images: OnceLock<usize>,
    companions: OnceLock<bool>,
    results: OnceLock<Box<[(usize, u64)]>>,
    cleared: OnceLock<HistoryMessage>,
}

impl Entry {
    fn new(message: &Message, typed: Option<Arc<Message>>, kept: Option<Kept>) -> Self {
        Self {
            typed: SwapOption::new(typed),
            kept: Atom::new(kept),
            role: message.role,
            output: message.output_config.clone(),
            same_wire: OnceLock::new(),
            layout: OnceLock::new(),
            digests: OnceLock::new(),
            images: OnceLock::new(),
            companions: OnceLock::new(),
            results: OnceLock::new(),
            cleared: OnceLock::new(),
        }
    }

    fn typed(&self) -> Arc<Message> {
        if let Some(message) = self.typed.load_full() {
            return message;
        }
        let kept = self.kept.get();
        let json = &Option::as_ref(&kept).expect("a message keeps its typed form or its bytes").json;
        Arc::new(serde_json::from_str(json).expect("a recorded message parses"))
    }
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        let typed = self.typed();
        Self::new(&typed, Some(Arc::clone(&typed)), Option::clone(&*self.kept.get()))
    }
}

pub(super) struct MessageMut<'a> {
    entry: &'a mut Entry,
    message: Option<Message>,
}

impl Deref for MessageMut<'_> {
    type Target = Message;

    fn deref(&self) -> &Message {
        self.message.as_ref().expect("a message being changed")
    }
}

impl DerefMut for MessageMut<'_> {
    fn deref_mut(&mut self) -> &mut Message {
        self.message.as_mut().expect("a message being changed")
    }
}

impl Drop for MessageMut<'_> {
    fn drop(&mut self) {
        if let Some(message) = self.message.take() {
            *self.entry = Entry::new(&message, None, None);
            self.entry.typed = SwapOption::new(Some(Arc::new(message)));
        }
    }
}

impl HistoryMessage {
    pub(super) fn make_mut(&mut self) -> MessageMut<'_> {
        self.1 = wire_id();
        let entry = Arc::make_mut(&mut self.0);
        let message = Arc::unwrap_or_clone(entry.typed());
        *entry = Entry::new(&message, None, None);
        MessageMut {
            entry,
            message: Some(message),
        }
    }

    pub(super) fn role(&self) -> Role {
        self.0.role
    }

    pub(super) fn view(&self) -> MessageView<'static> {
        MessageView::Shared(self.0.typed())
    }

    pub(super) fn any_block(&self, check: impl FnMut(&ContentBlock) -> bool) -> bool {
        self.0.typed().content.iter().any(check)
    }

    pub(super) fn is_effort_update(&self) -> bool {
        self.0.role == Role::System && self.0.output.is_some()
    }

    pub(super) fn output_config(&self) -> Option<&OutputConfig> {
        self.0.output.as_ref()
    }

    pub(super) fn mentions(&self, text: &str) -> bool {
        if self.0.typed.load().is_some() {
            return true;
        }
        let Some(kept) = &*self.0.kept.get() else {
            return true;
        };
        match serde_json::to_string(text) {
            Ok(escaped) => kept.json.contains(&escaped[1..escaped.len() - 1]),
            Err(_) => true,
        }
    }

    fn settled(message: Message) -> Self {
        match Kept::encode(&message) {
            Ok(kept) => Self(Arc::new(Entry::new(&message, None, Some(kept))), wire_id()),
            Err(_) => Self::from(message),
        }
    }

    pub(super) fn cleared(&self, clear: impl FnOnce(&mut Message)) -> Self {
        self.0
            .cleared
            .get_or_init(|| {
                let mut message = Message::clone(&self.0.typed());
                clear(&mut message);
                Self::settled(message)
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
                .typed()
                .content
                .iter()
                .enumerate()
                .filter_map(|(block, content)| measure(content).map(|tokens| (block, tokens)))
                .collect()
        })
    }

    pub(super) fn has_companions(&self, check: impl FnOnce(&Message) -> bool) -> bool {
        *self.0.companions.get_or_init(|| check(&self.0.typed()))
    }

    pub(super) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(super) fn into_message(self) -> Message {
        match Arc::try_unwrap(self.0) {
            Ok(entry) => Arc::unwrap_or_clone(entry.typed()),
            Err(entry) => Message::clone(&entry.typed()),
        }
    }

    pub(super) fn recorded(message: Message, json: Arc<str>, record: EncodedPayload) -> Self {
        let typed = Arc::new(message);
        Self(
            Arc::new(Entry::new(
                &typed,
                Some(Arc::clone(&typed)),
                Some(Kept { json, record }),
            )),
            wire_id(),
        )
    }

    fn kept(&self) -> serde_json::Result<Kept> {
        if let Some(kept) = &*self.0.kept.get() {
            return Ok(kept.clone());
        }
        let encoded = Kept::encode(&self.0.typed())?;
        Ok(self
            .0
            .kept
            .update(|kept| kept.get_or_insert_with(|| encoded.clone()).clone()))
    }

    pub(super) fn with_json<R>(&self, read: impl FnOnce(&str) -> R) -> serde_json::Result<R> {
        Ok(read(&self.kept()?.json))
    }

    pub(super) fn record(&self) -> serde_json::Result<EncodedPayload> {
        Ok(self.kept()?.record)
    }

    pub(super) fn committed(&self) {
        if self.0.kept.read(Option::is_some) {
            self.0.typed.store(None);
        }
    }
}

impl From<Message> for HistoryMessage {
    fn from(message: Message) -> Self {
        let typed = Arc::new(message);
        Self(
            Arc::new(Entry::new(&typed, Some(Arc::clone(&typed)), None)),
            wire_id(),
        )
    }
}

impl PartialEq for HistoryMessage {
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        if let (Some(left), Some(right)) = (&*self.0.kept.get(), &*other.0.kept.get()) {
            return left.json == right.json;
        }
        *self.0.typed() == *other.0.typed()
    }
}

impl Serialize for HistoryMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &*self.0.kept.get() {
            Some(kept) => serde_json::from_str::<&RawValue>(&kept.json)
                .map_err(serde::ser::Error::custom)?
                .serialize(serializer),
            None => self.0.typed().serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for HistoryMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Message::deserialize(deserializer).map(Self::settled)
    }
}

impl WireMessage for HistoryMessage {
    fn message(&self) -> MessageView<'_> {
        self.view()
    }

    fn wire_id(&self) -> Option<u64> {
        Some(self.1)
    }

    fn layout(&self) -> MessageLayout {
        *self
            .0
            .layout
            .get_or_init(|| <Message as WireMessage>::layout(&self.0.typed()))
    }

    fn encoded<'a>(
        &'a self,
        subscription: bool,
        encode: &dyn Fn(&Message) -> Result<String, ClaudeError>,
    ) -> Result<Encoded<'a>, ClaudeError> {
        let kept = self.kept()?;
        if !subscription {
            return Ok(Encoded::Shared(kept.json));
        }
        if let Some(same) = self.0.same_wire.get() {
            return Ok(if *same {
                Encoded::Shared(kept.json)
            } else {
                Encoded::Owned(crate::subscription_wire::encode_record(&kept.json)?)
            });
        }
        let wire = encode(&self.0.typed())?;
        let same = *self.0.same_wire.get_or_init(|| wire == *kept.json);
        Ok(if same {
            Encoded::Shared(kept.json)
        } else {
            Encoded::Owned(wire)
        })
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
        if !(message.mentions("image") || message.mentions("document")) || !message.any_block(&media) {
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
        if message.mentions("thinking") && message.any_block(|block| {
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
        !message.view().content.is_empty() || message.is_effort_update()
    });
}
