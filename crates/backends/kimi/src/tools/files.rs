use super::{APPLIED, Access, Builtins, Outcome, input};
use serde::Deserialize;
use serde_json::Value;
use std::io::Write as _;
use std::path::Path;

pub fn read(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("Read is not ported yet")
}

pub fn edit(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("Edit is not ported yet")
}

#[derive(Deserialize)]
struct WriteInput {
    path: String,
    content: String,
    mode: Option<String>,
}

/// Kimi Code's `WriteTool.execution`.
pub fn write(builtins: &Builtins, args: &Value) -> Outcome {
    let args: WriteInput = match input("Write", args) {
        Ok(args) => args,
        Err(error) => return error,
    };
    let path = match builtins.resolve("Write", &args.path, Access::Write) {
        Ok(path) => path,
        Err(error) => return error,
    };
    let append = args.mode.as_deref() == Some("append");
    let attempt = || write_text(&path, &args.content, append);
    let outcome = match attempt() {
        Err(error) if missing(&error) => match path.parent().map(ensure_parent) {
            Some(Some(message)) => return Outcome::error(message),
            _ => attempt(),
        },
        other => other,
    };
    match outcome {
        Ok(()) => Outcome::ok(format!(
            "{} {} bytes to {}",
            if append { "Appended" } else { "Wrote" },
            args.content.len(),
            args.path
        ))
        .with_note(APPLIED),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Outcome::error(format!(
            "Failed to write {}: parent directory does not exist.",
            args.path
        )),
        Err(error) => Outcome::error(fs_error(&error, if append { "append" } else { "write" })),
    }
}

fn write_text(path: &Path, content: &str, append: bool) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)?;
    file.write_all(content.as_bytes())
}

fn missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

fn ensure_parent(parent: &Path) -> Option<String> {
    match std::fs::metadata(parent) {
        Ok(stat) if !stat.is_dir() => Some(format!(
            "Parent path is not a directory: {}.",
            parent.display()
        )),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(parent)
            .err()
            .map(|error| fs_error(&error, "mkdir")),
        Err(_) => None,
    }
}

/// Kimi Code's host filesystem error message: `<op> failed: <reason>`, such as
/// `write failed: path is a directory`.
pub fn fs_error(error: &std::io::Error, op: &str) -> String {
    use std::io::ErrorKind;
    let reason = match error.kind() {
        ErrorKind::NotFound => "path does not exist",
        ErrorKind::IsADirectory => "path is a directory",
        ErrorKind::NotADirectory => "a path component is not a directory",
        ErrorKind::AlreadyExists => "path already exists",
        ErrorKind::PermissionDenied => "permission denied",
        ErrorKind::DirectoryNotEmpty => "directory is not empty",
        _ => "unrecognized filesystem error",
    };
    format!("{op} failed: {reason}")
}
