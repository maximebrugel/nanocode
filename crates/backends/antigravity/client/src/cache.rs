use crate::http::Client;
use crate::json::Json;
use nanocode_http::Method;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug)]
pub struct CacheConfig {
    pub enabled: bool,
    pub min_prefix_bytes: usize,
    pub ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_prefix_bytes: 16_384,
            ttl: Duration::from_secs(3600),
        }
    }
}

struct Entry {
    name: String,
    key: u64,
    expires: SystemTime,
}

pub struct ExplicitCache {
    config: CacheConfig,
    entry: Mutex<Option<Entry>>,
}

impl ExplicitCache {
    pub const fn new(config: CacheConfig) -> Self {
        Self {
            config,
            entry: Mutex::new(None),
        }
    }

    pub const fn config(&self) -> CacheConfig {
        self.config
    }

    pub async fn resolve(
        &self,
        client: &Client,
        model: &str,
        system: &Json,
        tools: &Json,
    ) -> Option<String> {
        if !self.config.enabled {
            return None;
        }
        let system = system.encode();
        let tools = tools.encode();
        if system.len() + tools.len() < self.config.min_prefix_bytes {
            return None;
        }
        let mut hasher = DefaultHasher::new();
        (model, &system, &tools).hash(&mut hasher);
        let key = hasher.finish();
        let now = nanocode_platform::system_time();
        let current = self.current(key);
        if let Some((name, expires)) = current {
            let left = expires.duration_since(now).unwrap_or_default();
            if left > self.config.ttl / 4 {
                return Some(name);
            }
            if left > Duration::ZERO && self.refresh(client, &name).await {
                return Some(name);
            }
        }
        self.create(client, model, &system, &tools, key).await
    }

    fn current(&self, key: u64) -> Option<(String, SystemTime)> {
        let entry = self.entry.lock().ok()?;
        entry
            .as_ref()
            .filter(|entry| entry.key == key)
            .map(|entry| (entry.name.clone(), entry.expires))
    }

    fn ttl(&self) -> String {
        format!("{}s", self.config.ttl.as_secs())
    }

    async fn refresh(&self, client: &Client, name: &str) -> bool {
        let body = Json::object([("ttl", Json::str(self.ttl()))]).encode();
        let path = format!("{}?updateMask=ttl", client.endpoint().resource_path(name));
        if client.json(Method::PATCH, &path, &body).await.is_err() {
            return false;
        }
        if let Ok(mut entry) = self.entry.lock()
            && let Some(entry) = entry.as_mut()
        {
            entry.expires = nanocode_platform::system_time() + self.config.ttl;
        }
        true
    }

    async fn create(
        &self,
        client: &Client,
        model: &str,
        system: &str,
        tools: &str,
        key: u64,
    ) -> Option<String> {
        let body = Json::object([
            ("model", Json::str(client.endpoint().cache_model(model))),
            ("systemInstruction", Json::Raw(system.to_owned())),
            ("tools", Json::Raw(tools.to_owned())),
            ("ttl", Json::str(self.ttl())),
        ])
        .encode();
        let collection = client.endpoint().cache_collection()?;
        let created = client.json(Method::POST, &collection, &body).await.ok()?;
        let name = created.get("name")?.as_str()?.to_owned();
        if let Ok(mut entry) = self.entry.lock() {
            *entry = Some(Entry {
                name: name.clone(),
                key,
                expires: nanocode_platform::system_time() + self.config.ttl,
            });
        }
        Some(name)
    }
}
