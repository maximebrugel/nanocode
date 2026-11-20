use std::collections::{BTreeMap, BTreeSet};
use std::mem;

use serde_json::Value;

use crate::sim::{Mutation, mutated};
use crate::{Op, Row};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    rows: BTreeMap<String, BTreeMap<String, Value>>,
    start: BTreeMap<(String, String), Option<Value>>,
}

impl Table {
    pub fn from_rows(rows: impl IntoIterator<Item = Row>) -> Self {
        let mut table = Self::default();
        for row in rows {
            table
                .rows
                .entry(row.table)
                .or_default()
                .insert(row.key, row.value);
        }
        table
    }

    pub fn get(&self, table: &str, key: &str) -> Option<&Value> {
        self.rows.get(table)?.get(key)
    }

    pub fn len(&self) -> usize {
        self.rows.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> Vec<Row> {
        self.rows
            .iter()
            .flat_map(|(table, rows)| {
                rows.iter().map(|(key, value)| Row {
                    table: table.clone(),
                    key: key.clone(),
                    value: value.clone(),
                })
            })
            .collect()
    }

    pub fn put(&mut self, table: &str, key: &str, value: Value, ops: &mut Vec<Op>) {
        if self.get(table, key) == Some(&value) {
            return;
        }
        ops.push(Op::Upsert {
            table: table.to_owned(),
            key: key.to_owned(),
            value: value.clone(),
        });
        self.write(table, key, Some(value));
    }

    pub fn del(&mut self, table: &str, key: &str, ops: &mut Vec<Op>) {
        if self.get(table, key).is_none() {
            return;
        }
        ops.push(Op::Delete {
            table: table.to_owned(),
            key: key.to_owned(),
        });
        self.write(table, key, None);
    }

    pub fn set(&mut self, op: Op) -> bool {
        match op {
            Op::Upsert { table, key, value } => {
                if self.get(&table, &key) == Some(&value) {
                    return false;
                }
                self.write(&table, &key, Some(value));
            }
            Op::Delete { table, key } => {
                if self.get(&table, &key).is_none() {
                    return false;
                }
                self.write(&table, &key, None);
            }
        }
        true
    }

    pub fn pending(&self) -> bool {
        !self.start.is_empty()
    }

    pub fn settle(&mut self, unchanged: &mut BTreeMap<String, BTreeSet<String>>) -> Vec<Op> {
        let mut ops = Vec::new();
        for ((table, key), before) in mem::take(&mut self.start) {
            let Some(now) = self.get(&table, &key) else {
                if before.is_none() {
                    unchanged.entry(table).or_default().insert(key);
                } else if !mutated(Mutation::SettleDropsDeletes) {
                    ops.push(Op::Delete { table, key });
                }
                continue;
            };
            if before.as_ref() == Some(now) {
                unchanged.entry(table).or_default().insert(key);
                continue;
            }
            let value = match before {
                Some(before) if mutated(Mutation::SettleKeepsStart) => before,
                _ => now.clone(),
            };
            ops.push(Op::Upsert { table, key, value });
        }
        ops
    }

    pub fn forget(&mut self) {
        self.start.clear();
    }

    fn write(&mut self, table: &str, key: &str, value: Option<Value>) {
        let before = match value {
            Some(value) => self
                .rows
                .entry(table.to_owned())
                .or_default()
                .insert(key.to_owned(), value),
            None => {
                let Some(rows) = self.rows.get_mut(table) else {
                    return;
                };
                let before = rows.remove(key);
                if rows.is_empty() {
                    self.rows.remove(table);
                }
                before
            }
        };
        self.start
            .entry((table.to_owned(), key.to_owned()))
            .or_insert(before);
    }
}
