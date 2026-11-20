use crate::discover::read_file;

pub const CORE_PROJECT_ENV: &str = "CLOUDSDK_CORE_PROJECT";
pub const ACTIVE_CONFIG_ENV: &str = "CLOUDSDK_ACTIVE_CONFIG_NAME";
const DEFAULT_CONFIGURATION: &str = "default";

fn present(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn config_dir(lookup: &(dyn Fn(&str) -> Option<String> + Sync)) -> Option<String> {
    if let Some(config) = present(lookup("CLOUDSDK_CONFIG")) {
        return Some(config);
    }
    if cfg!(windows) {
        return present(lookup("APPDATA")).map(|data| format!("{data}\\gcloud"));
    }
    present(lookup("HOME")).map(|home| format!("{home}/.config/gcloud"))
}

pub fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            name.trim().clone_into(&mut current);
            continue;
        }
        if current != section {
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            return present(Some(value.to_owned()));
        }
    }
    None
}

pub async fn active_project(lookup: &(dyn Fn(&str) -> Option<String> + Sync)) -> Option<String> {
    if let Some(project) = present(lookup(CORE_PROJECT_ENV)) {
        return Some(project);
    }
    let dir = config_dir(lookup)?;
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let name = match present(lookup(ACTIVE_CONFIG_ENV)) {
        Some(name) => name,
        None => read_file(format!("{dir}{separator}active_config"))
            .await
            .ok()
            .and_then(|raw| present(Some(String::from_utf8_lossy(&raw).into_owned())))
            .unwrap_or_else(|| DEFAULT_CONFIGURATION.to_owned()),
    };
    let raw = read_file(format!(
        "{dir}{separator}configurations{separator}config_{name}"
    ))
    .await
    .ok()?;
    ini_value(&String::from_utf8_lossy(&raw), "core", "project")
}
