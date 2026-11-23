use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, HashMap},
    hash::{BuildHasher, BuildHasherDefault, DefaultHasher},
    ops::Bound,
};

use nanocode_durability_core::{EncodedPayload, StoreError};
use serde_json::value::RawValue;

use crate::{EffectClass, Outcome, Record, Status};

#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub payload: String,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub at: u64,
    pub event: Box<RawValue>,
    pub seq: u64,
    pub turn: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OpenCall {
    pub effect: EffectClass,
    pub input: Box<RawValue>,
    pub tool: String,
    pub turn: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Segment {
    pub first: u64,
    pub last: u64,
    pub row: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Retry {
    pub attempts: u64,
    pub error: String,
    pub kind: String,
    pub retry_at: u64,
}

#[derive(Clone, Debug)]
pub enum Phase {
    Finished {
        last: u64,
        outcome: Outcome,
        result: Box<RawValue>,
    },
    Queued {
        retry: Option<Retry>,
    },
    Running {
        cancelling: bool,
        note: Option<String>,
        retry: Option<Retry>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Section {
    pub(crate) row: u64,
    pub(crate) rows: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Entry {
    pub(crate) checkpoint: bool,
    pub(crate) full: u64,
    pub(crate) legacy: bool,
    pub(crate) sections: Vec<Section>,
    pub(crate) written: u64,
}

#[derive(Clone, Debug, Default)]
struct Delta {
    cells: Vec<Record>,
    checkpoint: bool,
    keys: BTreeSet<String>,
}

impl Delta {
    fn note(&mut self, record: &Record) {
        match record {
            Record::Cell { .. } | Record::Forget { .. } => self.cells.push(record.clone()),
            Record::Checkpoint { .. } | Record::Patched { .. } => self.checkpoint = true,
            Record::Stored { key, .. } if !self.keys.contains(key) => {
                self.keys.insert(key.clone());
            }
            _ => {}
        }
    }

    fn is_empty(&self) -> bool {
        self.cells.is_empty() && !self.checkpoint && self.keys.is_empty()
    }
}

#[derive(Clone, Debug)]
enum Owner {
    Cold { entry: Entry, pending: Vec<Record> },
    Waiting { pending: Vec<Record> },
    Warm { delta: Delta, entry: Entry },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Page {
    Cold(Section),
    Loaded(Section),
}

impl Page {
    pub(crate) const fn section(self) -> Section {
        match self {
            Self::Cold(section) | Self::Loaded(section) => section,
        }
    }
}

pub(crate) struct Plan {
    pub(crate) groups: BTreeMap<u64, (u64, u64)>,
    pub(crate) kept: BTreeMap<String, Page>,
    pub(crate) pages: Vec<(String, Vec<Record>)>,
    pub(crate) superseded: Vec<std::ops::Range<u64>>,
}

#[derive(Clone, Debug)]
struct Links {
    chunks: Vec<String>,
    references: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Retired {
    pub digest: u128,
    pub first: u64,
    pub hand: Option<Box<str>>,
    pub last: u64,
    pub outcome: Outcome,
}

pub const RETIRED_KEY: &str = "$turnlog_retired";

#[must_use]
pub fn tag(text: &str) -> u128 {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    text.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(PRIME)
    })
}

const RETIRED_ROW: usize = 97;

fn text(row: &[u8], from: usize, to: usize) -> Result<&str, StoreError> {
    std::str::from_utf8(&row[from..to]).map_err(invalid)
}

#[derive(serde::Deserialize, serde::Serialize)]
struct RetiredIndex {
    hands: BTreeMap<String, Box<str>>,
    rows: String,
}

const fn outcome_code(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Cancelled => "x",
        Outcome::Completed => "c",
        Outcome::Failed => "f",
    }
}

fn outcome_of(code: &str) -> Option<Outcome> {
    match code {
        "x" => Some(Outcome::Cancelled),
        "c" => Some(Outcome::Completed),
        "f" => Some(Outcome::Failed),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Recovered {
    pub at: Option<u64>,
    pub attempt: u64,
}

#[derive(Clone, Debug)]
pub struct Turn {
    pub digest: String,
    pub edits: BTreeMap<String, String>,
    pub first: u64,
    pub hand: Option<String>,
    pub id: String,
    pub metadata: Box<RawValue>,
    pub phase: Phase,
    pub prompt: Box<RawValue>,
    pub recovered: Option<Recovered>,
}

#[derive(Clone, Debug, Default)]
pub struct Fold {
    calls: BTreeMap<String, OpenCall>,
    cells: BTreeMap<(String, u64), Vec<Box<RawValue>>>,
    checkpoints: BTreeMap<String, Checkpoint>,
    dead: BTreeMap<String, String>,
    ends: u64,
    events: Vec<Event>,
    filters: BTreeMap<u64, Box<[u64]>>,
    forgets: BTreeSet<String>,
    groups: BTreeMap<u64, (u64, u64)>,
    hot_bytes: usize,
    hosts: BTreeMap<String, Box<RawValue>>,
    index: BTreeMap<String, usize>,
    kept: Option<(u64, u64)>,
    last_event: u64,
    links: HashMap<String, Links>,
    marks: BTreeMap<String, Vec<String>>,
    models: BTreeMap<String, Vec<Box<RawValue>>>,
    owners: BTreeMap<String, Owner>,
    pages: BTreeMap<String, Page>,
    read: BTreeSet<String>,
    root: Option<Page>,
    segment_row: u64,
    segments: Vec<Segment>,
    seq: u64,
    stored: BTreeMap<String, String>,
    retired: BTreeMap<u128, Retired>,
    turns: Vec<Turn>,
}

#[derive(serde::Deserialize)]
struct CellHeader {
    origin_call_id: String,
}

#[derive(serde::Deserialize)]
struct CellOrigin {
    header: Option<CellHeader>,
}

fn invalid(detail: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(format!("turn log record is invalid: {detail}"))
}

fn owner_of(record: &Record) -> Option<&str> {
    match record {
        Record::Cell { session, .. } | Record::Forget { session, .. } => Some(session),
        Record::Checkpoint { state, .. } | Record::Patched { state, .. } => Some(state),
        Record::Stored { key, .. } => key.contains('\n').then_some(key.as_str()),
        _ => None,
    }
}

fn legacy_owner_of(record: &Record) -> Option<&str> {
    match record {
        Record::Stored { key, .. } => key.split_once('\n').map(|(state, content)| {
            if content.starts_with("b3:") {
                &key[..=state.len()]
            } else {
                state
            }
        }),
        record => owner_of(record),
    }
}

pub(crate) fn legacy_name(key: &str) -> Option<String> {
    let record = Record::Stored {
        key: key.to_owned(),
        value: String::new(),
    };
    legacy_owner_of(&record)
        .filter(|owner| *owner != key)
        .map(str::to_owned)
}

fn key_hash(owner: &str) -> u64 {
    BuildHasherDefault::<DefaultHasher>::default().hash_one(owner)
}

fn unheld(state: &str, base: u64) -> StoreError {
    invalid(format!(
        "checkpoint {state} patches revision {base}, which the log does not hold"
    ))
}

fn wait(pending: &mut Vec<Record>, record: Record) -> Result<(), StoreError> {
    let record = match record {
        Record::Patched {
            base,
            edits,
            revision,
            state,
        } => {
            let payload = pending
                .iter()
                .rev()
                .find_map(|waiting| match waiting {
                    Record::Checkpoint {
                        payload, revision, ..
                    } => Some((payload, *revision)),
                    _ => None,
                })
                .filter(|(_, held)| *held == base)
                .map(|(payload, _)| crate::patch::apply(payload, &edits))
                .ok_or_else(|| unheld(&state, base))??;
            Record::Checkpoint {
                payload,
                revision,
                state,
            }
        }
        record => record,
    };
    if matches!(record, Record::Checkpoint { .. }) {
        pending.retain(|waiting| !matches!(waiting, Record::Checkpoint { .. }));
    }
    pending.push(record);
    Ok(())
}

impl Fold {
    pub fn load(&mut self, seq: u64, body: &str) -> Result<(), StoreError> {
        if seq <= self.seq {
            return Err(invalid(format!(
                "row {seq} does not follow row {}",
                self.seq
            )));
        }
        crate::record::each(body, |record| {
            let forget = match &record {
                Record::Forget { session, .. } => Some(session.clone()),
                _ => None,
            };
            if let Some(pending) = self.defer(&record) {
                wait(pending, record)?;
                if let Some(session) = forget {
                    self.forgets.insert(session);
                }
                return Ok(());
            }
            self.take(record, Some(seq))
        })?;
        self.seq = seq;
        Ok(())
    }

    fn take(&mut self, record: Record, row: Option<u64>) -> Result<(), StoreError> {
        match record {
            Record::Stored { key, value } => {
                self.dead.remove(&key);
                self.stored.insert(key, value);
                Ok(())
            }
            record => self.fold_in(&record, row),
        }
    }

    fn page_of(&self, owner: &str) -> Option<(&String, &Page)> {
        self.pages
            .range::<str, _>((Bound::Unbounded, Bound::Included(owner)))
            .next_back()
    }

    fn cold_page(&self, owner: &str) -> Option<(&String, Section)> {
        match self.page_of(owner) {
            Some((first, Page::Cold(section)))
                if self
                    .filters
                    .get(&section.row)
                    .is_none_or(|filter| filter.binary_search(&key_hash(owner)).is_ok()) =>
            {
                Some((first, *section))
            }
            _ => None,
        }
    }

    fn page_end(&self, first: &str) -> Option<&String> {
        self.pages
            .range::<str, _>((Bound::Excluded(first), Bound::Unbounded))
            .next()
            .map(|(next, _)| next)
    }

    fn defer(&mut self, record: &Record) -> Option<&mut Vec<Record>> {
        let owner = owner_of(record)?;
        if !self.owners.contains_key(owner) {
            let state =
                if matches!(self.root, Some(Page::Cold(_))) || self.cold_page(owner).is_some() {
                    Owner::Waiting {
                        pending: Vec::new(),
                    }
                } else {
                    Owner::Warm {
                        delta: Delta::default(),
                        entry: Entry::default(),
                    }
                };
            self.owners.insert(owner.to_owned(), state);
        }
        match self.owners.get_mut(owner) {
            Some(Owner::Cold { pending, .. } | Owner::Waiting { pending }) => Some(pending),
            Some(Owner::Warm { delta, .. }) => {
                delta.note(record);
                None
            }
            None => None,
        }
    }

    pub(crate) fn unpaged(&self, owner: &str) -> Option<(String, Section)> {
        self.cold_page(owner)
            .map(|(first, section)| (first.clone(), section))
    }

    pub(crate) fn group_pages(&self, records: &[Record]) -> Vec<(String, Section)> {
        let mut pages = BTreeMap::new();
        for owner in records.iter().filter_map(owner_of) {
            if matches!(self.owners.get(owner), None | Some(Owner::Waiting { .. }))
                && let Some((first, section)) = self.cold_page(owner)
            {
                pages.insert(first, section);
            }
        }
        pages
            .into_iter()
            .map(|(first, section)| (first.clone(), section))
            .collect()
    }

    pub(crate) const fn cold_root(&self) -> Option<Section> {
        match self.root {
            Some(Page::Cold(section)) => Some(section),
            Some(Page::Loaded(_)) | None => None,
        }
    }

    pub(crate) fn rooted(&mut self, records: Vec<Record>) -> Result<(), StoreError> {
        let Some(Page::Cold(section)) = self.root else {
            return Ok(());
        };
        for record in records {
            match record {
                Record::Groups { groups } => {
                    self.groups = groups
                        .into_iter()
                        .map(|(row, rows, count)| (row, (rows, count)))
                        .collect();
                }
                Record::Index { first, row, rows } => {
                    self.pages.insert(first, Page::Cold(Section { row, rows }));
                }
                _ => {
                    return Err(invalid(
                        "an index root holds a record that is not an index page or a group",
                    ));
                }
            }
        }
        self.root = Some(Page::Loaded(section));
        Ok(())
    }

    pub(crate) fn indexed(
        &self,
        pages: &BTreeMap<String, Page>,
        groups: &BTreeMap<u64, (u64, u64)>,
    ) -> Option<Section> {
        let Some(Page::Loaded(section)) = self.root else {
            return None;
        };
        (self.groups == *groups
            && self.pages.len() == pages.len()
            && self
                .pages
                .iter()
                .zip(pages)
                .all(|((first, page), (other, kept))| {
                    first == other && page.section() == kept.section()
                }))
        .then_some(section)
    }

    pub(crate) fn root_section(&self) -> Option<Section> {
        self.root.map(Page::section)
    }

    pub(crate) fn cold_pages(&self) -> Vec<(String, Section)> {
        self.pages
            .iter()
            .filter_map(|(first, page)| match page {
                Page::Cold(section) => Some((first.clone(), *section)),
                Page::Loaded(_) => None,
            })
            .collect()
    }

    pub(crate) fn unlisted(&self) -> Vec<(String, Section)> {
        let mut pages = BTreeMap::new();
        for (owner, state) in &self.owners {
            if matches!(state, Owner::Warm { delta, .. } if !delta.is_empty())
                && let Some((first, Page::Cold(section))) = self.page_of(owner)
            {
                pages.insert(first.clone(), *section);
            }
        }
        pages.into_iter().collect()
    }

    pub(crate) fn paged(&mut self, first: &str, records: Vec<Record>) -> Result<(), StoreError> {
        let Some(page) = self.pages.get_mut(first) else {
            return Ok(());
        };
        let Page::Cold(section) = *page else {
            return Ok(());
        };
        *page = Page::Loaded(section);
        self.filters.remove(&section.row);
        let end = self.page_end(first).cloned();
        for record in records {
            let Record::Owner {
                checkpoint,
                full,
                legacy,
                owner,
                sections,
                written,
            } = record
            else {
                return Err(invalid("an index page holds a record that is not an owner"));
            };
            if owner.as_str() < first || end.as_ref().is_some_and(|end| owner >= *end) {
                return Err(invalid(format!(
                    "index page {first} holds owner {owner} outside its range"
                )));
            }
            let entry = Entry {
                checkpoint,
                full,
                legacy,
                sections: sections
                    .into_iter()
                    .map(|(row, rows)| Section { row, rows })
                    .collect(),
                written,
            };
            let pending = match self.owners.remove(&owner) {
                None => Vec::new(),
                Some(Owner::Waiting { pending }) => pending,
                Some(_) => {
                    return Err(invalid(format!(
                        "owner {owner} was loaded before its index page"
                    )));
                }
            };
            self.owners.insert(owner, Owner::Cold { entry, pending });
        }
        let fresh = self
            .owners
            .range::<str, _>((
                Bound::Included(first),
                end.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            ))
            .filter(|(_, state)| matches!(state, Owner::Waiting { .. }))
            .map(|(owner, _)| owner.clone())
            .collect::<Vec<_>>();
        for owner in fresh {
            if let Some(Owner::Waiting { pending }) = self.owners.remove(&owner) {
                for record in &pending {
                    self.apply(record, None)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn cold(&self, owner: &str) -> Option<&[Section]> {
        match self.owners.get(owner) {
            Some(Owner::Cold { entry, .. }) => Some(&entry.sections),
            _ => None,
        }
    }

    pub(crate) fn warm(
        &mut self,
        owner: &str,
        groups: Vec<(Section, Vec<Record>)>,
    ) -> Result<(), StoreError> {
        let mut pendings = Vec::new();
        let Some(Owner::Cold { entry, pending }) = self.owners.remove(owner) else {
            return Ok(());
        };
        pendings.push(pending);
        let legacy = entry.legacy;
        self.owners.insert(
            owner.to_owned(),
            Owner::Warm {
                delta: Delta {
                    checkpoint: legacy,
                    ..Delta::default()
                },
                entry,
            },
        );
        let mut joined: BTreeMap<String, Section> = BTreeMap::new();
        for (section, records) in groups {
            for record in records {
                if legacy {
                    if legacy_owner_of(&record) != Some(owner) {
                        continue;
                    }
                    if let Record::Stored { key, .. } = &record
                        && key != owner
                    {
                        if self.owners.contains_key(key) {
                            continue;
                        }
                        self.owners.insert(
                            key.clone(),
                            Owner::Warm {
                                delta: Delta {
                                    keys: BTreeSet::from([key.clone()]),
                                    ..Delta::default()
                                },
                                entry: Entry::default(),
                            },
                        );
                    }
                    self.take(record, None)?;
                    continue;
                }
                let Some(other) = owner_of(&record) else {
                    continue;
                };
                let take = if other == owner {
                    true
                } else if let Some(at) = joined.get(other) {
                    *at == section
                } else if matches!(self.owners.get(other), Some(Owner::Cold { entry, .. }) if entry.sections.as_slice() == [section] && !entry.legacy)
                {
                    if let Some(Owner::Cold { entry, pending }) = self.owners.remove(other) {
                        pendings.push(pending);
                        self.owners.insert(
                            other.to_owned(),
                            Owner::Warm {
                                delta: Delta::default(),
                                entry,
                            },
                        );
                    }
                    joined.insert(other.to_owned(), section);
                    true
                } else {
                    false
                };
                if take {
                    self.take(record, None)?;
                }
            }
        }
        for pending in pendings {
            for record in &pending {
                self.apply(record, None)?;
            }
        }
        Ok(())
    }

    pub(crate) fn touch(&mut self, state: &str) {
        let known = self
            .owners
            .range::<str, _>((Bound::Included(state), Bound::Unbounded))
            .next()
            .is_some_and(|(owner, _)| {
                owner
                    .strip_prefix(state)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('\n'))
            });
        if known && !self.read.contains(state) {
            self.read.insert(state.to_owned());
        }
    }

    pub(crate) fn evict(&mut self) {
        let read = std::mem::take(&mut self.read);
        let evicted = self
            .owners
            .iter()
            .filter(|(owner, state)| {
                matches!(state, Owner::Warm { delta, entry } if delta.is_empty() && !entry.legacy)
                    && !read.contains(
                        owner
                            .split_once('\n')
                            .map_or(owner.as_str(), |(state, _)| state),
                    )
            })
            .map(|(owner, _)| owner.clone())
            .collect::<Vec<_>>();
        for owner in evicted {
            let Some(Owner::Warm { entry, .. }) = self.owners.remove(&owner) else {
                continue;
            };
            if owner.contains('\n') {
                self.stored.remove(&owner);
                self.dead.remove(&owner);
            } else {
                if self.checkpoints.remove(&owner).is_some() {
                    self.marks.remove(&owner);
                }
                let cells = self
                    .cells
                    .range((owner.clone(), 0)..=(owner.clone(), u64::MAX))
                    .map(|(cell, _)| cell.clone())
                    .collect::<Vec<_>>();
                for cell in cells {
                    self.cells.remove(&cell);
                }
            }
            self.owners.insert(
                owner,
                Owner::Cold {
                    entry,
                    pending: Vec::new(),
                },
            );
        }
        let idle = self
            .pages
            .iter()
            .filter_map(|(first, page)| match page {
                Page::Loaded(section) => Some((first.clone(), *section)),
                Page::Cold(_) => None,
            })
            .filter(|(first, _)| {
                self.page_owners(first).all(|(_, state)| {
                    matches!(state, Owner::Cold { entry, pending } if pending.is_empty() && !entry.legacy && !entry.sections.is_empty())
                })
            })
            .collect::<Vec<_>>();
        for (first, section) in idle {
            let owners = self
                .page_owners(&first)
                .map(|(owner, _)| owner.clone())
                .collect::<Vec<_>>();
            let mut filter = owners
                .iter()
                .map(|owner| key_hash(owner))
                .collect::<Vec<_>>();
            filter.sort_unstable();
            for owner in owners {
                self.owners.remove(&owner);
            }
            self.filters.insert(section.row, filter.into_boxed_slice());
            self.pages.insert(first, Page::Cold(section));
        }
    }

    fn page_owners(&self, first: &str) -> impl Iterator<Item = (&String, &Owner)> {
        let end = self.page_end(first);
        self.owners.range::<str, _>((
            Bound::Included(first),
            end.map_or(Bound::Unbounded, |end| Bound::Excluded(end.as_str())),
        ))
    }

    pub(crate) fn is_legacy(&self, owner: &str) -> bool {
        matches!(self.owners.get(owner), Some(Owner::Cold { entry, .. }) if entry.legacy)
    }

    pub(crate) fn cold_keys(&self, state: &str) -> Vec<String> {
        let (low, high) = (format!("{state}\n"), format!("{state}\u{b}"));
        self.owners
            .range::<str, _>((
                Bound::Included(low.as_str()),
                Bound::Excluded(high.as_str()),
            ))
            .filter(|(_, owner)| !matches!(owner, Owner::Warm { .. }))
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn key_pages(&self, state: &str) -> Vec<(String, Section)> {
        let (low, high) = (format!("{state}\n"), format!("{state}\u{b}"));
        self.page_of(&low)
            .into_iter()
            .chain(self.pages.range::<str, _>((
                Bound::Excluded(low.as_str()),
                Bound::Excluded(high.as_str()),
            )))
            .filter_map(|(first, page)| match page {
                Page::Cold(section) => Some((first.clone(), *section)),
                Page::Loaded(_) => None,
            })
            .collect()
    }

    fn complete(&self, state: &str) -> bool {
        !matches!(
            self.owners.get(state),
            Some(Owner::Cold { .. } | Owner::Waiting { .. })
        ) && self.cold_page(state).is_none()
            && self.key_pages(state).is_empty()
            && self.cold_keys(state).is_empty()
    }

    pub(crate) fn waiting(&self) -> Vec<String> {
        self.owners
            .iter()
            .filter(|(_, state)| matches!(state, Owner::Waiting { .. }))
            .map(|(owner, _)| owner.clone())
            .collect()
    }

    pub(crate) fn forgetting(&self) -> Vec<String> {
        self.owners
            .iter()
            .filter(|(_, state)| {
                matches!(state, Owner::Cold { pending, .. } | Owner::Waiting { pending } if pending.iter().any(|record| matches!(record, Record::Forget { .. })))
            })
            .map(|(owner, _)| owner.clone())
            .collect()
    }

    pub(crate) fn take_forgets(&mut self) -> BTreeSet<String> {
        std::mem::take(&mut self.forgets)
    }

    pub(crate) fn writes(&self) -> Vec<&str> {
        self.owners
            .iter()
            .filter(|(_, state)| match state {
                Owner::Cold { pending, .. } | Owner::Waiting { pending } => !pending.is_empty(),
                Owner::Warm { delta, .. } => !delta.is_empty(),
            })
            .map(|(owner, _)| owner.as_str())
            .collect()
    }

    pub(crate) fn entry(&self, owner: &str) -> Option<&Entry> {
        match self.owners.get(owner) {
            Some(Owner::Cold { entry, .. } | Owner::Warm { entry, .. }) => Some(entry),
            Some(Owner::Waiting { .. }) | None => None,
        }
    }

    pub(crate) fn loaded(&self, owner: &str) -> bool {
        matches!(self.owners.get(owner), Some(Owner::Warm { .. }))
    }

    pub(crate) fn plan(
        &self,
        written: &BTreeMap<String, Option<Entry>>,
        groups: &[(Section, u64)],
    ) -> Result<Plan, StoreError> {
        if let Some(section) = self.cold_root() {
            return Err(invalid(format!(
                "a snapshot rewrites the index before its root at row {} is loaded",
                section.row
            )));
        }
        let mut refs = self.groups.clone();
        for (owner, entry) in written {
            let Some(old) = self.entry(owner) else {
                continue;
            };
            if entry
                .as_ref()
                .is_some_and(|entry| entry.sections.starts_with(&old.sections))
            {
                continue;
            }
            for section in &old.sections {
                let Some((_, count)) = refs.get_mut(&section.row) else {
                    return Err(invalid(format!(
                        "owner {owner} holds section {} that no group counts",
                        section.row
                    )));
                };
                *count = count.checked_sub(1).ok_or_else(|| {
                    invalid(format!("section {} is counted below zero", section.row))
                })?;
            }
        }
        for (section, count) in groups {
            refs.insert(section.row, (section.rows, *count));
        }
        let mut superseded = Vec::new();
        refs.retain(|row, (rows, count)| {
            if *count == 0 {
                superseded.push(*row..*row + *rows);
            }
            *count > 0
        });
        let touched = if self.pages.is_empty() {
            if self.owners.is_empty() && written.is_empty() {
                BTreeSet::new()
            } else {
                BTreeSet::from([String::new()])
            }
        } else {
            written
                .iter()
                .filter(|(owner, entry)| entry.as_ref() != self.entry(owner))
                .filter_map(|(owner, _)| self.page_of(owner).map(|(first, _)| first.clone()))
                .collect()
        };
        let mut kept = self.pages.clone();
        let mut pages = Vec::new();
        for first in touched {
            let end = self.page_end(&first).cloned();
            match kept.remove(&first) {
                Some(Page::Cold(_)) => {
                    return Err(invalid(format!(
                        "a snapshot rewrites index page {first} before it is loaded"
                    )));
                }
                Some(Page::Loaded(section)) => {
                    superseded.push(section.row..section.row + section.rows);
                }
                None => {}
            }
            let mut records = Vec::new();
            for (owner, state) in self.owners.range::<str, _>((
                Bound::Included(first.as_str()),
                end.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            )) {
                let entry = match (written.get(owner), state) {
                    (Some(entry), _) => entry.as_ref(),
                    (None, Owner::Cold { entry, .. } | Owner::Warm { entry, .. }) => Some(entry),
                    (None, Owner::Waiting { .. }) => {
                        return Err(invalid(format!(
                            "owner {owner} waits for index page {first}, which a snapshot rewrites"
                        )));
                    }
                };
                let Some(entry) = entry.filter(|entry| !entry.sections.is_empty()) else {
                    continue;
                };
                records.push(Record::Owner {
                    checkpoint: entry.checkpoint,
                    full: entry.full,
                    legacy: entry.legacy,
                    owner: owner.clone(),
                    sections: entry
                        .sections
                        .iter()
                        .map(|section| (section.row, section.rows))
                        .collect(),
                    written: entry.written,
                });
            }
            pages.push((first, records));
        }
        superseded.sort_by_key(|range| range.start);
        Ok(Plan {
            groups: refs,
            kept,
            pages,
            superseded,
        })
    }

    pub(crate) fn sectioned(
        &mut self,
        written: BTreeMap<String, Option<Entry>>,
        pages: BTreeMap<String, Page>,
        groups: BTreeMap<u64, (u64, u64)>,
        root: Option<Section>,
    ) {
        for (owner, entry) in written {
            let state = match (self.owners.remove(&owner), entry) {
                (Some(Owner::Warm { .. }), Some(entry)) => Owner::Warm {
                    delta: Delta::default(),
                    entry,
                },
                (Some(Owner::Cold { .. } | Owner::Waiting { .. }), Some(entry)) => Owner::Cold {
                    entry,
                    pending: Vec::new(),
                },
                (_, None) | (None, _) => continue,
            };
            self.owners.insert(owner, state);
        }
        self.pages = pages;
        let cold = self
            .pages
            .values()
            .filter_map(|page| match page {
                Page::Cold(section) => Some(section.row),
                Page::Loaded(_) => None,
            })
            .collect::<BTreeSet<_>>();
        self.filters.retain(|row, _| cold.contains(row));
        self.groups = groups;
        self.root = root.map(Page::Loaded);
    }

    pub(crate) fn delta_records(
        &self,
        owner: &str,
        mut take: impl FnMut(Record) -> Result<(), StoreError>,
    ) -> Result<bool, StoreError> {
        match self.owners.get(owner) {
            Some(Owner::Cold { pending, .. } | Owner::Waiting { pending }) => {
                let mut last = BTreeMap::new();
                for (position, record) in pending.iter().enumerate() {
                    if let Record::Stored { key, .. } = record {
                        last.insert(key.as_str(), position);
                    }
                }
                let mut checkpoint = false;
                for (position, record) in pending.iter().enumerate() {
                    match record {
                        Record::Stored { key, .. } if last.get(key.as_str()) != Some(&position) => {
                        }
                        record => {
                            checkpoint |= matches!(record, Record::Checkpoint { .. });
                            take(record.clone())?;
                        }
                    }
                }
                Ok(checkpoint)
            }
            Some(Owner::Warm { delta, .. }) => {
                for key in &delta.keys {
                    if let Some(value) = self.stored(key) {
                        take(Record::Stored {
                            key: key.clone(),
                            value: value.to_owned(),
                        })?;
                    }
                }
                let checkpoint = self.checkpoints.get(owner).filter(|_| delta.checkpoint);
                if let Some(checkpoint) = checkpoint {
                    take(Record::Checkpoint {
                        payload: checkpoint.payload.clone(),
                        revision: checkpoint.revision,
                        state: owner.to_owned(),
                    })?;
                }
                for record in &delta.cells {
                    take(record.clone())?;
                }
                Ok(checkpoint.is_some())
            }
            None => Ok(false),
        }
    }

    pub(crate) fn owner_records(
        &self,
        owner: &str,
        mut take: impl FnMut(Record) -> Result<(), StoreError>,
    ) -> Result<bool, StoreError> {
        if owner.contains('\n') {
            if let Some(value) = self.stored(owner) {
                take(Record::Stored {
                    key: owner.to_owned(),
                    value: value.to_owned(),
                })?;
            }
            return Ok(false);
        }
        let checkpoint = self.checkpoints.get(owner);
        if let Some(checkpoint) = checkpoint {
            take(Record::Checkpoint {
                payload: checkpoint.payload.clone(),
                revision: checkpoint.revision,
                state: owner.to_owned(),
            })?;
        }
        for ((_, cell), updates) in self
            .cells
            .range((owner.to_owned(), 0)..=(owner.to_owned(), u64::MAX))
        {
            for (position, update) in updates.iter().enumerate() {
                take(Record::Cell {
                    cell: *cell,
                    fresh: position == 0,
                    session: owner.to_owned(),
                    update: update.clone(),
                })?;
            }
        }
        Ok(checkpoint.is_some())
    }

    #[must_use]
    pub fn states(&self) -> Vec<&str> {
        let mut states = self
            .checkpoints
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for (owner, state) in &self.owners {
            let listed = match state {
                Owner::Cold { entry, pending } => {
                    entry.checkpoint
                        || pending
                            .iter()
                            .any(|record| matches!(record, Record::Checkpoint { .. }))
                }
                Owner::Waiting { pending } => pending
                    .iter()
                    .any(|record| matches!(record, Record::Checkpoint { .. })),
                Owner::Warm { .. } => false,
            };
            if listed {
                states.insert(owner.as_str());
            }
        }
        states.into_iter().collect()
    }

    pub(crate) const fn committed(&mut self, seq: u64) {
        self.seq = seq;
    }

    pub(crate) fn recovered_at(&mut self, turn: &str, seq: u64) {
        if let Some(turn) = self.turn_mut(turn)
            && let Some(recovered) = &mut turn.recovered
        {
            recovered.at = Some(seq);
        }
    }

    pub(crate) const fn reopen(&mut self, gap: u64) {
        self.last_event = self.last_event.saturating_add(gap);
    }

    pub(crate) const fn next_event(&mut self) -> u64 {
        self.last_event += 1;
        self.last_event
    }

    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    #[must_use]
    pub const fn last_event(&self) -> u64 {
        self.last_event
    }

    #[must_use]
    pub fn turn(&self, id: &str) -> Option<&Turn> {
        self.index.get(id).map(|&position| &self.turns[position])
    }

    #[must_use]
    pub fn retired(&self, id: &str) -> Option<&Retired> {
        self.retired.get(&tag(id))
    }

    #[must_use]
    pub fn retired_turns(&self) -> usize {
        self.retired.len()
    }

    fn retired_value(&self) -> Result<Box<RawValue>, StoreError> {
        use std::fmt::Write as _;
        let mut rows = String::with_capacity(self.retired.len() * RETIRED_ROW);
        let mut hands = BTreeMap::new();
        for (id, entry) in &self.retired {
            write!(
                rows,
                "{id:032x}{:032x}{:016x}{:016x}{}",
                entry.digest,
                entry.first,
                entry.last,
                outcome_code(entry.outcome)
            )
            .map_err(invalid)?;
            if let Some(hand) = &entry.hand {
                hands.insert(format!("{id:032x}"), hand.clone());
            }
        }
        serde_json::value::to_raw_value(&RetiredIndex { hands, rows }).map_err(invalid)
    }

    fn retired_from(&mut self, value: &RawValue) -> Result<(), StoreError> {
        let index: RetiredIndex = serde_json::from_str(value.get()).map_err(invalid)?;
        let rows = index.rows.as_bytes();
        if !index.rows.is_ascii() || rows.len() % RETIRED_ROW != 0 {
            return Err(invalid("retired index rows are malformed"));
        }

        let mut entries = Vec::with_capacity(rows.len() / RETIRED_ROW);
        for row in rows.chunks_exact(RETIRED_ROW) {
            let id = u128::from_str_radix(text(row, 0, 32)?, 16).map_err(invalid)?;
            let digest = u128::from_str_radix(text(row, 32, 64)?, 16).map_err(invalid)?;
            let first = u64::from_str_radix(text(row, 64, 80)?, 16).map_err(invalid)?;
            let last = u64::from_str_radix(text(row, 80, 96)?, 16).map_err(invalid)?;
            let code = text(row, 96, 97)?;
            let outcome =
                outcome_of(code).ok_or_else(|| invalid(format!("retired turn outcome {code}")))?;
            let hand = index.hands.get(text(row, 0, 32)?).cloned();
            entries.push((
                id,
                Retired {
                    digest,
                    first,
                    hand,
                    last,
                    outcome,
                },
            ));
        }
        if self.retired.is_empty() {
            self.retired = entries.into_iter().collect();
        } else {
            self.retired.extend(entries);
        }
        Ok(())
    }

    fn turn_mut(&mut self, id: &str) -> Option<&mut Turn> {
        self.index
            .get(id)
            .copied()
            .map(|position| &mut self.turns[position])
    }

    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    #[must_use]
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    #[must_use]
    pub const fn ends(&self) -> u64 {
        self.ends
    }

    #[must_use]
    pub const fn hot_bytes(&self) -> usize {
        self.hot_bytes
    }

    #[must_use]
    pub const fn segment_row(&self) -> u64 {
        self.segment_row
    }

    pub(crate) const fn start_segment(&mut self, row: u64) {
        self.segment_row = row;
    }

    #[must_use]
    pub fn unsealed(&self) -> Option<Segment> {
        self.kept.map(|(first, last)| Segment {
            first,
            last,
            row: self.segment_row,
        })
    }

    pub(crate) fn release(&mut self) {
        let events = std::mem::take(&mut self.events);
        if let (Some(first), Some(last)) = (events.first(), events.last()) {
            self.kept = Some((self.kept.map_or(first.seq, |(first, _)| first), last.seq));
        }
    }

    fn sealed(&self, keep: u64) -> Option<Segment> {
        let first = self
            .kept
            .map(|(first, _)| first)
            .or_else(|| self.events.first().map(|event| event.seq))?;
        let last = self
            .events
            .iter()
            .take_while(|event| event.seq < keep)
            .last()
            .map(|event| event.seq)
            .or_else(|| self.kept.map(|(_, last)| last))?;
        Some(Segment {
            first,
            last,
            row: self.segment_row,
        })
    }

    fn garbage(
        &self,
        links: &mut HashMap<String, Links>,
        marks: &mut BTreeMap<String, Vec<String>>,
        collectable: impl Fn(&str) -> bool,
    ) -> Option<Vec<bool>> {
        let mut firsts: HashMap<&str, usize> = HashMap::new();
        let mut holders = Vec::new();
        for (position, (key, value)) in self.stored.iter().enumerate() {
            if let Some((state, content)) = key.split_once('\n')
                && content.starts_with("b3:")
                && collectable(state)
            {
                let first = *firsts.entry(content).or_insert(holders.len());
                holders.push((first, position, state, value.as_str()));
            }
        }
        marks.retain(|state, _| self.checkpoints.contains_key(state));
        let mut queue = Vec::new();
        for (state, checkpoint) in self
            .checkpoints
            .iter()
            .filter(|(state, _)| collectable(state))
        {
            let references = marks.entry(state.clone()).or_insert_with(|| {
                EncodedPayload::references(&checkpoint.payload)
                    .map(str::to_owned)
                    .collect()
            });
            queue.extend(
                references
                    .iter()
                    .filter_map(|content| firsts.get_key_value(content.as_str()))
                    .map(|(content, _)| *content),
            );
        }
        let mut visited = vec![false; holders.len()];
        while let Some(content) = queue.pop() {
            let Some(&first) = firsts.get(content) else {
                continue;
            };
            if std::mem::replace(&mut visited[first], true) {
                continue;
            }
            let (_, _, state, value) = holders[first];
            let chunk_key = |chunk: &str| format!("{state}\n{chunk}");
            let known = match links.get(content) {
                Some(known) => {
                    if !known
                        .chunks
                        .iter()
                        .all(|chunk| self.stored.contains_key(&chunk_key(chunk)))
                    {
                        return None;
                    }
                    known
                }
                None => {
                    let requested = Cell::new(Vec::new());
                    let chunk = |chunk: &str| {
                        let mut seen = requested.take();
                        seen.push(chunk.to_owned());
                        requested.set(seen);
                        self.stored.get(&chunk_key(chunk)).map(String::as_str)
                    };
                    let json = EncodedPayload::assembled(value, &chunk)?;
                    let references = EncodedPayload::references(&json)
                        .map(str::to_owned)
                        .collect();
                    links.entry(content.to_owned()).or_insert(Links {
                        chunks: requested.take(),
                        references,
                    })
                }
            };
            queue.extend(
                known
                    .references
                    .iter()
                    .filter_map(|content| firsts.get_key_value(content.as_str()))
                    .map(|(content, _)| *content),
            );
        }
        links.retain(|content, _| firsts.contains_key(content.as_str()));
        let mut garbage = vec![false; self.stored.len()];
        for &(first, position, _, _) in &holders {
            garbage[position] = !visited[first];
        }
        Some(garbage)
    }

    pub(crate) fn collect(&mut self, collectable: &BTreeSet<String>) -> usize {
        let in_collectable = |key: &str| {
            key.split_once('\n')
                .is_some_and(|(state, _)| collectable.contains(state))
        };
        let before = self.dead.len();
        self.dead.retain(|key, _| !in_collectable(key));
        let mut collected = before - self.dead.len();
        let states = self
            .stored
            .keys()
            .filter_map(|key| key.split_once('\n').map(|(state, _)| state))
            .collect::<BTreeSet<_>>();
        let complete = states
            .into_iter()
            .filter(|state| self.complete(state))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let mut links = std::mem::take(&mut self.links);
        let mut marks = std::mem::take(&mut self.marks);
        let garbage = self.garbage(&mut links, &mut marks, |state| complete.contains(state));
        self.links = links;
        self.marks = marks;
        let Some(garbage) = garbage else {
            return collected;
        };
        let mut garbage = garbage.into_iter();
        let unreached = self
            .stored
            .extract_if(.., |_, _| garbage.next() == Some(true))
            .collect::<Vec<_>>();
        collected += unreached.len();
        self.dead.extend(
            unreached
                .into_iter()
                .filter(|(key, _)| !in_collectable(key)),
        );
        collected
    }

    fn references_of(&self, state: &str, value: &str) -> Option<Vec<String>> {
        let chunk = |chunk: &str| {
            self.stored
                .get(&format!("{state}\n{chunk}"))
                .map(String::as_str)
        };
        EncodedPayload::assembled(value, &chunk).map(|json| {
            EncodedPayload::references(&json)
                .map(|content| format!("{state}\n{content}"))
                .collect()
        })
    }

    fn holds_dead(&self, state: &str) -> bool {
        let prefix = format!("{state}\n");
        self.dead
            .range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
            .next()
            .is_some_and(|(key, _)| key.starts_with(&prefix))
    }

    pub(crate) fn revive(&mut self, pushed: &[Record]) -> Vec<Record> {
        if self.dead.is_empty() {
            return Vec::new();
        }
        let mut queue = Vec::new();
        for record in pushed {
            match record {
                Record::Checkpoint { payload, state, .. } if self.holds_dead(state) => queue
                    .extend(
                        EncodedPayload::references(payload)
                            .map(|content| format!("{state}\n{content}")),
                    ),
                Record::Stored { key, value } => {
                    if let Some((state, content)) = key.split_once('\n')
                        && content.starts_with("b3:")
                        && self.holds_dead(state)
                    {
                        queue.extend(self.references_of(state, value).into_iter().flatten());
                    }
                }
                _ => {}
            }
        }
        let mut revived = Vec::new();
        while let Some(key) = queue.pop() {
            let Some((key, value)) = self.dead.remove_entry(&key) else {
                continue;
            };
            if let Some((state, _)) = key.split_once('\n') {
                queue.extend(self.references_of(state, &value).into_iter().flatten());
            }
            let record = Record::Stored {
                key: key.clone(),
                value: value.clone(),
            };
            match self.defer(&record) {
                Some(pending) => pending.push(record.clone()),
                None => {
                    self.stored.insert(key, value);
                }
            }
            revived.push(record);
        }
        revived
    }

    fn drop_turns(&mut self, drop: impl Fn(&Turn) -> bool) {
        let models = &mut self.models;
        self.turns.retain(|turn| {
            let gone = drop(turn);
            if gone {
                models.remove(&turn.id);
            }
            !gone
        });
        self.index = self
            .turns
            .iter()
            .enumerate()
            .map(|(position, turn)| (turn.id.clone(), position))
            .collect();
    }

    pub(crate) fn unadmit(&mut self, id: &str, seq: u64) {
        self.drop_turns(|turn| turn.id == id);
        if let Some(position) = self.events.iter().rposition(|event| event.seq == seq) {
            self.hot_bytes -= self.events.remove(position).event.get().len();
        }
    }

    pub(crate) fn retire(&mut self, keep: usize) -> usize {
        let mut finished = self
            .turns
            .iter()
            .filter_map(|turn| match turn.phase {
                Phase::Finished { last, .. } => Some(last),
                _ => None,
            })
            .collect::<Vec<_>>();
        let excess = finished.len().saturating_sub(keep);
        if excess == 0 {
            return 0;
        }
        finished.sort_unstable();
        let cutoff = finished[excess - 1];
        for turn in &self.turns {
            if let Phase::Finished { last, outcome, .. } = turn.phase
                && last <= cutoff
            {
                self.retired.insert(
                    tag(&turn.id),
                    Retired {
                        digest: tag(&turn.digest),
                        first: turn.first,
                        hand: turn.hand.as_deref().map(Box::from),
                        last,
                        outcome,
                    },
                );
            }
        }
        self.drop_turns(
            |turn| matches!(turn.phase, Phase::Finished { last, .. } if last <= cutoff),
        );
        excess
    }

    pub(crate) fn seal(&mut self, row: u64, keep: u64) {
        if let Some(segment) = self.sealed(keep) {
            self.segments.push(segment);
            self.kept = None;
            self.events.retain(|event| event.seq >= keep);
            self.hot_bytes = self
                .events
                .iter()
                .map(|event| event.event.get().len())
                .sum();
        }
        self.segment_row = row;
    }

    #[must_use]
    pub fn checkpoint(&self, state: &str) -> Option<&Checkpoint> {
        self.checkpoints.get(state)
    }

    pub fn checkpoints(&self) -> impl Iterator<Item = (&str, &Checkpoint)> {
        self.checkpoints
            .iter()
            .map(|(state, checkpoint)| (state.as_str(), checkpoint))
    }

    #[must_use]
    pub fn stored_bytes(&self) -> usize {
        self.stored
            .iter()
            .chain(&self.dead)
            .map(|(key, value)| key.len() + value.len())
            .sum()
    }

    #[must_use]
    pub fn stored(&self, key: &str) -> Option<&str> {
        self.stored
            .get(key)
            .or_else(|| self.dead.get(key))
            .map(String::as_str)
    }

    #[must_use]
    pub fn host(&self, key: &str) -> Option<&RawValue> {
        self.hosts.get(key).map(AsRef::as_ref)
    }

    pub fn cells(&self) -> impl Iterator<Item = (&str, u64, &[Box<RawValue>])> {
        self.cells
            .iter()
            .map(|((session, cell), updates)| (session.as_str(), *cell, updates.as_slice()))
    }

    pub fn cells_of(
        &self,
        session: Option<&str>,
    ) -> impl Iterator<Item = (&str, u64, &[Box<RawValue>])> {
        let range = match session {
            Some(session) => (
                Bound::Included((session.to_owned(), 0)),
                Bound::Included((session.to_owned(), u64::MAX)),
            ),
            None => (Bound::Unbounded, Bound::Unbounded),
        };
        self.cells
            .range(range)
            .map(|((session, cell), updates)| (session.as_str(), *cell, updates.as_slice()))
    }

    #[must_use]
    pub fn models(&self, turn: &str) -> &[Box<RawValue>] {
        self.models.get(turn).map_or(&[], Vec::as_slice)
    }

    pub fn open_calls(&self) -> impl Iterator<Item = (&str, &OpenCall)> {
        self.calls.iter().map(|(id, call)| (id.as_str(), call))
    }

    #[must_use]
    pub fn unknown(&self) -> Vec<String> {
        self.calls
            .iter()
            .filter(|(_, call)| call.effect.needs_row())
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[must_use]
    pub fn recovering(&self) -> bool {
        self.turns.iter().any(|turn| {
            turn.recovered
                .is_some_and(|recovered| recovered.at == Some(self.seq))
        })
    }

    #[must_use]
    pub fn next_attempt(&self, turn: &str) -> u64 {
        match self.turn(turn).and_then(|turn| turn.recovered) {
            Some(Recovered {
                at: Some(at),
                attempt,
            }) if self.seq <= at => attempt + 1,
            Some(Recovered { at: None, attempt }) => attempt + 1,
            _ => 1,
        }
    }

    pub fn apply(&mut self, record: &Record, row: Option<u64>) -> Result<(), StoreError> {
        if let Some(pending) = self.defer(record) {
            return wait(pending, record.clone());
        }
        self.fold_in(record, row)
    }

    fn fold_in(&mut self, record: &Record, row: Option<u64>) -> Result<(), StoreError> {
        match record {
            Record::Admitted {
                digest,
                first,
                hand,
                metadata,
                prompt,
                turn,
            } => {
                if let Some(existing) = self.turn(turn) {
                    if !matches!(existing.phase, Phase::Finished { .. }) {
                        return Err(invalid(format!("turn {turn} was admitted twice")));
                    }
                    self.drop_turns(|old| old.id == *turn);
                }
                self.retired.remove(&tag(turn));
                self.index.insert(turn.clone(), self.turns.len());
                self.turns.push(Turn {
                    digest: digest.clone(),
                    edits: BTreeMap::new(),
                    first: *first,
                    hand: hand.clone(),
                    id: turn.clone(),
                    metadata: metadata.clone(),
                    phase: Phase::Queued { retry: None },
                    prompt: prompt.clone(),
                    recovered: None,
                });
            }
            Record::Call {
                call_id,
                effect,
                input,
                tool,
                turn,
            } => {
                self.calls.insert(
                    call_id.clone(),
                    OpenCall {
                        effect: *effect,
                        input: input.clone(),
                        tool: tool.clone(),
                        turn: turn.clone(),
                    },
                );
            }
            Record::Cell {
                cell,
                fresh,
                session,
                update,
            } => {
                let updates = self.cells.entry((session.clone(), *cell)).or_default();
                if *fresh {
                    updates.clear();
                }
                updates.push(update.clone());
            }
            Record::Patched {
                base,
                edits,
                revision,
                state,
            } => {
                let payload = self
                    .checkpoints
                    .get(state)
                    .filter(|held| held.revision == *base)
                    .map(|held| crate::patch::apply(&held.payload, edits))
                    .ok_or_else(|| unheld(state, *base))??;
                return self.fold_in(
                    &Record::Checkpoint {
                        payload,
                        revision: *revision,
                        state: state.clone(),
                    },
                    row,
                );
            }
            Record::Checkpoint {
                payload,
                revision,
                state,
            } => {
                self.marks.remove(state);
                self.checkpoints.insert(
                    state.clone(),
                    Checkpoint {
                        payload: payload.clone(),
                        revision: *revision,
                    },
                );
            }
            Record::Event {
                at,
                event,
                seq,
                turn,
            } => {
                if let Some(last) = self
                    .events
                    .last()
                    .map(|event| event.seq)
                    .or_else(|| self.kept.map(|(_, last)| last))
                    && *seq <= last
                {
                    return Err(invalid(format!("event {seq} does not follow event {last}")));
                }
                self.last_event = self.last_event.max(*seq);
                self.ends = self.ends.max(*seq);
                self.hot_bytes += event.get().len();
                self.events.push(Event {
                    at: *at,
                    event: event.clone(),
                    seq: *seq,
                    turn: turn.clone(),
                });
            }
            Record::Floor { seq } => {
                self.last_event = self.last_event.max(*seq);
            }
            Record::Forget { cell, session } => {
                let origin = self
                    .cells
                    .remove(&(session.clone(), *cell))
                    .and_then(|updates| updates.into_iter().next())
                    .and_then(|first| {
                        serde_json::from_str::<CellOrigin>(first.get())
                            .ok()
                            .and_then(|update| update.header)
                            .map(|header| format!("{}/", header.origin_call_id))
                    });
                if let Some(origin) = origin {
                    let forgotten = self
                        .calls
                        .range(origin.clone()..)
                        .map(|(id, _)| id)
                        .take_while(|id| id.starts_with(&origin))
                        .cloned()
                        .collect::<Vec<_>>();
                    for id in forgotten {
                        self.calls.remove(&id);
                    }
                }
            }
            Record::Host { key, value } if key == RETIRED_KEY => self.retired_from(value)?,
            Record::Host { key, value } => {
                self.hosts.insert(key.clone(), value.clone());
            }
            Record::Model { response, turn } => {
                self.models
                    .entry(turn.clone())
                    .or_default()
                    .push(response.clone());
            }
            Record::Recovery {
                at,
                attempt,
                turn,
                unknown,
            } => {
                for call_id in unknown {
                    self.calls.remove(call_id);
                }
                let Some(found) = self.turn_mut(turn) else {
                    return Err(invalid(format!("recovery of unknown turn {turn}")));
                };
                found.recovered = Some(Recovered {
                    at: at.or(row),
                    attempt: *attempt,
                });
            }
            Record::Result { call_id, .. } => {
                self.calls.remove(call_id);
            }
            Record::Groups { groups } => {
                self.groups = groups
                    .iter()
                    .map(|&(row, rows, count)| (row, (rows, count)))
                    .collect();
            }
            Record::Index { first, row, rows } => {
                self.pages.insert(
                    first.clone(),
                    Page::Cold(Section {
                        row: *row,
                        rows: *rows,
                    }),
                );
            }
            Record::Root { row, rows } => {
                self.root = Some(Page::Cold(Section {
                    row: *row,
                    rows: *rows,
                }));
            }
            Record::Owner { owner, .. } => {
                return Err(invalid(format!(
                    "owner {owner} is listed outside an index page"
                )));
            }
            Record::Section { owners, row, rows } => {
                let section = Section {
                    row: *row,
                    rows: *rows,
                };
                self.groups.entry(*row).or_insert((*rows, 0)).1 += owners.len() as u64;
                for (owner, checkpoint) in owners {
                    if let Owner::Cold { entry, .. } = self
                        .owners
                        .entry(owner.clone())
                        .or_insert_with(|| Owner::Cold {
                            entry: Entry {
                                legacy: true,
                                ..Entry::default()
                            },
                            pending: Vec::new(),
                        })
                    {
                        entry.checkpoint |= *checkpoint;
                        entry.sections.push(section);
                    }
                }
            }
            Record::Segment { first, last, row } => {
                self.ends = self.ends.max(*last);
                self.segments.push(Segment {
                    first: *first,
                    last: *last,
                    row: *row,
                });
            }
            Record::Status { status, turn } => self.status(turn, status)?,
            Record::Stored { key, value } => {
                self.dead.remove(key);
                self.stored.insert(key.clone(), value.clone());
            }
        }
        Ok(())
    }

    fn status(&mut self, id: &str, status: &Status) -> Result<(), StoreError> {
        let Some(turn) = self.turn_mut(id) else {
            return Err(invalid(format!("status of unknown turn {id}")));
        };
        if let Phase::Finished { .. } = turn.phase {
            return Err(invalid(format!("status of finished turn {id}")));
        }
        match status {
            Status::Cancelling => {
                if let Phase::Running { cancelling, .. } = &mut turn.phase {
                    *cancelling = true;
                }
            }
            Status::Finished {
                last,
                outcome,
                result,
            } => {
                turn.phase = Phase::Finished {
                    last: *last,
                    outcome: *outcome,
                    result: result.clone(),
                };
                self.calls
                    .retain(|_, call| call.turn.as_deref() != Some(id));
            }
            Status::Interrupted { note: text } => {
                if let Phase::Running { note, .. } = &mut turn.phase {
                    *note = Some(text.clone());
                }
            }
            Status::Replaced {
                digest,
                edit,
                edit_id,
                prompt,
            } => {
                turn.digest.clone_from(digest);
                turn.prompt = prompt.clone();
                turn.edits.insert(edit_id.clone(), edit.clone());
            }
            Status::Retrying {
                attempts,
                error,
                kind,
                retry_at,
            } => {
                let next = Some(Retry {
                    attempts: *attempts,
                    error: error.clone(),
                    kind: kind.clone(),
                    retry_at: *retry_at,
                });
                match &mut turn.phase {
                    Phase::Queued { retry } | Phase::Running { retry, .. } => *retry = next,
                    Phase::Finished { .. } => {}
                }
            }
            Status::Running => {
                turn.phase = Phase::Running {
                    cancelling: false,
                    note: None,
                    retry: None,
                };
            }
        }
        Ok(())
    }

    pub fn snapshot(&self, keep: u64) -> Result<String, StoreError> {
        let mut body = String::new();
        self.snapshot_records(keep, |record| {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&crate::encode(std::slice::from_ref(&record))?);
            Ok(())
        })?;
        Ok(body)
    }

    pub(crate) fn snapshot_records(
        &self,
        keep: u64,
        take: impl FnMut(Record) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.records(keep, true, take)
    }

    pub(crate) fn head_records(
        &self,
        keep: u64,
        index: Option<Section>,
        mut take: impl FnMut(Record) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if let Some(section) = index {
            take(Record::Root {
                row: section.row,
                rows: section.rows,
            })?;
        }
        self.records(keep, false, take)
    }

    fn records(
        &self,
        keep: u64,
        owned: bool,
        mut take: impl FnMut(Record) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for segment in self.segments.iter().copied().chain(self.sealed(keep)) {
            take(Record::Segment {
                first: segment.first,
                last: segment.last,
                row: segment.row,
            })?;
        }
        for (key, value) in &self.stored {
            if !owned && key.contains('\n') {
                continue;
            }
            take(Record::Stored {
                key: key.clone(),
                value: value.clone(),
            })?;
        }
        for (state, checkpoint) in self.checkpoints.iter().filter(|_| owned) {
            take(Record::Checkpoint {
                payload: checkpoint.payload.clone(),
                revision: checkpoint.revision,
                state: state.clone(),
            })?;
        }
        if !self.retired.is_empty() {
            take(Record::Host {
                key: RETIRED_KEY.to_owned(),
                value: self.retired_value()?,
            })?;
        }
        for turn in &self.turns {
            take(Record::Admitted {
                digest: turn.digest.clone(),
                first: turn.first,
                hand: turn.hand.clone(),
                metadata: turn.metadata.clone(),
                prompt: turn.prompt.clone(),
                turn: turn.id.clone(),
            })?;
            let status = |status| Record::Status {
                status,
                turn: turn.id.clone(),
            };
            for (edit_id, edit) in &turn.edits {
                take(status(Status::Replaced {
                    digest: turn.digest.clone(),
                    edit: edit.clone(),
                    edit_id: edit_id.clone(),
                    prompt: turn.prompt.clone(),
                }))?;
            }
            match &turn.phase {
                Phase::Queued { retry } => {
                    if let Some(retry) = retry {
                        take(status(retrying(retry)))?;
                    }
                }
                Phase::Running {
                    cancelling,
                    note,
                    retry,
                } => {
                    take(status(Status::Running))?;
                    if *cancelling {
                        take(status(Status::Cancelling))?;
                    }
                    if let Some(note) = note {
                        take(status(Status::Interrupted { note: note.clone() }))?;
                    }
                    if let Some(retry) = retry {
                        take(status(retrying(retry)))?;
                    }
                }
                Phase::Finished {
                    last,
                    outcome,
                    result,
                } => take(status(Status::Finished {
                    last: *last,
                    outcome: *outcome,
                    result: result.clone(),
                }))?,
            }
            if let Some(recovered) = turn.recovered {
                take(Record::Recovery {
                    at: recovered.at,
                    attempt: recovered.attempt,
                    turn: turn.id.clone(),
                    unknown: Vec::new(),
                })?;
            }
            for response in self.models(&turn.id) {
                take(Record::Model {
                    response: response.clone(),
                    turn: turn.id.clone(),
                })?;
            }
        }
        for (call_id, call) in &self.calls {
            take(Record::Call {
                call_id: call_id.clone(),
                effect: call.effect,
                input: call.input.clone(),
                tool: call.tool.clone(),
                turn: call.turn.clone(),
            })?;
        }
        for ((session, cell), updates) in self.cells.iter().filter(|_| owned) {
            for (position, update) in updates.iter().enumerate() {
                take(Record::Cell {
                    cell: *cell,
                    fresh: position == 0,
                    session: session.clone(),
                    update: update.clone(),
                })?;
            }
        }
        for (key, value) in &self.hosts {
            take(Record::Host {
                key: key.clone(),
                value: value.clone(),
            })?;
        }
        for event in self.events.iter().filter(|event| event.seq >= keep) {
            take(Record::Event {
                at: event.at,
                event: event.event.clone(),
                seq: event.seq,
                turn: event.turn.clone(),
            })?;
        }
        take(Record::Floor {
            seq: self.last_event,
        })?;
        Ok(())
    }
}

fn retrying(retry: &Retry) -> Status {
    Status::Retrying {
        attempts: retry.attempts,
        error: retry.error.clone(),
        kind: retry.kind.clone(),
        retry_at: retry.retry_at,
    }
}

#[cfg(test)]
mod stored_bytes_tests {
    use super::*;

    #[test]
    fn stored_bytes_counts_every_stored_record_once() {
        let mut fold = Fold::default();
        assert_eq!(fold.stored_bytes(), 0);
        let stored = |key: &str, value: &str| Record::Stored {
            key: key.to_owned(),
            value: value.to_owned(),
        };
        fold.apply(
            &stored(
                "state
b3:a",
                "1234",
            ),
            None,
        )
        .unwrap();
        fold.apply(
            &stored(
                "state
b3:b",
                "12",
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            fold.stored_bytes(),
            2 * "state
b3:a"
                .len()
                + 6
        );
        fold.apply(
            &stored(
                "state
b3:a",
                "1",
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            fold.stored_bytes(),
            2 * "state
b3:a"
                .len()
                + 3
        );
    }
}
