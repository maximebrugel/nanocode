use std::{io, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use nanocode_shell::{
    Commands, Fetch, FileSystem, HostInfo, Identity, Limits, Machine, NoCommands,
};
use nanocode_tool_runtime::{ToolExposure, Tools};
use nanocode_workspace::{Config, Platform, Workspace};
use nanocode_workspace_fs::{Caps, MemStore, SqlStore, Store, WorkspaceFs};

use crate::{Result, fetch::Http, sqlite::Sqlite};

const MEMORY: &str = "mem:";
const SQLITE_BUSY: Duration = Duration::from_secs(5);
const CAPS: Caps = Caps {
    file_bytes: 64 << 20,
    workspace_bytes: 1 << 30,
    paths: 1 << 20,
    ops: 1 << 16,
    path_bytes: 4096,
};
const LIMITS: Limits = Limits {
    fuel: u64::MAX,
    output: 16 << 20,
    file: 64 << 20,
    operations: u64::MAX,
    depth: nanocode_shell::DEFAULT_DEPTH,
};
const PYTHON: nanocode_python::Budget = nanocode_python::Budget {
    steps: u64::MAX,
    time: Duration::from_secs(600),
    memory: None,
    sleep: Duration::from_secs(600),
    depth: 1000,
};
const SLICE: u32 = 4096;
const DIRECTORY: u32 = 0o755;
const PATH: &str = "/usr/local/bin:/usr/bin:/bin";

#[derive(Clone, Debug)]
pub enum Backing {
    Sqlite(PathBuf),
    Memory,
}

impl Backing {
    pub fn parse(value: String) -> Self {
        match value == MEMORY {
            true => Self::Memory,
            false => Self::Sqlite(value.into()),
        }
    }
}

async fn open(store: Arc<dyn Store>, root: &str) -> io::Result<WorkspaceFs> {
    let files = WorkspaceFs::open(store, root, CAPS, Rc::new(Platform)).await?;
    let mut path = Vec::with_capacity(root.len());
    for part in root.split('/').filter(|part| !part.is_empty()) {
        path.push(b'/');
        path.extend_from_slice(part.as_bytes());
        match files.mkdir(&path, DIRECTORY).get().await {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    Ok(files)
}

pub async fn tools(backing: &Backing, root: PathBuf) -> Result<Tools> {
    let store: Arc<dyn Store> = match backing {
        Backing::Memory => Arc::new(MemStore::new()),
        Backing::Sqlite(path) => {
            let host = Sqlite::open(path, SQLITE_BUSY)?;
            Arc::new(SqlStore::open(Arc::new(host)).await?)
        }
    };
    let key = root.to_string_lossy().into_owned();
    store.load(&key).await?;
    let home = key.clone().into_bytes();
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
    let workspace = Workspace::start(Config {
        files: Arc::new(move || -> Rc<dyn FileSystem> {
            let opened = nanocode_sync::block_on(open(Arc::clone(&store), &key));
            Rc::new(opened.expect("the workspace loaded once before its worker started"))
        }),
        machine: Arc::new(|| -> Rc<dyn Machine> { Rc::new(Platform) }),
        fetch: Some(Arc::new(|| -> Rc<dyn Fetch> {
            Rc::new(Http::new().expect("the HTTP client builds"))
        })),
        credentials: None,
        commands: Arc::new(|| -> Rc<dyn Commands> {
            Rc::new(nanocode_python::Command::new(PYTHON, Rc::new(NoCommands)))
        }),
        identity: Identity {
            info: HostInfo::default(),
            pid: std::process::id(),
            user: user.clone().into_bytes(),
            group: user.clone().into_bytes(),
            home: home.clone(),
        },
        limits: LIMITS,
        env: vec![
            (b"HOME".to_vec(), home.clone()),
            (b"PWD".to_vec(), home),
            (b"USER".to_vec(), user.into_bytes()),
            (b"PATH".to_vec(), PATH.as_bytes().to_vec()),
        ],
        slice: SLICE,
    })?;
    let tools = nanocode_backend_codex::builtin_tools::host_tools(
        root,
        Arc::new(workspace.clone()),
        Arc::new(workspace),
    )?;
    Ok(tools
        .into_builder()
        .exposure(ToolExposure::DirectOnly)
        .build()?)
}
