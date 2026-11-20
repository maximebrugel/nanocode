use crate::{SYSTEM, cgroup, confine::CHANNEL};
use nanocode_executor::process::Command;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

pub struct Bubblewrap {
    bwrap: OsString,
    arguments: Vec<OsString>,
    work: OsString,
}

impl Bubblewrap {
    pub fn new(bwrap: &Path, work: &Path, inside: &Path) -> Self {
        let mut arguments: Vec<OsString> = [
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        for system in SYSTEM {
            arguments.extend(["--ro-bind-try", system, system].map(OsString::from));
        }
        arguments.extend([OsString::from("--bind"), work.into(), inside.into()]);
        Self {
            bwrap: bwrap.into(),
            arguments,
            work: inside.into(),
        }
    }

    pub fn read(&mut self, source: &Path, target: &Path) -> &mut Self {
        self.arguments
            .extend([OsString::from("--ro-bind"), source.into(), target.into()]);
        self
    }

    pub fn bind(&mut self, source: &Path, target: &Path) -> &mut Self {
        self.arguments
            .extend([OsString::from("--bind"), source.into(), target.into()]);
        self
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        self.sandboxed(Command::new(&self.bwrap), program)
    }

    pub fn command_with_channel(&self, launcher: &Path, program: impl AsRef<OsStr>) -> Command {
        self.command_joined(launcher, None, true, program)
    }

    pub fn command_joined(
        &self,
        launcher: &Path,
        procs: Option<&Path>,
        channel: bool,
        program: impl AsRef<OsStr>,
    ) -> Command {
        let mut command = Command::new(launcher);
        if let Some(procs) = procs {
            command.arg(cgroup::ARGUMENT).arg(procs);
        }
        if channel {
            command.arg(CHANNEL);
        }
        command.arg(&self.bwrap);
        self.sandboxed(command, program)
    }

    fn sandboxed(&self, mut command: Command, program: impl AsRef<OsStr>) -> Command {
        command
            .args(&self.arguments)
            .arg("--chdir")
            .arg(&self.work)
            .arg("--")
            .arg(program)
            .env_clear()
            .env("HOME", &self.work)
            .kill_on_drop(true);
        command
    }
}
