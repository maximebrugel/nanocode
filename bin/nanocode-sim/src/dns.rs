use std::{
    collections::{BTreeMap, VecDeque},
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Range,
    pin::pin,
    process::ExitCode,
    task::{Context, Poll, Waker},
    time::Duration,
};

use nanocode_executor::dns::{self, Config, Wire};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const LABEL_LETTERS: &[u8] = b"abcxyz019-";
const LABELS_MAX: u64 = 3;
const LABEL_BYTES_MAX: u64 = 6;
const SEARCH_MAX: u64 = 3;
const NDOTS_MAX: u64 = 3;
const SERVERS_MAX: u64 = 3;
const ATTEMPTS_MAX: u64 = 3;
const ADDRESSES_MAX: u64 = 3;
const CHAIN_MAX: u64 = 3;
const GARBAGE_BYTES_MAX: u64 = 40;
const HOSTS_PPM: u64 = 150_000;
const ABSOLUTE_PPM: u64 = 100_000;
const NO_AAAA_PPM: u64 = 100_000;
const USE_VC_PPM: u64 = 80_000;
const UPPER_PPM: u64 = 300_000;
const GARBAGE_PPM: u64 = 200_000;
const DUPLICATE_PPM: u64 = 150_000;
const TRUNCATE_PPM: u64 = 200_000;
const REVERSE_PPM: u64 = 300_000;
const COMPRESS_PPM: u64 = 500_000;
const EMPTY_ENTRY_PPM: u64 = 250_000;
const CNAME_PPM: u64 = 300_000;
const NODATA_PPM: u64 = 150_000;
const SAME_ID_PPM: u64 = 100_000;
const TYPE_AAAA: u16 = 28;
const TYPE_CNAME: u16 = 5;
const CLASS_IN: u16 = 1;
const HEADER_BYTES: usize = 12;
const QUESTION_OFFSET: u16 = 12;
const POINTER: u16 = 0xc000;
const FLAGS_ANSWER: u16 = 0x8180;
const FLAG_TRUNCATED: u16 = 0x0200;
const RCODE_NAME_ERROR: u16 = 3;
const RCODE_SERVER_FAILURE: u16 = 2;
const RCODE_REFUSED: u16 = 5;
const TTL: u32 = 60;
const PORT: u16 = 53;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsMutation {
    None,
    CutAnswer,
    DropSearch,
}

impl DnsMutation {
    const ALL: [Self; 3] = [Self::None, Self::CutAnswer, Self::DropSearch];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CutAnswer => "cut_answer",
            Self::DropSearch => "drop_search",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .skip(1)
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DnsStats {
    pub seeds: u64,
    pub queries: u64,
    pub truncated: u64,
    pub streamed: u64,
    pub garbage: u64,
    pub found: u64,
    pub not_found: u64,
    pub from_hosts: u64,
}

impl DnsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.queries += other.queries;
        self.truncated += other.truncated;
        self.streamed += other.streamed;
        self.garbage += other.garbage;
        self.found += other.found;
        self.not_found += other.not_found;
        self.from_hosts += other.from_hosts;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Healthy,
    Silent,
    Failing,
}

#[derive(Clone, Debug)]
enum Entry {
    Addresses(Vec<Ipv6Addr>, Vec<Ipv4Addr>),
    Alias(String),
    NoData,
}

struct World {
    absolute: bool,
    config: Config,
    hosts: String,
    listed: Vec<IpAddr>,
    name: String,
    roles: Vec<Role>,
    zone: BTreeMap<String, Entry>,
}

struct FakeWire {
    mutation: DnsMutation,
    rng: Rng,
    roles: Vec<Role>,
    servers: Vec<SocketAddr>,
    current: Option<usize>,
    queue: VecDeque<Vec<u8>>,
    truncated: Vec<Vec<u8>>,
    zone: BTreeMap<String, Entry>,
    stats: DnsStats,
    tcp: bool,
    trace: u64,
    wrong: Option<String>,
    id: Option<u16>,
}

fn label(rng: &mut Rng) -> String {
    let length = rng.range(1, LABEL_BYTES_MAX);
    (0..length)
        .map(|index| {
            let letters = if index == 0 {
                &LABEL_LETTERS[..6]
            } else {
                LABEL_LETTERS
            };
            char::from(letters[rng.pick(letters.len())])
        })
        .collect()
}

fn name(rng: &mut Rng, labels: u64) -> String {
    (0..labels)
        .map(|_| label(rng))
        .collect::<Vec<_>>()
        .join(".")
}

fn shout(rng: &mut Rng, name: &str) -> String {
    name.chars()
        .map(|letter| {
            if rng.chance(UPPER_PPM) {
                letter.to_ascii_uppercase()
            } else {
                letter
            }
        })
        .collect()
}

fn v4(rng: &mut Rng) -> Ipv4Addr {
    Ipv4Addr::from(u32::try_from(rng.below(1 << 32)).expect("a draw below 2^32 fits in 32 bits"))
}

fn v6(rng: &mut Rng) -> Ipv6Addr {
    Ipv6Addr::from(u128::from(rng.draw()) << 64 | u128::from(rng.draw()))
}

fn candidates(world: &World) -> Vec<String> {
    let name = world.name.to_ascii_lowercase();
    if world.absolute {
        return vec![name];
    }
    let searched = world
        .config
        .search
        .iter()
        .map(|domain| format!("{name}.{domain}"));
    let dots = u32::try_from(name.matches('.').count()).expect("a name has few dots");
    if dots >= world.config.ndots {
        std::iter::once(name.clone()).chain(searched).collect()
    } else {
        searched.chain(std::iter::once(name.clone())).collect()
    }
}

impl World {
    fn of(seed: u64) -> Self {
        let mut rng = Rng::derive(seed, "dns-world");
        let search: Vec<String> = (0..rng.below(SEARCH_MAX + 1))
            .map(|_| format!("{}.sim", label(&mut rng)))
            .collect();
        let servers = rng.range(1, SERVERS_MAX);
        let healthy = rng.below(servers);
        let roles = (0..servers)
            .map(|index| match (index == healthy, rng.below(3)) {
                (true, _) | (false, 0) => Role::Healthy,
                (false, 1) => Role::Silent,
                _ => Role::Failing,
            })
            .collect::<Vec<_>>();
        let config = Config {
            attempts: u32::try_from(rng.range(1, ATTEMPTS_MAX)).expect("few attempts"),
            nameservers: (0..servers)
                .map(|index| {
                    let host = u8::try_from(index + 1).expect("few servers");
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 53, 0, host)), PORT)
                })
                .collect(),
            ndots: u32::try_from(rng.below(NDOTS_MAX + 1)).expect("few dots"),
            no_aaaa: rng.chance(NO_AAAA_PPM),
            search,
            tcp: rng.chance(USE_VC_PPM),
            timeout: Duration::from_secs(1),
        };
        let labels = rng.range(1, LABELS_MAX);
        let name = name(&mut rng, labels);
        let absolute = rng.chance(ABSOLUTE_PPM);
        let mut world = Self {
            absolute,
            config,
            hosts: String::new(),
            listed: Vec::new(),
            name: shout(&mut rng, &name),
            roles,
            zone: BTreeMap::new(),
        };
        let mut targets = 0;
        for candidate in candidates(&world) {
            if rng.chance(EMPTY_ENTRY_PPM) {
                continue;
            }
            let mut owner = candidate;
            for _ in 0..rng.below(CHAIN_MAX + 1) {
                if !rng.chance(CNAME_PPM) {
                    break;
                }
                targets += 1;
                let target = format!("t{targets}.{}.cdn.sim", label(&mut rng));
                world.zone.insert(owner, Entry::Alias(target.clone()));
                owner = target;
            }
            let entry = if rng.chance(NODATA_PPM) {
                Entry::NoData
            } else {
                Entry::Addresses(
                    (0..rng.below(ADDRESSES_MAX + 1))
                        .map(|_| v6(&mut rng))
                        .collect(),
                    (0..rng.below(ADDRESSES_MAX + 1))
                        .map(|_| v4(&mut rng))
                        .collect(),
                )
            };
            world.zone.insert(owner, entry);
        }
        if rng.chance(HOSTS_PPM) {
            let mut lines = vec![
                "# hosts".to_owned(),
                format!("10.9.9.9 other.sim {}x", world.name),
            ];
            for _ in 0..rng.range(1, ADDRESSES_MAX) {
                let address = if rng.chance(500_000) {
                    IpAddr::V4(v4(&mut rng))
                } else {
                    IpAddr::V6(v6(&mut rng))
                };
                if !world.listed.contains(&address) {
                    world.listed.push(address);
                }
                lines.push(format!(
                    "{address}\talias.sim {} # comment",
                    shout(&mut rng, &name)
                ));
            }
            world.hosts = lines.join("\n");
        }
        world
    }

    fn describe(&self) -> String {
        format!(
            "name={}{} roles={:?} attempts={} ndots={} search={:?} no_aaaa={} tcp={} zone={} hosts={}",
            self.name,
            if self.absolute { "." } else { "" },
            self.roles,
            self.config.attempts,
            self.config.ndots,
            self.config.search,
            self.config.no_aaaa,
            self.config.tcp,
            self.zone.len(),
            self.listed.len()
        )
    }

    fn expected(&self) -> Option<Vec<IpAddr>> {
        if !self.listed.is_empty() {
            return Some(self.listed.clone());
        }
        for candidate in candidates(self) {
            let addresses = addresses(&self.zone, &candidate, self.config.no_aaaa);
            if !addresses.is_empty() {
                return Some(addresses);
            }
        }
        None
    }
}

fn chain(zone: &BTreeMap<String, Entry>, name: &str) -> (Vec<(String, String)>, Option<String>) {
    let mut aliases = Vec::new();
    let mut owner = name.to_owned();
    loop {
        match zone.get(&owner) {
            Some(Entry::Alias(target)) => {
                aliases.push((owner.clone(), target.clone()));
                owner.clone_from(target);
            }
            Some(_) => return (aliases, Some(owner)),
            None => return (aliases, None),
        }
    }
}

fn addresses(zone: &BTreeMap<String, Entry>, name: &str, no_aaaa: bool) -> Vec<IpAddr> {
    let (_, last) = chain(zone, name);
    match last.and_then(|last| zone.get(&last)) {
        Some(Entry::Addresses(six, four)) => six
            .iter()
            .filter(|_| !no_aaaa)
            .map(|address| IpAddr::V6(*address))
            .chain(four.iter().map(|address| IpAddr::V4(*address)))
            .collect(),
        _ => Vec::new(),
    }
}

fn read_query(message: &[u8]) -> Option<(u16, Vec<u8>, String, u16)> {
    let id = u16::from_be_bytes([*message.first()?, *message.get(1)?]);
    let mut at = HEADER_BYTES;
    let mut labels = Vec::new();
    loop {
        let length = usize::from(*message.get(at)?);
        if length == 0 {
            break;
        }
        labels.push(String::from_utf8(message.get(at + 1..at + 1 + length)?.to_vec()).ok()?);
        at += 1 + length;
    }
    let raw = message.get(HEADER_BYTES..=at)?.to_vec();
    let kind = u16::from_be_bytes([*message.get(at + 1)?, *message.get(at + 2)?]);
    Some((id, raw, labels.join(".").to_ascii_lowercase(), kind))
}

fn put_name(
    message: &mut Vec<u8>,
    name: &str,
    offsets: &mut BTreeMap<String, u16>,
    compress: bool,
) {
    if compress && let Some(offset) = offsets.get(name) {
        message.extend_from_slice(&(POINTER | offset).to_be_bytes());
        return;
    }
    offsets.insert(
        name.to_owned(),
        u16::try_from(message.len()).expect("a reply fits 64 KiB"),
    );
    for label in name.split('.') {
        message.push(u8::try_from(label.len()).expect("a label is short"));
        message.extend_from_slice(label.as_bytes());
    }
    message.push(0);
}

impl FakeWire {
    fn answer(&mut self, id: u16, question: &[u8], name: &str, kind: u16, stream: bool) -> Vec<u8> {
        let (aliases, last) = chain(&self.zone, name);
        let mut records: Vec<(String, u16, Vec<u8>)> = Vec::new();
        for (owner, _) in &aliases {
            records.push((owner.clone(), TYPE_CNAME, Vec::new()));
        }
        let addresses = addresses(&self.zone, name, false);
        let typed: Vec<IpAddr> = addresses
            .into_iter()
            .filter(|address| address.is_ipv6() == (kind == TYPE_AAAA))
            .collect();
        let tail = last.clone().unwrap_or_default();
        for address in &typed {
            let data = match address {
                IpAddr::V4(address) => address.octets().to_vec(),
                IpAddr::V6(address) => address.octets().to_vec(),
            };
            records.push((tail.clone(), kind, data));
        }
        let rcode = if last.is_none() { RCODE_NAME_ERROR } else { 0 };
        let truncate = !stream && !typed.is_empty() && self.rng.chance(TRUNCATE_PPM);
        if self.mutation == DnsMutation::CutAnswer && typed.len() > 1 {
            records.pop();
        }
        let shown = if truncate {
            records.len() / 2
        } else {
            records.len()
        };
        let mut flags = FLAGS_ANSWER | rcode;
        if truncate {
            flags |= FLAG_TRUNCATED;
        }
        let mut message = Vec::new();
        let count = u16::try_from(shown).expect("few records");
        for word in [id, flags, 1, count, 0, 0] {
            message.extend_from_slice(&word.to_be_bytes());
        }
        message.extend_from_slice(question);
        message.extend_from_slice(&kind.to_be_bytes());
        message.extend_from_slice(&CLASS_IN.to_be_bytes());
        let mut offsets = BTreeMap::from([(name.to_owned(), QUESTION_OFFSET)]);
        let mut targets = aliases.iter().map(|(_, target)| target.clone());
        for (owner, record_kind, data) in records.into_iter().take(shown) {
            let compress = self.rng.chance(COMPRESS_PPM);
            let shown_owner = if compress {
                owner.clone()
            } else {
                shout(&mut self.rng, &owner)
            };
            put_name(&mut message, &shown_owner, &mut offsets, compress);
            message.extend_from_slice(&record_kind.to_be_bytes());
            message.extend_from_slice(&CLASS_IN.to_be_bytes());
            message.extend_from_slice(&TTL.to_be_bytes());
            let length_at = message.len();
            message.extend_from_slice(&[0, 0]);
            if record_kind == TYPE_CNAME {
                let target = targets.next().expect("each alias record has a target");
                put_name(
                    &mut message,
                    &target,
                    &mut offsets,
                    self.rng.chance(COMPRESS_PPM),
                );
            } else {
                message.extend_from_slice(&data);
            }
            let length = u16::try_from(message.len() - length_at - 2).expect("short data");
            message[length_at..length_at + 2].copy_from_slice(&length.to_be_bytes());
        }
        if truncate {
            self.stats.truncated += 1;
            self.truncated.push(question.to_vec());
        }
        message
    }

    fn garbage(&mut self, id: u16, question: &[u8], kind: u16) -> Vec<u8> {
        self.stats.garbage += 1;
        let mut message = Vec::new();
        match self.rng.below(4) {
            0 => {
                for word in [id, FLAGS_ANSWER, 1, 1, 0, 0] {
                    message.extend_from_slice(&word.to_be_bytes());
                }
                message.extend_from_slice(&(POINTER | QUESTION_OFFSET).to_be_bytes());
                message.extend_from_slice(&kind.to_be_bytes());
                message.extend_from_slice(&CLASS_IN.to_be_bytes());
            }
            1 => {
                for word in [id, FLAGS_ANSWER, 1, 1, 0, 0] {
                    message.extend_from_slice(&word.to_be_bytes());
                }
                message.extend_from_slice(&question[..question.len() - 1]);
                message.extend_from_slice(b"\x05other\x00");
                message.extend_from_slice(&kind.to_be_bytes());
                message.extend_from_slice(&CLASS_IN.to_be_bytes());
                message.extend_from_slice(&(POINTER | QUESTION_OFFSET).to_be_bytes());
                message.extend_from_slice(&kind.to_be_bytes());
                message.extend_from_slice(&CLASS_IN.to_be_bytes());
                message.extend_from_slice(&TTL.to_be_bytes());
                message.extend_from_slice(&[0, 4, 6, 6, 6, 6]);
            }
            2 => {
                for word in [id, FLAGS_ANSWER, 1, 9, 0, 0] {
                    message.extend_from_slice(&word.to_be_bytes());
                }
                message.extend_from_slice(question);
                message.extend_from_slice(&kind.to_be_bytes());
                message.extend_from_slice(&CLASS_IN.to_be_bytes());
                message.extend_from_slice(&(POINTER | (QUESTION_OFFSET + 1)).to_be_bytes());
            }
            _ => {
                message.extend_from_slice(&id.wrapping_add(1).to_be_bytes());
                for _ in 0..self.rng.below(GARBAGE_BYTES_MAX) {
                    message.push(u8::try_from(self.rng.below(256)).expect("a byte"));
                }
            }
        }
        message
    }

    fn note(&mut self, event: &str) {
        self.trace = fnv(self.trace, event.as_bytes());
    }
}

impl Wire for FakeWire {
    fn id(&mut self) -> io::Result<u16> {
        let id = match self.id {
            Some(id) if self.rng.chance(SAME_ID_PPM) => id,
            _ => u16::try_from(self.rng.below(1 << 16)).expect("a draw below 2^16 fits"),
        };
        self.id = Some(id);
        Ok(id)
    }

    fn open(&mut self, server: SocketAddr, _: Duration) -> io::Result<()> {
        let index = self.servers.iter().position(|known| *known == server);
        self.note(&format!("open {server}"));
        if index.is_none() {
            self.wrong = Some(format!(
                "the resolver opened {server}, which is not configured"
            ));
        }
        self.current = index;
        self.queue.clear();
        Ok(())
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let Some(message) = self.queue.pop_front() else {
            self.note("timeout");
            return Ok(None);
        };
        buffer[..message.len()].copy_from_slice(&message);
        Ok(Some(message.len()))
    }

    async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.stats.queries += 1;
        let Some((id, question, name, kind)) = read_query(message) else {
            self.wrong = Some("the resolver sent a malformed query".to_owned());
            return Ok(());
        };
        self.note(&format!("send {name} {kind}"));
        let Some(index) = self.current else {
            return Ok(());
        };
        match self.roles[index] {
            Role::Silent => {}
            Role::Failing => {
                let rcode = if self.rng.chance(500_000) {
                    RCODE_SERVER_FAILURE
                } else {
                    RCODE_REFUSED
                };
                let mut reply = Vec::new();
                for word in [id, FLAGS_ANSWER | rcode, 1, 0, 0, 0] {
                    reply.extend_from_slice(&word.to_be_bytes());
                }
                reply.extend_from_slice(&question);
                reply.extend_from_slice(&kind.to_be_bytes());
                reply.extend_from_slice(&CLASS_IN.to_be_bytes());
                self.queue.push_back(reply);
            }
            Role::Healthy => {
                if self.rng.chance(GARBAGE_PPM) {
                    let garbage = self.garbage(id, &question, kind);
                    self.queue.push_back(garbage);
                }
                let reply = self.answer(id, &question, &name, kind, false);
                if self.rng.chance(DUPLICATE_PPM) {
                    self.queue.push_back(reply.clone());
                }
                if self.rng.chance(REVERSE_PPM) {
                    self.queue.push_front(reply);
                } else {
                    self.queue.push_back(reply);
                }
            }
        }
        Ok(())
    }

    async fn stream(
        &mut self,
        server: SocketAddr,
        message: &[u8],
        _: Duration,
    ) -> io::Result<Vec<u8>> {
        self.stats.streamed += 1;
        let Some((id, question, name, kind)) = read_query(message) else {
            self.wrong = Some("the resolver streamed a malformed query".to_owned());
            return Err(io::Error::other("malformed"));
        };
        self.note(&format!("stream {server} {name} {kind}"));
        if let Some(position) = self
            .truncated
            .iter()
            .position(|truncated| *truncated == question)
        {
            self.truncated.remove(position);
        } else if !self.tcp {
            self.wrong = Some(format!(
                "the resolver streamed {name} without a truncated reply"
            ));
        }
        let index = self.servers.iter().position(|known| *known == server);
        match index.map(|index| self.roles[index]) {
            Some(Role::Healthy) => Ok(self.answer(id, &question, &name, kind, true)),
            _ => Err(io::Error::from(io::ErrorKind::ConnectionRefused)),
        }
    }
}

pub struct DnsOutcome {
    pub digest: u64,
    pub wrong: Option<String>,
    pub stats: DnsStats,
    pub shape: String,
}

fn simulate(seed: u64, mutation: DnsMutation) -> DnsOutcome {
    let world = World::of(seed);
    let shape = world.describe();
    let mut config = world.config.clone();
    if mutation == DnsMutation::DropSearch {
        config.search.clear();
    }
    let mut wire = FakeWire {
        mutation,
        rng: Rng::derive(seed, "dns-wire"),
        roles: world.roles.clone(),
        servers: world.config.nameservers.clone(),
        current: None,
        queue: VecDeque::new(),
        truncated: Vec::new(),
        zone: world.zone.clone(),
        stats: DnsStats {
            seeds: 1,
            ..DnsStats::default()
        },
        tcp: world.config.tcp,
        trace: fnv(FNV_OFFSET, shape.as_bytes()),
        wrong: None,
        id: None,
    };
    let queried = if world.absolute {
        format!("{}.", world.name)
    } else {
        world.name.clone()
    };
    let Poll::Ready(got) = pin!(dns::lookup(&config, &world.hosts, &queried, &mut wire))
        .poll(&mut Context::from_waker(Waker::noop()))
    else {
        panic!("the lookup waited on a wire whose every call is ready at once");
    };
    let expected = world.expected();
    let mut wrong = wire.wrong.take();
    match (&got, &expected) {
        (Ok(got), Some(expected)) if got == expected => {
            wire.stats.found += 1;
            wire.stats.from_hosts += u64::from(!world.listed.is_empty());
        }
        (Err(error), None) if error.kind() == io::ErrorKind::NotFound => wire.stats.not_found += 1,
        _ => {
            wrong.get_or_insert_with(|| format!("resolved {got:?}, the zone holds {expected:?}"));
        }
    }
    wire.note(&format!("{got:?}"));
    DnsOutcome {
        digest: wire.trace,
        wrong,
        stats: wire.stats,
        shape,
    }
}

pub fn run_seed(seed: u64, mutation: DnsMutation) -> DnsOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        DnsOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: DnsStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: DnsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut DnsStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!("seed {seed} digest {:016x} {}", first.digest, first.shape);
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: trace digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: DnsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario dns --seed {seed}");
    if mutation != DnsMutation::None {
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
    mutation: DnsMutation,
    totals: &DnsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=dns mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        DnsMutation::None if failed == 0 => ExitCode::SUCCESS,
        DnsMutation::None => ExitCode::FAILURE,
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
