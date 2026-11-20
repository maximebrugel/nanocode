use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::io;

use super::repo::{parse_tree, tag_target};
use super::store::{Kind, Oid, apply_delta, corrupt, object_oid};
use crate::codec::{Format, Inflater, InputEnd, Progress, compress, crc32};
use crate::sum_hash::{Algo, Digest};

const SIGNATURE: &[u8] = b"PACK";
const READ_VERSIONS: [u32; 2] = [2, 3];
const WRITE_VERSION: u32 = 2;
const HEAD: usize = 12;
const OID: usize = 20;
const COMMIT: u8 = 1;
const TREE: u8 = 2;
const BLOB: u8 = 3;
const TAG: u8 = 4;
const OFS_DELTA: u8 = 6;
const REF_DELTA: u8 = 7;
const MORE: u8 = 0x80;
const LOW7: u8 = 0x7f;
const LOW4: u8 = 0x0f;
const IDX_MAGIC: [u8; 4] = [0xff, b't', b'O', b'c'];
const IDX_VERSION: u32 = 2;
const FANOUT: usize = 256;
const LARGE: u32 = 0x8000_0000;
const GIT_DEFAULT_ZLIB_LEVEL: u8 = 6;
const MODE_GITLINK: u32 = 0o160000;
const BLOCK_BYTES: usize = size_of::<u128>();
const INSERT_BYTES_MAX: usize = LOW7 as usize;
const COPY: u8 = 0x80;
const COPY_SIZE_BIT: u8 = 0x10;
const COPY_SIZE_BYTES: usize = 3;
const COPY_BYTES_MAX: usize = 0x10000;
const OFS_BYTES_MAX: usize = (usize::BITS as usize).div_ceil(7);

#[derive(Clone, Copy)]
enum Base {
    Whole(Kind),
    Offset(usize),
    Named(Oid),
}

struct Entry {
    offset: usize,
    data: usize,
    end: usize,
    size: usize,
    base: Base,
    crc: u32,
}

pub struct Indexed {
    entries: Vec<Entry>,
    found: Vec<(Oid, Kind)>,
    links: Vec<Vec<Oid>>,
    by_oid: HashMap<Oid, usize>,
    pub checksum: Oid,
    /// One inflater for every `read`: a new one allocates and zeroes its window and
    /// tables, which costs more than inflating a small object.
    inflater: RefCell<Inflater>,
    /// The objects `index_keeping` inflated, by entry, each handed out by its first
    /// `read`: a clone reads every object of its pack once, so keeping them spares
    /// inflating each a second time. Empty for `index`.
    kept: RefCell<Vec<Option<Vec<u8>>>>,
}

fn code(kind: Kind) -> u8 {
    match kind {
        Kind::Commit => COMMIT,
        Kind::Tree => TREE,
        Kind::Blob => BLOB,
        Kind::Tag => TAG,
    }
}

fn byte(pack: &[u8], at: &mut usize) -> io::Result<u8> {
    let b = *pack
        .get(*at)
        .ok_or_else(|| corrupt("the pack ends inside an entry"))?;
    *at += 1;
    Ok(b)
}

fn sha1(data: &[u8]) -> Oid {
    let mut digest = Digest::new(Algo::Sha1);
    digest.update(data);
    Oid::from_bytes(&digest.finish())
}

fn inflate(inflater: &mut Inflater, input: &[u8], size: usize) -> io::Result<(Vec<u8>, usize)> {
    inflater.reset();
    let mut out = Vec::with_capacity(size);
    match inflater.inflate(input, InputEnd::Final, &mut out, size) {
        Ok(Progress::Done { consumed }) if out.len() == size => Ok((out, consumed)),
        _ => Err(corrupt("a pack entry does not inflate to its size")),
    }
}

fn links(kind: Kind, data: &[u8]) -> io::Result<Vec<Oid>> {
    let bad = || corrupt("a pack object names others in a malformed way");
    match kind {
        Kind::Blob => Ok(Vec::new()),
        Kind::Tag => Ok(vec![tag_target(data).ok_or_else(bad)?]),
        Kind::Tree => Ok(parse_tree(data)?
            .into_iter()
            .filter(|entry| entry.mode != MODE_GITLINK)
            .map(|entry| entry.oid)
            .collect()),
        Kind::Commit => {
            let mut out = Vec::new();
            for line in data.split(|&b| b == b'\n') {
                if line.is_empty() {
                    break;
                }
                if let Some(hex) = line
                    .strip_prefix(b"tree ")
                    .or_else(|| line.strip_prefix(b"parent "))
                {
                    out.push(Oid::from_hex(hex).ok_or_else(bad)?);
                }
            }
            if out.is_empty() {
                return Err(bad());
            }
            Ok(out)
        }
    }
}

fn entry(pack: &[u8], offset: usize, end: usize) -> io::Result<(u8, Entry)> {
    let bad = || corrupt("a pack entry's header is malformed");
    let body = &pack[..end];
    let mut at = offset;
    let mut c = byte(body, &mut at)?;
    let kind = (c >> 4) & (LOW7 >> 4);
    let mut size = usize::from(c & LOW4);
    let mut shift = 4u32;
    while c & MORE != 0 {
        c = byte(body, &mut at)?;
        let part = usize::from(c & LOW7)
            .checked_shl(shift)
            .filter(|part| part >> shift == usize::from(c & LOW7))
            .ok_or_else(bad)?;
        size |= part;
        shift += 7;
    }
    let base = match kind {
        COMMIT => Base::Whole(Kind::Commit),
        TREE => Base::Whole(Kind::Tree),
        BLOB => Base::Whole(Kind::Blob),
        TAG => Base::Whole(Kind::Tag),
        OFS_DELTA => {
            let mut c = byte(body, &mut at)?;
            let mut back = usize::from(c & LOW7);
            while c & MORE != 0 {
                c = byte(body, &mut at)?;
                back = back
                    .checked_add(1)
                    .and_then(|b| b.checked_mul(usize::from(MORE)))
                    .ok_or_else(bad)?
                    | usize::from(c & LOW7);
            }
            match offset.checked_sub(back) {
                Some(base) if back > 0 => Base::Offset(base),
                _ => return Err(bad()),
            }
        }
        REF_DELTA => {
            let name = body.get(at..at + OID).ok_or_else(bad)?;
            at += OID;
            Base::Named(Oid::from_bytes(name))
        }
        _ => return Err(corrupt("a pack entry has an unknown type")),
    };
    Ok((
        kind,
        Entry {
            offset,
            data: at,
            end: at,
            size,
            base,
            crc: 0,
        },
    ))
}

pub fn index(pack: &[u8]) -> io::Result<Indexed> {
    index_with(pack, false)
}

/// `index`, keeping every object it inflates for one `read` each.
pub fn index_keeping(pack: &[u8]) -> io::Result<Indexed> {
    index_with(pack, true)
}

fn index_with(pack: &[u8], keep: bool) -> io::Result<Indexed> {
    if pack.len() < HEAD + OID || &pack[..SIGNATURE.len()] != SIGNATURE {
        return Err(corrupt("the remote did not send a pack"));
    }
    let version = u32::from_be_bytes([pack[4], pack[5], pack[6], pack[7]]);
    if !READ_VERSIONS.contains(&version) {
        return Err(corrupt("the pack is of a version this git does not read"));
    }
    let count = u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]) as usize;
    let end = pack.len() - OID;
    let checksum = Oid::from_bytes(&pack[end..]);
    if sha1(&pack[..end]) != checksum {
        return Err(corrupt("the pack's checksum does not match its content"));
    }
    let mut inflater = Inflater::new(Format::Zlib);
    let mut entries: Vec<Entry> = Vec::new();
    let mut found: Vec<Option<(Oid, Kind)>> = Vec::new();
    let mut named: Vec<Vec<Oid>> = Vec::new();
    let mut by_offset = HashMap::new();
    let mut kept: Vec<Option<Vec<u8>>> = Vec::new();
    let mut at = HEAD;
    for _ in 0..count {
        let (_, mut item) = entry(pack, at, end)?;
        let (data, consumed) = inflate(&mut inflater, &pack[item.data..end], item.size)?;
        item.end = item.data + consumed;
        item.crc = crc32(&pack[item.offset..item.end]);
        by_offset.insert(item.offset, entries.len());
        found.push(match item.base {
            Base::Whole(kind) => Some((object_oid(kind, &data), kind)),
            Base::Offset(_) | Base::Named(_) => None,
        });
        named.push(match item.base {
            Base::Whole(kind) => links(kind, &data)?,
            Base::Offset(_) | Base::Named(_) => Vec::new(),
        });
        if keep {
            kept.push(match item.base {
                Base::Whole(_) => Some(data),
                Base::Offset(_) | Base::Named(_) => None,
            });
        }
        at = item.end;
        entries.push(item);
    }
    if at != end {
        return Err(corrupt("the pack holds more than its entries"));
    }
    let mut by_base: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut by_name: HashMap<Oid, Vec<usize>> = HashMap::new();
    for (i, item) in entries.iter().enumerate() {
        match item.base {
            Base::Whole(_) => {}
            Base::Offset(base) => {
                let base = *by_offset
                    .get(&base)
                    .ok_or_else(|| corrupt("a delta's base is not an entry of the pack"))?;
                by_base.entry(base).or_default().push(i);
            }
            Base::Named(name) => by_name.entry(name).or_default().push(i),
        }
    }
    for root in 0..entries.len() {
        let Base::Whole(kind) = entries[root].base else {
            continue;
        };
        let Some((oid, _)) = found[root] else {
            continue;
        };
        if !by_base.contains_key(&root) && !by_name.contains_key(&oid) {
            continue;
        }
        let item = &entries[root];
        let data = match kept.get(root) {
            Some(Some(data)) => data.clone(),
            _ => inflate(&mut inflater, &pack[item.data..item.end], item.size)?.0,
        };
        let mut stack = vec![(root, oid, data)];
        while let Some((at, oid, data)) = stack.pop() {
            let mut children = by_base.remove(&at).unwrap_or_default();
            children.extend(by_name.remove(&oid).unwrap_or_default());
            for child in children {
                let item = &entries[child];
                let (delta, _) = inflate(&mut inflater, &pack[item.data..item.end], item.size)?;
                let whole = apply_delta(&data, &delta)?;
                let oid = object_oid(kind, &whole);
                found[child] = Some((oid, kind));
                named[child] = links(kind, &whole)?;
                if keep {
                    kept[child] = Some(whole.clone());
                }
                stack.push((child, oid, whole));
            }
        }
    }
    let found: Vec<(Oid, Kind)> = found
        .into_iter()
        .collect::<Option<_>>()
        .ok_or_else(|| corrupt("the pack has deltas whose bases it does not hold"))?;
    let mut by_oid = HashMap::new();
    for (i, (oid, _)) in found.iter().enumerate() {
        by_oid.entry(*oid).or_insert(i);
    }
    Ok(Indexed {
        entries,
        found,
        links: named,
        by_oid,
        checksum,
        inflater: RefCell::new(inflater),
        kept: RefCell::new(kept),
    })
}

impl Indexed {
    pub fn outside(&self, tips: &[Oid]) -> Vec<Oid> {
        let mut seen = std::collections::HashSet::new();
        let mut outside = Vec::new();
        let mut stack = tips.to_vec();
        while let Some(oid) = stack.pop() {
            if !seen.insert(oid) {
                continue;
            }
            match self.by_oid.get(&oid) {
                Some(&i) => stack.extend(self.links[i].iter().copied()),
                None => outside.push(oid),
            }
        }
        outside
    }

    pub fn kind(&self, oid: &Oid) -> Option<Kind> {
        self.by_oid.get(oid).map(|&i| self.found[i].1)
    }

    pub fn read(&self, pack: &[u8], oid: &Oid) -> io::Result<Option<(Kind, Vec<u8>)>> {
        let Some(&first) = self.by_oid.get(oid) else {
            return Ok(None);
        };
        if let Some(data) = self.kept.borrow_mut().get_mut(first).and_then(Option::take) {
            return Ok(Some((self.found[first].1, data)));
        }
        let mut chain = vec![first];
        loop {
            let last = chain[chain.len() - 1];
            let next = match self.entries[last].base {
                Base::Whole(_) => break,
                Base::Offset(offset) => self
                    .entries
                    .binary_search_by_key(&offset, |item| item.offset)
                    .map_err(|_| corrupt("a delta's base is not an entry of the pack"))?,
                Base::Named(name) => *self
                    .by_oid
                    .get(&name)
                    .ok_or_else(|| corrupt("the pack has deltas whose bases it does not hold"))?,
            };
            chain.push(next);
        }
        let mut inflater = self.inflater.borrow_mut();
        let mut data = Vec::new();
        for (step, &i) in chain.iter().rev().enumerate() {
            let item = &self.entries[i];
            let (part, _) = inflate(&mut inflater, &pack[item.data..item.end], item.size)?;
            data = if step == 0 {
                part
            } else {
                apply_delta(&data, &part)?
            };
        }
        Ok(Some((self.found[first].1, data)))
    }

    pub fn idx(&self) -> Vec<u8> {
        let mut sorted: Vec<(Oid, u32, u64)> = self
            .by_oid
            .values()
            .map(|&i| {
                let item = &self.entries[i];
                (self.found[i].0, item.crc, item.offset as u64)
            })
            .collect();
        sorted.sort_by_key(|a| a.0.0);
        let mut out = Vec::new();
        out.extend_from_slice(&IDX_MAGIC);
        out.extend_from_slice(&IDX_VERSION.to_be_bytes());
        let mut counts = [0u32; FANOUT];
        for (oid, _, _) in &sorted {
            counts[usize::from(oid.0[0])] += 1;
        }
        let mut total = 0u32;
        for count in counts {
            total += count;
            out.extend_from_slice(&total.to_be_bytes());
        }
        for (oid, _, _) in &sorted {
            out.extend_from_slice(&oid.0);
        }
        for (_, crc, _) in &sorted {
            out.extend_from_slice(&crc.to_be_bytes());
        }
        let mut large = Vec::new();
        for (_, _, offset) in &sorted {
            match u32::try_from(*offset) {
                Ok(small) if small < LARGE => out.extend_from_slice(&small.to_be_bytes()),
                _ => {
                    let slot = LARGE | large.len() as u32;
                    out.extend_from_slice(&slot.to_be_bytes());
                    large.push(*offset);
                }
            }
        }
        for offset in large {
            out.extend_from_slice(&offset.to_be_bytes());
        }
        out.extend_from_slice(&self.checksum.0);
        let own = sha1(&out);
        out.extend_from_slice(&own.0);
        out
    }
}

pub struct Object<'a> {
    pub kind: Kind,
    pub data: &'a [u8],
    pub name: u32,
}

#[derive(Clone, Copy)]
pub struct Deltas {
    pub window: usize,
    pub depth: usize,
}

struct Source<'a> {
    kind: Kind,
    data: &'a [u8],
    offset: usize,
    depth: usize,
    blocks: HashMap<u128, usize>,
}

pub fn name_hash(name: &[u8]) -> u32 {
    name.iter()
        .filter(|c| !c.is_ascii_whitespace())
        .fold(0u32, |hash, &c| {
            (hash >> 2).wrapping_add(u32::from(c) << 24)
        })
}

fn block(data: &[u8], at: usize) -> u128 {
    let mut word = [0u8; BLOCK_BYTES];
    word.copy_from_slice(&data[at..at + BLOCK_BYTES]);
    u128::from_le_bytes(word)
}

fn varint(out: &mut Vec<u8>, mut value: usize) {
    while value > usize::from(LOW7) {
        out.push(value as u8 & LOW7 | MORE);
        value >>= 7;
    }
    out.push(value as u8);
}

fn header(out: &mut Vec<u8>, code: u8, mut size: usize) {
    let mut c = (code << 4) | (size as u8 & LOW4);
    size >>= 4;
    while size > 0 {
        out.push(c | MORE);
        c = size as u8 & LOW7;
        size >>= 7;
    }
    out.push(c);
}

fn back(out: &mut Vec<u8>, mut distance: usize) {
    assert!(distance > 0);
    let mut bytes = [0u8; OFS_BYTES_MAX];
    let mut at = OFS_BYTES_MAX - 1;
    bytes[at] = distance as u8 & LOW7;
    distance >>= 7;
    while distance > 0 {
        distance -= 1;
        at -= 1;
        bytes[at] = MORE | (distance as u8 & LOW7);
        distance >>= 7;
    }
    out.extend_from_slice(&bytes[at..]);
}

fn insert(out: &mut Vec<u8>, data: &[u8]) {
    for part in data.chunks(INSERT_BYTES_MAX) {
        out.push(part.len() as u8);
        out.extend_from_slice(part);
    }
}

fn copy(out: &mut Vec<u8>, mut offset: usize, mut size: usize) {
    while size > 0 {
        let part = size.min(COPY_BYTES_MAX);
        let op = out.len();
        out.push(COPY);
        let offset_bytes = u32::try_from(offset)
            .expect("a delta source fits the copy offset")
            .to_le_bytes();
        for (bit, byte) in offset_bytes.into_iter().enumerate() {
            if byte != 0 {
                out[op] |= 1 << bit;
                out.push(byte);
            }
        }
        let size_bytes = (part as u32).to_le_bytes();
        for (bit, &byte) in size_bytes[..COPY_SIZE_BYTES].iter().enumerate() {
            if byte != 0 {
                out[op] |= COPY_SIZE_BIT << bit;
                out.push(byte);
            }
        }
        offset += part;
        size -= part;
    }
}

impl<'a> Source<'a> {
    fn new(kind: Kind, data: &'a [u8], offset: usize, depth: usize) -> Self {
        let mut blocks = HashMap::with_capacity(data.len() / BLOCK_BYTES);
        if u32::try_from(data.len()).is_ok() {
            for at in (0..data.len().saturating_sub(BLOCK_BYTES - 1)).step_by(BLOCK_BYTES) {
                blocks.entry(block(data, at)).or_insert(at);
            }
        }
        Source {
            kind,
            data,
            offset,
            depth,
            blocks,
        }
    }

    fn delta(&self, target: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, self.data.len());
        varint(&mut out, target.len());
        let mut pending = 0;
        let mut at = 0;
        while at + BLOCK_BYTES <= target.len() {
            let Some(&found) = self.blocks.get(&block(target, at)) else {
                at += 1;
                continue;
            };
            let behind = self.data[..found]
                .iter()
                .rev()
                .zip(target[pending..at].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let ahead = self.data[found..]
                .iter()
                .zip(&target[at..])
                .take_while(|(a, b)| a == b)
                .count();
            assert!(ahead >= BLOCK_BYTES);
            insert(&mut out, &target[pending..at - behind]);
            copy(&mut out, found - behind, behind + ahead);
            at += ahead;
            pending = at;
        }
        insert(&mut out, &target[pending..]);
        out
    }
}

fn best_delta(
    window: &VecDeque<Source<'_>>,
    object: &Object<'_>,
    deltas: Deltas,
) -> Option<(usize, Vec<u8>)> {
    let mut best: Option<(usize, Vec<u8>)> = None;
    for (i, source) in window.iter().enumerate() {
        let bound = best
            .as_ref()
            .map_or(object.data.len(), |(_, delta)| delta.len());
        if source.kind != object.kind
            || source.depth >= deltas.depth
            || source.blocks.is_empty()
            || object.data.len().saturating_sub(source.data.len()) >= bound
        {
            continue;
        }
        let delta = source.delta(object.data);
        if delta.len() < bound {
            best = Some((i, delta));
        }
    }
    best
}

pub fn write(objects: &[Object<'_>], deltas: Deltas) -> Vec<u8> {
    let mut order: Vec<usize> = (0..objects.len()).collect();
    order.sort_by_key(|&i| {
        let object = &objects[i];
        (
            code(object.kind),
            object.name,
            Reverse(object.data.len()),
            i,
        )
    });
    let mut out = Vec::new();
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&WRITE_VERSION.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    let mut window: VecDeque<Source<'_>> = VecDeque::with_capacity(deltas.window);
    for i in order {
        let object = &objects[i];
        let offset = out.len();
        let whole = compress(object.data, GIT_DEFAULT_ZLIB_LEVEL, Format::Zlib);
        let chosen = best_delta(&window, object, deltas).and_then(|(base, delta)| {
            let packed = compress(&delta, GIT_DEFAULT_ZLIB_LEVEL, Format::Zlib);
            (packed.len() < whole.len()).then_some((base, delta, packed))
        });
        let depth = match chosen {
            Some((base, delta, packed)) => {
                let source = &window[base];
                assert!(apply_delta(source.data, &delta).is_ok_and(|whole| whole == object.data));
                header(&mut out, OFS_DELTA, delta.len());
                back(&mut out, offset - source.offset);
                out.extend_from_slice(&packed);
                source.depth + 1
            }
            None => {
                header(&mut out, code(object.kind), object.data.len());
                out.extend_from_slice(&whole);
                0
            }
        };
        if deltas.window > 0 {
            if window.len() == deltas.window {
                window.pop_front();
            }
            window.push_back(Source::new(object.kind, object.data, offset, depth));
        }
    }
    let own = sha1(&out);
    out.extend_from_slice(&own.0);
    out
}
