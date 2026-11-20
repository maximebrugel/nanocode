use std::{collections::BTreeMap, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use crate::sim::{Mutation, mutated};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Key {
    pub source: String,
    pub seq: u64,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    pub key: Key,
    pub target: String,
    pub part: u32,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "delivery", rename_all = "snake_case")]
pub enum Row {
    Pending(Delivery),
    Delivered(Delivery),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Exists,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ingest {
    First,
    Again,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outbox {
    #[serde(with = "rows")]
    rows: BTreeMap<Key, Row>,
}

impl Outbox {
    pub const fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
        }
    }

    pub fn post(&mut self, delivery: Delivery) -> Result<(), Refusal> {
        if mutated(Mutation::DeliveryLost) && delivery.key.index > 0 {
            return Ok(());
        }
        match self.rows.get(&delivery.key) {
            None => {
                self.rows
                    .insert(delivery.key.clone(), Row::Pending(delivery));
                Ok(())
            }
            Some(Row::Pending(held) | Row::Delivered(held)) if *held == delivery => Ok(()),
            Some(_) => Err(Refusal::Exists),
        }
    }

    pub fn undelivered(&self, part: u32, max: NonZeroUsize) -> Vec<&Delivery> {
        self.rows
            .values()
            .filter_map(|row| match row {
                Row::Pending(delivery) if delivery.part == part => Some(delivery),
                Row::Pending(_) | Row::Delivered(_) => None,
            })
            .take(max.get())
            .collect()
    }

    pub fn next(&self) -> Option<&Delivery> {
        self.rows.values().find_map(|row| match row {
            Row::Pending(delivery) => Some(delivery),
            Row::Delivered(_) => None,
        })
    }

    pub fn pending_of<'a>(
        &'a self,
        source: &'a str,
        seq: u64,
    ) -> impl Iterator<Item = &'a Key> + 'a {
        self.rows.iter().filter_map(move |(key, row)| {
            (matches!(row, Row::Pending(_)) && key.source == source && key.seq == seq)
                .then_some(key)
        })
    }

    pub fn ingest(&mut self, key: &Key) -> Result<Ingest, Refusal> {
        let row = self.rows.get_mut(key).ok_or(Refusal::Unknown)?;
        let first = match row {
            Row::Pending(delivery) => Some(delivery.clone()),
            Row::Delivered(_) => None,
        };
        match first {
            Some(delivery) => {
                *row = Row::Delivered(delivery);
                Ok(Ingest::First)
            }
            None if mutated(Mutation::DeliveryIngestedTwice) => Ok(Ingest::First),
            None => Ok(Ingest::Again),
        }
    }

    pub fn pending(&self) -> usize {
        self.rows
            .values()
            .filter(|row| matches!(row, Row::Pending(_)))
            .count()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

mod rows {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{Key, Row};

    pub fn serialize<S: Serializer>(
        rows: &BTreeMap<Key, Row>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        rows.values().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<Key, Row>, D::Error> {
        let rows = Vec::<Row>::deserialize(deserializer)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let key = match &row {
                    Row::Pending(delivery) | Row::Delivered(delivery) => delivery.key.clone(),
                };
                (key, row)
            })
            .collect())
    }
}
