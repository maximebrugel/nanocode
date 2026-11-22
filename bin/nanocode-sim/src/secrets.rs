use std::{ops::Range, process::ExitCode};

use nanocode_http::types::header::{HeaderMap, HeaderName, HeaderValue};
use nanocode_sandbox::egress::{
    Grant, Injecting, Secret,
    sim::{self as egress_sim, Mutation},
    substitute,
};

use crate::rng::{FNV_OFFSET, Rng, fnv};

pub use egress_sim::Mutation as SecretsMutation;

const PREFIX: &str = "nc-secret-";
const HOSTS: [&str; 3] = ["api.one.test", "api.two.test", "git.three.test"];
const HEADERS: [&str; 2] = ["authorization", "x-api-key"];
const PATHS: [&str; 4] = ["/v1", "/v1/chat$", "/repos/a", "/Upload"];
const TARGETS: [&str; 11] = [
    "/",
    "/v1",
    "/v1/chat",
    "/v1/chat?stream=1",
    "/v1/chatter",
    "/v10",
    "/V1/models",
    "/repos/a.git/info/refs",
    "/repos/ab",
    "/upload/x",
    "/v1/chat/more",
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
}

impl SecretsStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.requests += other.requests;
        self.injected += other.injected;
        self.withheld += other.withheld;
    }
}

fn reaches(paths: &[String], target: &str) -> bool {
    let target = target.to_ascii_lowercase();
    paths.is_empty()
        || paths.iter().any(|prefix| {
            let prefix = prefix.to_ascii_lowercase();
            match prefix.strip_suffix('$') {
                Some(exact) => target
                    .strip_prefix(exact)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('?')),
                None => target.strip_prefix(prefix.as_str()).is_some_and(|rest| {
                    rest.is_empty()
                        || rest.starts_with('/')
                        || rest.starts_with('?')
                        || rest.starts_with(".git/")
                        || rest == ".git"
                }),
            }
        })
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
        .map(|_| pick(rng, &PATHS).to_owned())
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
    let shown = format!("{grant:?}");
    if let Some(leaked) = grant.secrets.iter().find(|secret| shown.contains(&secret.value)) {
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
            let applies = secret.hosts.iter().any(|pattern| pattern == host)
                && reaches(&secret.paths, target)
                && sent.iter().any(|(name, carried)| {
                    *name == secret.header && carried.contains(&secret.placeholder)
                });
            let written = headers
                .iter()
                .any(|(_, value)| value.to_str().unwrap_or_default().contains(&secret.value));
            match (applies, written) {
                (true, true) => stats.injected += 1,
                (false, false) => stats.withheld += 1,
                (true, false) => {
                    return Err(format!(
                        "{} was not substituted for {host}{target}",
                        secret.name
                    ));
                }
                (false, true) => {
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
                return Err(format!("an echoed {name} header shows {}'s value", leaked.name));
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
        Ok(again) => Some(format!("nondeterminism: digest {digest:016x} then {again:016x}")),
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
