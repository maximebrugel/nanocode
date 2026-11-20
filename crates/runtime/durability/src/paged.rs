use std::collections::HashSet;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    DurableSession, EncodedPayload, Result, StoreRecord, session::DurableOwner,
    shared_store::SharedStore,
};

#[derive(Clone, Copy)]
enum Source<'a> {
    Owner(&'a DurableOwner),
    Session(&'a DurableSession),
    Store(&'a SharedStore, &'a str),
}

#[derive(Clone, Copy)]
pub struct PayloadReader<'a>(Source<'a>);

impl<'a> From<&'a DurableOwner> for PayloadReader<'a> {
    fn from(value: &'a DurableOwner) -> Self {
        Self(Source::Owner(value))
    }
}

impl<'a> From<&'a DurableSession> for PayloadReader<'a> {
    fn from(value: &'a DurableSession) -> Self {
        Self(Source::Session(value))
    }
}

impl<'a> PayloadReader<'a> {
    pub(crate) const fn store(store: &'a SharedStore, state_id: &'a str) -> Self {
        Self(Source::Store(store, state_id))
    }

    pub async fn load_payloads(self, payloads: Vec<EncodedPayload>) -> Result<Vec<EncodedPayload>> {
        match self.0 {
            Source::Owner(owner) => owner.load_payloads(payloads).await,
            Source::Session(session) => session.resolve_many(payloads).await,
            Source::Store(store, state_id) => match store.payload_memo.as_deref() {
                Some(memo) => memo.load_many(&payloads, store, state_id).await,
                None => EncodedPayload::load_many(&payloads, &mut store.clone(), state_id).await,
            },
        }
    }

    /// Loads payloads the caller decodes into values it owns and then drops, so a
    /// store's payload memo hands back and forgets its copies of them and keeps
    /// none: kept, they would hold the decoded values' bytes a second time.
    pub async fn take_payloads(self, payloads: Vec<EncodedPayload>) -> Result<Vec<EncodedPayload>> {
        match self.0 {
            Source::Store(store, state_id) => match store.payload_memo.as_deref() {
                Some(memo) => memo.take_many(&payloads, store, state_id).await,
                None => EncodedPayload::load_many(&payloads, &mut store.clone(), state_id).await,
            },
            Source::Owner(_) | Source::Session(_) => self.load_payloads(payloads).await,
        }
    }

    pub async fn load_payload(self, payload: EncodedPayload) -> Result<EncodedPayload> {
        match self.0 {
            Source::Owner(owner) => owner.load_payload(payload).await,
            Source::Session(session) => session.resolve(&payload).await,
            Source::Store(store, state_id) => match store.payload_memo.as_deref() {
                Some(memo) => memo
                    .load_many(std::slice::from_ref(&payload), store, state_id)
                    .await?
                    .pop()
                    .ok_or_else(|| {
                        crate::Error::InvalidState("payload load lost a payload".into())
                    }),
                None => payload.load(&mut store.clone(), state_id).await,
            },
        }
    }
}

const GROUP: usize = 16;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Sequence {
    tail: Option<Tail>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Tail {
    Sealed(Sealed),
    Chained(EncodedPayload),
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Sealed {
    levels: Vec<Vec<EncodedPayload>>,
    items: Vec<EncodedPayload>,
}

#[derive(Deserialize)]
struct Page {
    previous: Option<EncodedPayload>,
    items: Vec<EncodedPayload>,
}

#[derive(Deserialize, Serialize)]
struct Group {
    entries: Vec<EncodedPayload>,
}

pub struct Prepared {
    pub payload: EncodedPayload,
    pub keys: HashSet<String>,
}

#[derive(Serialize)]
struct GroupRef<'a> {
    entries: &'a [EncodedPayload],
}

#[derive(Clone, Default)]
pub struct SequenceLog {
    levels: Vec<Vec<EncodedPayload>>,
    released: Vec<usize>,
}

impl SequenceLog {
    #[must_use]
    pub fn len(&self) -> usize {
        self.levels.first().map_or(0, Vec::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn items(&self) -> &[EncodedPayload] {
        self.levels.first().map_or(&[], Vec::as_slice)
    }

    pub fn truncate(&mut self, len: usize) {
        let mut count = len;
        for level in &mut self.levels {
            level.truncate(count);
            count /= GROUP;
        }
        while self.levels.last().is_some_and(Vec::is_empty) {
            self.levels.pop();
        }
        self.released.truncate(self.levels.len());
        for (released, level) in self.released.iter_mut().zip(&self.levels) {
            *released = (*released).min(level.len());
        }
    }

    pub fn push(&mut self, item: EncodedPayload) -> Result<()> {
        self.push_known(item, None)
    }

    pub fn push_known(
        &mut self,
        item: EncodedPayload,
        known: Option<&HashSet<String>>,
    ) -> Result<()> {
        let mut entry = item;
        let mut level = 0;
        loop {
            if self.levels.len() == level {
                self.levels.push(Vec::new());
                self.released.push(0);
            }
            let entries = &mut self.levels[level];
            entries.push(entry);
            if !entries.len().is_multiple_of(GROUP) {
                return Ok(());
            }
            let group = &entries[entries.len() - GROUP..];
            let encoded = EncodedPayload::encode(&GroupRef { entries: group })?;
            entry = if known.is_some_and(|known| known.contains(encoded.key())) {
                encoded.reference()
            } else {
                encoded.with_payloads(group.to_vec())
            };
            level += 1;
        }
    }

    pub fn sync(
        &mut self,
        unchanged: usize,
        items: impl IntoIterator<Item = Result<EncodedPayload>>,
    ) -> Result<()> {
        self.sync_known(unchanged, items, None)
    }

    pub fn sync_known(
        &mut self,
        unchanged: usize,
        items: impl IntoIterator<Item = Result<EncodedPayload>>,
        known: Option<&HashSet<String>>,
    ) -> Result<()> {
        self.truncate(unchanged);
        for item in items {
            self.push_known(item?, known)?;
        }
        Ok(())
    }

    fn unsealed(entries: &[EncodedPayload]) -> &[EncodedPayload] {
        &entries[entries.len() / GROUP * GROUP..]
    }

    #[must_use]
    pub fn sequence(&self) -> Sequence {
        let Some((items, pages)) = self.levels.split_first() else {
            return Sequence::default();
        };
        let refs = |entries: &[EncodedPayload]| {
            Self::unsealed(entries)
                .iter()
                .map(EncodedPayload::reference)
                .collect::<Vec<_>>()
        };
        Sequence {
            tail: Some(Tail::Sealed(Sealed {
                levels: pages.iter().map(|pages| refs(pages)).collect(),
                items: refs(items),
            })),
        }
    }

    pub fn frontier(&self) -> impl Iterator<Item = &EncodedPayload> {
        self.levels
            .iter()
            .flat_map(|entries| Self::unsealed(entries))
    }

    pub fn release(&mut self) {
        for (released, entries) in self.released.iter_mut().zip(&mut self.levels) {
            for entry in &mut entries[*released..] {
                *entry = entry.reference();
            }
            *released = entries.len();
        }
    }
}

pub struct PagedWriter<'a> {
    known: &'a HashSet<String>,
    keys: HashSet<String>,
    records: Vec<StoreRecord>,
}

impl<'a> PagedWriter<'a> {
    #[must_use]
    pub fn new(known: &'a HashSet<String>) -> Self {
        Self {
            known,
            keys: HashSet::new(),
            records: Vec::new(),
        }
    }

    fn keep(&mut self, payload: &EncodedPayload) {
        let key = payload.key();
        if !self.keys.contains(key) && !self.known.contains(key) {
            let mut payload = payload.clone();
            payload.stage(&mut self.records);
        }
        self.keys.insert(key.to_owned());
    }

    pub fn sequence<T: Serialize>(&mut self, items: &[T]) -> Result<Sequence> {
        self.sequence_iter(items)
    }

    pub fn sequence_iter<'i, T: Serialize + 'i>(
        &mut self,
        items: impl IntoIterator<Item = &'i T>,
    ) -> Result<Sequence> {
        let mut log = SequenceLog::default();
        for item in items {
            let payload = EncodedPayload::encode(item)?;
            self.keep(&payload);
            log.push(payload.reference())?;
        }
        for page in log.levels.iter().skip(1).flatten() {
            self.keep(page);
        }
        Ok(log.sequence())
    }

    pub fn finish<T: Serialize>(self, value: &T) -> Result<Prepared> {
        Ok(Prepared {
            payload: EncodedPayload::encode(value)?.with_records(self.records),
            keys: self.keys,
        })
    }
}

pub async fn load_sequence<T: DeserializeOwned>(
    reader: PayloadReader<'_>,
    sequence: Sequence,
    keys: &mut HashSet<String>,
) -> Result<Vec<T>> {
    load_sequence_payloads(reader, sequence, keys)
        .await?
        .into_iter()
        .map(|payload| payload.decode())
        .collect()
}

pub async fn load_sequence_payloads(
    reader: PayloadReader<'_>,
    sequence: Sequence,
    keys: &mut HashSet<String>,
) -> Result<Vec<EncodedPayload>> {
    let references = sequence_references(reader, sequence, keys).await?;
    if references.is_empty() {
        return Ok(Vec::new());
    }
    reader.load_payloads(references).await
}

pub async fn sequence_references(
    reader: PayloadReader<'_>,
    sequence: Sequence,
    keys: &mut HashSet<String>,
) -> Result<Vec<EncodedPayload>> {
    let references = match sequence.tail {
        None => return Ok(Vec::new()),
        Some(Tail::Chained(tail)) => chained(reader, tail, keys).await?,
        Some(Tail::Sealed(sealed)) => unsealed(reader, sealed, keys).await?,
    };
    for reference in &references {
        keys.insert(reference.key().to_owned());
    }
    Ok(references)
}

async fn unsealed(
    reader: PayloadReader<'_>,
    sealed: Sealed,
    keys: &mut HashSet<String>,
) -> Result<Vec<EncodedPayload>> {
    let mut levels = sealed.levels;
    let mut pending = levels.pop().unwrap_or_default();
    loop {
        for reference in &pending {
            keys.insert(reference.key().to_owned());
        }
        let mut entries = Vec::new();
        for page in reader.load_payloads(std::mem::take(&mut pending)).await? {
            entries.extend(page.decode::<Group>()?.entries);
        }
        match levels.pop() {
            Some(inline) => {
                entries.extend(inline);
                pending = entries;
            }
            None => {
                entries.extend(sealed.items);
                return Ok(entries);
            }
        }
    }
}

async fn chained(
    reader: PayloadReader<'_>,
    tail: EncodedPayload,
    keys: &mut HashSet<String>,
) -> Result<Vec<EncodedPayload>> {
    let mut pages = Vec::new();
    let mut next = Some(tail);
    while let Some(reference) = next {
        keys.insert(reference.key().to_owned());
        let page: Page = reader.load_payload(reference).await?.decode()?;
        next = page.previous;
        pages.push(page.items);
    }
    Ok(pages.into_iter().rev().flatten().collect())
}
