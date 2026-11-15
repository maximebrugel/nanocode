use std::{
    path::{Path, PathBuf},
    process::Command,
};

use nanocode_computer_repl::{ComputerConfig, browser, browser_socket};

use crate::{
    cli::{CuaArgs, CuaCommand},
    host::{self, Result},
};

mod command;

const CUA_DRIVER_INSTALLER: &str =
    "https://github.com/trycua/cua/releases/download/cua-driver-rs-v0.38.0/install.sh";

fn home() -> Result<PathBuf> {
    Ok(std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?)
}

fn state() -> Result<PathBuf> {
    Ok(host::home_dir(None)?.join("browser"))
}

fn selected(home: &Path, names: &[String]) -> Result<Vec<browser::Browser>> {
    if names.is_empty() {
        return Ok(browser::browsers(home));
    }
    Ok(names
        .iter()
        .map(|name| browser::browser(home, name))
        .collect::<std::io::Result<_>>()?)
}

fn driver() -> Option<PathBuf> {
    ComputerConfig::cua_driver().ok().and_then(|config| {
        config
            .environment
            .get(std::ffi::OsStr::new("CUA_DRIVER_PATH"))
            .map(PathBuf::from)
    })
}

#[cfg(target_os = "macos")]
fn embedded_driver() -> Result<PathBuf> {
    Ok(nanocode_computer_repl::embedded_driver().ok_or_else(|| {
        format!(
            "the built-in cua-driver, nanocode-cua-driver, is missing beside {}; reinstall nanocode",
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        )
    })?)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed ({status})").into());
    }
    Ok(())
}

pub async fn cua(args: CuaArgs) -> Result<()> {
    command::cua(args).await
}
