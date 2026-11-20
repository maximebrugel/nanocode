use nanocode_agent_config::configuration::{AgentResult, RuntimeBackend, RuntimeRequest};
use nanocode_agent_core::{
    BackendId, HarnessFamily, ModelSelection, NanocodeError, Result, Thinking,
    backend::BackendFuture,
};
use nanocode_backend_antigravity_client::{CacheConfig, Endpoint, GoogleConfig, RetryPolicy};
use nanocode_durability::CheckpointFormat;
use std::sync::Arc;

pub const FAMILY: &str = "antigravity";
pub const DEFAULT_MODEL: &str = "gemini-4-neon";

#[derive(Clone, Default)]
pub struct AntigravityHarness {
    endpoint: Option<Endpoint>,
    google: GoogleConfig,
    retry: Option<RetryPolicy>,
    cache: CacheConfig,
    model: Option<String>,
    app_data: Option<std::path::PathBuf>,
    #[cfg(not(target_family = "wasm"))]
    connection: Arc<nanocode_sync::OnceCell<Connection>>,
}

#[cfg(not(target_family = "wasm"))]
#[derive(Clone)]
struct Connection {
    sender: Arc<dyn nanocode_backend_antigravity_client::Sender>,
    endpoint: Endpoint,
}

impl From<GoogleConfig> for AntigravityHarness {
    fn from(google: GoogleConfig) -> Self {
        Self::new().google(google)
    }
}

impl AntigravityHarness {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn endpoint(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }

    #[must_use]
    pub fn google(mut self, google: GoogleConfig) -> Self {
        self.google = google;
        self
    }

    #[must_use]
    pub const fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    #[must_use]
    pub const fn explicit_cache(mut self, cache: CacheConfig) -> Self {
        self.cache = cache;
        self
    }

    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn app_data(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.app_data = Some(path.into());
        self
    }

    pub fn selection(model: impl Into<String>) -> ModelSelection {
        ModelSelection::new(HarnessFamily::new(FAMILY), model)
    }
}

#[cfg(not(target_family = "wasm"))]
pub const fn level(thinking: Thinking) -> Option<&'static str> {
    match thinking {
        Thinking::None => Some("MINIMAL"),
        Thinking::Low => Some("LOW"),
        Thinking::Medium => Some("MEDIUM"),
        _ => Some("HIGH"),
    }
}

impl RuntimeBackend for AntigravityHarness {
    fn id(&self) -> BackendId {
        BackendId::new(FAMILY)
    }

    fn is_harness(&self) -> bool {
        true
    }

    fn default_model(&self) -> ModelSelection {
        Self::selection(
            self.model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        )
    }

    fn default_thinking(&self) -> Option<Thinking> {
        Some(Thinking::High)
    }

    fn validate_model(&self, model: &str) -> Result<()> {
        let lower = model.to_ascii_lowercase();
        match lower.starts_with("gemini") || lower.starts_with("claude") {
            true => Ok(()),
            false => Err(NanocodeError::InvalidRequest(format!(
                "the antigravity harness runs gemini-* models (or claude-* through an Antigravity gateway), not {model:?}"
            ))),
        }
    }

    fn checkpoint_format(&self) -> Option<Arc<dyn CheckpointFormat>> {
        None
    }

    fn build(&self, request: RuntimeRequest) -> BackendFuture<AgentResult> {
        let harness = self.clone();
        Box::pin(async move { harness.assemble(request).await })
    }
}

#[cfg(target_family = "wasm")]
impl AntigravityHarness {
    async fn assemble(&self, _request: RuntimeRequest) -> AgentResult {
        Err(NanocodeError::UnsupportedCapability {
            capability: "antigravity harness on wasm",
        })
    }
}

#[cfg(not(target_family = "wasm"))]
impl AntigravityHarness {
    async fn assemble(&self, request: RuntimeRequest) -> AgentResult {
        use crate::driver::{Config, Driver};
        use crate::nanotools::NanoTools;
        use crate::profile::Profile;
        use crate::request::{Ids, Prefix};
        use nanocode_agent_config::configuration::DelegationAuthority;
        use nanocode_agent_core::{AgentHandle, backend::BackendRuntime};
        use nanocode_backend_antigravity_client::{Client, ExplicitCache};
        use nanocode_backend_antigravity_tools::{DECLARATIONS, Engine};
        use nanocode_tool_contracts::ToolCollection as _;
        use nanocode_tool_host::{Tools, runtime::ToolRuntime};
        let invalid = |error: String| NanocodeError::InvalidRequest(error);
        let RuntimeRequest {
            request: spec,
            settings,
            defaults,
            workspace,
            subagents,
            services,
        } = request;
        let settings = settings.or(&defaults);
        let model = spec.model.clone();
        let session = spec.options.session_id.clone().unwrap_or_else(|| {
            new_id(&format!(
                "{}{:?}",
                model.as_str(),
                nanocode_platform::system_time()
            ))
        });
        let mut fresh = false;
        let connection = self
            .connection
            .get_or_try_init(|| async {
                fresh = true;
                let sender = Client::native_sender().map_err(|error| invalid(error.to_string()))?;
                let endpoint = match self.endpoint.clone() {
                    Some(endpoint) => endpoint,
                    None => Endpoint::discover(
                        &self.google,
                        &|name: &str| std::env::var(name).ok(),
                        Arc::clone(&sender),
                        model.as_str(),
                    )
                    .await
                    .map_err(invalid)?,
                };
                Ok::<_, NanocodeError>(Connection { sender, endpoint })
            })
            .await?
            .clone();
        let notice = connection.endpoint.notice.clone().filter(|_| fresh);
        let Connection { sender, endpoint } = connection;
        if endpoint.route.is_vertex() && model.as_str().to_ascii_lowercase().starts_with("claude") {
            return Err(invalid(format!(
                "{model} on Vertex AI is served by Anthropic's Model Garden publisher (streamRawPredict with the Messages API), not the Gemini generateContent API the antigravity harness speaks; run it with --harness claude and CLAUDE_CODE_USE_VERTEX=1 (ANTHROPIC_VERTEX_PROJECT_ID, CLOUD_ML_REGION), which uses the same Application Default Credentials",
                model = model.as_str()
            )));
        }
        let mut client = Client::new(endpoint, sender);
        if let Some(retry) = self.retry {
            client = client.with_retry(retry);
        }
        let profile = Profile::for_model(model.as_str()).with_level(level(spec.thinking));
        let root = spec.parent.as_ref().map_or_else(
            || session.clone(),
            |parent| parent.root_session_id().to_owned(),
        );
        let handle = AgentHandle::new(session.clone(), model.clone(), spec.spawn_factory.clone())
            .with_root_session_id(root)
            .with_spawn_factory(spec.spawn_factory.clone());
        let selected = match &settings.tools {
            Some(tools) => tools.materialize(handle.clone()),
            None => Tools::builder().without_defaults().build(),
        }
        .map_err(|error| invalid(error.to_string()))?;
        let selected = services
            .prepare_tools(&handle, selected, None)
            .map_err(|error| invalid(error.to_string()))?;
        let mut additive = services.shared_tools(false).await?;
        if let Some(subagents) = &subagents {
            additive.extend(subagents.tools(handle.clone(), DelegationAuthority::Turn));
        }
        let selected = selected.add_shared_tools(additive)?;
        let runtime = ToolRuntime::new_with_tools(workspace.to_path_buf(), Vec::new(), &selected);
        let nano = NanoTools::new(runtime, &session);
        let app_data = self.app_data.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
                .join(".nanocode")
                .join("antigravity")
        });
        let brain = app_data.join("brain").join(&session);
        let shell = services.process_executors().shell(Vec::new());
        let search = crate::search::GoogleSearch {
            client: client.clone(),
            model: profile.model.clone(),
        };
        let engine = Engine::new(
            shell,
            workspace.to_path_buf(),
            brain.clone(),
            session.clone(),
            Some(Arc::new(search)),
        );
        let skills = skills(&workspace);
        let system = crate::prompts::system(&crate::prompts::Environment {
            os: std::env::consts::OS,
            workspace: &workspace,
            app_data: &app_data,
            conversation: &session,
            skills: &skills,
        });
        let system = match settings.instructions.as_deref() {
            Some(extra) if !extra.trim().is_empty() => format!("{system}\n{extra}"),
            _ => system,
        };
        let mut declarations = DECLARATIONS
            .lines()
            .map(|line| {
                match line.starts_with("{\"name\":\"read_file\"") && profile.claude_read_file {
                    true => nanocode_backend_antigravity_tools::READ_FILE_CLAUDE
                        .trim_end()
                        .to_owned(),
                    false => line.to_owned(),
                }
            })
            .collect::<Vec<_>>();
        declarations.extend(nano.declarations.iter().cloned());
        let prefix = Prefix::new(&system, &declarations);
        let mut cache_config = self.cache;
        if std::env::var("NANOCODE_ANTIGRAVITY_EXPLICIT_CACHE").is_ok_and(|value| value == "1") {
            cache_config.enabled = true;
        }
        let cache = cache_config
            .enabled
            .then(|| Arc::new(ExplicitCache::new(cache_config)));
        let ids = Ids {
            cascade: session.clone(),
            trajectory: new_id(&format!("trajectory{session}")),
        };
        let (runtime, events) = BackendRuntime::new(session.clone());
        let driver = Driver::new(Config {
            client,
            profile,
            prefix,
            ids,
            engine,
            nano,
            model,
            thinking: spec.thinking,
            session,
            cache,
            logs: brain.join(".system_generated").join("logs"),
            notice,
        });
        Ok((runtime.bind(driver), events))
    }
}

#[cfg(not(target_family = "wasm"))]
fn skills(workspace: &std::path::Path) -> Vec<(String, String, String)> {
    let Ok(home) = nanocode_home::AgentHome::from_env() else {
        return Vec::new();
    };
    let project = nanocode_home::ProjectHome::discover(workspace);
    home.skills_for(&project)
        .skills
        .into_iter()
        .map(|skill| {
            let text = std::fs::read_to_string(&skill.skill_md).unwrap_or_default();
            let description = text
                .strip_prefix("---")
                .and_then(|rest| {
                    rest.split_once(
                        "
---",
                    )
                })
                .and_then(|(front, _)| {
                    front
                        .lines()
                        .find_map(|line| line.strip_prefix("description:"))
                        .map(|value| value.trim().trim_matches('"').to_owned())
                })
                .unwrap_or_default();
            (
                skill.name,
                skill.skill_md.display().to_string(),
                description,
            )
        })
        .collect()
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn new_id(seed: &str) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut first = DefaultHasher::new();
    (seed, 1_u8).hash(&mut first);
    let mut second = DefaultHasher::new();
    (seed, 2_u8).hash(&mut second);
    let hex = format!("{:016x}{:016x}", first.finish(), second.finish());
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}
