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

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
pub struct Sequence {
    tail: Option<Tail>,
}

impl Sequence {
    #[must_use]
    pub fn count(&self) -> Option<usize> {
        match &self.tail {
            None => Some(0),
            Some(Tail::Chained(_)) => None,
            Some(Tail::Sealed(sealed)) => Some(sealed.counts()[0]),
        }
    }
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
enum Tail {
    Sealed(Sealed),
    Chained(EncodedPayload),
}

#[derive(Clone, Default, Deserialize, PartialEq, Serialize)]
struct Sealed {
    levels: Vec<Vec<EncodedPayload>>,
    items: Vec<EncodedPayload>,
}

impl Sealed {
    fn counts(&self) -> Vec<usize> {
        let unsealed = std::iter::once(&self.items)
            .chain(&self.levels)
            .map(Vec::len)
            .collect::<Vec<_>>();
        let mut counts = vec![0; unsealed.len()];
        let mut above = 0;
        for (level, unsealed) in unsealed.iter().enumerate().rev() {
            above = above * GROUP + unsealed;
            counts[level] = above;
        }
        counts
    }
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
    skipped: Vec<usize>,
}

impl SequenceLog {
    #[must_use]
    pub fn resume(sequence: &Sequence) -> Option<Self> {
        let sealed = match &sequence.tail {
            None => return Some(Self::default()),
            Some(Tail::Chained(_)) => return None,
            Some(Tail::Sealed(sealed)) => sealed,
        };
        let counts = sealed.counts();
        let levels = std::iter::once(&sealed.items)
            .chain(&sealed.levels)
            .map(|entries| {
                entries
                    .iter()
                    .map(EncodedPayload::reference)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let skipped = counts
            .iter()
            .zip(&levels)
            .map(|(count, entries)| count - entries.len())
            .collect();
        Some(Self {
            released: levels.iter().map(Vec::len).collect(),
            levels,
            skipped,
        })
    }

    #[must_use]
    pub fn forget_sealed(&self) -> Option<Self> {
        Self::resume(&self.sequence())
    }

    #[must_use]
    pub fn held(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }

    fn skipped(&self, level: usize) -> usize {
        self.skipped.get(level).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.skipped(0) + self.levels.first().map_or(0, Vec::len)
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
        assert!(
            len >= self.skipped(0),
            "a resumed sequence log truncated into the prefix it no longer holds"
        );
        let mut count = len;
        for (level, entries) in self.levels.iter_mut().enumerate() {
            entries.truncate(count - self.skipped.get(level).copied().unwrap_or(0));
            count /= GROUP;
        }
        while self.levels.len() > self.skipped.len()
            && self.levels.last().is_some_and(Vec::is_empty)
        {
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
            let skipped = self.skipped(level);
            let entries = &mut self.levels[level];
            entries.push(entry);
            if !(skipped + entries.len()).is_multiple_of(GROUP) {
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

    fn unsealed(&self, level: usize) -> &[EncodedPayload] {
        let skipped = self.skipped(level);
        let entries = &self.levels[level];
        &entries[(skipped + entries.len()) / GROUP * GROUP - skipped..]
    }

    #[must_use]
    pub fn sequence(&self) -> Sequence {
        if self.levels.is_empty() {
            return Sequence::default();
        }
        let refs = |level: usize| {
            self.unsealed(level)
                .iter()
                .map(EncodedPayload::reference)
                .collect::<Vec<_>>()
        };
        Sequence {
            tail: Some(Tail::Sealed(Sealed {
                levels: (1..self.levels.len()).map(refs).collect(),
                items: refs(0),
            })),
        }
    }

    pub fn frontier(&self) -> impl Iterator<Item = &EncodedPayload> {
        (0..self.levels.len()).flat_map(move |level| self.unsealed(level))
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

#[cfg(test)]
mod resume_tests {
    use super::*;

    fn keys(log: &SequenceLog) -> Vec<String> {
        log.frontier().map(|entry| entry.key().to_owned()).collect()
    }

    #[test]
    fn resumed_sequence_log_matches_full_log() {
        let items = (0..=300_u32)
            .map(|item| EncodedPayload::encode(&format!("item {item}")).unwrap())
            .collect::<Vec<_>>();
        let mut full = SequenceLog::default();
        let mut expected = vec![(
            serde_json::to_string(&full.sequence()).unwrap(),
            keys(&full),
        )];
        for item in &items {
            full.push(item.clone()).unwrap();
            expected.push((
                serde_json::to_string(&full.sequence()).unwrap(),
                keys(&full),
            ));
            assert_eq!(full.sequence().count(), Some(full.len()));
        }
        let mut head = SequenceLog::default();
        for split in 0..=items.len() {
            if split > 0 {
                head.push(items[split - 1].clone()).unwrap();
            }
            let mut released = head.clone();
            released.release();
            let mut resumed = SequenceLog::resume(&released.sequence()).unwrap();
            assert_eq!(resumed.len(), split, "split {split}");
            assert_eq!(resumed.held(), keys(&resumed).len(), "split {split}");
            for n in split..=items.len() {
                if n > split {
                    resumed.push(items[n - 1].clone()).unwrap();
                }
                assert_eq!(resumed.len(), n, "n {n} split {split}");
                let (sequence, frontier) = &expected[n];
                assert_eq!(
                    &serde_json::to_string(&resumed.sequence()).unwrap(),
                    sequence,
                    "n {n} split {split}"
                );
                assert_eq!(&keys(&resumed), frontier, "n {n} split {split}");
            }
        }
    }
}
