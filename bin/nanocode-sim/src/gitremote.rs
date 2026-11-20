use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    ops::Range,
    pin::Pin,
    process::ExitCode,
    rc::Rc,
    time::Duration,
};

use nanocode_bytes::Bytes;
use nanocode_compression::inflate::{Inflate, InflateFlush, InflateStatus};
use nanocode_compression::miniz::{Format, Level, compress};
use nanocode_digest::Sha1;
use nanocode_shell::{
    Config, Credential, Credentials, Ended, Failure, Fetch, FileSystem, HostInfo, Identity, Kind,
    Limits, Machine, MemFs, NoCommands, Request, Response, Script, Shell,
};
use nanocode_sync::block_on;

use crate::rng::{FNV_OFFSET, Rng, fnv};

const HOST: &str = "sim.test";
const REPO: &str = "/team/app.git";
const MAIN: &str = "refs/heads/main";
const USER: &str = "agent";
const WORK: &str = "/work/app";
const SECRET_BYTES: usize = 24;
const OID: usize = 20;
const ZERO: [u8; OID] = [0; OID];
const PPM_DELTA: u64 = 500_000;
const PPM_REF_DELTA: u64 = 300_000;
const PPM_USERINFO: u64 = 500_000;
const PPM_QUIET: u64 = 300_000;
const PPM_SHALLOW_CLONE: u64 = 400_000;
const PPM_UNSHALLOW: u64 = 250_000;
const PPM_FORCE: u64 = 250_000;
const PPM_WILDCARD: u64 = 600_000;
const PPM_REFUSE: u64 = 150_000;
const PPM_SPEC_PRUNE: u64 = 400_000;
const PPM_TOPIC_REWRITE: u64 = 500_000;
const PPM_TOPIC_DELETE: u64 = 300_000;
const DEPTH_MAX: u64 = 3;
const TOPICS_MAX: u64 = 2;
const INFINITE_DEPTH: u64 = 0x7fff_ffff;
const FILE_NAMES: [&str; 6] = [
    "README.md",
    "src/lib.rs",
    "src/main.rs",
    "docs/a.txt",
    "b",
    "src/deep/c.rs",
];
const OPS_MIN: u64 = 4;
const OPS_MAX: u64 = 20;
const OP_KINDS: u64 = 10;
const LOCAL_COMMITS_MAX: u64 = 3;
const SCRIPT_BUDGET: u32 = 1 << 20;
const BLOB: u8 = 3;
const TREE: u8 = 2;
const COMMIT: u8 = 1;
const OFS_DELTA: u8 = 6;
const REF_DELTA: u8 = 7;
const HTTP_OK: u16 = 200;
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_NOT_FOUND: u16 = 404;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitRemoteMutation {
    None,
    StoresSecret,
    UpdatesRefOnBadPack,
    ServerDropsObject,
    ServerKeepsRef,
    StaleTracking,
    PushWrongBase,
    ForgetsShallow,
    KeepsPruned,
    ForcesRefspec,
}

impl GitRemoteMutation {
    const ALL: [Self; 10] = [
        Self::None,
        Self::StoresSecret,
        Self::UpdatesRefOnBadPack,
        Self::ServerDropsObject,
        Self::ServerKeepsRef,
        Self::StaleTracking,
        Self::PushWrongBase,
        Self::ForgetsShallow,
        Self::KeepsPruned,
        Self::ForcesRefspec,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::StoresSecret => "stores_secret",
            Self::UpdatesRefOnBadPack => "updates_ref_on_bad_pack",
            Self::ServerDropsObject => "server_drops_object",
            Self::ServerKeepsRef => "server_keeps_ref",
            Self::StaleTracking => "stale_tracking",
            Self::PushWrongBase => "push_wrong_base",
            Self::ForgetsShallow => "forgets_shallow",
            Self::KeepsPruned => "keeps_pruned",
            Self::ForcesRefspec => "forces_refspec",
        }
    }

    pub fn names() -> String {
        Self::ALL[1..]
            .iter()
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GitRemoteStats {
    pub clones: u64,
    pub fetches: u64,
    pub pulls: u64,
    pub pushes: u64,
    pub rejected_pushes: u64,
    pub bad_packs: u64,
    pub deltas: u64,
    pub pushed_deltas: u64,
    pub credential_asks: u64,
    pub shallow_clones: u64,
    pub shallow_fetches: u64,
    pub unshallows: u64,
    pub pruned: u64,
    pub spec_fetches: u64,
    pub rejected_refs: u64,
    pub refused: u64,
}

impl GitRemoteStats {
    pub const fn add(&mut self, other: &Self) {
        self.clones += other.clones;
        self.fetches += other.fetches;
        self.pulls += other.pulls;
        self.pushes += other.pushes;
        self.rejected_pushes += other.rejected_pushes;
        self.bad_packs += other.bad_packs;
        self.deltas += other.deltas;
        self.pushed_deltas += other.pushed_deltas;
        self.credential_asks += other.credential_asks;
        self.shallow_clones += other.shallow_clones;
        self.shallow_fetches += other.shallow_fetches;
        self.unshallows += other.unshallows;
        self.pruned += other.pruned;
        self.spec_fetches += other.spec_fetches;
        self.rejected_refs += other.rejected_refs;
        self.refused += other.refused;
    }
}

pub struct GitRemoteOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: GitRemoteStats,
    shape: String,
}

type Oid = [u8; OID];

fn hex(oid: &Oid) -> String {
    oid.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &[u8]) -> Option<Oid> {
    let text = std::str::from_utf8(text.get(..2 * OID)?).ok()?;
    let mut oid = ZERO;
    for (i, slot) in oid.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(oid)
}

fn object_id(kind: u8, data: &[u8]) -> Oid {
    let name = match kind {
        COMMIT => "commit",
        TREE => "tree",
        _ => "blob",
    };
    let mut whole = format!("{name} {}\0", data.len()).into_bytes();
    whole.extend_from_slice(data);
    Sha1::digest(&whole)
}

fn pkt(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
    out.extend_from_slice(data);
}

type Commands<'a> = (Vec<Option<Vec<u8>>>, &'a [u8]);

fn pkts(mut body: &[u8]) -> Option<Commands<'_>> {
    let mut lines = Vec::new();
    while body.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&body[..4]).ok()?, 16).ok()?;
        if len == 0 {
            lines.push(None);
            body = &body[4..];
            if lines.iter().filter(|line| line.is_none()).count() == 1 {
                return Some((lines, body));
            }
            continue;
        }
        let line = body.get(4..len)?;
        lines.push(Some(line.to_vec()));
        body = &body[len..];
    }
    Some((lines, body))
}

fn every_line(mut body: &[u8]) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    while body.len() >= 4 {
        let Some(len) = std::str::from_utf8(&body[..4])
            .ok()
            .and_then(|head| usize::from_str_radix(head, 16).ok())
        else {
            break;
        };
        if len < 4 {
            body = &body[4..];
            continue;
        }
        let Some(line) = body.get(4..len) else {
            break;
        };
        lines.push(line.to_vec());
        body = &body[len..];
    }
    lines
}

fn base64(data: &[u8]) -> String {
    const SYMBOLS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let mut wide = [0u8; 3];
        wide[..chunk.len()].copy_from_slice(chunk);
        let bits = u32::from(wide[0]) << 16 | u32::from(wide[1]) << 8 | u32::from(wide[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(SYMBOLS[(bits >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Corruption {
    Truncate,
    FlipChecksum,
    FlipResealed,
    DropWanted,
}

const CORRUPTIONS: [Corruption; 4] = [
    Corruption::Truncate,
    Corruption::FlipChecksum,
    Corruption::FlipResealed,
    Corruption::DropWanted,
];

struct Server {
    objects: RefCell<BTreeMap<Oid, (u8, Vec<u8>)>>,
    refs: RefCell<BTreeMap<String, Oid>>,
    secret: String,
    rng: RefCell<Rng>,
    corrupt: Cell<Option<Corruption>>,
    mutation: GitRemoteMutation,
    leaks: RefCell<Vec<String>>,
    deltas: Cell<u64>,
    pushed_deltas: Cell<u64>,
    received: RefCell<Option<Result<BTreeSet<Oid>, String>>>,
    trace: RefCell<Vec<String>>,
}

struct Entry {
    start: usize,
    kind: u8,
    base: Option<usize>,
    distance_end: usize,
    data: Vec<u8>,
}

fn entries(pack: &[u8]) -> Result<Vec<Entry>, String> {
    if pack.len() < 12 + OID || &pack[..4] != b"PACK" {
        return Err("not a pack".to_owned());
    }
    let end = pack.len() - OID;
    let count = u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]) as usize;
    let byte_at = |at: usize| {
        pack[..end]
            .get(at)
            .copied()
            .ok_or("an entry runs past the pack")
    };
    let mut at = 12;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let start = at;
        let mut byte = byte_at(at)?;
        at += 1;
        let kind = (byte >> 4) & 7;
        let mut size = usize::from(byte & 0x0f);
        let mut shift = 4;
        while byte & 0x80 != 0 {
            byte = byte_at(at)?;
            at += 1;
            size |= usize::from(byte & 0x7f) << shift;
            shift += 7;
        }
        let base = match kind {
            COMMIT | TREE | BLOB => None,
            OFS_DELTA => {
                let mut byte = byte_at(at)?;
                at += 1;
                let mut distance = usize::from(byte & 0x7f);
                while byte & 0x80 != 0 {
                    byte = byte_at(at)?;
                    at += 1;
                    distance = ((distance + 1) << 7) | usize::from(byte & 0x7f);
                }
                Some(
                    start
                        .checked_sub(distance)
                        .filter(|_| distance > 0)
                        .ok_or("a delta's base lies before the pack")?,
                )
            }
            _ => return Err(format!("the client sent a type {kind} entry")),
        };
        let distance_end = at;
        let mut state = Inflate::new(15).map_err(|_| "inflate".to_owned())?;
        let mut data = vec![0; size + 1];
        match state.inflate(&pack[at..end], &mut data, InflateFlush::NoFlush) {
            Ok(InflateStatus::StreamEnd) if state.total_out() as usize == size => {}
            _ => return Err("an entry does not inflate to its size".to_owned()),
        }
        data.truncate(size);
        at += state.total_in() as usize;
        out.push(Entry {
            start,
            kind,
            base,
            distance_end,
            data,
        });
    }
    if at != end {
        return Err("trailing bytes".to_owned());
    }
    Ok(out)
}

fn read_size(delta: &[u8], at: &mut usize) -> Result<usize, String> {
    let mut size = 0;
    let mut shift = 0;
    loop {
        let byte = *delta.get(*at).ok_or("a delta's header is cut short")?;
        *at += 1;
        size |= usize::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Ok(size);
        }
    }
}

fn apply(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let mut at = 0;
    if read_size(delta, &mut at)? != base.len() {
        return Err("a delta names a base of another size".to_owned());
    }
    let size = read_size(delta, &mut at)?;
    let mut out = Vec::with_capacity(size);
    while at < delta.len() {
        let op = delta[at];
        at += 1;
        if op & 0x80 == 0 {
            let len = usize::from(op);
            let data = delta
                .get(at..at + len)
                .filter(|_| len > 0)
                .ok_or("a delta's insert is cut short")?;
            out.extend_from_slice(data);
            at += len;
            continue;
        }
        let mut word = [0usize; 2];
        for (bit, slot) in (0..7).map(|bit| (bit, usize::from(bit >= 4))) {
            if op & (1 << bit) != 0 {
                let byte = *delta.get(at).ok_or("a delta's copy is cut short")?;
                at += 1;
                let shift = 8 * if bit >= 4 { bit - 4 } else { bit };
                word[slot] |= usize::from(byte) << shift;
            }
        }
        let [offset, len] = word;
        let len = if len == 0 { 0x10000 } else { len };
        let data = base
            .get(offset..offset + len)
            .ok_or("a delta copies past its base")?;
        out.extend_from_slice(data);
    }
    if out.len() != size {
        return Err("a delta resolves to another size".to_owned());
    }
    Ok(out)
}

fn wrong_base(pack: &[u8]) -> Vec<u8> {
    let mut out = pack.to_vec();
    let Ok(found) = entries(pack) else {
        return out;
    };
    let Some(entry) = found.iter().find(|entry| entry.base.is_some()) else {
        return out;
    };
    out[entry.distance_end - 1] ^= 1;
    let end = out.len() - OID;
    let seal = Sha1::digest(&out[..end]);
    out[end..].copy_from_slice(&seal[..]);
    out
}

fn respond(status: u16, kind: &str, body: Vec<u8>) -> Response {
    Response {
        protocol: "HTTP/1.1".to_owned(),
        status,
        reason: String::new(),
        headers: if kind.is_empty() {
            Vec::new()
        } else {
            vec![("Content-Type".to_owned(), kind.to_owned())]
        },
        body: Bytes::from(body),
        url: String::new(),
        redirects: 0,
    }
}

fn varint_size(out: &mut Vec<u8>, mut size: usize) {
    loop {
        let byte = (size & 0x7f) as u8;
        size >>= 7;
        if size == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    varint_size(&mut out, base.len());
    varint_size(&mut out, target.len());
    let common = base
        .iter()
        .zip(target)
        .take_while(|(a, b)| a == b)
        .count()
        .min(0xffff);
    if common > 0 {
        out.push(0x80 | 0x10 | 0x20);
        out.push((common & 0xff) as u8);
        out.push((common >> 8) as u8);
    }
    for chunk in target[common..].chunks(0x7f) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out
}

fn entry_header(out: &mut Vec<u8>, kind: u8, mut size: usize) {
    let mut byte = (kind << 4) | (size & 0x0f) as u8;
    size >>= 4;
    while size > 0 {
        out.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    out.push(byte);
}

fn ofs(out: &mut Vec<u8>, mut back: usize) {
    let mut bytes = vec![(back & 0x7f) as u8];
    back >>= 7;
    while back > 0 {
        back -= 1;
        bytes.push(0x80 | (back & 0x7f) as u8);
        back >>= 7;
    }
    bytes.reverse();
    out.extend_from_slice(&bytes);
}

fn parse_commit(data: &[u8]) -> (Oid, Vec<Oid>) {
    let mut tree = ZERO;
    let mut parents = Vec::new();
    for line in data.split(|&byte| byte == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(b"tree ") {
            tree = unhex(rest).unwrap_or(ZERO);
        } else if let Some(rest) = line.strip_prefix(b"parent ") {
            parents.extend(unhex(rest));
        }
    }
    (tree, parents)
}

fn parse_tree(mut data: &[u8]) -> Vec<(Vec<u8>, bool, Oid)> {
    let mut entries = Vec::new();
    while let Some(space) = data.iter().position(|&byte| byte == b' ') {
        let mode = &data[..space];
        let Some(nul) = data[space..].iter().position(|&byte| byte == 0) else {
            break;
        };
        let name = data[space + 1..space + nul].to_vec();
        let start = space + nul + 1;
        let Some(raw) = data.get(start..start + OID) else {
            break;
        };
        let mut oid = ZERO;
        oid.copy_from_slice(raw);
        entries.push((name, mode == b"40000", oid));
        data = &data[start + OID..];
    }
    entries
}

impl Server {
    fn reach(
        &self,
        tips: &[Oid],
        stop: &BTreeSet<Oid>,
        cut: &BTreeSet<Oid>,
    ) -> Result<Vec<Oid>, String> {
        let objects = self.objects.borrow();
        let mut seen = BTreeSet::new();
        let mut order = Vec::new();
        let mut stack: Vec<Oid> = tips.to_vec();
        while let Some(oid) = stack.pop() {
            if stop.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            let (kind, data) = objects
                .get(&oid)
                .ok_or_else(|| format!("server lacks object {}", hex(&oid)))?;
            order.push(oid);
            match *kind {
                COMMIT => {
                    let (tree, parents) = parse_commit(data);
                    stack.push(tree);
                    if !cut.contains(&oid) {
                        stack.extend(parents);
                    }
                }
                TREE => stack.extend(parse_tree(data).into_iter().map(|(_, _, oid)| oid)),
                _ => {}
            }
        }
        Ok(order)
    }

    fn within(&self, tips: &[Oid], depth: u64) -> (BTreeSet<Oid>, BTreeSet<Oid>) {
        let objects = self.objects.borrow();
        let mut level: BTreeSet<Oid> = BTreeSet::new();
        let mut boundary = BTreeSet::new();
        let mut queue: VecDeque<(Oid, u64)> = tips.iter().map(|tip| (*tip, 1)).collect();
        while let Some((oid, at)) = queue.pop_front() {
            let Some((COMMIT, data)) = objects.get(&oid).map(|(k, d)| (*k, d)) else {
                continue;
            };
            if !level.insert(oid) {
                continue;
            }
            let (_, parents) = parse_commit(data);
            if at >= depth {
                if !parents.is_empty() {
                    boundary.insert(oid);
                }
                continue;
            }
            queue.extend(parents.into_iter().map(|parent| (parent, at + 1)));
        }
        (level, boundary)
    }

    fn descends(&self, new: Oid, old: Oid, cut: &BTreeSet<Oid>) -> bool {
        let objects = self.objects.borrow();
        let mut seen = BTreeSet::new();
        let mut stack = vec![new];
        while let Some(oid) = stack.pop() {
            if oid == old {
                return true;
            }
            if cut.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            if let Some((COMMIT, data)) = objects.get(&oid).map(|(k, d)| (*k, d)) {
                stack.extend(parse_commit(data).1);
            }
        }
        false
    }

    fn walk(&self, tip: Oid, shallow: &BTreeSet<Oid>) -> Vec<Oid> {
        let objects = self.objects.borrow();
        let mut out = Vec::new();
        let mut at = Some(tip);
        while let Some(oid) = at {
            out.push(oid);
            at = None;
            if shallow.contains(&oid) {
                break;
            }
            if let Some((COMMIT, data)) = objects.get(&oid).map(|(k, d)| (*k, d)) {
                at = parse_commit(data).1.first().copied();
            }
        }
        out
    }

    fn advert(&self, service: &str) -> Vec<u8> {
        let mut body = Vec::new();
        pkt(&mut body, format!("# service={service}\n").as_bytes());
        body.extend_from_slice(b"0000");
        let refs = self.refs.borrow();
        let caps = if service == "git-upload-pack" {
            format!("multi_ack_detailed shallow ofs-delta symref=HEAD:{MAIN} agent=git/sim")
        } else {
            "report-status delete-refs ofs-delta agent=git/sim".to_owned()
        };
        let mut lines: Vec<(String, Oid)> = Vec::new();
        if service == "git-upload-pack"
            && let Some(main) = refs.get(MAIN)
        {
            lines.push(("HEAD".to_owned(), *main));
        }
        lines.extend(refs.iter().map(|(name, oid)| (name.clone(), *oid)));
        if lines.is_empty() {
            pkt(
                &mut body,
                format!("{} capabilities^{{}}\0{caps}\n", hex(&ZERO)).as_bytes(),
            );
        }
        for (i, (name, oid)) in lines.iter().enumerate() {
            let line = if i == 0 {
                format!("{} {name}\0{caps}\n", hex(oid))
            } else {
                format!("{} {name}\n", hex(oid))
            };
            pkt(&mut body, line.as_bytes());
        }
        body.extend_from_slice(b"0000");
        body
    }

    fn pack(&self, oids: &[Oid]) -> Vec<u8> {
        let objects = self.objects.borrow();
        let mut rng = self.rng.borrow_mut();
        let mut out = b"PACK".to_vec();
        out.extend_from_slice(&2u32.to_be_bytes());
        out.extend_from_slice(&(oids.len() as u32).to_be_bytes());
        let mut placed: Vec<(Oid, usize)> = Vec::new();
        for oid in oids {
            let (kind, data) = &objects[oid];
            let offset = out.len();
            let base = placed
                .iter()
                .rev()
                .find(|(other, _)| objects[other].0 == *kind)
                .copied();
            match base {
                Some((base, at)) if rng.chance(PPM_DELTA) => {
                    let body = delta(&objects[&base].1, data);
                    if rng.chance(PPM_REF_DELTA) {
                        entry_header(&mut out, REF_DELTA, body.len());
                        out.extend_from_slice(&base);
                    } else {
                        entry_header(&mut out, OFS_DELTA, body.len());
                        ofs(&mut out, offset - at);
                    }
                    out.extend_from_slice(&compress(&body, Level::DEFAULT, Format::Zlib));
                    self.deltas.set(self.deltas.get() + 1);
                }
                _ => {
                    entry_header(&mut out, *kind, data.len());
                    out.extend_from_slice(&compress(data, Level::DEFAULT, Format::Zlib));
                }
            }
            placed.push((*oid, offset));
        }
        let sum = Sha1::digest(&out);
        out.extend_from_slice(&sum);
        out
    }

    fn upload(&self, body: &[u8]) -> Response {
        let mut wants = Vec::new();
        let mut haves = Vec::new();
        let mut client_shallow = BTreeSet::new();
        let mut depth = None;
        let lines = every_line(body);
        for line in lines {
            if let Some(oid) = line.strip_prefix(b"want ").and_then(unhex) {
                wants.push(oid);
            } else if let Some(oid) = line.strip_prefix(b"have ").and_then(unhex) {
                haves.push(oid);
            } else if let Some(oid) = line.strip_prefix(b"shallow ").and_then(unhex) {
                client_shallow.insert(oid);
            } else if let Some(rest) = line.strip_prefix(b"deepen ") {
                depth = String::from_utf8_lossy(rest).trim().parse::<u64>().ok();
            }
        }
        let common: Vec<Oid> = haves
            .iter()
            .filter(|oid| self.objects.borrow().contains_key(*oid))
            .copied()
            .collect();
        let none = BTreeSet::new();
        let mut update = Vec::new();
        let reached = match depth {
            None => match self.reach(&common, &none, &client_shallow) {
                Ok(old) => self.reach(&wants, &old.into_iter().collect(), &none),
                Err(why) => Err(why),
            },
            Some(depth) => {
                let (within, boundary) = self.within(&wants, depth);
                for oid in &boundary {
                    pkt(&mut update, format!("shallow {}\n", hex(oid)).as_bytes());
                }
                for oid in client_shallow.intersection(&within) {
                    if !boundary.contains(oid) {
                        pkt(&mut update, format!("unshallow {}\n", hex(oid)).as_bytes());
                    }
                }
                let mut cut = boundary;
                cut.extend(client_shallow.difference(&within).copied());
                match self.reach(&common, &none, &client_shallow) {
                    Ok(has) => {
                        let has: BTreeSet<Oid> = has.into_iter().collect();
                        self.reach(&wants, &none, &cut)
                            .map(|all| all.into_iter().filter(|oid| !has.contains(oid)).collect())
                    }
                    Err(why) => Err(why),
                }
            }
        };
        let Ok(mut send) = reached else {
            return respond(500, "", Vec::new());
        };
        if depth.is_some() || !client_shallow.is_empty() {
            update.extend_from_slice(b"0000");
        }
        send.reverse();
        let corruption = self.corrupt.take();
        if corruption == Some(Corruption::DropWanted) {
            send.retain(|oid| !wants.contains(oid));
        }
        let mut pack = self.pack(&send);
        let mut rng = self.rng.borrow_mut();
        match corruption {
            Some(Corruption::Truncate) => {
                let cut = 1 + rng.pick(pack.len() - 1);
                pack.truncate(cut);
            }
            Some(Corruption::FlipChecksum) => {
                let at = 12 + rng.pick(pack.len() - 12 - OID);
                pack[at] ^= 0x40;
            }
            Some(Corruption::FlipResealed) => {
                let end = pack.len() - OID;
                let at = 12 + rng.pick(end - 12);
                pack[at] ^= 0x01;
                let sum = Sha1::digest(&pack[..end]);
                pack[end..].copy_from_slice(&sum);
            }
            Some(Corruption::DropWanted) | None => {}
        }
        let mut out = update;
        pkt(&mut out, b"NAK\n");
        out.extend_from_slice(&pack);
        respond(HTTP_OK, "application/x-git-upload-pack-result", out)
    }

    fn unpack(&self, pack: &[u8]) -> Result<Vec<(Oid, u8, Vec<u8>)>, String> {
        let end = pack.len().saturating_sub(OID);
        if pack.len() < 12 + OID || Sha1::digest(&pack[..end])[..] != pack[end..] {
            return Err("pack checksum".to_owned());
        }
        let mut resolved: BTreeMap<usize, (u8, Vec<u8>)> = BTreeMap::new();
        let mut out = Vec::new();
        for entry in entries(pack)? {
            let (kind, data) = match entry.base {
                None => (entry.kind, entry.data),
                Some(base) => {
                    let (kind, source) = resolved
                        .get(&base)
                        .ok_or("a delta's base offset is not an entry")?;
                    self.pushed_deltas.set(self.pushed_deltas.get() + 1);
                    (*kind, apply(source, &entry.data)?)
                }
            };
            out.push((object_id(kind, &data), kind, data.clone()));
            resolved.insert(entry.start, (kind, data));
        }
        let unique: BTreeSet<Oid> = out.iter().map(|(oid, _, _)| *oid).collect();
        if unique.len() != out.len() {
            return Err("a pack repeats an object".to_owned());
        }
        Ok(out)
    }

    fn receive(&self, body: &[u8]) -> Response {
        let Some((lines, pack)) = pkts(body) else {
            return respond(400, "", Vec::new());
        };
        let mut commands = Vec::new();
        for line in lines.into_iter().flatten() {
            let line = match line.iter().position(|&byte| byte == 0) {
                Some(nul) => line[..nul].to_vec(),
                None => line,
            };
            let line = String::from_utf8_lossy(&line).trim_end().to_owned();
            let mut parts = line.splitn(3, ' ');
            let (Some(old), Some(new), Some(name)) = (parts.next(), parts.next(), parts.next())
            else {
                return respond(400, "", Vec::new());
            };
            let (Some(old), Some(new)) = (unhex(old.as_bytes()), unhex(new.as_bytes())) else {
                return respond(400, "", Vec::new());
            };
            commands.push((old, new, name.to_owned()));
        }
        let mut out = Vec::new();
        let unpacked = if pack.is_empty() {
            Ok(Vec::new())
        } else if self.mutation == GitRemoteMutation::PushWrongBase {
            self.unpack(&wrong_base(pack))
        } else {
            self.unpack(pack)
        };
        *self.received.borrow_mut() = Some(match &unpacked {
            Ok(objects) => Ok(objects.iter().map(|(oid, _, _)| *oid).collect()),
            Err(why) => Err(why.clone()),
        });
        match unpacked {
            Err(why) => {
                pkt(&mut out, format!("unpack {why}\n").as_bytes());
                for (_, _, name) in &commands {
                    pkt(&mut out, format!("ng {name} unpacker error\n").as_bytes());
                }
            }
            Ok(objects) => {
                let drop_one = self.mutation == GitRemoteMutation::ServerDropsObject;
                let mut dropped = false;
                for (oid, kind, data) in objects {
                    if drop_one && !dropped && kind == BLOB {
                        dropped = true;
                        continue;
                    }
                    self.objects.borrow_mut().insert(oid, (kind, data));
                }
                pkt(&mut out, b"unpack ok\n");
                for (old, new, name) in &commands {
                    let current = self.refs.borrow().get(name).copied().unwrap_or(ZERO);
                    if current != *old {
                        pkt(&mut out, format!("ng {name} stale info\n").as_bytes());
                        continue;
                    }
                    if self.mutation != GitRemoteMutation::ServerKeepsRef {
                        if *new == ZERO {
                            self.refs.borrow_mut().remove(name);
                        } else {
                            self.refs.borrow_mut().insert(name.clone(), *new);
                        }
                    }
                    pkt(&mut out, format!("ok {name}\n").as_bytes());
                }
            }
        }
        out.extend_from_slice(b"0000");
        respond(HTTP_OK, "application/x-git-receive-pack-result", out)
    }

    fn handle(&self, request: &Request) -> Response {
        let secret = self.secret.as_bytes();
        let authorized = format!(
            "Basic {}",
            base64(format!("{USER}:{}", self.secret).as_bytes())
        );
        if request.url.contains(&self.secret) {
            self.leaks.borrow_mut().push(format!("url {}", request.url));
        }
        let mut auth = None;
        for (key, value) in &request.headers {
            if key.eq_ignore_ascii_case("authorization") {
                auth = Some(value.clone());
            } else if value.as_bytes().windows(secret.len()).any(|w| w == secret) {
                self.leaks.borrow_mut().push(format!("header {key}"));
            }
        }
        if request.body.windows(secret.len()).any(|w| w == secret)
            && !request.url.ends_with("git-receive-pack")
        {
            self.leaks.borrow_mut().push("body".to_owned());
        }
        let Some(rest) = request.url.strip_prefix(&format!("http://{HOST}{REPO}")) else {
            return respond(HTTP_NOT_FOUND, "", Vec::new());
        };
        let receive = rest.contains("git-receive-pack");
        self.trace
            .borrow_mut()
            .push(format!("{} {rest} auth={}", request.method, auth.is_some()));
        if receive && auth.as_deref() != Some(authorized.as_str()) {
            return respond(HTTP_UNAUTHORIZED, "", Vec::new());
        }
        match (request.method.as_str(), rest) {
            ("GET", "/info/refs?service=git-upload-pack") => respond(
                HTTP_OK,
                "application/x-git-upload-pack-advertisement",
                self.advert("git-upload-pack"),
            ),
            ("GET", "/info/refs?service=git-receive-pack") => respond(
                HTTP_OK,
                "application/x-git-receive-pack-advertisement",
                self.advert("git-receive-pack"),
            ),
            ("POST", "/git-upload-pack") => self.upload(&request.body),
            ("POST", "/git-receive-pack") => self.receive(&request.body),
            _ => respond(HTTP_NOT_FOUND, "", Vec::new()),
        }
    }

    fn put(&self, kind: u8, data: Vec<u8>) -> Oid {
        let oid = object_id(kind, &data);
        self.objects.borrow_mut().insert(oid, (kind, data));
        oid
    }

    fn commit(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        parent: Option<Oid>,
        message: &str,
        time: u64,
    ) -> Oid {
        let tree = self.tree(files, "");
        let mut body = format!("tree {}\n", hex(&tree));
        if let Some(parent) = parent {
            body.push_str(&format!("parent {}\n", hex(&parent)));
        }
        body.push_str(&format!(
            "author S <s@sim> {time} +0000\ncommitter S <s@sim> {time} +0000\n\n{message}\n"
        ));
        self.put(COMMIT, body.into_bytes())
    }

    fn tree(&self, files: &BTreeMap<String, Vec<u8>>, prefix: &str) -> Oid {
        let mut entries: BTreeMap<String, (bool, Oid)> = BTreeMap::new();
        for (path, data) in files {
            let Some(rest) = path.strip_prefix(prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((dir, _)) => {
                    if !entries.contains_key(dir) {
                        let sub = self.tree(files, &format!("{prefix}{dir}/"));
                        entries.insert(dir.to_owned(), (true, sub));
                    }
                }
                None => {
                    let oid = self.put(BLOB, data.clone());
                    entries.insert(rest.to_owned(), (false, oid));
                }
            }
        }
        let mut sorted: Vec<(Vec<u8>, bool, Oid)> = entries
            .into_iter()
            .map(|(name, (dir, oid))| {
                let mut key = name.into_bytes();
                if dir {
                    key.push(b'/');
                }
                (key, dir, oid)
            })
            .collect();
        sorted.sort();
        let mut body = Vec::new();
        for (mut name, dir, oid) in sorted {
            if dir {
                name.pop();
                body.extend_from_slice(b"40000 ");
            } else {
                body.extend_from_slice(b"100644 ");
            }
            body.extend_from_slice(&name);
            body.push(0);
            body.extend_from_slice(&oid);
        }
        self.put(TREE, body)
    }

    fn files(&self, commit: &Oid) -> BTreeMap<String, Vec<u8>> {
        let objects = self.objects.borrow();
        let mut out = BTreeMap::new();
        let Some((_, data)) = objects.get(commit) else {
            return out;
        };
        let (tree, _) = parse_commit(data);
        let mut stack = vec![(String::new(), tree)];
        while let Some((prefix, tree)) = stack.pop() {
            let Some((_, data)) = objects.get(&tree) else {
                continue;
            };
            for (name, dir, oid) in parse_tree(data) {
                let path = format!("{prefix}{}", String::from_utf8_lossy(&name));
                if dir {
                    stack.push((format!("{path}/"), oid));
                } else if let Some((_, blob)) = objects.get(&oid) {
                    out.insert(path, blob.clone());
                }
            }
        }
        out
    }
}

struct Wire(Rc<Server>);

impl Fetch for Wire {
    fn fetch(&self, request: Request) -> Pin<Box<dyn Future<Output = Result<Response, Failure>>>> {
        let mut response = self.0.handle(&request);
        response.url = request.url;
        Box::pin(std::future::ready(Ok(response)))
    }
}

struct Keys {
    secret: String,
    asks: Cell<u64>,
}

impl Credentials for Keys {
    fn credential(&self, url: &str) -> Pin<Box<dyn Future<Output = Option<Credential>>>> {
        self.asks.set(self.asks.get() + 1);
        let answer = (!url.contains('@')).then(|| Credential {
            user: USER.to_owned(),
            secret: self.secret.clone(),
        });
        Box::pin(std::future::ready(answer))
    }
}

/// A git server over smart HTTP that starts with no refs, for other scenarios
/// whose jobs push to it: its fetcher, the credentials it takes, its URL and
/// the branches it holds.
pub(crate) struct Origin {
    server: Rc<Server>,
    keys: Rc<Keys>,
}

impl Origin {
    pub(crate) fn new(seed: u64) -> Self {
        let mut rng = Rng::derive(seed, "gitremote-origin");
        let secret: String = (0..SECRET_BYTES)
            .map(|_| char::from(b'a' + rng.below(26) as u8))
            .collect();
        let server = Rc::new(Server {
            objects: RefCell::new(BTreeMap::new()),
            refs: RefCell::new(BTreeMap::new()),
            secret: secret.clone(),
            rng: RefCell::new(rng),
            corrupt: Cell::new(None),
            mutation: GitRemoteMutation::None,
            leaks: RefCell::new(Vec::new()),
            deltas: Cell::new(0),
            pushed_deltas: Cell::new(0),
            received: RefCell::new(None),
            trace: RefCell::new(Vec::new()),
        });
        let keys = Rc::new(Keys {
            secret,
            asks: Cell::new(0),
        });
        Self { server, keys }
    }

    pub(crate) fn fetch(&self) -> Rc<dyn Fetch> {
        Rc::new(Wire(self.server.clone()))
    }

    pub(crate) fn credentials(&self) -> Rc<dyn Credentials> {
        self.keys.clone()
    }

    pub(crate) fn url() -> String {
        format!("http://{HOST}{REPO}")
    }

    /// Each branch the server holds, by its short name, to its commit in hex.
    pub(crate) fn heads(&self) -> BTreeMap<String, String> {
        self.server
            .refs
            .borrow()
            .iter()
            .filter_map(|(name, oid)| Some((name.strip_prefix("refs/heads/")?.to_owned(), hex(oid))))
            .collect()
    }
}

struct Clock {
    now: Cell<u64>,
}

impl Machine for Clock {
    fn now(&self) -> Duration {
        let now = self.now.get() + 1;
        self.now.set(now);
        Duration::from_secs(1_700_000_000 + now)
    }

    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::ready(()))
    }

    fn random(&self, bytes: &mut [u8]) {
        bytes.fill(0x5a);
    }
}

fn now<T: 'static>(reply: nanocode_shell::Reply<T>) -> std::io::Result<T> {
    block_on(reply.get())
}

fn snapshot(fs: &MemFs) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![b"/".to_vec()];
    while let Some(dir) = stack.pop() {
        let Ok(names) = now(fs.list(&dir)) else {
            continue;
        };
        for name in names {
            let mut path = dir.clone();
            if !path.ends_with(b"/") {
                path.push(b'/');
            }
            path.extend_from_slice(&name);
            match now(fs.stat(&path, false)).map(|stat| stat.kind) {
                Ok(Kind::Dir) => {
                    out.insert(path.clone(), Vec::new());
                    stack.push(path);
                }
                Ok(Kind::File) => {
                    out.insert(
                        path.clone(),
                        now(fs.read(&path, u64::MAX))
                            .map(Vec::from)
                            .unwrap_or_default(),
                    );
                }
                Ok(Kind::Symlink) => {
                    out.insert(path.clone(), now(fs.read_link(&path)).unwrap_or_default());
                }
                Err(_) => {}
            }
        }
    }
    out
}

struct World {
    rng: Rng,
    server: Rc<Server>,
    fs: Rc<MemFs>,
    shell: Shell,
    keys: Rc<Keys>,
    url: String,
    trace: Vec<String>,
    stats: GitRemoteStats,
    files: BTreeMap<String, Vec<u8>>,
    local_ahead: bool,
    base: Oid,
    message: String,
    mutation: GitRemoteMutation,
    commits: u64,
    single: bool,
    shallow: BTreeSet<Oid>,
    tracked: BTreeMap<String, Oid>,
    mirror: BTreeMap<String, Oid>,
}

impl World {
    fn server_heads(&self) -> BTreeMap<String, Oid> {
        self.server
            .refs
            .borrow()
            .iter()
            .filter_map(|(name, oid)| Some((name.strip_prefix("refs/heads/")?.to_owned(), *oid)))
            .collect()
    }

    fn refs_under(&self, dir: &str) -> BTreeMap<String, Oid> {
        let prefix = format!("{WORK}/.git/{dir}");
        snapshot(&self.fs)
            .into_iter()
            .filter_map(|(path, data)| {
                let path = String::from_utf8(path).ok()?;
                Some((path.strip_prefix(&prefix)?.to_owned(), unhex(&data)?))
            })
            .collect()
    }

    fn check_refs(&self, dir: &str, want: &BTreeMap<String, Oid>) -> Result<(), String> {
        let got = self.refs_under(dir);
        if &got == want {
            return Ok(());
        }
        let names = |map: &BTreeMap<String, Oid>| {
            map.iter()
                .map(|(name, oid)| format!("{name}={}", &hex(oid)[..7]))
                .collect::<Vec<_>>()
                .join(" ")
        };
        Err(format!(
            "{dir} holds [{}] where the model holds [{}]",
            names(&got),
            names(want)
        ))
    }

    fn track_fetch(&mut self, prune: bool) -> Vec<(String, Oid)> {
        let heads = self.server_heads();
        if self.single {
            if let Some(main) = heads.get("main") {
                self.tracked.insert("main".to_owned(), *main);
            }
            return Vec::new();
        }
        let gone: Vec<(String, Oid)> = self
            .tracked
            .iter()
            .filter(|(name, _)| prune && !heads.contains_key(*name))
            .map(|(name, oid)| (name.clone(), *oid))
            .collect();
        for (name, _) in &gone {
            self.tracked.remove(name);
        }
        self.tracked.extend(heads);
        gone
    }

    fn check_shallow(&mut self) -> Result<(), String> {
        let got: BTreeSet<Oid> = self
            .fs
            .get(format!("{WORK}/.git/shallow").as_bytes())
            .map(|data| data.split(|&b| b == b'\n').filter_map(unhex).collect())
            .unwrap_or_default();
        if got != self.shallow {
            return Err(format!(
                ".git/shallow lists {} commits where the server cut {}",
                got.len(),
                self.shallow.len()
            ));
        }
        let tip = self
            .read_ref("refs/remotes/origin/main")
            .ok_or("no origin/main")?;
        let want: String = self
            .server
            .walk(tip, &self.shallow)
            .iter()
            .map(|oid| format!("{}\n", hex(oid)))
            .collect();
        let (status, text) = self.run(&format!(
            "cd {WORK} && git log --format=%H refs/remotes/origin/main"
        ))?;
        if status != 0 || text != want {
            return Err(format!(
                "log of origin/main exited {status} with {} lines where the shallow history has {}",
                text.lines().count(),
                want.lines().count()
            ));
        }
        let (status, text) = self.run(&format!(
            "cd {WORK} && git rev-parse --is-shallow-repository && git status --porcelain"
        ))?;
        if status != 0 || text != format!("{}\n", !self.shallow.is_empty()) {
            return Err(format!(
                "rev-parse and status of a shallow repository: {text}"
            ));
        }
        Ok(())
    }

    fn server_topic(&mut self) {
        let n = self.rng.below(TOPICS_MAX);
        let name = format!("refs/heads/topic-{n}");
        let old = self.server.refs.borrow().get(&name).copied();
        let parent = match old {
            Some(_) if self.rng.chance(PPM_TOPIC_DELETE) => {
                self.server.refs.borrow_mut().remove(&name);
                self.trace.push(format!("server deleted {name}"));
                return;
            }
            Some(old) if !self.rng.chance(PPM_TOPIC_REWRITE) => Some(old),
            _ => self.server_main(),
        };
        self.commits += 1;
        let mut files = self.files.clone();
        files.insert(
            format!("topic-{n}.txt"),
            format!("topic {} {}\n", self.commits, self.rng.below(1000)).into_bytes(),
        );
        let oid = self.server.commit(
            &files,
            parent,
            &format!("topic {}", self.commits),
            self.commits,
        );
        self.server.refs.borrow_mut().insert(name.clone(), oid);
        self.trace.push(format!("server {name} {}", hex(&oid)));
    }

    fn deepen(&mut self) -> Result<(), String> {
        let unshallow = self.rng.chance(PPM_UNSHALLOW);
        let depth = if unshallow {
            INFINITE_DEPTH
        } else {
            self.rng.range(1, DEPTH_MAX + 2)
        };
        let arg = if unshallow {
            " --unshallow".to_owned()
        } else {
            format!(" --depth {depth}")
        };
        let before = snapshot(&self.fs);
        let (status, text) = self.run(&format!("cd {WORK} && git fetch -q{arg}"))?;
        if unshallow && self.shallow.is_empty() {
            if status == 0 || snapshot(&self.fs) != before {
                return Err("--unshallow of a complete repository changed it".to_owned());
            }
            return Ok(());
        }
        if status != 0 {
            return Err(format!("fetch{arg} exited {status}: {text}"));
        }
        let main = self.server_main().ok_or("no main")?;
        let (within, boundary) = self.server.within(&[main], depth);
        self.shallow.extend(boundary.iter().copied());
        self.shallow
            .retain(|oid| boundary.contains(oid) || !within.contains(oid));
        self.track_fetch(false);
        if unshallow {
            self.stats.unshallows += 1;
        } else {
            self.stats.shallow_fetches += 1;
        }
        if self.mutation == GitRemoteMutation::ForgetsShallow && !self.shallow.is_empty() {
            self.fs
                .insert(format!("{WORK}/.git/shallow").as_bytes(), Bytes::new())
                .map_err(|e| e.to_string())?;
        }
        self.check_refs("refs/remotes/origin/", &self.tracked)?;
        self.check_shallow()
    }

    fn spec_fetch(&mut self) -> Result<(), String> {
        let heads = self.server_heads();
        if self.rng.chance(PPM_REFUSE) {
            let before = snapshot(&self.fs);
            let (status, _) = self.run(&format!("cd {WORK} && git fetch -q origin main:main"))?;
            if status == 0 || snapshot(&self.fs) != before {
                return Err("a fetch into the checked-out branch changed it".to_owned());
            }
            self.stats.refused += 1;
            return Ok(());
        }
        let force = self.rng.chance(PPM_FORCE);
        let prune = self.rng.chance(PPM_SPEC_PRUNE);
        let plus = if force { "+" } else { "" };
        let wild = self.rng.chance(PPM_WILDCARD);
        let (spec, picked): (String, Vec<String>) = if wild {
            (
                format!("'{plus}refs/heads/*:refs/remotes/mirror/*'"),
                heads.keys().cloned().collect(),
            )
        } else {
            let names: Vec<&String> = heads
                .keys()
                .filter(|name| name.as_str() != "main" || heads.len() == 1)
                .collect();
            let name = names[self.rng.pick(names.len())].clone();
            (
                format!("{plus}refs/heads/{name}:refs/remotes/mirror/{name}"),
                vec![name],
            )
        };
        let flag = if prune { " --prune" } else { "" };
        let (status, text) = self.run(&format!("cd {WORK} && git fetch -q{flag} origin {spec}"))?;
        let mut rejected = Vec::new();
        for name in &picked {
            let new = heads[name];
            match self.mirror.get(name) {
                Some(old)
                    if *old != new && !force && !self.server.descends(new, *old, &self.shallow) =>
                {
                    rejected.push(name.clone());
                }
                _ => {
                    self.mirror.insert(name.clone(), new);
                }
            }
            if !self.single || name == "main" {
                self.tracked.insert(name.clone(), new);
            }
        }
        if prune && wild {
            let before = self.mirror.len();
            self.mirror.retain(|name, _| heads.contains_key(name));
            self.stats.pruned += (before - self.mirror.len()) as u64;
        }
        if status != i32::from(!rejected.is_empty()) {
            return Err(format!(
                "fetch {spec} exited {status} with {} refs to reject: {text}",
                rejected.len()
            ));
        }
        if self.mutation == GitRemoteMutation::ForcesRefspec
            && let Some(name) = rejected.first()
        {
            self.fs
                .insert(
                    format!("{WORK}/.git/refs/remotes/mirror/{name}").as_bytes(),
                    Bytes::from(format!("{}\n", hex(&heads[name]))),
                )
                .map_err(|e| e.to_string())?;
        }
        self.stats.spec_fetches += 1;
        self.stats.rejected_refs += rejected.len() as u64;
        self.check_refs("refs/remotes/mirror/", &self.mirror)?;
        self.check_refs("refs/remotes/origin/", &self.tracked)
    }

    fn run(&mut self, script: &str) -> Result<(i32, String), String> {
        let ran = block_on(self.shell.run(
            Script {
                source: script.as_bytes().to_vec(),
                name: b"sh".to_vec(),
                args: Vec::new(),
                options: Vec::new(),
                stdin: None,
            },
            SCRIPT_BUDGET,
        ));
        let status = match ran.ended {
            Ended::Exited(status) => status,
            other => return Err(format!("{script}: ended {other:?}")),
        };
        let mut text = String::from_utf8_lossy(&ran.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&ran.stderr));
        if text.contains(&self.server.secret) {
            return Err(format!("{script}: the secret was printed"));
        }
        self.trace.push(format!("{script} -> {status}"));
        Ok((status, text))
    }

    fn read_ref(&self, name: &str) -> Option<Oid> {
        let path = format!("{WORK}/.git/{name}");
        let data = self.fs.get(path.as_bytes()).ok()?;
        unhex(&data)
    }

    fn server_main(&self) -> Option<Oid> {
        self.server.refs.borrow().get(MAIN).copied()
    }

    fn check_tree(&self, commit: &Oid) -> Result<(), String> {
        let want = self.server.files(commit);
        for (path, data) in &want {
            let got = self
                .fs
                .get(format!("{WORK}/{path}").as_bytes())
                .map_err(|_| format!("work tree lacks {path}"))?;
            if got[..] != data[..] {
                return Err(format!("work tree's {path} differs from the commit"));
            }
        }
        Ok(())
    }

    fn check_secret(&self) -> Result<(), String> {
        let secret = self.server.secret.as_bytes();
        for (path, data) in snapshot(&self.fs) {
            if data.windows(secret.len()).any(|w| w == secret) {
                return Err(format!(
                    "the secret is stored in {}",
                    String::from_utf8_lossy(&path)
                ));
            }
        }
        if let Some(leak) = self.server.leaks.borrow().first() {
            return Err(format!(
                "the secret left outside the Authorization header: {leak}"
            ));
        }
        Ok(())
    }

    fn server_advance(&mut self) {
        let name = FILE_NAMES[self.rng.pick(FILE_NAMES.len())];
        let mut data = self.files.get(name).cloned().unwrap_or_default();
        data.extend_from_slice(
            format!("line {} {}\n", self.commits, self.rng.below(1000)).as_bytes(),
        );
        self.files.insert(name.to_owned(), data);
        self.commits += 1;
        let parent = self.server_main();
        let oid = self.server.commit(
            &self.files,
            parent,
            &format!("server {}", self.commits),
            self.commits,
        );
        self.server.refs.borrow_mut().insert(MAIN.to_owned(), oid);
        self.trace.push(format!("server main {}", hex(&oid)));
    }

    fn clone_repo(&mut self) -> Result<(), String> {
        let quiet = if self.rng.chance(PPM_QUIET) {
            " -q"
        } else {
            ""
        };
        let depth = self
            .rng
            .chance(PPM_SHALLOW_CLONE)
            .then(|| self.rng.range(1, DEPTH_MAX));
        let arg = depth.map(|d| format!(" --depth {d}")).unwrap_or_default();
        let (status, text) = self.run(&format!("git clone{quiet}{arg} {} {WORK}", self.url))?;
        if status != 0 {
            return Err(format!("clone exited {status}: {text}"));
        }
        self.stats.clones += 1;
        let main = self.server_main().ok_or("the server has no main")?;
        if let Some(depth) = depth {
            self.single = true;
            self.shallow = self.server.within(&[main], depth).1;
            self.stats.shallow_clones += 1;
        }
        self.track_fetch(false);
        self.check_refs("refs/remotes/origin/", &self.tracked)?;
        if self.mutation == GitRemoteMutation::ForgetsShallow && !self.shallow.is_empty() {
            self.fs
                .insert(format!("{WORK}/.git/shallow").as_bytes(), Bytes::new())
                .map_err(|e| e.to_string())?;
        }
        self.check_shallow()?;
        if self.read_ref("refs/remotes/origin/main") != Some(main)
            || self.read_ref(MAIN) != Some(main)
        {
            return Err("clone's refs differ from the server's".to_owned());
        }
        let config = self
            .fs
            .get(format!("{WORK}/.git/config").as_bytes())
            .map_err(|e| e.to_string())?;
        if config.contains(&b'@') {
            return Err("clone wrote user info into .git/config".to_owned());
        }
        self.base = main;
        if self.mutation == GitRemoteMutation::StoresSecret {
            let mut data = config.to_vec();
            data.extend_from_slice(
                format!("[credential]\n\tpassword = {}\n", self.server.secret).as_bytes(),
            );
            self.fs
                .insert(format!("{WORK}/.git/config").as_bytes(), Bytes::from(data))
                .map_err(|e| e.to_string())?;
        }
        self.check_tree(&main)
    }

    fn fetch(&mut self) -> Result<(), String> {
        let tracking = format!("{WORK}/.git/refs/remotes/origin/main");
        let before = self
            .fs
            .get(tracking.as_bytes())
            .map_err(|e| e.to_string())?;
        let quiet = if self.rng.chance(PPM_QUIET) {
            " -q"
        } else {
            ""
        };
        let prune = self.rng.chance(PPM_SPEC_PRUNE);
        let flag = if prune { " --prune" } else { "" };
        let (status, text) = self.run(&format!("cd {WORK} && git fetch{quiet}{flag}"))?;
        if status != 0 {
            return Err(format!("fetch exited {status}: {text}"));
        }
        self.stats.fetches += 1;
        let gone = self.track_fetch(prune);
        self.stats.pruned += gone.len() as u64;
        if self.mutation == GitRemoteMutation::KeepsPruned
            && let Some((name, oid)) = gone.first()
        {
            self.fs
                .insert(
                    format!("{WORK}/.git/refs/remotes/origin/{name}").as_bytes(),
                    Bytes::from(format!("{}\n", hex(oid))),
                )
                .map_err(|e| e.to_string())?;
        }
        if self.mutation == GitRemoteMutation::StaleTracking {
            self.fs
                .insert(tracking.as_bytes(), before)
                .map_err(|e| e.to_string())?;
        }
        if self.read_ref("refs/remotes/origin/main") != self.server_main() {
            return Err("fetch left origin/main behind the server".to_owned());
        }
        self.check_refs("refs/remotes/origin/", &self.tracked)?;
        self.check_shallow()
    }

    fn diverged(&self) -> bool {
        self.local_ahead && self.server_main() != Some(self.base)
    }

    fn pull(&mut self) -> Result<(), String> {
        let before = self.read_ref(MAIN);
        let diverged = self.diverged();
        let (status, text) = self.run(&format!("cd {WORK} && git pull --ff-only"))?;
        self.track_fetch(false);
        let main = self.server_main().ok_or("no main")?;
        let hidden = !self.local_ahead
            && main != self.base
            && !self.server.descends(main, self.base, &self.shallow);
        if diverged || hidden {
            if status == 0 || self.read_ref(MAIN) != before {
                return Err("pull --ff-only of a diverged history moved main".to_owned());
            }
            return Ok(());
        }
        if status != 0 {
            return Err(format!("pull exited {status}: {text}"));
        }
        self.stats.pulls += 1;
        if self.local_ahead {
            if self.read_ref(MAIN) != before {
                return Err("pull moved a main that was already ahead".to_owned());
            }
            return Ok(());
        }
        if self.read_ref(MAIN) != Some(main) {
            return Err("pull did not move main to the server's".to_owned());
        }
        self.base = main;
        self.check_tree(&main)
    }

    fn local_commit(&mut self) -> Result<(), String> {
        let name = FILE_NAMES[self.rng.pick(FILE_NAMES.len())];
        let dir = name
            .rsplit_once('/')
            .map(|(dir, _)| format!("mkdir -p {dir} && "))
            .unwrap_or_default();
        for _ in 0..=self.rng.below(LOCAL_COMMITS_MAX) {
            let line = format!("local {} {}", self.commits, self.rng.below(1000));
            self.commits += 1;
            self.message = format!("local {}", self.commits);
            let (status, text) = self.run(&format!(
                "cd {WORK} && {dir}echo '{line}' >> {name} && git add -A && git commit -qm '{}'",
                self.message
            ))?;
            if status != 0 {
                return Err(format!("commit exited {status}: {text}"));
            }
        }
        self.local_ahead = true;
        Ok(())
    }

    fn push(&mut self) -> Result<(), String> {
        let before = self.server_main();
        let local = self.read_ref(MAIN).ok_or("no local main")?;
        let accepted = self.local_ahead && !self.diverged();
        let current = before == Some(local);
        let had: BTreeSet<Oid> = self.server.objects.borrow().keys().copied().collect();
        self.server.received.borrow_mut().take();
        let (status, text) = self.run(&format!("cd {WORK} && git push"))?;
        self.check_pushed_pack(&had, local)?;
        if !accepted && !current {
            if status == 0 || self.server_main() != before {
                return Err("a non-fast-forward push changed the server".to_owned());
            }
            self.stats.rejected_pushes += 1;
            return Ok(());
        }
        if status != 0 {
            return Err(format!("push exited {status}: {text}"));
        }
        if current {
            return Ok(());
        }
        self.stats.pushes += 1;
        if self.server_main() != Some(local) {
            return Err("the server's main is not the pushed commit".to_owned());
        }
        self.server
            .reach(&[local], &BTreeSet::new(), &BTreeSet::new())?;
        let readable = self
            .server
            .objects
            .borrow()
            .get(&local)
            .is_some_and(|(kind, body)| {
                *kind == COMMIT && String::from_utf8_lossy(body).contains(&self.message)
            });
        if !readable {
            return Err("the server cannot read the pushed commit".to_owned());
        }
        if self.read_ref("refs/remotes/origin/main") != Some(local) {
            return Err("push did not move origin/main".to_owned());
        }
        self.tracked.insert("main".to_owned(), local);
        self.local_ahead = false;
        self.base = local;
        self.files = self.server.files(&local);
        Ok(())
    }

    fn check_pushed_pack(&self, had: &BTreeSet<Oid>, local: Oid) -> Result<(), String> {
        let Some(received) = self.server.received.borrow_mut().take() else {
            return Ok(());
        };
        let sent = received.map_err(|why| format!("a pushed pack does not resolve: {why}"))?;
        let Ok(reachable) = self
            .server
            .reach(&[local], &BTreeSet::new(), &BTreeSet::new())
        else {
            return Ok(());
        };
        let reachable: BTreeSet<Oid> = reachable.into_iter().collect();
        if let Some(stray) = sent.iter().find(|oid| !reachable.contains(*oid)) {
            return Err(format!(
                "a pushed pack carries {} that the pushed commit does not reach",
                hex(stray)
            ));
        }
        if let Some(missing) = reachable
            .iter()
            .find(|oid| !had.contains(*oid) && !sent.contains(*oid))
        {
            return Err(format!(
                "a pushed pack lacks {}, which the server did not have",
                hex(missing)
            ));
        }
        Ok(())
    }

    fn bad_fetch(&mut self) -> Result<(), String> {
        self.server_advance();
        let corruption = CORRUPTIONS[self.rng.pick(CORRUPTIONS.len())];
        self.server.corrupt.set(Some(corruption));
        let before = snapshot(&self.fs);
        let (status, _) = self.run(&format!("cd {WORK} && git fetch"))?;
        self.server.corrupt.set(None);
        self.stats.bad_packs += 1;
        if self.mutation == GitRemoteMutation::UpdatesRefOnBadPack
            && let Some(main) = self.server_main()
        {
            self.fs
                .insert(
                    format!("{WORK}/.git/refs/remotes/origin/main").as_bytes(),
                    Bytes::from(format!("{}\n", hex(&main))),
                )
                .map_err(|e| e.to_string())?;
        }
        if status == 0 {
            return Err(format!("a {corruption:?} pack was accepted"));
        }
        if snapshot(&self.fs) != before {
            return Err(format!(
                "a rejected {corruption:?} pack changed the filesystem"
            ));
        }
        self.trace.push(format!("rejected {corruption:?}"));
        Ok(())
    }
}

fn simulate(seed: u64, mutation: GitRemoteMutation) -> GitRemoteOutcome {
    let mut rng = Rng::derive(seed, "gitremote");
    let secret: String = (0..SECRET_BYTES)
        .map(|_| char::from(b'a' + rng.below(26) as u8))
        .collect();
    let server = Rc::new(Server {
        objects: RefCell::new(BTreeMap::new()),
        refs: RefCell::new(BTreeMap::new()),
        secret: secret.clone(),
        rng: RefCell::new(Rng::derive(seed, "gitremote-server")),
        corrupt: Cell::new(None),
        mutation,
        leaks: RefCell::new(Vec::new()),
        deltas: Cell::new(0),
        pushed_deltas: Cell::new(0),
        received: RefCell::new(None),
        trace: RefCell::new(Vec::new()),
    });
    let machine = Rc::new(Clock { now: Cell::new(0) });
    let fs = Rc::new(MemFs::new(machine.clone()));
    let _ = now(fs.mkdir(b"/work", 0o755));
    let _ = now(fs.mkdir(b"/home", 0o755));
    let keys = Rc::new(Keys {
        secret: secret.clone(),
        asks: Cell::new(0),
    });
    let config = Config {
        fs: fs.clone(),
        commands: Rc::new(NoCommands),
        machine,
        identity: Identity {
            info: HostInfo::default(),
            pid: 2,
            user: USER.as_bytes().to_vec(),
            group: USER.as_bytes().to_vec(),
            home: b"/home".to_vec(),
        },
        limits: Limits {
            fuel: u64::MAX / 4,
            output: 1 << 20,
            file: 1 << 26,
            operations: u64::MAX / 4,
            depth: 64,
        },
        env: [
            ("HOME", "/home"),
            ("GIT_AUTHOR_NAME", "Agent"),
            ("GIT_AUTHOR_EMAIL", "agent@sim"),
            ("GIT_COMMITTER_NAME", "Agent"),
            ("GIT_COMMITTER_EMAIL", "agent@sim"),
        ]
        .iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect(),
        cwd: b"/work".to_vec(),
        without: Vec::new(),
        strict: false,
    };
    let mut shell = Shell::new(config);
    shell.set_fetch(Rc::new(Wire(server.clone())));
    shell.set_credentials(keys.clone());
    let url = if rng.chance(PPM_USERINFO) {
        format!("http://{USER}:{secret}@{HOST}{REPO}")
    } else {
        format!("http://{HOST}{REPO}")
    };
    let mut world = World {
        rng,
        server: server.clone(),
        fs,
        shell,
        keys,
        url,
        trace: Vec::new(),
        stats: GitRemoteStats::default(),
        files: BTreeMap::new(),
        local_ahead: false,
        base: ZERO,
        message: String::new(),
        mutation,
        commits: 0,
        single: false,
        shallow: BTreeSet::new(),
        tracked: BTreeMap::new(),
        mirror: BTreeMap::new(),
    };
    let initial = world.rng.range(1, DEPTH_MAX + 2);
    for _ in 0..initial {
        world.server_advance();
    }
    for _ in 0..world.rng.below(TOPICS_MAX) {
        world.server_topic();
    }
    let ops = world.rng.range(OPS_MIN, OPS_MAX);
    let mut plan: Vec<u64> = vec![3, 4];
    plan.extend((0..ops).map(|_| world.rng.below(OP_KINDS)));
    plan.push(OP_KINDS);
    let mut shape = format!("ops={} initial={initial}", plan.len());
    let mut wrong = world.clone_repo().err();
    if wrong.is_none() {
        for op in plan {
            let step = match op {
                0 => {
                    world.server_advance();
                    Ok(())
                }
                1 => world.fetch(),
                2 => world.pull(),
                3 => world.local_commit(),
                4 => world.push(),
                5 | 8 => {
                    world.server_topic();
                    Ok(())
                }
                6 | 9 => world.spec_fetch(),
                7 if world.single => world.deepen(),
                7 => world.fetch(),
                _ => world.bad_fetch(),
            };
            if let Err(why) = step.and_then(|()| world.check_secret()) {
                wrong = Some(format!("op {op}: {why}"));
                break;
            }
        }
    } else if let Err(why) = world.check_secret() {
        wrong = Some(why);
    }
    if wrong.is_none()
        && let Err(why) = world.check_secret()
    {
        wrong = Some(why);
    }
    world.stats.deltas = server.deltas.get();
    world.stats.pushed_deltas = server.pushed_deltas.get();
    world.stats.credential_asks = world.keys.asks.get();
    shape.push_str(&format!(
        " pushes={} bad={} shallow={} specs={}",
        world.stats.pushes,
        world.stats.bad_packs,
        world.stats.shallow_clones,
        world.stats.spec_fetches
    ));
    let digest = world
        .trace
        .iter()
        .chain(server.trace.borrow().iter())
        .fold(FNV_OFFSET, |hash, event| fnv(hash, event.as_bytes()));
    GitRemoteOutcome {
        digest,
        wrong,
        stats: world.stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: GitRemoteMutation) -> GitRemoteOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        GitRemoteOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: GitRemoteStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: GitRemoteMutation,
    twice: bool,
    verbose: bool,
    totals: &mut GitRemoteStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} stats={:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: GitRemoteMutation) -> String {
    let mut command = format!("nanocode-sim --scenario git_remote --seed {seed}");
    if mutation != GitRemoteMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: GitRemoteMutation,
    totals: &GitRemoteStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=git_remote mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        GitRemoteMutation::None if failed == 0 => ExitCode::SUCCESS,
        GitRemoteMutation::None => ExitCode::FAILURE,
        _ if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        _ => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
