use crate::source::{self, Source};
use crate::{CredentialKind, Error, Found, Token};
use nanocode_http::layer::Sender;
use std::time::Duration;

pub const METADATA_HOST: &str = "169.254.169.254";
pub const METADATA_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(not(target_family = "wasm"))]
pub const GCLOUD_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(target_family = "wasm"))]
pub const GCLOUD_TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);
const WELL_KNOWN_FILE: &str = "application_default_credentials.json";

fn lookup_text(lookup: &(dyn Fn(&str) -> Option<String> + Sync), name: &str) -> Option<String> {
    lookup(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn well_known_path(lookup: &(dyn Fn(&str) -> Option<String> + Sync)) -> Option<String> {
    if let Some(config) = lookup_text(lookup, "CLOUDSDK_CONFIG") {
        return Some(format!("{config}/{WELL_KNOWN_FILE}"));
    }
    if cfg!(windows) {
        return lookup_text(lookup, "APPDATA")
            .map(|data| format!("{data}\\gcloud\\{WELL_KNOWN_FILE}"));
    }
    lookup_text(lookup, "HOME").map(|home| format!("{home}/.config/gcloud/{WELL_KNOWN_FILE}"))
}

#[cfg(not(target_family = "wasm"))]
pub async fn read_file(path: String) -> Result<Vec<u8>, String> {
    nanocode_platform::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[cfg(target_family = "wasm")]
pub async fn read_file(_path: String) -> Result<Vec<u8>, String> {
    Err("credential files are not readable in WASM".to_owned())
}

async fn from_file(path: &str, origin: String) -> Result<Found, Error> {
    let raw = read_file(path.to_owned())
        .await
        .map_err(|error| Error::new(format!("cannot read {origin}: {error}")))?;
    let value = serde_json::from_slice(&raw)
        .map_err(|error| Error::new(format!("{origin} is not JSON: {error}")))?;
    let parsed = source::parse(&value).map_err(|error| Error::new(format!("{origin}: {error}")))?;
    Ok(Found {
        source: parsed.source,
        kind: parsed.kind,
        origin,
        project: parsed.project,
        quota_project: parsed.quota_project,
        token: None,
    })
}

async fn metadata(sender: &dyn Sender, host: &str) -> Result<Found, String> {
    let base = match host.starts_with("http://") || host.starts_with("https://") {
        true => host.trim_end_matches('/').to_owned(),
        false => format!("http://{host}"),
    };
    let probe = async {
        let reply = source::metadata_get(sender, &format!("{base}/"))
            .await
            .map_err(|error| error.to_string())?;
        let flavor = reply
            .headers
            .get(source::METADATA_FLAVOR.0)
            .and_then(|value| value.to_str().ok());
        match flavor == Some(source::METADATA_FLAVOR.1) {
            true => Ok(()),
            false => Err(format!("{base} is not a Google metadata server")),
        }
    };
    nanocode_platform::timeout(METADATA_PROBE_TIMEOUT, probe)
        .await
        .map_err(|_| format!("{base} did not answer within {METADATA_PROBE_TIMEOUT:?}"))??;
    let project = nanocode_platform::timeout(
        METADATA_PROBE_TIMEOUT,
        source::metadata_project(sender, &base),
    )
    .await
    .ok()
    .and_then(Result::ok);
    Ok(Found {
        source: Source::Metadata(base.clone()),
        kind: CredentialKind::MetadataServer,
        origin: format!("the metadata server at {base}"),
        project,
        quota_project: None,
        token: None,
    })
}

#[cfg(not(target_family = "wasm"))]
pub async fn gcloud_token() -> Result<Token, Error> {
    let run = nanocode_platform::spawn_blocking(|| {
        std::process::Command::new("gcloud")
            .args(["auth", "print-access-token", "--quiet"])
            .stdin(std::process::Stdio::null())
            .output()
    });
    let output = nanocode_platform::timeout(GCLOUD_TIMEOUT, run)
        .await
        .map_err(|_| {
            Error::new(format!(
                "gcloud auth print-access-token took over {GCLOUD_TIMEOUT:?}"
            ))
        })?
        .map_err(|error| Error::new(format!("cannot run gcloud: {error}")))?
        .map_err(|error| Error::new(format!("cannot run gcloud: {error}")))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match output.status.success() && !token.is_empty() {
        true => Ok(Token::lasting(token, Some(GCLOUD_TOKEN_LIFETIME))),
        false => Err(Error::new(format!(
            "gcloud auth print-access-token failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

#[cfg(target_family = "wasm")]
pub async fn gcloud_token() -> Result<Token, Error> {
    Err(Error::new("the gcloud CLI is not available in WASM"))
}

fn flag(lookup: &(dyn Fn(&str) -> Option<String> + Sync), name: &str) -> bool {
    lookup_text(lookup, name)
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

pub async fn find(
    lookup: &(dyn Fn(&str) -> Option<String> + Sync),
    sender: &dyn Sender,
) -> Result<Found, Error> {
    let quota_override = lookup_text(lookup, "GOOGLE_CLOUD_QUOTA_PROJECT");
    let with_quota = |mut found: Found| {
        if found.quota_project.is_none() {
            found.quota_project.clone_from(&quota_override);
        }
        found
    };
    if let Some(token) = lookup_text(lookup, "GOOGLE_OAUTH_ACCESS_TOKEN") {
        return Ok(with_quota(Found {
            source: Source::Static(token),
            kind: CredentialKind::AccessToken,
            origin: "GOOGLE_OAUTH_ACCESS_TOKEN".to_owned(),
            project: None,
            quota_project: None,
            token: None,
        }));
    }
    if let Some(path) = lookup_text(lookup, "GOOGLE_APPLICATION_CREDENTIALS") {
        let origin = format!("GOOGLE_APPLICATION_CREDENTIALS ({path})");
        return from_file(&path, origin).await.map(with_quota);
    }
    let mut tried = vec![
        "GOOGLE_OAUTH_ACCESS_TOKEN (unset)".to_owned(),
        "GOOGLE_APPLICATION_CREDENTIALS (unset)".to_owned(),
    ];
    match well_known_path(lookup) {
        Some(path) => match read_file(path.clone()).await {
            Ok(_) => return from_file(&path, path.clone()).await.map(with_quota),
            Err(error) => tried.push(format!("{path} ({error})")),
        },
        None => tried.push("the gcloud credentials file (no HOME or CLOUDSDK_CONFIG)".to_owned()),
    }
    match flag(lookup, "NO_GCE_CHECK") {
        true => tried.push("the metadata server (NO_GCE_CHECK is set)".to_owned()),
        false => {
            let host = lookup_text(lookup, "GCE_METADATA_HOST")
                .or_else(|| lookup_text(lookup, "GCE_METADATA_IP"))
                .unwrap_or_else(|| METADATA_HOST.to_owned());
            match metadata(sender, &host).await {
                Ok(found) => return Ok(with_quota(found)),
                Err(error) => tried.push(format!("the metadata server ({error})")),
            }
        }
    }
    match gcloud_token().await {
        Ok(token) => {
            return Ok(with_quota(Found {
                source: Source::Gcloud,
                kind: CredentialKind::Gcloud,
                origin: "gcloud auth print-access-token".to_owned(),
                project: None,
                quota_project: None,
                token: Some(token),
            }));
        }
        Err(error) => tried.push(error.to_string()),
    }
    Err(Error::new(format!(
        "no Google Application Default Credentials found; tried {}. Run gcloud auth application-default login, set GOOGLE_APPLICATION_CREDENTIALS to a credentials file, or set GOOGLE_OAUTH_ACCESS_TOKEN",
        tried.join("; ")
    )))
}
