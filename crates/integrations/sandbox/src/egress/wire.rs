use crate::egress::address::host_matches;
use serde::{Deserialize, Serialize};

const FRAMING: [&str; 9] = [
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "via",
];

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Credential {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub secrets: Vec<Secret>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    pub header: String,
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    pub placeholder: String,
    pub value: String,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = if crate::egress::sim::mutated(crate::egress::sim::Mutation::DebugShowsValue) {
            self.value.as_str()
        } else {
            "<redacted>"
        };
        formatter
            .debug_struct("Secret")
            .field("header", &self.header)
            .field("hosts", &self.hosts)
            .field("name", &self.name)
            .field("paths", &self.paths)
            .field("placeholder", &self.placeholder)
            .field("value", &value)
            .finish()
    }
}

pub const EXACT_PATH: &str = "$";

#[must_use]
pub fn path_reached(paths: &[String], path: &str) -> bool {
    if paths.is_empty() || crate::egress::sim::mutated(crate::egress::sim::Mutation::PathsIgnored)
    {
        return true;
    }
    let path = path.to_ascii_lowercase();
    paths.iter().any(|prefix| {
        let prefix = prefix.to_ascii_lowercase();
        if let Some(exact) = prefix.strip_suffix(EXACT_PATH) {
            return path
                .strip_prefix(exact)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('?'));
        }
        path.strip_prefix(prefix.as_str()).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('/')
                || rest.starts_with('?')
                || rest.starts_with(".git/")
                || rest == ".git"
        })
    })
}

impl Grant {
    #[must_use]
    pub fn bound(&self, host: &str, path: &str) -> Vec<&Secret> {
        self.secrets
            .iter()
            .filter(|secret| secret.injects(host) && secret.reaches(path))
            .collect()
    }

    #[must_use]
    pub fn set(&self, host: &str, path: &str) -> Vec<&Secret> {
        self.secrets
            .iter()
            .filter(|secret| secret.sets(host) && secret.reaches(path))
            .take(1)
            .collect()
    }
}

impl Secret {
    #[must_use]
    pub fn reaches(&self, path: &str) -> bool {
        path_reached(&self.paths, path)
    }

    #[must_use]
    pub fn binds(&self, host: &str) -> Option<&str> {
        self.hosts
            .iter()
            .find(|pattern| binds(pattern, host))
            .map(String::as_str)
    }

    #[must_use]
    pub fn injects(&self, host: &str) -> bool {
        !self.header.is_empty() && !framing(&self.header) && self.binds(host).is_some()
    }

    #[must_use]
    pub fn sets(&self, host: &str) -> bool {
        let token = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c);
        !self.header.is_empty()
            && self.header.bytes().all(token)
            && !self.value.bytes().any(|c| matches!(c, b'\r' | b'\n' | 0))
            && self
                .hosts
                .iter()
                .any(|pattern| !pattern.is_empty() && host_matches(pattern, host))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ca {
    pub cert: String,
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub callers: Vec<Calls>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calls {
    pub called: Vec<Called>,
    pub caller: Credential,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Called {
    pub host: String,
    pub intercepted: bool,
    pub port: i32,
    pub refused: bool,
    pub requests: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

#[must_use]
pub fn binds(pattern: &str, host: &str) -> bool {
    !pattern.starts_with("*.") && host_matches(pattern, host)
}

#[must_use]
pub fn framing(header: &str) -> bool {
    let header = header.to_ascii_lowercase();
    FRAMING.contains(&header.as_str()) || header.starts_with("proxy-")
}

#[must_use]
pub fn valid_header(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !framing(name)
}

#[must_use]
pub fn valid_host(pattern: &str) -> bool {
    pattern.len() <= 253
        && pattern.contains('.')
        && pattern.parse::<std::net::Ipv4Addr>().is_err()
        && pattern.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        && !pattern
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

#[must_use]
pub fn normalize_hosts<'a>(
    hosts: impl IntoIterator<Item = &'a str>,
    most: usize,
) -> Option<Vec<String>> {
    let mut hosts: Vec<String> = hosts
        .into_iter()
        .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
        .collect();
    hosts.sort();
    hosts.dedup();
    (!hosts.is_empty() && hosts.len() <= most && hosts.iter().all(|host| valid_host(host)))
        .then_some(hosts)
}
