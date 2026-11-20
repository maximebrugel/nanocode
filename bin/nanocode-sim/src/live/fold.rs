use std::collections::BTreeMap;

use nanocode_live_fold::{Op, Row};
use serde_json::Value;

pub type Rows = BTreeMap<(String, String), Value>;

pub fn to_rows(rows: &Rows) -> Vec<Row> {
    rows.iter()
        .map(|((table, key), value)| Row {
            table: table.clone(),
            key: key.clone(),
            value: value.clone(),
        })
        .collect()
}

pub fn from_rows(rows: Vec<Row>) -> Rows {
    rows.into_iter()
        .map(|row| ((row.table, row.key), row.value))
        .collect()
}

pub fn puts(ops: &[Op]) -> Vec<(String, String, Value)> {
    ops.iter()
        .filter_map(|op| match op {
            Op::Upsert { table, key, value } => Some((table.clone(), key.clone(), value.clone())),
            Op::Delete { .. } => None,
        })
        .collect()
}
