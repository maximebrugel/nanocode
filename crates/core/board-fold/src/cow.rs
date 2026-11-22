//! Copy-on-write ordered collections for the fold: a sorted run of entries cut into chunks of at
//! most [`CHUNK`], each behind an `Arc`. Cloning one copies only the chunk pointers, and a write
//! copies the one chunk it lands in when a clone still shares it, so a board can freeze its fold
//! for a checkpoint in O(chunks) and keep folding. Each chunk also caches its compact encoding,
//! which only a write to that chunk clears: a checkpoint re-encodes only the chunks that changed
//! since the last one.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    ops::{Bound, RangeBounds},
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The zstd level a chunk's encoding is packed at: the fastest, as a checkpoint packs only the
/// chunks written since the last one.
const PACK_LEVEL: i32 = 1;

/// The most entries a chunk holds before it splits in two.
pub const CHUNK: usize = 32;

/// One chunk: its entries in order, and its compact encoding once a checkpoint made it.
pub struct Chunk<T> {
    pub(crate) items: Vec<T>,
    pub(crate) encoded: OnceLock<Arc<[u8]>>,
}

impl<T> Chunk<T> {
    const fn new(items: Vec<T>) -> Self {
        Self {
            items,
            encoded: OnceLock::new(),
        }
    }
}

impl<T: Clone> Clone for Chunk<T> {
    /// A copy made to write to: it starts without the encoding, which the write would stale.
    fn clone(&self) -> Self {
        Self::new(self.items.clone())
    }
}

/// The chunk at `at`, unshared and with its cached encoding cleared, ready to write to.
fn writable<T: Clone>(chunks: &mut [Arc<Chunk<T>>], at: usize) -> &mut Vec<T> {
    let chunk = Arc::make_mut(&mut chunks[at]);
    chunk.encoded = OnceLock::new();
    &mut chunk.items
}

/// A sorted map with copy-on-write chunks; the subset of `BTreeMap`'s API the fold uses.
pub struct CowMap<K, V> {
    chunks: Vec<Arc<Chunk<(K, V)>>>,
    len: usize,
}

impl<K, V> CowMap<K, V> {
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&K, &V)> + Clone {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.items.iter().map(|(key, value)| (key, value)))
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + Clone {
        self.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + Clone {
        self.iter().map(|(_, value)| value)
    }

    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        self.iter().next()
    }

    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        self.iter().next_back()
    }
}

impl<K: Ord, V> CowMap<K, V> {
    /// The chunk a key belongs in: the first whose last key is not below it, or the last chunk.
    fn chunk_of<Q>(&self, key: &Q) -> usize
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let at = self.chunks.partition_point(|chunk| {
            chunk
                .items
                .last()
                .is_some_and(|(last, _)| last.borrow() < key)
        });
        at.min(self.chunks.len().saturating_sub(1))
    }

    /// The chunk and the position in it of a key, or where it would go.
    fn find<Q>(&self, key: &Q) -> Option<(usize, Result<usize, usize>)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.chunks.is_empty() {
            return None;
        }
        let at = self.chunk_of(key);
        let found = self.chunks[at]
            .items
            .binary_search_by(|(probe, _)| probe.borrow().cmp(key));
        Some((at, found))
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.find(key)? {
            (at, Ok(index)) => Some(&self.chunks[at].items[index].1),
            _ => None,
        }
    }

    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.find(key)? {
            (at, Ok(index)) => {
                let (key, value) = &self.chunks[at].items[index];
                Some((key, value))
            }
            _ => None,
        }
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    /// The chunk and index of the first entry at or past `bound`.
    fn position(&self, bound: Bound<&K>) -> (usize, usize) {
        let key = match bound {
            Bound::Unbounded => return (0, 0),
            Bound::Included(key) | Bound::Excluded(key) => key,
        };
        match self.find(key) {
            None => (0, 0),
            Some((at, Ok(index))) if matches!(bound, Bound::Excluded(_)) => (at, index + 1),
            Some((at, Ok(index) | Err(index))) => (at, index),
        }
    }

    pub fn range<R: RangeBounds<K>>(&self, range: R) -> impl Iterator<Item = (&K, &V)> + Clone
    where
        K: Clone,
    {
        let (chunk, index) = self.position(range.start_bound());
        let end = range.end_bound().cloned();
        self.chunks
            .get(chunk)
            .into_iter()
            .flat_map(move |first| first.items[index.min(first.items.len())..].iter())
            .chain(
                self.chunks
                    .get(chunk + 1..)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|chunk| chunk.items.iter()),
            )
            .map(|(key, value)| (key, value))
            .take_while(move |(key, _)| match &end {
                Bound::Included(end) => *key <= end,
                Bound::Excluded(end) => *key < end,
                Bound::Unbounded => true,
            })
    }
}

impl<K: Ord + Clone, V: Clone> CowMap<K, V> {
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.find(key)? {
            (at, Ok(index)) => Some(&mut writable(&mut self.chunks, at)[index].1),
            _ => None,
        }
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let Some((at, found)) = self.find(&key) else {
            self.chunks.push(Arc::new(Chunk::new(vec![(key, value)])));
            self.len = 1;
            return None;
        };
        match found {
            Ok(index) => Some(std::mem::replace(
                &mut writable(&mut self.chunks, at)[index].1,
                value,
            )),
            Err(index) => {
                writable(&mut self.chunks, at).insert(index, (key, value));
                self.len += 1;
                self.split(at);
                None
            }
        }
    }

    /// Splits the chunk at `at` in two once it outgrows [`CHUNK`].
    fn split(&mut self, at: usize) {
        if self.chunks[at].items.len() <= CHUNK {
            return;
        }
        let items = writable(&mut self.chunks, at);
        let tail = items.split_off(items.len() / 2);
        self.chunks.insert(at + 1, Arc::new(Chunk::new(tail)));
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.remove_entry(key).map(|(_, value)| value)
    }

    pub fn remove_entry<Q>(&mut self, key: &Q) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (at, Ok(index)) = self.find(key)? else {
            return None;
        };
        let removed = writable(&mut self.chunks, at).remove(index);
        self.len -= 1;
        if self.chunks[at].items.is_empty() {
            self.chunks.remove(at);
        }
        Some(removed)
    }

    pub const fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        Entry { map: self, key }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.chunks.iter_mut().flat_map(|chunk| {
            let chunk = Arc::make_mut(chunk);
            chunk.encoded = OnceLock::new();
            chunk.items.iter_mut().map(|(key, value)| (&*key, value))
        })
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.iter_mut().map(|(_, value)| value)
    }

    /// Keeps the entries `keep` returns true for. It sees each value mutably, as
    /// BTreeMap::retain does, so every chunk becomes this map's own.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        let mut at = 0;
        while at < self.chunks.len() {
            let items = writable(&mut self.chunks, at);
            let before = items.len();
            items.retain_mut(|(key, value)| keep(key, value));
            self.len -= before - items.len();
            if self.chunks[at].items.is_empty() {
                self.chunks.remove(at);
            } else {
                at += 1;
            }
        }
    }

    pub fn extend(&mut self, entries: impl IntoIterator<Item = (K, V)>) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }

    /// Builds a map from entries already in key order with no repeats, chunk by chunk.
    fn from_sorted(entries: Vec<(K, V)>) -> Self {
        let len = entries.len();
        let mut chunks = Vec::with_capacity(len.div_ceil(CHUNK));
        let mut entries = entries.into_iter().peekable();
        while entries.peek().is_some() {
            let items: Vec<_> = entries.by_ref().take(CHUNK).collect();
            chunks.push(Arc::new(Chunk::new(items)));
        }
        Self { chunks, len }
    }
}

/// A slot in a [`CowMap`], as `BTreeMap::entry` gives.
pub struct Entry<'a, K, V> {
    map: &'a mut CowMap<K, V>,
    key: K,
}

impl<'a, K: Ord + Clone, V: Clone> Entry<'a, K, V> {
    pub fn or_insert_with(self, make: impl FnOnce() -> V) -> &'a mut V {
        if !self.map.contains_key(&self.key) {
            self.map.insert(self.key.clone(), make());
        }
        self.map
            .get_mut(&self.key)
            .expect("an entry holds its key once inserted")
    }

    pub fn or_insert(self, value: V) -> &'a mut V {
        self.or_insert_with(|| value)
    }

    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}

impl<K: Ord + Borrow<Q>, V, Q: Ord + ?Sized> std::ops::Index<&Q> for CowMap<K, V> {
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("no entry for the key in the CowMap")
    }
}

impl<'a, K: Clone, V: Clone> IntoIterator for &'a mut CowMap<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = Box<dyn Iterator<Item = (&'a K, &'a mut V)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.chunks.iter_mut().flat_map(|chunk| {
            let chunk = Arc::make_mut(chunk);
            chunk.encoded = OnceLock::new();
            chunk.items.iter_mut().map(|(key, value)| (&*key, value))
        }))
    }
}

impl<K, V> Default for CowMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> Clone for CowMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for CowMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && (self
                .chunks
                .iter()
                .zip(&other.chunks)
                .all(|(left, right)| Arc::ptr_eq(left, right))
                && self.chunks.len() == other.chunks.len()
                || self.iter().eq(other.iter()))
    }
}

impl<K: Eq, V: Eq> Eq for CowMap<K, V> {}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for CowMap<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for CowMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(entries: I) -> Self {
        let mut entries: Vec<_> = entries.into_iter().collect();
        // Later duplicates win, as BTreeMap's collect keeps the last value for a key.
        entries.reverse();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries.dedup_by(|later, earlier| later.0 == earlier.0);
        Self::from_sorted(entries)
    }
}

impl<K: Clone, V: Clone> IntoIterator for CowMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::iter::FlatMap<
        std::vec::IntoIter<Arc<Chunk<(K, V)>>>,
        Vec<(K, V)>,
        fn(Arc<Chunk<(K, V)>>) -> Vec<(K, V)>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn items<T: Clone>(chunk: Arc<Chunk<T>>) -> Vec<T> {
            Arc::try_unwrap(chunk).map_or_else(|shared| shared.items.clone(), |own| own.items)
        }
        self.chunks.into_iter().flat_map(items as fn(_) -> _)
    }
}

impl<'a, K, V> IntoIterator for &'a CowMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Box<dyn DoubleEndedIterator<Item = (&'a K, &'a V)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// As a map, as BTreeMap serializes; the fold's `pairs` fields serialize it as a sequence.
impl<K: Serialize, V: Serialize> Serialize for CowMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if is_chunked() {
            serialize_chunks(&self.chunks, serializer)
        } else {
            serializer.collect_map(self.iter())
        }
    }
}

impl<'de, K, V> Deserialize<'de> for CowMap<K, V>
where
    K: serde::de::DeserializeOwned + Ord + Clone,
    V: serde::de::DeserializeOwned + Clone,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if is_chunked() {
            let chunks = deserialize_chunks(deserializer)?;
            let len = chunks.iter().map(|chunk| chunk.items.len()).sum();
            return Ok(Self { chunks, len });
        }
        std::collections::BTreeMap::<K, V>::deserialize(deserializer)
            .map(|map| Self::from_sorted(map.into_iter().collect()))
    }
}

/// A sorted set with copy-on-write chunks: a [`CowMap`] to `()`.
#[derive(Clone, PartialEq, Eq)]
pub struct CowSet<T>(CowMap<T, ()>);

impl<T> Default for CowSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> CowSet<T> {
    pub const fn new() -> Self {
        Self(CowMap::new())
    }

    pub const fn len(&self) -> usize {
        self.0.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + Clone {
        self.0.keys()
    }

    pub fn first(&self) -> Option<&T> {
        self.0.first_key_value().map(|(key, ())| key)
    }

    pub fn last(&self) -> Option<&T> {
        self.0.last_key_value().map(|(key, ())| key)
    }
}

impl<T: Ord> CowSet<T> {
    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.contains_key(value)
    }

    pub fn get<Q>(&self, value: &Q) -> Option<&T>
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.get_key_value(value).map(|(key, ())| key)
    }

    pub fn range<R: RangeBounds<T>>(&self, range: R) -> impl Iterator<Item = &T> + Clone
    where
        T: Clone,
    {
        self.0.range(range).map(|(key, ())| key)
    }
}

impl<T: Ord + Clone> CowSet<T> {
    pub fn insert(&mut self, value: T) -> bool {
        self.0.insert(value, ()).is_none()
    }

    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.remove(value).is_some()
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.0.retain(|key, ()| keep(key));
    }

    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            self.insert(value);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for CowSet<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_set().entries(self.iter()).finish()
    }
}

impl<T: Ord + Clone> FromIterator<T> for CowSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        Self(values.into_iter().map(|value| (value, ())).collect())
    }
}

impl<T: Clone> IntoIterator for CowSet<T> {
    type Item = T;
    type IntoIter = std::iter::Map<<CowMap<T, ()> as IntoIterator>::IntoIter, fn((T, ())) -> T>;

    fn into_iter(self) -> Self::IntoIter {
        fn key<T>((key, ()): (T, ())) -> T {
            key
        }
        self.0.into_iter().map(key as fn(_) -> _)
    }
}

impl<'a, T> IntoIterator for &'a CowSet<T> {
    type Item = &'a T;
    type IntoIter = Box<dyn DoubleEndedIterator<Item = &'a T> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<T: Serialize> Serialize for CowSet<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if is_chunked() {
            self.0.serialize(serializer)
        } else {
            serializer.collect_seq(self.iter())
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned + Ord + Clone> Deserialize<'de> for CowSet<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if is_chunked() {
            return CowMap::deserialize(deserializer).map(Self);
        }
        std::collections::BTreeSet::<T>::deserialize(deserializer).map(|set| {
            Self(CowMap::from_sorted(
                set.into_iter().map(|value| (value, ())).collect(),
            ))
        })
    }
}

impl<T: Ord> PartialOrd for CowSet<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Ord> Ord for CowSet<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.iter().cmp(other.iter())
    }
}

/// An append-only list with copy-on-write chunks of [`CHUNK`] items: the fold's entries.
pub struct CowVec<T> {
    chunks: Vec<Arc<Chunk<T>>>,
    len: usize,
}

impl<T> CowVec<T> {
    pub const fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + Clone {
        self.chunks.iter().flat_map(|chunk| chunk.items.iter())
    }

    /// The items from index `start` on.
    pub fn iter_from(&self, start: usize) -> impl DoubleEndedIterator<Item = &T> + Clone {
        let chunk = start / CHUNK;
        let index = start % CHUNK;
        self.chunks
            .get(chunk..)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .flat_map(move |(at, chunk)| {
                chunk.items[if at == 0 {
                    index.min(chunk.items.len())
                } else {
                    0
                }..]
                    .iter()
            })
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.chunks.get(index / CHUNK)?.items.get(index % CHUNK)
    }

    pub fn last(&self) -> Option<&T> {
        self.chunks.last()?.items.last()
    }

    /// The index of the first item `pred` is false for, in a list `pred` partitions.
    pub fn partition_point(&self, mut pred: impl FnMut(&T) -> bool) -> usize {
        let full = self
            .chunks
            .partition_point(|chunk| chunk.items.last().is_some_and(&mut pred));
        match self.chunks.get(full) {
            Some(chunk) => full * CHUNK + chunk.items.partition_point(pred),
            None => self.len,
        }
    }
}

impl<T: Clone> CowVec<T> {
    pub fn push(&mut self, item: T) {
        if self
            .chunks
            .last()
            .is_none_or(|chunk| chunk.items.len() >= CHUNK)
        {
            self.chunks
                .push(Arc::new(Chunk::new(Vec::with_capacity(CHUNK))));
        }
        let at = self.chunks.len() - 1;
        writable(&mut self.chunks, at).push(item);
        self.len += 1;
    }
}

impl<T> std::ops::Index<usize> for CowVec<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        self.get(index).expect("an index within the CowVec")
    }
}

impl<T: Clone> CowVec<T> {
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
}

impl<T> Default for CowVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Clone for CowVec<T> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<T: PartialEq> PartialEq for CowVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Eq> Eq for CowVec<T> {}

impl<T: fmt::Debug> fmt::Debug for CowVec<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Clone> FromIterator<T> for CowVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(items: I) -> Self {
        let mut list = Self::new();
        for item in items {
            list.push(item);
        }
        list
    }
}

impl<'a, T> IntoIterator for &'a CowVec<T> {
    type Item = &'a T;
    type IntoIter = Box<dyn DoubleEndedIterator<Item = &'a T> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<T: Serialize> Serialize for CowVec<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if is_chunked() {
            serialize_chunks(&self.chunks, serializer)
        } else {
            serializer.collect_seq(self.iter())
        }
    }
}

impl<'de, T: serde::de::DeserializeOwned + Clone> Deserialize<'de> for CowVec<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if is_chunked() {
            // Every chunk but the last is full, as push leaves them, so an index maps to its
            // chunk by division.
            let chunks = deserialize_chunks::<T, D>(deserializer)?;
            let full = chunks
                .split_last()
                .is_none_or(|(_, rest)| rest.iter().all(|chunk| chunk.items.len() == CHUNK));
            if !full {
                return Err(serde::de::Error::custom(
                    "a list chunk short of CHUNK before the last",
                ));
            }
            let len = chunks.iter().map(|chunk| chunk.items.len()).sum();
            return Ok(Self { chunks, len });
        }
        Vec::<T>::deserialize(deserializer).map(|items| items.into_iter().collect())
    }
}

thread_local! {
    /// Set while compact::snapshot or compact::restore runs on this thread: Cow collections
    /// then serialize as a sequence of chunk blobs, each one standalone compact encoding.
    static CHUNKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `work` with Cow collections (de)serializing chunk by chunk.
pub(crate) fn chunked<T>(work: impl FnOnce() -> T) -> T {
    let was = CHUNKED.replace(true);
    let out = work();
    CHUNKED.set(was);
    out
}

fn is_chunked() -> bool {
    CHUNKED.get()
}

/// A chunk's cached compact encoding, encoding it first if no checkpoint has since its last write.
fn blob<T: Serialize>(chunk: &Chunk<T>) -> Result<Arc<[u8]>, crate::compact::Error> {
    if let Some(encoded) = chunk.encoded.get() {
        return Ok(Arc::clone(encoded));
    }
    let plain = crate::compact::encode(&chunk.items)?;
    let encoded: Arc<[u8]> = nanocode_zstd::compress(&plain, PACK_LEVEL)
        .map_err(|error| crate::compact::Error::from(error.to_string()))?
        .into();
    let _ = chunk.encoded.set(Arc::clone(&encoded));
    Ok(encoded)
}

struct Blob(Arc<[u8]>);

impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

/// Serializes chunks as their blobs.
fn serialize_chunks<T: Serialize, S: Serializer>(
    chunks: &[Arc<Chunk<T>>],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let blobs = chunks
        .iter()
        .map(|chunk| blob(chunk).map(Blob))
        .collect::<Result<Vec<_>, _>>()
        .map_err(serde::ser::Error::custom)?;
    serializer.collect_seq(blobs)
}

struct Borrowed<'de>(&'de [u8]);

impl<'de> Deserialize<'de> for Borrowed<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Bytes;
        impl<'de> serde::de::Visitor<'de> for Bytes {
            type Value = Borrowed<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a chunk's compact encoding")
            }

            fn visit_borrowed_bytes<E: serde::de::Error>(
                self,
                bytes: &'de [u8],
            ) -> Result<Self::Value, E> {
                Ok(Borrowed(bytes))
            }
        }
        deserializer.deserialize_bytes(Bytes)
    }
}

/// Decodes chunk blobs into chunks that keep their blob as their cached encoding.
fn deserialize_chunks<'de, T: serde::de::DeserializeOwned, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Arc<Chunk<T>>>, D::Error> {
    Vec::<Borrowed<'de>>::deserialize(deserializer)?
        .into_iter()
        .map(|Borrowed(bytes)| {
            let mut decoder = nanocode_zstd::StreamingDecoder::default();
            decoder
                .push(bytes)
                .and_then(|()| decoder.finish())
                .map_err(serde::de::Error::custom)?;
            let plain = decoder.take_output();
            let items: Vec<T> =
                crate::compact::decode_owned(&plain).map_err(serde::de::Error::custom)?;
            let chunk = Chunk::new(items);
            let _ = chunk.encoded.set(Arc::from(bytes));
            Ok(Arc::new(chunk))
        })
        .filter(|chunk| !matches!(chunk, Ok(chunk) if chunk.items.is_empty()))
        .collect()
}

/// How a map field marked `with = "crate::pairs"` (de)serializes: as a sequence of its pairs, or
/// for a Cow map inside a compact snapshot, as its chunk blobs.
pub trait Pairs<K, V>: Sized {
    fn serialize_pairs<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
    fn deserialize_pairs<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
}

impl<K, V> Pairs<K, V> for std::collections::BTreeMap<K, V>
where
    K: Serialize + serde::de::DeserializeOwned + Ord,
    V: Serialize + serde::de::DeserializeOwned,
{
    fn serialize_pairs<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self)
    }

    fn deserialize_pairs<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<(K, V)>::deserialize(deserializer).map(|pairs| pairs.into_iter().collect())
    }
}

impl<K, V> Pairs<K, V> for CowMap<K, V>
where
    K: Serialize + serde::de::DeserializeOwned + Ord + Clone,
    V: Serialize + serde::de::DeserializeOwned + Clone,
{
    fn serialize_pairs<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if is_chunked() {
            serialize_chunks(&self.chunks, serializer)
        } else {
            serializer.collect_seq(self.iter())
        }
    }

    fn deserialize_pairs<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if is_chunked() {
            let chunks = deserialize_chunks(deserializer)?;
            let len = chunks.iter().map(|chunk| chunk.items.len()).sum();
            Ok(Self { chunks, len })
        } else {
            Vec::<(K, V)>::deserialize(deserializer).map(|pairs| pairs.into_iter().collect())
        }
    }
}
