use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod golden;
pub mod live;
pub mod replica;
pub mod seal;
pub mod sim;
pub mod table;

pub use live::{Batch, Entry, Live, Resume, Ring, View};
pub use replica::{Gap, Replica};
pub use seal::{Encoded, Open, Seal, outline_ops, outline_rows, snapshot_frame};
pub use table::Table;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    pub partition: u32,
    pub epoch: u64,
    pub pos: u64,
}

impl Cursor {
    pub const fn new(partition: u32, epoch: u64, pos: u64) -> Self {
        Self {
            partition,
            epoch,
            pos,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ViewKey {
    pub name: String,
    pub args: BTreeMap<String, String>,
}

impl ViewKey {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: BTreeMap::new(),
        }
    }

    pub fn arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.insert(key.into(), value.into());
        self
    }

    pub fn canonical(&self) -> String {
        encode(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub table: String,
    pub key: String,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Op {
    #[serde(rename = "put")]
    Upsert {
        table: String,
        key: String,
        value: Value,
    },
    #[serde(rename = "del")]
    Delete { table: String, key: String },
}

impl Op {
    pub fn table(&self) -> &str {
        match self {
            Self::Upsert { table, .. } | Self::Delete { table, .. } => table,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Self::Upsert { key, .. } | Self::Delete { key, .. } => key,
        }
    }
}

impl From<Row> for Op {
    fn from(row: Row) -> Self {
        Self::Upsert {
            table: row.table,
            key: row.key,
            value: row.value,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub view: ViewKey,
    pub from: Cursor,
    pub to: Cursor,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub view: ViewKey,
    pub cursor: Cursor,
    pub rows: Vec<Row>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Full,
    Outline,
    Denied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub client: String,
    pub mid: u64,
    pub name: String,
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "AckWire", try_from = "AckWire")]
pub struct Ack {
    pub client: String,
    pub mid: u64,
    pub result: Result<Cursor, Failure>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AckWire {
    client: String,
    mid: u64,
    cursor: Option<Cursor>,
    error: Option<Failure>,
}

impl From<Ack> for AckWire {
    fn from(ack: Ack) -> Self {
        let (cursor, error) = match ack.result {
            Ok(cursor) => (Some(cursor), None),
            Err(failure) => (None, Some(failure)),
        };
        Self {
            client: ack.client,
            mid: ack.mid,
            cursor,
            error,
        }
    }
}

impl TryFrom<AckWire> for Ack {
    type Error = String;

    fn try_from(wire: AckWire) -> Result<Self, String> {
        let result = match (wire.cursor, wire.error) {
            (Some(cursor), None) => Ok(cursor),
            (None, Some(failure)) => Err(failure),
            (Some(_), Some(_)) => return Err("an ack carries a cursor and an error".into()),
            (None, None) => return Err("an ack carries neither a cursor nor an error".into()),
        };
        Ok(Self {
            client: wire.client,
            mid: wire.mid,
            result,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrCode {
    Denied,
    UnknownView,
    BadFrame,
    Overloaded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientFrame {
    Sub {
        id: u32,
        view: ViewKey,
        cursor: Option<Cursor>,
    },
    Unsub {
        id: u32,
    },
    Mut(Intent),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    Snap {
        id: u32,
        cursor: Cursor,
        rows: Vec<Row>,
    },
    Diff {
        id: u32,
        from: Cursor,
        to: Cursor,
        ops: Vec<Op>,
    },
    Ack(Ack),
    Eph {
        id: u32,
        body: Value,
    },
    Err {
        id: Option<u32>,
        code: ErrCode,
        message: String,
    },
}

pub fn encode<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("a live frame serializes")
}

pub fn decode_client(text: &str) -> Result<ClientFrame, serde_json::Error> {
    serde_json::from_str(text)
}

pub fn decode_server(text: &str) -> Result<ServerFrame, serde_json::Error> {
    serde_json::from_str(text)
}
