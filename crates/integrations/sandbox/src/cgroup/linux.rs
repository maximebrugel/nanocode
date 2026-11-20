use super::{Controllers, Limits, Memory, Pids, Reason, Refused, Unavailable, Usage};
use nanocode_platform::sleep;
use nanocode_sys::libc::{EACCES, EBUSY, EPERM, EROFS};
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const ROOT: &str = "/sys/fs/cgroup";
const MEMBERSHIP: &str = "/proc/self/cgroup";
const UNIFIED: &str = "0::";
const PROBE: &str = "probe-";
const SETTLE_POLL: Duration = Duration::from_millis(1);

pub struct Tree {
    base: PathBuf,
    controllers: Controllers,
    stranded: Arc<AtomicU64>,
}

pub struct Group {
    path: PathBuf,
    procs: PathBuf,
    controllers: Controllers,
    removed: bool,
    stranded: Arc<AtomicU64>,
}

pub fn detect() -> Result<Tree, Unavailable> {
    let root = Path::new(ROOT);
    if !root.join("cgroup.controllers").is_file() {
        return Err(Unavailable::NotUnified);
    }
    let own = own_cgroup().ok_or(Unavailable::NotUnified)?;
    let own = root.join(own);
    let mut candidates = vec![own.clone()];
    if own != root {
        candidates.extend(own.parent().map(Path::to_path_buf));
    }
    assert!(candidates.len() <= 2);
    let mut refused = Vec::with_capacity(candidates.len());
    for base in candidates {
        match claim(&base) {
            Ok(controllers) => {
                return Ok(Tree {
                    base,
                    controllers,
                    stranded: Arc::new(AtomicU64::new(0)),
                });
            }
            Err(reason) => refused.push(Refused { path: base, reason }),
        }
    }
    Err(Unavailable::Refused(refused))
}

pub fn join(procs: &Path) -> io::Result<()> {
    fs::write(procs, std::process::id().to_string())
        .map_err(|error| context(error, "joining", procs))
}

fn own_cgroup() -> Option<PathBuf> {
    let membership = fs::read_to_string(MEMBERSHIP).ok()?;
    let path = membership
        .lines()
        .find_map(|line| line.strip_prefix(UNIFIED))?;
    let path = Path::new(path);
    path.components()
        .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        .then(|| path.strip_prefix("/").unwrap_or(path).to_path_buf())
}

fn claim(base: &Path) -> Result<Controllers, Reason> {
    let available = fs::read_to_string(base.join("cgroup.controllers"))
        .map(|text| parse_controllers(&text))
        .map_err(|_| Reason::Unreadable)?;
    if available == Controllers::default() {
        return Err(Reason::NoControllers);
    }
    let enabled = fs::read_to_string(base.join("cgroup.subtree_control"))
        .map(|text| parse_controllers(&text))
        .map_err(|_| Reason::Unreadable)?;
    let probe = base.join(format!("{PROBE}{}", nanocode_platform::uuid_v4().simple()));
    fs::create_dir(&probe).map_err(|error| refusal(&error))?;
    fs::remove_dir(&probe).map_err(|error| refusal(&error))?;
    let wanted = [
        (available.cpu && !enabled.cpu, "+cpu"),
        (available.memory && !enabled.memory, "+memory"),
        (available.pids && !enabled.pids, "+pids"),
        (available.io && !enabled.io, "+io"),
    ];
    let change: Vec<&str> = wanted
        .iter()
        .filter_map(|(want, token)| want.then_some(*token))
        .collect();
    if !change.is_empty() {
        fs::write(base.join("cgroup.subtree_control"), change.join(" "))
            .map_err(|error| refusal(&error))?;
    }
    let enabled = fs::read_to_string(base.join("cgroup.subtree_control"))
        .map(|text| parse_controllers(&text))
        .map_err(|_| Reason::Unreadable)?;
    if enabled == Controllers::default() {
        return Err(Reason::NoControllers);
    }
    Ok(enabled)
}

fn refusal(error: &io::Error) -> Reason {
    match error.raw_os_error() {
        Some(EBUSY) => Reason::InternalProcesses,
        Some(EACCES | EPERM | EROFS) => Reason::NotWritable,
        _ => Reason::Unreadable,
    }
}

fn parse_controllers(text: &str) -> Controllers {
    let mut controllers = Controllers::default();
    for name in text.split_whitespace() {
        match name {
            "cpu" => controllers.cpu = true,
            "memory" => controllers.memory = true,
            "pids" => controllers.pids = true,
            "io" => controllers.io = true,
            _ => {}
        }
    }
    controllers
}

fn context(error: io::Error, doing: &str, path: &Path) -> io::Error {
    io::Error::new(error.kind(), format!("{doing} {}: {error}", path.display()))
}

fn missing(controller: &str, base: &Path) -> io::Error {
    io::Error::new(
        ErrorKind::Unsupported,
        format!(
            "the cgroup subtree at {} does not delegate the {controller} controller",
            base.display()
        ),
    )
}

impl Tree {
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.base
    }

    #[must_use]
    pub const fn controllers(&self) -> Controllers {
        self.controllers
    }

    #[must_use]
    pub fn stranded(&self) -> u64 {
        self.stranded.load(Ordering::Relaxed)
    }

    pub fn create(&self, name: &str, limits: &Limits) -> io::Result<Group> {
        assert!(!name.is_empty());
        assert!(!name.contains('/'));
        assert!(!name.starts_with(PROBE));
        let writes = self.writes(limits)?;
        let path = self.base.join(name);
        fs::create_dir(&path).map_err(|error| context(error, "creating", &path))?;
        let group = Group {
            procs: path.join("cgroup.procs"),
            path,
            controllers: self.controllers,
            removed: false,
            stranded: Arc::clone(&self.stranded),
        };
        for (file, value) in writes {
            let file = group.path.join(file);
            fs::write(&file, &value)
                .map_err(|error| context(error, &format!("writing {value} to"), &file))?;
        }
        Ok(group)
    }

    fn writes(&self, limits: &Limits) -> io::Result<Vec<(&'static str, String)>> {
        let mut writes = Vec::with_capacity(6);
        if let Some(cpu) = limits.cpu {
            if !self.controllers.cpu {
                return Err(missing("cpu", &self.base));
            }
            let value = format!("{} {}", cpu.quota.as_micros(), cpu.period.as_micros());
            writes.push(("cpu.max", value));
        }
        let memory = [
            ("memory.high", limits.memory_high),
            ("memory.max", limits.memory_max),
            ("memory.swap.max", limits.swap_max),
        ];
        for (file, bytes) in memory {
            if let Some(bytes) = bytes {
                if !self.controllers.memory {
                    return Err(missing("memory", &self.base));
                }
                writes.push((file, bytes.to_string()));
            }
        }
        if let Some(pids) = limits.pids_max {
            if !self.controllers.pids {
                return Err(missing("pids", &self.base));
            }
            writes.push(("pids.max", pids.to_string()));
        }
        if let Some(weight) = limits.io_weight {
            if !self.controllers.io {
                return Err(missing("io", &self.base));
            }
            writes.push(("io.weight", format!("default {weight}")));
        }
        assert!(writes.len() <= 6);
        Ok(writes)
    }
}

impl Group {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn procs(&self) -> &Path {
        &self.procs
    }

    pub fn kill(&self) -> io::Result<()> {
        assert!(!self.removed);
        let file = self.path.join("cgroup.kill");
        fs::write(&file, "1").map_err(|error| context(error, "writing 1 to", &file))
    }

    pub fn usage(&self) -> io::Result<Usage> {
        assert!(!self.removed);
        let cpu = self.keyed("cpu.stat")?;
        let micros = |key| value(&cpu, key, "cpu.stat").map(Duration::from_micros);
        let memory = if self.controllers.memory {
            let events = self.keyed("memory.events")?;
            let count = |key| value(&events, key, "memory.events");
            Some(Memory {
                peak: self.single("memory.peak")?,
                high: count("high")?,
                max: count("max")?,
                oom: count("oom")?,
                oom_kill: count("oom_kill")?,
            })
        } else {
            None
        };
        let pids = if self.controllers.pids {
            let events = self.keyed("pids.events")?;
            Some(Pids {
                peak: self.single("pids.peak")?,
                max: value(&events, "max", "pids.events")?,
            })
        } else {
            None
        };
        Ok(Usage {
            cpu: micros("usage_usec")?,
            throttled: if self.controllers.cpu {
                Some(micros("throttled_usec")?)
            } else {
                None
            },
            memory,
            pids,
        })
    }

    pub async fn remove(mut self, settle: Duration) -> io::Result<()> {
        assert!(!self.removed);
        let began = Instant::now();
        loop {
            match fs::remove_dir(&self.path) {
                Ok(()) => {
                    self.removed = true;
                    return Ok(());
                }
                Err(error) if error.raw_os_error() == Some(EBUSY) && began.elapsed() < settle => {
                    sleep(SETTLE_POLL).await;
                }
                Err(error) => return Err(context(error, "removing", &self.path)),
            }
        }
    }

    fn keyed(&self, file: &str) -> io::Result<Vec<(String, u64)>> {
        let path = self.path.join(file);
        let text = fs::read_to_string(&path).map_err(|error| context(error, "reading", &path))?;
        text.lines()
            .map(|line| {
                let (key, number) = line.split_once(' ').ok_or_else(|| malformed(&path, line))?;
                let number = number.parse().map_err(|_| malformed(&path, line))?;
                Ok((key.to_owned(), number))
            })
            .collect()
    }

    fn single(&self, file: &str) -> io::Result<Option<u64>> {
        let path = self.path.join(file);
        match fs::read_to_string(&path) {
            Ok(text) => text
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| malformed(&path, text.trim())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(context(error, "reading", &path)),
        }
    }
}

fn value(entries: &[(String, u64)], key: &str, file: &str) -> io::Result<u64> {
    entries
        .iter()
        .find_map(|(name, number)| (name == key).then_some(*number))
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, format!("{file} has no {key}")))
}

fn malformed(path: &Path, line: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("{} has a malformed line: {line}", path.display()),
    )
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        let killed = fs::write(self.path.join("cgroup.kill"), "1");
        if killed.is_err() || fs::remove_dir(&self.path).is_err() {
            self.stranded.fetch_add(1, Ordering::Relaxed);
        }
    }
}
