mod discover;
mod gcloud;
mod sign;
mod source;
mod time;
mod wire;

use nanocode_http::layer::Sender;
use nanocode_sync::{Atom, SingleFlight};
use source::Source;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
pub use gcloud::{ACTIVE_CONFIG_ENV, CORE_PROJECT_ENV};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectSource {
    Configured,
    Environment,
    QuotaProject,
    Credentials,
    GcloudConfig,
}

impl ProjectSource {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Configured => "--google-project",
            Self::Environment => "GOOGLE_CLOUD_PROJECT",
            Self::QuotaProject => "the credentials' quota project",
            Self::Credentials => "the credentials' project",
            Self::GcloudConfig => "the gcloud configuration",
        }
    }

    pub const fn is_explicit(self) -> bool {
        matches!(self, Self::Configured | Self::Environment)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }

    pub fn detail(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    AccessToken,
    AuthorizedUser,
    ServiceAccount,
    ImpersonatedServiceAccount,
    ExternalAccount,
    ExternalAccountAuthorizedUser,
    MetadataServer,
    Gcloud,
}

impl CredentialKind {
    pub const fn is_user(self) -> bool {
        matches!(
            self,
            Self::AuthorizedUser | Self::ExternalAccountAuthorizedUser | Self::Gcloud
        )
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::AccessToken => "access token",
            Self::AuthorizedUser => "authorized_user",
            Self::ServiceAccount => "service_account",
            Self::ImpersonatedServiceAccount => "impersonated_service_account",
            Self::ExternalAccount => "external_account",
            Self::ExternalAccountAuthorizedUser => "external_account_authorized_user",
            Self::MetadataServer => "metadata server",
            Self::Gcloud => "gcloud CLI",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Token {
    value: String,
    expires: Option<SystemTime>,
}

impl Token {
    pub(crate) fn lasting(value: String, lifetime: Option<Duration>) -> Self {
        Self {
            value,
            expires: lifetime.map(|lifetime| nanocode_platform::system_time() + lifetime),
        }
    }

    pub(crate) const fn until(value: String, expires: SystemTime) -> Self {
        Self {
            value,
            expires: Some(expires),
        }
    }

    fn is_fresh(&self, now: SystemTime) -> bool {
        self.expires.is_none_or(|expires| {
            expires
                .duration_since(now)
                .is_ok_and(|left| left > REFRESH_MARGIN)
        })
    }
}

pub(crate) struct Found {
    source: Source,
    kind: CredentialKind,
    origin: String,
    project: Option<String>,
    quota_project: Option<String>,
    token: Option<Token>,
}

struct Inner {
    source: Source,
    kind: CredentialKind,
    origin: String,
    project: Option<String>,
    quota_project: Option<String>,
    gcloud_project: Option<String>,
    sender: Arc<dyn Sender>,
    cached: Atom<Option<Token>>,
    flight: SingleFlight<Result<Token, Error>>,
}

impl Inner {
    fn fresh(&self) -> Option<Token> {
        let now = nanocode_platform::system_time();
        self.cached
            .read(|cached| cached.as_ref().filter(|token| token.is_fresh(now)).cloned())
    }
}

#[derive(Clone)]
pub struct GoogleAuth {
    inner: Arc<Inner>,
    fallback_project: Option<String>,
}

impl PartialEq for GoogleAuth {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) && self.fallback_project == other.fallback_project
    }
}

impl Eq for GoogleAuth {}

impl std::fmt::Debug for GoogleAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleAuth")
            .field("kind", &self.inner.kind)
            .field("origin", &self.inner.origin)
            .field("project", &self.inner.project)
            .field("user_project", &self.user_project())
            .finish_non_exhaustive()
    }
}

impl GoogleAuth {
    pub fn access_token(token: impl Into<String>, sender: Arc<dyn Sender>) -> Self {
        Self::from_found(
            Found {
                source: Source::Static(token.into()),
                kind: CredentialKind::AccessToken,
                origin: "an access token".to_owned(),
                project: None,
                quota_project: None,
                token: None,
            },
            sender,
        )
    }

    pub async fn discover(
        lookup: &(dyn Fn(&str) -> Option<String> + Sync),
        sender: Arc<dyn Sender>,
    ) -> Result<Self, Error> {
        let found = discover::find(lookup, sender.as_ref()).await?;
        let gcloud_project = gcloud::active_project(lookup).await;
        Ok(Self::from_found(found, sender).with_gcloud_project(gcloud_project))
    }

    fn with_gcloud_project(mut self, project: Option<String>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.gcloud_project = project;
        }
        self
    }

    fn from_found(found: Found, sender: Arc<dyn Sender>) -> Self {
        Self {
            inner: Arc::new(Inner {
                source: found.source,
                kind: found.kind,
                origin: found.origin,
                project: found.project,
                quota_project: found.quota_project,
                gcloud_project: None,
                sender,
                cached: Atom::new(found.token),
                flight: SingleFlight::default(),
            }),
            fallback_project: None,
        }
    }

    pub fn kind(&self) -> CredentialKind {
        self.inner.kind
    }

    pub fn origin(&self) -> &str {
        &self.inner.origin
    }

    pub fn project(&self) -> Option<&str> {
        self.inner.project.as_deref()
    }

    #[must_use]
    pub fn with_user_project(mut self, project: impl Into<String>) -> Self {
        self.fallback_project = Some(project.into());
        self
    }

    pub fn user_project(&self) -> Option<&str> {
        self.inner.quota_project.as_deref().or_else(|| {
            self.fallback_project
                .as_deref()
                .filter(|_| self.inner.kind.is_user())
        })
    }

    pub fn identity(&self) -> String {
        format!(
            "{}:{}",
            self.inner.kind.name(),
            self.inner.source.identity()
        )
    }

    pub fn gcloud_project(&self) -> Option<&str> {
        self.inner.gcloud_project.as_deref()
    }

    pub fn project_source(
        &self,
        configured: Option<&str>,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Option<(String, ProjectSource)> {
        let present = |value: Option<String>, source: ProjectSource| {
            value
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|value| (value, source))
        };
        present(configured.map(str::to_owned), ProjectSource::Configured)
            .or_else(|| present(lookup("GOOGLE_CLOUD_PROJECT"), ProjectSource::Environment))
            .or_else(|| {
                present(
                    self.inner.quota_project.clone(),
                    ProjectSource::QuotaProject,
                )
            })
            .or_else(|| present(self.inner.project.clone(), ProjectSource::Credentials))
            .or_else(|| {
                present(
                    self.inner.gcloud_project.clone(),
                    ProjectSource::GcloudConfig,
                )
            })
    }

    pub fn resolve_project(
        &self,
        configured: Option<&str>,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Option<String> {
        self.project_source(configured, lookup)
            .map(|(project, _)| project)
    }

    pub async fn token(&self) -> Result<String, Error> {
        if let Some(token) = self.inner.fresh() {
            return Ok(token.value);
        }
        let inner = Arc::clone(&self.inner);
        self.inner
            .flight
            .run(move || async move {
                if let Some(token) = inner.fresh() {
                    return Ok(token);
                }
                let token = inner
                    .source
                    .fetch(inner.sender.as_ref())
                    .await
                    .map_err(|error| {
                        Error::new(format!(
                            "Google {} credentials from {} failed: {error}",
                            inner.kind.name(),
                            inner.origin
                        ))
                    })?;
                inner.cached.set(Some(token.clone()));
                Ok(token)
            })
            .await
            .map(|token| token.value)
    }

    pub fn invalidate(&self, rejected: &str) {
        self.inner.cached.update(|cached| {
            if cached.as_ref().is_some_and(|token| token.value == rejected) {
                *cached = None;
            }
        });
    }
}
