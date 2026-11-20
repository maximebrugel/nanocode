use serde::Serialize;

use crate::sim::{Mutation, count_encode, mutated};
use crate::{Access, Cursor, Op, Row, Snapshot, ViewKey, encode};

pub trait Seal {
    type Reader: ?Sized;

    fn access(&self, view: &ViewKey, reader: &Self::Reader) -> Access;

    fn outline(&self, view: &ViewKey, op: &Op) -> Option<Op>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Open;

impl Seal for Open {
    type Reader = ();

    fn access(&self, _view: &ViewKey, _reader: &()) -> Access {
        Access::Full
    }

    fn outline(&self, _view: &ViewKey, op: &Op) -> Option<Op> {
        Some(op.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    kind: &'static str,
    tail: String,
}

#[derive(Serialize)]
struct DiffBody<'a> {
    from: &'a Cursor,
    to: &'a Cursor,
    ops: &'a [Op],
}

#[derive(Serialize)]
struct SnapBody<'a> {
    cursor: &'a Cursor,
    rows: &'a [Row],
}

impl Encoded {
    pub fn diff(from: &Cursor, to: &Cursor, ops: &[Op]) -> Self {
        Self::body("diff", &DiffBody { from, to, ops })
    }

    pub fn snap(cursor: &Cursor, rows: &[Row]) -> Self {
        Self::body("snap", &SnapBody { cursor, rows })
    }

    fn body<T: Serialize>(kind: &'static str, body: &T) -> Self {
        count_encode();
        let json = encode(body);
        let fields = json
            .strip_prefix('{')
            .expect("a frame body encodes as an object");
        Self {
            kind,
            tail: format!(",{fields}"),
        }
    }

    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    pub fn head(&self, id: u32) -> String {
        format!("{{\"t\":\"{}\",\"id\":{id}", self.kind)
    }

    pub fn tail(&self) -> &str {
        &self.tail
    }

    pub fn frame(&self, id: u32) -> String {
        let mut frame = self.head(id);
        frame.push_str(&self.tail);
        frame
    }
}

pub fn outline_ops<S: Seal + ?Sized>(seal: &S, view: &ViewKey, ops: &[Op]) -> Vec<Op> {
    ops.iter().filter_map(|op| seal.outline(view, op)).collect()
}

pub fn outline_rows<S: Seal + ?Sized>(seal: &S, view: &ViewKey, rows: &[Row]) -> Vec<Row> {
    rows.iter()
        .filter_map(|row| match seal.outline(view, &Op::from(row.clone()))? {
            Op::Upsert { table, key, value } => Some(Row { table, key, value }),
            Op::Delete { .. } => None,
        })
        .collect()
}

pub fn snapshot_frame<S: Seal + ?Sized>(
    snapshot: &Snapshot,
    access: Access,
    seal: &S,
) -> Option<Encoded> {
    match access {
        Access::Denied => None,
        Access::Outline if !mutated(Mutation::OutlineLeaks) => Some(Encoded::snap(
            &snapshot.cursor,
            &outline_rows(seal, &snapshot.view, &snapshot.rows),
        )),
        Access::Full | Access::Outline => Some(Encoded::snap(&snapshot.cursor, &snapshot.rows)),
    }
}
