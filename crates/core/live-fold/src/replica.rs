use crate::table::Table;
use crate::{Cursor, Op, Row, ServerFrame};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replica {
    cursor: Option<Cursor>,
    table: Table,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    pub have: Option<Cursor>,
    pub from: Cursor,
}

impl Replica {
    pub const fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    pub const fn table(&self) -> &Table {
        &self.table
    }

    pub fn rows(&self) -> Vec<Row> {
        self.table.rows()
    }

    pub fn snap(&mut self, cursor: Cursor, rows: Vec<Row>) {
        self.table = Table::from_rows(rows);
        self.cursor = Some(cursor);
    }

    pub fn diff(&mut self, from: Cursor, to: Cursor, ops: Vec<Op>) -> Result<(), Gap> {
        if self.cursor != Some(from) {
            return Err(Gap {
                have: self.cursor,
                from,
            });
        }
        for op in ops {
            self.table.set(op);
        }
        self.table.forget();
        self.cursor = Some(to);
        Ok(())
    }

    pub fn frame(&mut self, frame: ServerFrame) -> Result<bool, Gap> {
        match frame {
            ServerFrame::Snap { cursor, rows, .. } => self.snap(cursor, rows),
            ServerFrame::Diff { from, to, ops, .. } => self.diff(from, to, ops)?,
            ServerFrame::Ack(_) | ServerFrame::Eph { .. } | ServerFrame::Err { .. } => {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
