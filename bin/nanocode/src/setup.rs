use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use nanocode::antigravity::{
    Client, DEFAULT_MODEL, Endpoint, GoogleAuth, GoogleConfig, Probe, Project, Projects,
    RetryPolicy, Route, Saved, Store, VERTEX_GLOBAL_LOCATION, VERTEX_SERVICE,
};
use nanocode_http::Method;

use crate::{
    cli::{SetupArgs, SetupCommand},
    host::Result,
};

const GCLOUD: &str = "gcloud";
const ADC_LOGIN: [&str; 3] = ["auth", "application-default", "login"];
const SETTLED_SUCCESSES: usize = 3;
const SETTLE_ATTEMPTS: usize = 60;
const SETTLE_GAP: Duration = Duration::from_secs(5);
const COUNT_TOKENS_BODY: &str = r#"{"contents":[{"role":"user","parts":[{"text":"ping"}]}]}"#;
const API_KEY_HELP: &str = "To use a Gemini API key instead, create one at https://aistudio.google.com/apikey and run GEMINI_API_KEY=... nanocode -m gemini-4-neon (gemini-4-neon needs a paid-tier key: enable billing for the key's project in AI Studio).";

fn lookup(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn setup(args: SetupArgs) -> Result<()> {
    match args.command {
        SetupCommand::Google {
            google,
            model,
            check,
        } => {
            let config = google.config();
            let model = model.unwrap_or_else(|| DEFAULT_MODEL.to_owned());
            match check {
                true => self::check(&config, &model).await,
                false => self::google(&config, &model).await,
            }
        }
    }
}

fn on_path(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

async fn ask(question: String) -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    nanocode_platform::spawn_blocking(move || {
        eprint!("{question} ");
        drop(std::io::stderr().flush());
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        Some(line.trim().to_owned())
    })
    .await
    .ok()
    .flatten()
}

async fn confirm(question: String) -> bool {
    ask(format!("{question} [y/N]"))
        .await
        .is_some_and(|answer| matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes"))
}

async fn adc_login(gcloud: Option<&std::path::Path>) -> Result<()> {
    let Some(gcloud) = gcloud.map(std::path::Path::to_path_buf) else {
        return Err(format!(
            "gcloud is not installed: install the Google Cloud CLI (https://cloud.google.com/sdk/docs/install), then run {GCLOUD} {}, or use a Gemini API key. {API_KEY_HELP}",
            ADC_LOGIN.join(" ")
        )
        .into());
    };
    eprintln!("Running {GCLOUD} {} ...", ADC_LOGIN.join(" "));
    let status = nanocode_platform::spawn_blocking(move || {
        std::process::Command::new(gcloud).args(ADC_LOGIN).status()
    })
    .await??;
    match status.success() {
        true => Ok(()),
        false => Err(format!("{GCLOUD} {} failed ({status})", ADC_LOGIN.join(" ")).into()),
    }
}

async fn credentials(
    sender: &Arc<dyn nanocode_http::layer::Sender>,
    gcloud: Option<&std::path::Path>,
) -> Result<GoogleAuth> {
    let found = GoogleAuth::discover(&lookup, Arc::clone(sender)).await;
    let failure = match &found {
        Ok(auth) => match auth.token().await {
            Ok(_) => None,
            Err(error) => Some(format!(
                "the {} credentials from {} could not get a token: {error}",
                auth.kind().name(),
                auth.origin()
            )),
        },
        Err(error) => Some(format!("no Application Default Credentials: {error}")),
    };
    let Some(failure) = failure else {
        return found.map_err(|error| error.to_string().into());
    };
    eprintln!("{failure}");
    let relogin = match &found {
        Ok(auth) => auth.kind().is_user(),
        Err(_) => true,
    };
    if !relogin {
        return Err(failure.into());
    }
    adc_login(gcloud).await?;
    let auth = GoogleAuth::discover(&lookup, Arc::clone(sender))
        .await
        .map_err(|error| error.to_string())?;
    auth.token().await.map_err(|error| error.to_string())?;
    Ok(auth)
}

async fn remember(store: &Store, auth: &GoogleAuth, project: &str, location: &str, model: &str) {
    let saved = Saved {
        project: project.to_owned(),
        location: location.to_owned(),
        model: model.to_owned(),
    };
    match store.save(&auth.identity(), saved).await {
        Ok(path) => eprintln!("Saved {project} ({location}) in {path}."),
        Err(error) => eprintln!("Could not save the choice: {error}"),
    }
    eprintln!(
        "Done. Run: nanocode -m {model}\nOverride with --google-project or GOOGLE_CLOUD_PROJECT; check with nanocode setup google --check."
    );
}

async fn settle(projects: &Projects, project: &str, location: &str, model: &str) -> bool {
    let mut streak = 0;
    for attempt in 0..SETTLE_ATTEMPTS {
        if attempt > 0 {
            nanocode_platform::sleep(SETTLE_GAP).await;
        }
        match projects.call(project, location, model).await {
            Ok(()) => streak += 1,
            Err(error) => {
                streak = 0;
                eprintln!(
                    "  not ready yet: {}",
                    error.to_string().lines().next().unwrap_or_default()
                );
            }
        }
        if streak >= SETTLED_SUCCESSES {
            return true;
        }
    }
    false
}

async fn google(config: &GoogleConfig, model: &str) -> Result<()> {
    let gcloud = on_path(GCLOUD);
    match &gcloud {
        Some(path) => eprintln!("gcloud: {}", path.display()),
        None => eprintln!("gcloud: not found on PATH"),
    }
    let sender = Client::native_sender()?;
    let auth = credentials(&sender, gcloud.as_deref()).await?;
    eprintln!("Credentials: {} from {}", auth.kind().name(), auth.origin());
    let location = config
        .location
        .clone()
        .or_else(|| lookup("GOOGLE_CLOUD_LOCATION"))
        .unwrap_or_else(|| VERTEX_GLOBAL_LOCATION.to_owned());
    let projects = Projects::new(auth.clone(), Arc::clone(&sender), &lookup);
    let store = Store::locate(config.state.clone(), &lookup);
    let preferred = auth.project_source(config.project.as_deref(), &lookup);
    let mut preferred_probe = None;
    if let Some((project, source)) = &preferred {
        eprintln!(
            "Probing {project} (from {}) with {model} in {location} ...",
            source.name()
        );
        let probe = projects.probe(project, &location, model).await;
        eprintln!("  {project}: {}", probe.describe());
        if probe.is_ready() {
            remember(&store, &auth, project, &location, model).await;
            return Ok(());
        }
        preferred_probe = Some(Project {
            id: project.clone(),
            name: project.clone(),
        })
        .zip(Some(probe));
    }
    eprintln!("Listing your active Google Cloud projects and probing each ...");
    let skip = preferred.as_ref().map(|(project, _)| project.as_str());
    let discovery = projects.discover(&location, model, skip).await?;
    let mut probed = preferred_probe.into_iter().collect::<Vec<_>>();
    probed.extend(discovery.probed);
    for (project, probe) in &probed {
        eprintln!("  {:<32} {}", project.id, probe.describe());
    }
    if let Some(chosen) = discovery.chosen {
        eprintln!("Picked {}.", chosen.id);
        remember(&store, &auth, &chosen.id, &location, model).await;
        return Ok(());
    }
    let disabled: Vec<String> = probed
        .iter()
        .filter(|(_, probe)| *probe == Probe::Disabled)
        .map(|(project, _)| project.id.clone())
        .collect();
    if disabled.is_empty() {
        return Err(format!(
            "No project you can reach has Vertex AI ready, and none only needs {VERTEX_SERVICE} enabled (see the list above: fix billing or permissions in one, or pass --google-project). {API_KEY_HELP}"
        )
        .into());
    }
    eprintln!("Vertex AI ({VERTEX_SERVICE}) is disabled in:");
    for (index, project) in disabled.iter().enumerate() {
        eprintln!("  {}. {project}", index + 1);
    }
    let choice = ask(format!(
        "Enable it in which project? [1-{}, Enter to skip]",
        disabled.len()
    ))
    .await
    .and_then(|answer| answer.parse::<usize>().ok())
    .and_then(|index| index.checked_sub(1))
    .and_then(|index| disabled.get(index).cloned());
    let Some(project) = choice else {
        return Err(format!(
            "Nothing enabled. Enable it yourself with: gcloud services enable {VERTEX_SERVICE} --project PROJECT, then rerun nanocode setup google. {API_KEY_HELP}"
        )
        .into());
    };
    if !confirm(format!(
        "Enable {VERTEX_SERVICE} in {project}? Vertex AI calls will bill to {project}'s billing account."
    ))
    .await
    {
        return Err(format!("Left {project} unchanged. {API_KEY_HELP}").into());
    }
    eprintln!("Enabling {VERTEX_SERVICE} in {project} ...");
    projects.enable(&project).await?;
    eprintln!(
        "Enabled. Waiting until {SETTLED_SUCCESSES} calls in a row succeed (Google can take a few minutes) ..."
    );
    if !settle(&projects, &project, &location, model).await {
        return Err(format!(
            "{VERTEX_SERVICE} is enabled in {project} but {model} still does not answer reliably; wait a few minutes and rerun nanocode setup google"
        )
        .into());
    }
    remember(&store, &auth, &project, &location, model).await;
    Ok(())
}

async fn check(config: &GoogleConfig, model: &str) -> Result<()> {
    let sender = Client::native_sender()?;
    let endpoint = Endpoint::discover(config, &lookup, Arc::clone(&sender), model).await?;
    let method = endpoint.route.method();
    println!("auth:     {}", method.name());
    if let nanocode::antigravity::Auth::Google(auth) = &endpoint.auth {
        println!("creds:    {} from {}", auth.kind().name(), auth.origin());
    }
    match &endpoint.route {
        Route::Vertex { project, location } => {
            println!("project:  {project}");
            println!("location: {location}");
        }
        Route::GeminiApi | Route::Gateway | Route::VertexExpress => {}
    }
    println!("base:     {}", endpoint.base_url);
    println!("model:    {model}");
    if let Some(notice) = &endpoint.notice {
        println!("note:     {notice}");
    }
    let client = Client::new(endpoint, sender).with_retry(RetryPolicy::NONE);
    let path = client.endpoint().model_path(model, "countTokens");
    let started = nanocode_platform::now();
    match client.json(Method::POST, &path, COUNT_TOKENS_BODY).await {
        Ok(_) => {
            println!("probe:    ok (countTokens in {:?})", started.elapsed());
            Ok(())
        }
        Err(error) => {
            println!("probe:    failed");
            Err(error.to_string().into())
        }
    }
}
