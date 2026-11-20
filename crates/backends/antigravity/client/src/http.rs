use crate::sse::SseDecoder;
use crate::wire::{GenerateResponse, Part, Reply, merge_parts};
use nanocode_http::Method;
use nanocode_http::header::{HeaderMap, HeaderName, HeaderValue};
use nanocode_http::layer::{Request, Sender};
use nanocode_sync::StreamExt;
use std::sync::Arc;
use std::time::Duration;

pub const USER_AGENT_GATEWAY: &str = "antigravity/cli/1.5.13 (aidev_client; os_type=linux; arch=amd64; cl=1025641825; auth_method=gateway)";
pub const USER_AGENT_API_KEY: &str = "antigravity/cli/1.5.13 (aidev_client; os_type=linux; arch=amd64; cl=1025641825; auth_method=api_key)";
pub const GATEWAY_TIER: &str = "CiphbnRpZ3Jhdml0eS5jbGlfZ2F0ZXdheS5lbnZfcHJvZC50aWVyX3BhaWQ=";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    ApiKey(String),
    Bearer(String),
    None,
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub base_url: String,
    pub auth: Auth,
    pub user_agent: String,
    pub gateway: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub initial: Duration,
    pub multiplier: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 4,
            initial: Duration::from_secs(1),
            multiplier: 2,
        }
    }
}

#[derive(Debug)]
pub enum ClientError {
    Transport(String),
    Status { status: u16, body: String },
    Decode(String),
    Blocked(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(detail) => write!(formatter, "Gemini transport error: {detail}"),
            Self::Status { status, body } => {
                write!(formatter, "Gemini API returned {status}: {body}")
            }
            Self::Decode(detail) => {
                write!(formatter, "Gemini response could not be decoded: {detail}")
            }
            Self::Blocked(detail) => write!(formatter, "Gemini blocked the request: {detail}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl Endpoint {
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
        if let Some(url) = get("AGY_LLM_GATEWAY_URL") {
            return Some(Self {
                base_url: url.trim_end_matches('/').to_owned(),
                auth: get("AGY_LLM_GATEWAY_API_KEY").map_or(Auth::None, Auth::Bearer),
                user_agent: USER_AGENT_GATEWAY.to_owned(),
                gateway: true,
            });
        }
        let key = get("GEMINI_API_KEY").or_else(|| get("GOOGLE_API_KEY"))?;
        let base = get("GEMINI_BASE_URL")
            .or_else(|| get("GOOGLE_GEMINI_BASE_URL"))
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        Some(Self {
            base_url: base.trim_end_matches('/').to_owned(),
            auth: Auth::ApiKey(key),
            user_agent: USER_AGENT_API_KEY.to_owned(),
            gateway: false,
        })
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let mut put = |name: &'static str, value: &str| {
            if let Ok(value) = HeaderValue::from_str(value) {
                headers.insert(HeaderName::from_static(name), value);
            }
        };
        put("user-agent", &self.user_agent);
        put("content-type", "application/json");
        if self.gateway {
            put("x-goog-ext-525006001-bin", GATEWAY_TIER);
        }
        put("accept-encoding", "gzip");
        match &self.auth {
            Auth::ApiKey(key) => put("x-goog-api-key", key),
            Auth::Bearer(token) => put("authorization", &format!("Bearer {token}")),
            Auth::None => {}
        }
        headers
    }
}

#[derive(Clone)]
pub struct Client {
    endpoint: Endpoint,
    sender: Arc<dyn Sender>,
    retry: RetryPolicy,
}

#[cfg(not(target_family = "wasm"))]
struct NativeSender(nanocode_http::Client);

#[cfg(not(target_family = "wasm"))]
impl Sender for NativeSender {
    fn send(
        &self,
        request: Request,
    ) -> nanocode_http::layer::BoxFuture<
        '_,
        Result<nanocode_http::layer::Response, nanocode_http::layer::Error>,
    > {
        Box::pin(async move {
            nanocode_http::ClientSender::new(&self.0)
                .send(request)
                .await
        })
    }
}

impl Client {
    pub fn new(endpoint: Endpoint, sender: Arc<dyn Sender>) -> Self {
        Self {
            endpoint,
            sender,
            retry: RetryPolicy::default(),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn native(endpoint: Endpoint) -> Result<Self, ClientError> {
        nanocode_tls::install_default_rustls_crypto_provider();
        let http = nanocode_http::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(600))
            .build()
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(Self::new(endpoint, Arc::new(NativeSender(http))))
    }

    #[must_use]
    pub const fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint.base_url)
    }

    async fn send_once(
        &self,
        method: Method,
        path: &str,
        body: &str,
    ) -> Result<nanocode_http::layer::Response, Attempt> {
        let response = self
            .sender
            .send(Request {
                method,
                url: self.url(path),
                headers: self.endpoint.headers(),
                body: body.as_bytes().to_vec(),
            })
            .await
            .map_err(|error| Attempt::Retry(ClientError::Transport(error.detail().to_owned())))?;
        let status = response.status;
        if status.is_success() {
            return Ok(response);
        }
        let retry = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
        let body = collect(response.body).await.unwrap_or_default();
        let error = ClientError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        Err(match retry {
            true => Attempt::Retry(error),
            false => Attempt::Fatal(error),
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: &str,
    ) -> Result<nanocode_http::layer::Response, ClientError> {
        let mut delay = self.retry.initial;
        let mut attempt = 0;
        loop {
            match self.send_once(method.clone(), path, body).await {
                Ok(response) => return Ok(response),
                Err(Attempt::Fatal(error)) => return Err(error),
                Err(Attempt::Retry(error)) if attempt >= self.retry.max_retries => {
                    return Err(error);
                }
                Err(Attempt::Retry(_)) => {
                    nanocode_platform::sleep(delay).await;
                    delay = delay.saturating_mul(self.retry.multiplier);
                    attempt += 1;
                }
            }
        }
    }

    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let response = self.send(method, path, body).await?;
        let bytes = collect(response.body)
            .await
            .map_err(|error| ClientError::Transport(error.detail().to_owned()))?;
        serde_json::from_slice(&bytes).map_err(|error| ClientError::Decode(error.to_string()))
    }

    pub async fn generate(&self, model: &str, body: &str) -> Result<Reply, ClientError> {
        let value = self
            .json(
                Method::POST,
                &format!("/v1beta/models/{model}:generateContent"),
                body,
            )
            .await?;
        let response: GenerateResponse = serde_json::from_value(value)
            .map_err(|error| ClientError::Decode(error.to_string()))?;
        let mut reply = Reply::default();
        let mut chunks = Vec::new();
        absorb(&mut reply, response, &mut chunks);
        reply.parts = merge_parts(chunks);
        Ok(reply)
    }

    pub async fn stream(
        &self,
        model: &str,
        body: &str,
        on_part: &mut (dyn FnMut(&Part) + Send),
    ) -> Result<Reply, ClientError> {
        let path = format!("/v1beta/models/{model}:streamGenerateContent?alt=sse");
        let mut delay = self.retry.initial;
        let mut attempt = 0;
        loop {
            match self.stream_once(&path, body, on_part).await {
                Ok(reply) => return Ok(reply),
                Err((false, error)) | Err((true, error)) if attempt >= self.retry.max_retries => {
                    return Err(error);
                }
                Err((false, error)) => return Err(error),
                Err((true, _)) => {
                    nanocode_platform::sleep(delay).await;
                    delay = delay.saturating_mul(self.retry.multiplier);
                    attempt += 1;
                }
            }
        }
    }

    async fn stream_once(
        &self,
        path: &str,
        body: &str,
        on_part: &mut (dyn FnMut(&Part) + Send),
    ) -> Result<Reply, (bool, ClientError)> {
        let response = match self.send_once(Method::POST, path, body).await {
            Ok(response) => response,
            Err(Attempt::Retry(error)) => return Err((true, error)),
            Err(Attempt::Fatal(error)) => return Err((false, error)),
        };
        let mut body = response.body;
        let mut decoder = SseDecoder::default();
        let mut reply = Reply::default();
        let mut chunks = Vec::new();
        let mut delivered = false;
        loop {
            let (events, done) = match body.next().await {
                Some(Ok(bytes)) => (decoder.push(&bytes), false),
                Some(Err(error)) => {
                    return Err((
                        !delivered,
                        ClientError::Transport(error.detail().to_owned()),
                    ));
                }
                None => (decoder.finish(), true),
            };
            for event in events {
                if event.trim().is_empty() || event.trim() == "[DONE]" {
                    continue;
                }
                let response: GenerateResponse = serde_json::from_str(&event)
                    .map_err(|error| (false, ClientError::Decode(format!("{error}: {event}"))))?;
                let start = chunks.len();
                absorb(&mut reply, response, &mut chunks);
                for part in &chunks[start..] {
                    delivered = true;
                    on_part(part);
                }
            }
            if done {
                break;
            }
        }
        reply.parts = merge_parts(chunks);
        if reply.parts.is_empty() && reply.finish_reason.is_none() {
            return Err((
                true,
                ClientError::Decode("stream ended without a candidate".to_owned()),
            ));
        }
        Ok(reply)
    }
}

enum Attempt {
    Retry(ClientError),
    Fatal(ClientError),
}

fn absorb(reply: &mut Reply, response: GenerateResponse, chunks: &mut Vec<Part>) {
    if let Some(usage) = response.usage_metadata {
        reply.usage = usage;
    }
    if response.model_version.is_some() {
        reply.model_version = response.model_version;
    }
    for candidate in response.candidates.into_iter().take(1) {
        if let Some(content) = candidate.content {
            chunks.extend(content.parts);
        }
        if candidate.finish_reason.is_some() {
            reply.finish_reason = candidate.finish_reason;
        }
        if candidate.grounding_metadata.is_some() {
            reply.grounding = candidate.grounding_metadata;
        }
    }
}

async fn collect(
    mut body: nanocode_http::layer::Body,
) -> Result<Vec<u8>, nanocode_http::layer::Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}
