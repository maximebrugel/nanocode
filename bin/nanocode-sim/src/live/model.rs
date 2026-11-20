use std::collections::BTreeMap;

use nanocode_live_fold::{Access, Op, Row, Seal, View, ViewKey};
use serde_json::{Value, json};

use super::fold::{Rows, to_rows};

pub const READERS: [&str; 3] = ["admin", "guest", "stranger"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    pub title: String,
    pub sealed: bool,
    pub posts: Vec<String>,
    pub version: u64,
}

pub type State = BTreeMap<u32, Thread>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    List(u32),
    Thread(u32),
}

impl Shape {
    pub fn parse(view: &ViewKey, partitions: u32, threads: u32) -> Option<Self> {
        let number = |key: &str| view.args.get(key)?.parse::<u32>().ok();
        match (view.name.as_str(), view.args.len()) {
            ("list", 1) => number("part")
                .filter(|part| *part < partitions)
                .map(Self::List),
            ("thread", 1) => number("id").filter(|id| *id < threads).map(Self::Thread),
            _ => None,
        }
    }

    pub fn key(self) -> ViewKey {
        match self {
            Self::List(part) => ViewKey::new("list").arg("part", part.to_string()),
            Self::Thread(id) => ViewKey::new("thread").arg("id", id.to_string()),
        }
    }

    pub const fn partition(self, partitions: u32) -> u32 {
        match self {
            Self::List(part) => part,
            Self::Thread(id) => id % partitions,
        }
    }

    pub const fn covers(self, thread: u32, partitions: u32) -> bool {
        match self {
            Self::List(part) => thread % partitions == part,
            Self::Thread(id) => id == thread,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub partitions: u32,
    pub versioned: bool,
}

pub fn thread_of(record: &Value) -> u32 {
    record
        .get("thread")
        .and_then(Value::as_u64)
        .and_then(|thread| u32::try_from(thread).ok())
        .expect("a record names its thread")
}

pub fn rejected(record: &Value) -> bool {
    record.get("error").is_some_and(|error| !error.is_null()) || record.get("repeat").is_some()
}

fn text(record: &Value, key: &str) -> String {
    record
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub fn effect(state: &mut State, record: &Value) {
    if rejected(record) {
        return;
    }
    if record.get("kind").and_then(Value::as_str) == Some("drop") {
        state.remove(&thread_of(record));
        return;
    }
    let thread = state.entry(thread_of(record)).or_default();
    thread.version += 1;
    match record.get("kind").and_then(Value::as_str) {
        Some("rename") => thread.title = text(record, "title"),
        Some("post") => thread.posts.push(text(record, "body")),
        Some("seal") => thread.sealed = record.get("on").and_then(Value::as_bool) == Some(true),
        _ => {}
    }
}

fn thread_row(thread: &Thread, layout: Layout) -> Value {
    let mut row =
        json!({"title": thread.title, "sealed": thread.sealed, "posts": thread.posts.len()});
    if layout.versioned {
        row["v"] = json!(thread.version);
    }
    row
}

pub fn post_key(id: u32, index: usize) -> String {
    format!("{id}/{index:04}")
}

fn post_row(thread: &Thread, index: usize, layout: Layout) -> Value {
    let mut row = json!({"body": thread.posts[index], "sealed": thread.sealed});
    if layout.versioned {
        row["v"] = json!(thread.version);
    }
    row
}

fn thread_ops(id: u32, thread: &Thread, shape: Shape, layout: Layout, ops: &mut Vec<Op>) {
    ops.push(Op::Upsert {
        table: "thread".to_owned(),
        key: id.to_string(),
        value: thread_row(thread, layout),
    });
    if matches!(shape, Shape::List(_)) {
        return;
    }
    for index in 0..thread.posts.len() {
        ops.push(Op::Upsert {
            table: "post".to_owned(),
            key: post_key(id, index),
            value: post_row(thread, index, layout),
        });
    }
}

pub fn reference(state: &State, shape: Shape, access: Access, layout: Layout) -> Rows {
    let mut rows = Rows::new();
    if access == Access::Denied {
        return rows;
    }
    let mut ops = Vec::new();
    for (id, thread) in state {
        if !shape.covers(*id, layout.partitions) {
            continue;
        }
        ops.clear();
        thread_ops(*id, thread, shape, layout, &mut ops);
        for op in ops.drain(..) {
            if let Op::Upsert { table, key, value } = op {
                let hidden = access == Access::Outline && table == "post" && thread.sealed;
                if !hidden {
                    rows.insert((table, key), value);
                }
            }
        }
    }
    rows
}

pub struct ThreadsView {
    shape: Shape,
    layout: Layout,
    state: State,
}

impl ThreadsView {
    pub const fn new(shape: Shape, layout: Layout) -> Self {
        Self {
            shape,
            layout,
            state: State::new(),
        }
    }
}

impl View for ThreadsView {
    type Record = Value;

    fn apply(&mut self, _pos: u64, record: &Value, ops: &mut Vec<Op>) {
        if rejected(record) {
            return;
        }
        let id = thread_of(record);
        if !self.shape.covers(id, self.layout.partitions) {
            return;
        }
        let posts = self.state.get(&id).map_or(0, |thread| thread.posts.len());
        effect(&mut self.state, record);
        let Some(thread) = self.state.get(&id) else {
            ops.push(Op::Delete {
                table: "thread".to_owned(),
                key: id.to_string(),
            });
            if matches!(self.shape, Shape::Thread(_)) {
                for index in 0..posts {
                    ops.push(Op::Delete {
                        table: "post".to_owned(),
                        key: post_key(id, index),
                    });
                }
            }
            return;
        };
        thread_ops(id, thread, self.shape, self.layout, ops);
    }

    fn rows(&self) -> Vec<Row> {
        to_rows(&reference(
            &self.state,
            self.shape,
            Access::Full,
            self.layout,
        ))
    }
}

pub struct Policy {
    pub partitions: u32,
    pub threads: u32,
}

impl Seal for Policy {
    type Reader = str;

    fn access(&self, view: &ViewKey, reader: &str) -> Access {
        match (Shape::parse(view, self.partitions, self.threads), reader) {
            (None, _) => Access::Denied,
            (Some(_), "admin") => Access::Full,
            (Some(Shape::Thread(_)), "stranger") => Access::Denied,
            (Some(_), _) => Access::Outline,
        }
    }

    fn outline(&self, _view: &ViewKey, op: &Op) -> Option<Op> {
        match op {
            Op::Upsert { table, key, value }
                if table == "post" && value.get("sealed") == Some(&Value::Bool(true)) =>
            {
                Some(Op::Delete {
                    table: table.clone(),
                    key: key.clone(),
                })
            }
            op => Some(op.clone()),
        }
    }
}
