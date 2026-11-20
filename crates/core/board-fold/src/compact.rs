use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde::ser::{self, Serialize};
use serde::{Deserialize, Serializer};

use crate::{
    Entry, EventId, Fold, Rejection, Task, TaskId, access, intent, messages, mirror, steer, watch,
};

pub const MAGIC: &[u8; 8] = b"NCFOLD\x00\x01";

const SHARED_MAX: usize = 96;

const NULL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const UINT: u8 = 3;
const NINT: u8 = 4;
const FLOAT: u8 = 5;
const SHARE: u8 = 6;
const SHARED: u8 = 7;
const TEXT: u8 = 8;
const BYTES: u8 = 9;
const SEQ: u8 = 10;
const MAP: u8 = 11;
const SEQ_OPEN: u8 = 12;
const MAP_OPEN: u8 = 13;
const END: u8 = 14;

const SECTIONS: [&str; 9] = [
    "head", "tasks", "seen", "entries", "access", "mirror", "said", "steers", "intents",
];

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

struct Pairs<'a, K, V>(&'a BTreeMap<K, V>);

impl<K: Serialize, V: Serialize> Serialize for Pairs<'_, K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0)
    }
}

fn map<K: Ord, V>(pairs: Vec<(K, V)>) -> BTreeMap<K, V> {
    pairs.into_iter().collect()
}

type Labelled = BTreeMap<String, BTreeMap<String, BTreeSet<TaskId>>>;

type Head = (
    u64,
    u64,
    Vec<(TaskId, BTreeSet<TaskId>)>,
    Vec<(TaskId, BTreeSet<TaskId>)>,
    BTreeSet<TaskId>,
    Vec<(String, BTreeMap<String, BTreeSet<TaskId>>)>,
    watch::Marks,
);

enum Part {
    Head(Head),
    Tasks(Vec<(TaskId, Task)>),
    Seen(Vec<(EventId, Result<u64, Rejection>)>),
    Entries(Vec<Entry>),
    Access(access::Access),
    Mirror(mirror::Mirror),
    Said(messages::Said),
    Steers(steer::Steers),
    Intents(intent::Intents),
}

type Job<'a, T> = Box<dyn FnOnce() -> T + Send + 'a>;

const THREADS_FROM_EVENTS: usize = 1024;

const THREADS_FROM_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spread {
    Inline,
    Threads,
}

impl Spread {
    pub fn for_fold(fold: &Fold) -> Self {
        if fold.seen.len() < THREADS_FROM_EVENTS {
            Self::Inline
        } else {
            Self::Threads
        }
    }

    pub const fn for_snapshot(snapshot: &[u8]) -> Self {
        if snapshot.len() < THREADS_FROM_BYTES {
            Self::Inline
        } else {
            Self::Threads
        }
    }
}

fn all<T: Send>(spread: Spread, jobs: Vec<Job<'_, T>>) -> Vec<T> {
    match spread {
        Spread::Inline => jobs.into_iter().map(|job| job()).collect(),
        Spread::Threads => threads(jobs),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn threads<T: Send>(jobs: Vec<Job<'_, T>>) -> Vec<T> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs.into_iter().map(|job| scope.spawn(job)).collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

#[cfg(target_arch = "wasm32")]
fn threads<T: Send>(jobs: Vec<Job<'_, T>>) -> Vec<T> {
    jobs.into_iter().map(|job| job()).collect()
}

pub fn snapshot(fold: &Fold, spread: Spread) -> Result<Vec<u8>, Error> {
    let Fold {
        next,
        now_ms,
        tasks,
        children,
        dependents,
        seen,
        entries,
        access,
        mirror,
        live,
        posted: _,
        labelled,
        said,
        marks,
        steers,
        intents,
        refused: _,
    } = fold;
    let jobs: Vec<Job<'_, Result<Vec<u8>, Error>>> = vec![
        Box::new(move || {
            encode(&(
                next,
                now_ms,
                Pairs(children),
                Pairs(dependents),
                live,
                Pairs(labelled),
                marks,
            ))
        }),
        Box::new(move || encode(&Pairs(tasks))),
        Box::new(move || encode(&Pairs(seen))),
        Box::new(move || encode(entries)),
        Box::new(move || encode(access)),
        Box::new(move || encode(mirror)),
        Box::new(move || encode(said)),
        Box::new(move || encode(steers)),
        Box::new(move || encode(intents)),
    ];
    let bodies = all(spread, jobs)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let mut writer = Writer::default();
    writer.out.extend_from_slice(MAGIC);
    writer.varint(SECTIONS.len() as u64);
    for (name, body) in SECTIONS.iter().zip(&bodies) {
        writer.varint(name.len() as u64);
        writer.out.extend_from_slice(name.as_bytes());
        writer.varint(body.len() as u64);
    }
    let mut out = writer.out;
    out.reserve(bodies.iter().map(Vec::len).sum());
    for body in &bodies {
        out.extend_from_slice(body);
    }
    Ok(out)
}

pub fn restore(snapshot: &[u8], spread: Spread) -> Result<Fold, Error> {
    let Some(rest) = snapshot.strip_prefix(MAGIC.as_slice()) else {
        return fail("not a compact fold snapshot");
    };
    let mut reader = Reader::new(rest);
    let count = reader.len()?;
    if count != SECTIONS.len() {
        return fail(format!(
            "snapshot has {count} sections, expected {}",
            SECTIONS.len()
        ));
    }
    let mut lengths = Vec::with_capacity(count);
    for expected in SECTIONS {
        let size = reader.len()?;
        let name = reader.str(size)?;
        if name != expected {
            return fail(format!("snapshot section {name}, expected {expected}"));
        }
        lengths.push(reader.len()?);
    }
    let mut bodies = Vec::with_capacity(count);
    for length in lengths {
        bodies.push(reader.take(length)?);
    }
    if reader.at != rest.len() {
        return fail("snapshot has bytes after its last section");
    }
    let [
        head,
        tasks,
        seen,
        entries,
        access,
        mirror,
        said,
        steers,
        intents,
    ] = <[&[u8]; 9]>::try_from(bodies).map_err(|_| Error("snapshot sections".to_owned()))?;
    let jobs: Vec<Job<'_, Result<Part, Error>>> = vec![
        Box::new(move || decode(head).map(Part::Head)),
        Box::new(move || decode(tasks).map(Part::Tasks)),
        Box::new(move || decode(seen).map(Part::Seen)),
        Box::new(move || decode(entries).map(Part::Entries)),
        Box::new(move || decode(access).map(Part::Access)),
        Box::new(move || decode(mirror).map(Part::Mirror)),
        Box::new(move || decode(said).map(Part::Said)),
        Box::new(move || decode(steers).map(Part::Steers)),
        Box::new(move || decode(intents).map(Part::Intents)),
    ];
    let mut parts = all(spread, jobs).into_iter().zip(SECTIONS);
    let mut part = || match parts.next() {
        Some((Ok(part), _)) => Ok(part),
        Some((Err(error), name)) => fail(format!("snapshot section {name}: {error}")),
        None => fail("snapshot ended early"),
    };
    let Part::Head((next, now_ms, children, dependents, live, labelled, marks)) = part()? else {
        return fail("snapshot head out of order");
    };
    let Part::Tasks(tasks) = part()? else {
        return fail("snapshot tasks out of order");
    };
    let Part::Seen(seen) = part()? else {
        return fail("snapshot seen out of order");
    };
    let Part::Entries(entries) = part()? else {
        return fail("snapshot entries out of order");
    };
    let Part::Access(access) = part()? else {
        return fail("snapshot access out of order");
    };
    let Part::Mirror(mirror) = part()? else {
        return fail("snapshot mirror out of order");
    };
    let Part::Said(said) = part()? else {
        return fail("snapshot said out of order");
    };
    let Part::Steers(steers) = part()? else {
        return fail("snapshot steers out of order");
    };
    let Part::Intents(intents) = part()? else {
        return fail("snapshot intents out of order");
    };
    let labelled: Labelled = map(labelled);
    Ok(Fold {
        next,
        now_ms,
        tasks: map(tasks),
        children: map(children),
        dependents: map(dependents),
        seen: map(seen),
        entries,
        access,
        mirror,
        live,
        posted: BTreeMap::new(),
        labelled,
        said,
        marks,
        steers,
        intents,
        refused: None,
    })
}

pub fn encode<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut writer = Writer::default();
    value.serialize(&mut writer)?;
    Ok(writer.out)
}

pub fn decode<'de, T: Deserialize<'de>>(input: &'de [u8]) -> Result<T, Error> {
    let mut reader = Reader::new(input);
    let value = T::deserialize(&mut reader)?;
    if reader.at != input.len() {
        return fail("bytes after the value");
    }
    Ok(value)
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    shared: HashMap<Box<str>, u64>,
}

impl Writer {
    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.out.push((value & 0x7f) as u8 | 0x80);
            value >>= 7;
        }
        self.out.push(value as u8);
    }

    fn head(&mut self, tag: u8, value: u64) {
        self.out.push(tag);
        self.varint(value);
    }

    fn text(&mut self, text: &str) {
        if text.len() > SHARED_MAX {
            self.head(TEXT, text.len() as u64);
        } else if let Some(&index) = self.shared.get(text) {
            self.head(SHARED, index);
            return;
        } else {
            let index = self.shared.len() as u64;
            self.shared.insert(text.into(), index);
            self.head(SHARE, text.len() as u64);
        }
        self.out.extend_from_slice(text.as_bytes());
    }

    fn open(&mut self, counted: u8, open: u8, len: Option<usize>) -> Option<usize> {
        match len {
            Some(len) => self.head(counted, len as u64),
            None => self.out.push(open),
        }
        len
    }

    fn variant(&mut self, variant: &str) {
        self.head(MAP, 1);
        self.text(variant);
    }
}

struct Compound<'a> {
    writer: &'a mut Writer,
    left: Option<usize>,
}

impl Compound<'_> {
    fn item(&mut self) -> Result<(), Error> {
        match &mut self.left {
            Some(0) => fail("more items than the declared length"),
            Some(left) => {
                *left -= 1;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn end(self) -> Result<(), Error> {
        match self.left {
            Some(0) => Ok(()),
            Some(left) => fail(format!("{left} fewer items than the declared length")),
            None => {
                self.writer.out.push(END);
                Ok(())
            }
        }
    }
}

impl<'a> Serializer for &'a mut Writer {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;

    fn serialize_bool(self, value: bool) -> Result<(), Error> {
        self.out.push(if value { TRUE } else { FALSE });
        Ok(())
    }

    fn serialize_i8(self, value: i8) -> Result<(), Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<(), Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<(), Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<(), Error> {
        if value < 0 {
            self.head(NINT, !value.cast_unsigned());
        } else {
            self.head(UINT, value.cast_unsigned());
        }
        Ok(())
    }

    fn serialize_u8(self, value: u8) -> Result<(), Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<(), Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<(), Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<(), Error> {
        self.head(UINT, value);
        Ok(())
    }

    fn serialize_f32(self, value: f32) -> Result<(), Error> {
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<(), Error> {
        self.out.push(FLOAT);
        self.out.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn serialize_char(self, value: char) -> Result<(), Error> {
        self.text(value.encode_utf8(&mut [0; 4]));
        Ok(())
    }

    fn serialize_str(self, value: &str) -> Result<(), Error> {
        self.text(value);
        Ok(())
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<(), Error> {
        self.head(BYTES, value.len() as u64);
        self.out.extend_from_slice(value);
        Ok(())
    }

    fn serialize_none(self) -> Result<(), Error> {
        self.out.push(NULL);
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        self.out.push(NULL);
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.text(variant);
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.variant(variant);
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Compound<'a>, Error> {
        let left = self.open(SEQ, SEQ_OPEN, len);
        Ok(Compound { writer: self, left })
    }

    fn serialize_tuple(self, len: usize) -> Result<Compound<'a>, Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Compound<'a>, Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Compound<'a>, Error> {
        self.variant(variant);
        self.serialize_seq(Some(len))
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Compound<'a>, Error> {
        let left = self.open(MAP, MAP_OPEN, len);
        Ok(Compound { writer: self, left })
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Compound<'a>, Error> {
        self.serialize_map(None)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Compound<'a>, Error> {
        self.variant(variant);
        self.serialize_map(None)
    }
}

impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.item()?;
        value.serialize(&mut *self.writer)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.item()?;
        key.serialize(&mut *self.writer)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut *self.writer)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.writer.text(key);
        value.serialize(&mut *self.writer)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        ser::SerializeStruct::serialize_field(self, key, value)
    }

    fn end(self) -> Result<(), Error> {
        Compound::end(self)
    }
}

struct Reader<'de> {
    input: &'de [u8],
    at: usize,
    shared: Vec<&'de str>,
}

impl<'de> Reader<'de> {
    const fn new(input: &'de [u8]) -> Self {
        Self {
            input,
            at: 0,
            shared: Vec::new(),
        }
    }

    fn peek(&self) -> Result<u8, Error> {
        match self.input.get(self.at) {
            Some(&byte) => Ok(byte),
            None => fail("snapshot ends mid-value"),
        }
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let byte = self.peek()?;
        self.at += 1;
        Ok(byte)
    }

    fn varint(&mut self) -> Result<u64, Error> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            let part = u64::from(byte & 0x7f);
            if shift > 63 || (shift == 63 && part > 1) {
                return fail("varint overflows 64 bits");
            }
            value |= part << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn len(&mut self) -> Result<usize, Error> {
        usize::try_from(self.varint()?).map_err(|error| Error(error.to_string()))
    }

    fn take(&mut self, len: usize) -> Result<&'de [u8], Error> {
        let Some(end) = self
            .at
            .checked_add(len)
            .filter(|end| *end <= self.input.len())
        else {
            return fail("snapshot ends mid-value");
        };
        let bytes = &self.input[self.at..end];
        self.at = end;
        Ok(bytes)
    }

    fn str(&mut self, len: usize) -> Result<&'de str, Error> {
        std::str::from_utf8(self.take(len)?).map_err(|error| Error(error.to_string()))
    }

    fn text(&mut self, tag: u8) -> Result<&'de str, Error> {
        match tag {
            SHARE => {
                let len = self.len()?;
                let text = self.str(len)?;
                self.shared.push(text);
                Ok(text)
            }
            SHARED => {
                let index = self.len()?;
                match self.shared.get(index) {
                    Some(text) => Ok(text),
                    None => fail(format!("string reference {index} is not defined yet")),
                }
            }
            TEXT => {
                let len = self.len()?;
                self.str(len)
            }
            other => fail(format!("expected a string, found tag {other}")),
        }
    }
}

struct Items<'a, 'de> {
    reader: &'a mut Reader<'de>,
    left: Option<usize>,
    done: bool,
}

impl<'a, 'de> Items<'a, 'de> {
    fn new(reader: &'a mut Reader<'de>, counted: bool) -> Result<Self, Error> {
        let left = if counted { Some(reader.len()?) } else { None };
        Ok(Self {
            reader,
            left,
            done: false,
        })
    }

    fn more(&mut self) -> Result<bool, Error> {
        match &mut self.left {
            Some(0) => Ok(false),
            Some(left) => {
                *left -= 1;
                Ok(true)
            }
            None if self.done => Ok(false),
            None if self.reader.peek()? == END => {
                self.reader.at += 1;
                self.done = true;
                Ok(false)
            }
            None => Ok(true),
        }
    }

    fn finish(self) -> Result<(), Error> {
        match self.left {
            Some(0) => Ok(()),
            None if self.done => Ok(()),
            _ => fail("value left items unread"),
        }
    }
}

impl<'de> SeqAccess<'de> for Items<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if !self.more()? {
            return Ok(None);
        }
        seed.deserialize(&mut *self.reader).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        self.left
    }
}

impl<'de> MapAccess<'de> for Items<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if !self.more()? {
            return Ok(None);
        }
        seed.deserialize(&mut *self.reader).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(&mut *self.reader)
    }

    fn size_hint(&self) -> Option<usize> {
        self.left
    }
}

struct Variant<'a, 'de> {
    reader: &'a mut Reader<'de>,
    unit: bool,
}

impl<'de> EnumAccess<'de> for Variant<'_, 'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let variant = seed.deserialize(&mut *self.reader)?;
        Ok((variant, self))
    }
}

impl<'de> VariantAccess<'de> for Variant<'_, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        if self.unit {
            return Ok(());
        }
        <()>::deserialize(self.reader)
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        if self.unit {
            return fail("expected a variant with a value, found a unit variant");
        }
        seed.deserialize(self.reader)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value, Error> {
        if self.unit {
            return fail("expected a tuple variant, found a unit variant");
        }
        de::Deserializer::deserialize_any(self.reader, visitor)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if self.unit {
            return fail("expected a struct variant, found a unit variant");
        }
        de::Deserializer::deserialize_any(self.reader, visitor)
    }
}

impl<'de> de::Deserializer<'de> for &mut Reader<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let tag = self.byte()?;
        match tag {
            NULL => visitor.visit_unit(),
            FALSE => visitor.visit_bool(false),
            TRUE => visitor.visit_bool(true),
            UINT => visitor.visit_u64(self.varint()?),
            NINT => visitor.visit_i64((!self.varint()?).cast_signed()),
            FLOAT => {
                let bytes = self.take(8)?;
                let mut raw = [0; 8];
                raw.copy_from_slice(bytes);
                visitor.visit_f64(f64::from_le_bytes(raw))
            }
            SHARE | SHARED | TEXT => visitor.visit_borrowed_str(self.text(tag)?),
            BYTES => {
                let len = self.len()?;
                visitor.visit_borrowed_bytes(self.take(len)?)
            }
            SEQ | SEQ_OPEN => {
                let mut items = Items::new(self, tag == SEQ)?;
                let value = visitor.visit_seq(&mut items)?;
                items.finish()?;
                Ok(value)
            }
            MAP | MAP_OPEN => {
                let mut items = Items::new(self, tag == MAP)?;
                let value = visitor.visit_map(&mut items)?;
                items.finish()?;
                Ok(value)
            }
            other => fail(format!("unknown tag {other}")),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.peek()? == NULL {
            self.at += 1;
            return visitor.visit_none();
        }
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.peek()? {
            SHARE | SHARED | TEXT => visitor.visit_enum(Variant {
                reader: self,
                unit: true,
            }),
            MAP => {
                self.at += 1;
                let count = self.len()?;
                if count != 1 {
                    return fail(format!("expected one variant, found a map of {count}"));
                }
                visitor.visit_enum(Variant {
                    reader: self,
                    unit: false,
                })
            }
            other => fail(format!("expected an enum, found tag {other}")),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::Spread;
    use crate::tasks::Terms;
    use crate::{Event, EventId, Fold, NewTask, Op, TaskId};

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    fn random_fold(rng: &mut Rng) -> Fold {
        let mut fold = Fold::new();
        let events = rng.below(300);
        let mut posted = 0u64;
        for seq in 1..=events {
            let task = TaskId(format!("t{}", rng.below(posted + 1)));
            let op = match rng.below(7) {
                0 if seq == 1 => Op::Charter {},
                0 | 1 | 2 => {
                    posted += 1;
                    Op::Post {
                        task: NewTask {
                            id: TaskId(format!("t{posted}")),
                            parent: (posted > 1 && rng.below(2) == 0)
                                .then(|| TaskId(format!("t{}", 1 + rng.below(posted - 1)))),
                            spec: format!("spec {}", rng.next()),
                            labels: BTreeMap::from([(
                                format!("k{}", rng.below(4)),
                                format!("v{}", rng.below(9)),
                            )]),
                            needs: Vec::new(),
                            terms: Terms::default(),
                        },
                    }
                }
                3 => Op::Claim {
                    task,
                    lease_ms: 1 + rng.below(10_000),
                    fence: None,
                    cap: None,
                },
                4 => Op::Reply {
                    task,
                    body: format!("reply {}", rng.next()),
                },
                5 => Op::Relabel {
                    task,
                    set: BTreeMap::from([(format!("k{}", rng.below(4)), "x".to_owned())]),
                    unset: BTreeSet::from([format!("k{}", rng.below(4))]),
                },
                _ => Op::Rank {
                    task,
                    rank: Some(rng.below(100)),
                },
            };
            let body = serde_json::to_string(&Event {
                id: EventId(format!("user:w{}.{seq}", rng.below(3))),
                by: format!("user:w{}", rng.below(3)),
                at_ms: seq * 10,
                op,
            })
            .expect("an event encodes");
            let _ = fold.apply(seq, &body);
        }
        fold
    }

    #[test]
    fn inline_and_threads_agree_on_random_folds() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..200 {
            let fold = random_fold(&mut rng);
            let inline = fold.snapshot_spread(Spread::Inline);
            assert_eq!(inline, fold.snapshot_spread(Spread::Threads));
            let restored = Fold::restore_spread(&inline, Spread::Inline).expect("restores");
            assert_eq!(restored, fold);
            assert_eq!(
                Fold::restore_spread(&inline, Spread::Threads).expect("restores"),
                restored
            );
            let mut mangled = inline.clone();
            if !mangled.is_empty() {
                let at = rng.below(mangled.len() as u64) as usize;
                mangled[at] ^= 1 << rng.below(8);
            }
            assert_eq!(
                Fold::restore_spread(&mangled, Spread::Inline).map_err(|e| e.to_string()),
                Fold::restore_spread(&mangled, Spread::Threads).map_err(|e| e.to_string()),
            );
        }
    }
}
