use std::{fmt, path::PathBuf, time::Duration};

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod system;
#[cfg(not(target_os = "linux"))]
#[path = "other.rs"]
mod system;

pub use system::{Group, Tree, detect, join};

pub const ARGUMENT: &str = "tool-cgroup";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub quota: Duration,
    pub period: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub cpu: Option<Cpu>,
    pub memory_max: Option<u64>,
    pub memory_high: Option<u64>,
    pub swap_max: Option<u64>,
    pub pids_max: Option<u64>,
    pub io_weight: Option<u16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Controllers {
    pub cpu: bool,
    pub memory: bool,
    pub pids: bool,
    pub io: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Memory {
    pub peak: Option<u64>,
    pub high: u64,
    pub max: u64,
    pub oom: u64,
    pub oom_kill: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pids {
    pub peak: Option<u64>,
    pub max: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub cpu: Duration,
    pub throttled: Option<Duration>,
    pub memory: Option<Memory>,
    pub pids: Option<Pids>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Unreadable,
    NotWritable,
    NoControllers,
    InternalProcesses,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub path: PathBuf,
    pub reason: Reason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Unsupported,
    NotUnified,
    Refused(Vec<Refused>),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unreadable => "its control files cannot be read",
            Self::NotWritable => "it is not writable by this user",
            Self::NoControllers => "none of the cpu, memory, pids and io controllers reach it",
            Self::InternalProcesses => {
                "it holds processes, so it cannot hand controllers to children"
            }
        })
    }
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("cgroup v2 needs Linux"),
            Self::NotUnified => f.write_str("no unified cgroup v2 hierarchy at /sys/fs/cgroup"),
            Self::Refused(refused) => {
                f.write_str("no delegated cgroup v2 subtree")?;
                for Refused { path, reason } in refused {
                    write!(f, "; {}: {reason}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Unavailable {}
