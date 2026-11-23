use std::{ops::Range, process::ExitCode};

use nanocode_http::types::header::{HeaderMap, HeaderName, HeaderValue};
use nanocode_sandbox::egress::{
    Grant, Injecting, Secret,
    sim::{self as egress_sim, Mutation},
    substitute, valid_path,
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

pub use egress_sim::Mutation as SecretsMutation;

const PREFIX: &str = "nc-secret-";
const HOSTS: [&str; 3] = ["api.one.test", "api.two.test", "git.three.test"];
const HEADERS: [&str; 2] = ["authorization", "x-api-key"];
const PATHS: [&str; 7] = [
    "/v1",
    "/v1/chat$",
    "/repos/a",
    "/Upload",
    "/repos/a/b$",
    "/",
    "/$",
];
const INVALID: [&str; 16] = [
    "",
    "$",
    "v1",
    "/v1/",
    "/v1/$",
    "//v1",
    "/v1//chat",
    "/v1/../repos",
    "/./v1",
    "/v1/..",
    "/v1%2fchat",
    "/repos;a",
    "/repos\\a",
    "/v1?x",
    "/v1 chat",
    "/v1$$",
];
const TARGETS: [&str; 50] = [
    "/",
    "/?x=1",
    "/v1",
    "/v1/",
    "/v1/chat",
    "/v1/chat/",
    "/v1/chat?stream=1",
    "/v1/chat?/../../repos/a",
    "/v1?/repos/a",
    "/v1/chatter",
    "/v10",
    "/V1/models",
    "/v1/chat/more",
    "/repos/a",
    "/repos/a.git",
    "/repos/a.git/info/refs",
    "/repos/a.gitx",
    "/repos/ab",
    "/repos/a/b",
    "/repos/a/b?x",
    "/repos/A/B",
    "/upload/x",
    "/v1/../repos/b",
    "/v1/./chat",
    "/v1/.",
    "/v1/..",
    "/v1/chat/..",
    "/v1/%2e%2e/admin",
    "/v1/%2E%2E/admin",
    "/v1/.%2e/admin",
    "/v1/%2e/chat",
    "/v1/..%2fadmin",
    "/v1%2f../admin",
    "/v1/x%2F..%2F..%2Fadmin",
    "/v1/..\\admin",
    "/v1\\..\\admin",
    "/v1/..%5cadmin",
    "/v1/..%5Cadmin",
    "/v1//chat",
    "//v1",
    "/v1/..;/admin",
    "/repos/a/..;x/b",
    "/v1/%252e%252e/admin",
    "/v1/%%32%65%%32%65/admin",
    "/v1/%zz",
    "/v1/x%",
    "/v1/%41",
    "/v1/chat%2fmore",
    "/repos/a/b;x",
    "/repos/a.git/../b",
];
const SECRETS_MAX: u64 = 4;
const REQUESTS_MIN: u64 = 20;
const REQUESTS_MAX: u64 = 80;

pub fn names() -> String {
    SecretsMutation::ALL
        .iter()
        .map(|mutation| mutation.name())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SecretsStats {
    pub seeds: u64,
    pub requests: u64,
    pub injected: u64,
    pub withheld: u64,
    pub ambiguous: u64,
}

impl SecretsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.requests += other.requests;
        self.injected += other.injected;
        self.withheld += other.withheld;
        self.ambiguous += other.ambiguous;
    }
}

type Segments = Vec<Vec<u8>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Inside,
    Unclear,
    Outside,
}

fn unescape(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = bytes.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    Some(out)
}

fn resolved(segments: impl IntoIterator<Item = Vec<u8>>) -> Segments {
    let mut out = Segments::new();
    for segment in segments {
        match segment.as_slice() {
            b"." => {}
            b".." => {
                out.pop();
            }
            _ => out.push(segment),
        }
    }
    out
}

fn decoded_view(bytes: &[u8]) -> Segments {
    let slashed = bytes
        .iter()
        .map(|&b| if b == b'\\' { b'/' } else { b })
        .collect::<Vec<_>>();
    resolved(
        slashed
            .split(|&b| b == b'/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| {
                segment
                    .split(|&b| b == b';')
                    .next()
                    .unwrap_or_default()
                    .to_vec()
            }),
    )
}

fn views(target: &str) -> Option<(Segments, Vec<Option<Segments>>)> {
    let end = target.find(['?', '#']).unwrap_or(target.len());
    let rest = target[..end].strip_prefix('/')?.as_bytes();
    let raw = if rest.is_empty() {
        Segments::new()
    } else {
        rest.split(|&b| b == b'/').map(<[u8]>::to_vec).collect()
    };
    let once = unescape(rest);
    let twice = once.as_deref().and_then(unescape);
    let views = vec![
        Some(raw.clone()),
        Some(resolved(raw.clone())),
        once.as_deref().map(decoded_view),
        twice.as_deref().map(decoded_view),
    ];
    Some((raw, views))
}

fn entry_segments(entry: &str) -> (Segments, bool) {
    let (path, exact) = entry
        .strip_suffix('$')
        .map_or((entry, false), |path| (path, true));
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase().into_bytes())
        .collect();
    (segments, exact)
}

fn inside(entry: &(Segments, bool), view: &Segments) -> bool {
    let (segments, exact) = entry;
    let view = view
        .iter()
        .map(|segment| segment.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if *exact {
        return view == *segments;
    }
    let Some((last, parents)) = segments.split_last() else {
        return true;
    };
    view.len() >= segments.len()
        && view[..parents.len()] == *parents
        && (view[parents.len()] == *last
            || view[parents.len()] == [last.as_slice(), b".git"].concat())
}

fn scope(paths: &[String], target: &str) -> Scope {
    if paths.is_empty() {
        return Scope::Inside;
    }
    let entries = paths
        .iter()
        .filter(|entry| !INVALID.contains(&entry.as_str()))
        .map(|entry| entry_segments(entry))
        .collect::<Vec<_>>();
    let Some((raw, views)) = views(target) else {
        return Scope::Outside;
    };
    let reached = |view: &Segments| entries.iter().any(|entry| inside(entry, view));
    if views.iter().flatten().any(|view| !reached(view)) {
        return Scope::Outside;
    }
    if views.iter().all(|view| view.as_ref() == Some(&raw)) {
        Scope::Inside
    } else {
        Scope::Unclear
    }
}

fn pick<'a>(rng: &mut Rng, from: &[&'a str]) -> &'a str {
    from[rng.pick(from.len())]
}

fn secret(rng: &mut Rng, seed: u64, index: u64) -> Secret {
    let mut hosts = (0..rng.range(1, 2))
        .map(|_| pick(rng, &HOSTS).to_owned())
        .collect::<Vec<_>>();
    hosts.sort();
    hosts.dedup();
    let paths = (0..rng.below(3))
        .map(|_| {
            if rng.below(8) == 0 {
                pick(rng, &INVALID).to_owned()
            } else {
                pick(rng, &PATHS).to_owned()
            }
        })
        .collect();
    Secret {
        header: pick(rng, &HEADERS).to_owned(),
        hosts,
        name: format!("SECRET_{index}"),
        paths,
        placeholder: format!("{PREFIX}K{index}"),
        value: format!("real-{seed:x}-{index}-{}", rng.draw()),
    }
}

fn run(seed: u64, stats: &mut SecretsStats) -> Result<u64, String> {
    let mut rng = Rng::derive(seed, "secrets");
    let grant = Grant {
        secrets: (0..rng.range(1, SECRETS_MAX))
            .map(|index| secret(&mut rng, seed, index))
            .collect(),
    };
    if let Some(entry) = PATHS.iter().find(|entry| !valid_path(entry)) {
        return Err(format!("the path entry {entry:?} was refused"));
    }
    if let Some(entry) = INVALID.iter().find(|entry| valid_path(entry)) {
        return Err(format!("the path entry {entry:?} was accepted"));
    }
    let invalid = grant
        .secrets
        .iter()
        .flat_map(|secret| &secret.paths)
        .any(|entry| INVALID.contains(&entry.as_str()));
    if grant.invalid_path().is_some() != invalid {
        return Err(format!(
            "the grant's invalid path is {:?} while one is listed: {invalid}",
            grant.invalid_path()
        ));
    }
    let shown = format!("{grant:?}");
    if let Some(leaked) = grant
        .secrets
        .iter()
        .find(|secret| shown.contains(&secret.value))
    {
        return Err(format!("debugging the grant shows {}'s value", leaked.name));
    }
    let mut digest = FNV_OFFSET;
    for _ in 0..rng.range(REQUESTS_MIN, REQUESTS_MAX) {
        stats.requests += 1;
        let host = pick(&mut rng, &HOSTS);
        let target = pick(&mut rng, &TARGETS);
        let mut headers = HeaderMap::new();
        let mut sent = Vec::new();
        for name in HEADERS {
            if rng.below(3) == 0 {
                continue;
            }
            let carried = grant
                .secrets
                .iter()
                .filter(|_| rng.below(2) == 0)
                .map(|secret| secret.placeholder.clone())
                .collect::<Vec<_>>();
            let value = format!("Bearer {}", carried.join(" "));
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::try_from(value.as_str()).expect("a placeholder header is valid"),
            );
            sent.push((name, carried));
        }
        let original = headers.clone();
        let bound = grant.bound(host, target);
        let pairs = substitute(&mut headers, &bound, Injecting::Swap { prefix: PREFIX });
        for secret in &grant.secrets {
            let carried = secret.hosts.iter().any(|pattern| pattern == host)
                && sent.iter().any(|(name, carried)| {
                    *name == secret.header && carried.contains(&secret.placeholder)
                });
            let scope = if carried {
                scope(&secret.paths, target)
            } else {
                Scope::Outside
            };
            let written = headers
                .iter()
                .any(|(_, value)| value.to_str().unwrap_or_default().contains(&secret.value));
            match (scope, written) {
                (Scope::Inside, true) => stats.injected += 1,
                (Scope::Outside, false) => stats.withheld += 1,
                (Scope::Unclear, _) => stats.ambiguous += 1,
                (Scope::Inside, false) => {
                    return Err(format!(
                        "{} was not substituted for {host}{target}",
                        secret.name
                    ));
                }
                (Scope::Outside, true) => {
                    return Err(format!(
                        "{} reached {host}{target}, outside its hosts {:?} and paths {:?}",
                        secret.name, secret.hosts, secret.paths
                    ));
                }
            }
        }
        for (name, value) in &headers {
            let echoed = pairs.redact(value.as_bytes());
            if let Some(leaked) = grant
                .secrets
                .iter()
                .find(|secret| String::from_utf8_lossy(&echoed).contains(&secret.value))
            {
                return Err(format!(
                    "an echoed {name} header shows {}'s value",
                    leaked.name
                ));
            }
            let before = original.get(name).map(HeaderValue::as_bytes);
            if before != Some(echoed.as_slice()) {
                return Err(format!(
                    "an echoed {name} header does not read back as what the caller sent"
                ));
            }
        }
        digest = fnv(fnv(digest, host.as_bytes()), target.as_bytes());
        digest = fnv(digest, &stats.injected.to_le_bytes());
    }
    Ok(digest)
}

pub fn judge(
    seed: u64,
    mutation: SecretsMutation,
    twice: bool,
    verbose: bool,
    totals: &mut SecretsStats,
) -> Option<String> {
    let once = |stats: &mut SecretsStats| {
        egress_sim::mutate(mutation);
        let result = run(seed, stats);
        egress_sim::mutate(Mutation::None);
        result
    };
    let mut stats = SecretsStats {
        seeds: 1,
        ..SecretsStats::default()
    };
    let first = once(&mut stats);
    totals.add(&stats);
    if verbose {
        println!("seed {seed} {first:?} stats={stats:?}");
    }
    let digest = match first {
        Ok(digest) => digest,
        Err(wrong) => return Some(wrong),
    };
    if !twice {
        return None;
    }
    match once(&mut SecretsStats::default()) {
        Ok(again) if again == digest => None,
        Ok(again) => Some(format!(
            "nondeterminism: digest {digest:016x} then {again:016x}"
        )),
        Err(wrong) => Some(format!("nondeterminism: the second run failed: {wrong}")),
    }
}

pub fn replay(seed: u64, mutation: SecretsMutation) -> String {
    let mut command = format!("nanocode-sim --scenario secrets --seed {seed}");
    if mutation != SecretsMutation::None {
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
    mutation: SecretsMutation,
    totals: &SecretsStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=secrets mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        SecretsMutation::None if failed == 0 => ExitCode::SUCCESS,
        SecretsMutation::None => ExitCode::FAILURE,
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
