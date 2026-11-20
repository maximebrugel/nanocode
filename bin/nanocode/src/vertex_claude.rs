use nanocode::antigravity::{Client, GoogleAuth, vertex_base_url};
use nanocode_http::Method;
use nanocode_http::layer::{BoxFuture, Error, Layer, Next, Request, Response, Sender};
use nanocode_http::types::header::{AUTHORIZATION, HeaderName, HeaderValue};
use nanocode_sync::{Atom, SingleFlight};
use std::sync::Arc;

pub const USE_VERTEX: &str = "CLAUDE_CODE_USE_VERTEX";
pub const ANTHROPIC_VERSION: &str = "vertex-2023-10-16";
const PROJECT_VARIABLES: [&str; 2] = ["ANTHROPIC_VERTEX_PROJECT_ID", "GOOGLE_CLOUD_PROJECT"];
const REGION_VARIABLES: [&str; 2] = ["CLOUD_ML_REGION", "GOOGLE_CLOUD_LOCATION"];
const BASE_URL: &str = "ANTHROPIC_VERTEX_BASE_URL";
const DEFAULT_REGION: &str = "global";
const USER_PROJECT: &str = "x-goog-user-project";
const UNAUTHORIZED: u16 = 401;

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn enabled() -> bool {
    env(USE_VERTEX)
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

#[derive(Clone)]
struct Resolved {
    auth: GoogleAuth,
    project: String,
}

struct Shared {
    sender: Arc<dyn Sender>,
    base: String,
    region: String,
    resolved: Atom<Option<Resolved>>,
    flight: SingleFlight<Result<Resolved, String>>,
}

#[derive(Clone)]
pub struct VertexClaude {
    shared: Arc<Shared>,
}

impl VertexClaude {
    pub fn from_env() -> Result<Self, String> {
        let region = REGION_VARIABLES
            .iter()
            .find_map(|name| env(name))
            .unwrap_or_else(|| DEFAULT_REGION.to_owned());
        let base = env(BASE_URL)
            .unwrap_or_else(|| vertex_base_url(&region))
            .trim_end_matches('/')
            .to_owned();
        let sender = Client::native_sender().map_err(|error| format!("{USE_VERTEX}: {error}"))?;
        Ok(Self {
            shared: Arc::new(Shared {
                sender,
                base,
                region,
                resolved: Atom::new(None),
                flight: SingleFlight::default(),
            }),
        })
    }

    pub fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.shared.base)
    }

    async fn resolve(&self) -> Result<Resolved, String> {
        if let Some(resolved) = self.shared.resolved.read(Clone::clone) {
            return Ok(resolved);
        }
        let shared = Arc::clone(&self.shared);
        self.shared
            .flight
            .run(move || async move {
                let lookup = |name: &str| std::env::var(name).ok();
                let auth = GoogleAuth::discover(&lookup, Arc::clone(&shared.sender))
                    .await
                    .map_err(|error| format!("Claude on Vertex AI ({USE_VERTEX}): {error}"))?;
                let configured = PROJECT_VARIABLES.iter().find_map(|name| env(name));
                let project = auth.resolve_project(configured.as_deref(), &lookup).ok_or_else(|| {
                    format!(
                        "Claude on Vertex AI needs a Google Cloud project: set ANTHROPIC_VERTEX_PROJECT_ID or GOOGLE_CLOUD_PROJECT (the {} credentials from {} name none)",
                        auth.kind().name(),
                        auth.origin()
                    )
                })?;
                let resolved = Resolved {
                    auth: auth.with_user_project(project.clone()),
                    project,
                };
                shared.resolved.set(Some(resolved.clone()));
                Ok(resolved)
            })
            .await
    }

    fn rewrite(&self, request: &mut Request, project: &str) -> Result<(), Error> {
        if request.method != Method::POST || !request.url.ends_with("/messages") {
            return Ok(());
        }
        let mut body: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&request.body).map_err(|error| {
                Error::new(format!(
                    "Claude on Vertex AI: the Messages request is not a JSON object: {error}"
                ))
            })?;
        let model = body
            .remove("model")
            .and_then(|model| model.as_str().map(str::to_owned))
            .ok_or_else(|| {
                Error::new("Claude on Vertex AI: the Messages request names no model")
            })?;
        let stream = body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut ordered = serde_json::Map::new();
        ordered.insert(
            "anthropic_version".to_owned(),
            serde_json::Value::String(ANTHROPIC_VERSION.to_owned()),
        );
        ordered.extend(body);
        request.body = serde_json::to_vec(&ordered)
            .map_err(|error| Error::new(format!("Claude on Vertex AI: {error}")))?
            .into();
        request.url = format!(
            "{}/v1/projects/{project}/locations/{}/publishers/anthropic/models/{model}:{}",
            self.shared.base,
            self.shared.region,
            if stream {
                "streamRawPredict"
            } else {
                "rawPredict"
            }
        );
        request.headers.remove("x-api-key");
        request.headers.remove("anthropic-version");
        Ok(())
    }

    async fn authorize(resolved: &Resolved, request: &mut Request) -> Result<String, Error> {
        let token = resolved
            .auth
            .token()
            .await
            .map_err(|error| Error::new(format!("Claude on Vertex AI: {error}")))?;
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| Error::new(format!("Claude on Vertex AI: {error}")))?;
        value.set_sensitive(true);
        request.headers.insert(AUTHORIZATION, value);
        if let Some(project) = resolved.auth.user_project()
            && let Ok(value) = HeaderValue::from_str(project)
        {
            request
                .headers
                .insert(HeaderName::from_static(USER_PROJECT), value);
        }
        Ok(token)
    }
}

impl Layer for VertexClaude {
    fn send<'a>(
        &'a self,
        mut request: Request,
        next: Next<'a>,
    ) -> BoxFuture<'a, Result<Response, Error>> {
        Box::pin(async move {
            let resolved = self.resolve().await.map_err(Error::new)?;
            self.rewrite(&mut request, &resolved.project)?;
            let retry = Request {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            };
            let token = Self::authorize(&resolved, &mut request).await?;
            let response = next.send(request).await?;
            if response.status.as_u16() != UNAUTHORIZED {
                return Ok(response);
            }
            resolved.auth.invalidate(&token);
            let mut retry = retry;
            Self::authorize(&resolved, &mut retry).await?;
            next.send(retry).await
        })
    }
}
