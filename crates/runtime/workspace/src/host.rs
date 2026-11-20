use std::{ffi::OsString, path::Path, sync::Arc, time::Duration};

use nanocode_process_contracts::{
    ExecCommand, ExecCommandResult, FileHost, ForegroundCommand, ForegroundExecutor,
    ForegroundOutput, ProcessExecutors, ProcessFuture, ShellExecutor, WriteStdin,
    effective_token_limit, redact_and_limit_in,
};

use crate::{
    Command, Ended, Finished, Limit, Workspace,
    sim::{self, Mutation},
};

fn bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}

fn absolute(path: &Path) -> Result<Vec<u8>, String> {
    match path.is_absolute() || path.as_os_str().as_encoded_bytes().starts_with(b"/") {
        true => Ok(bytes(path)),
        false => Err(format!(
            "{}: the workspace takes absolute paths",
            path.display()
        )),
    }
}

impl FileHost for Workspace {
    fn read<'a>(&'a self, path: &'a Path) -> ProcessFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            let data = FileHost::read_bytes(self, path).await?;
            data.map(|data| {
                String::from_utf8(data)
                    .map_err(|_| format!("{}: the file is not UTF-8 text", path.display()))
            })
            .transpose()
        })
    }

    fn write<'a>(
        &'a self,
        path: &'a Path,
        contents: &'a str,
    ) -> ProcessFuture<'a, Result<(), String>> {
        FileHost::write_bytes(self, path, contents.as_bytes())
    }

    fn remove<'a>(&'a self, path: &'a Path) -> ProcessFuture<'a, Result<(), String>> {
        Box::pin(async move { Self::remove(self, &absolute(path)?).await })
    }

    fn read_bytes<'a>(
        &'a self,
        path: &'a Path,
    ) -> ProcessFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move { Self::read(self, &absolute(path)?).await })
    }

    fn write_bytes<'a>(
        &'a self,
        path: &'a Path,
        data: &'a [u8],
    ) -> ProcessFuture<'a, Result<(), String>> {
        Box::pin(async move { Self::write(self, &absolute(path)?, data).await })
    }
}

fn status(finished: &Finished) -> (i32, Option<String>) {
    match &finished.ran.ended {
        Ended::Exited(_) if sim::mutated(Mutation::ExitIgnored) => (0, None),
        // Killed by a signal: the status a parent shell's `$?` shows for it.
        Ended::Exited(status) if *status < 0 => (
            128 - status,
            Some(format!("the command was killed by signal {}", -status)),
        ),
        Ended::Exited(status) => (*status, None),
        Ended::Cancelled if finished.timed_out => (
            124,
            Some("the command ran past its timeout and was stopped".to_owned()),
        ),
        Ended::Cancelled => (130, Some("the command was cancelled".to_owned())),
        Ended::Refused(Limit::Fuel) => (
            126,
            Some("the command ran past this workspace's step limit".to_owned()),
        ),
        Ended::Refused(Limit::Operations) => (
            126,
            Some("the command made more file operations than this workspace allows".to_owned()),
        ),
        Ended::Refused(Limit::Missing) => (
            127,
            Some(format!(
                "this workspace has no such command: {}",
                finished
                    .ran
                    .missing
                    .iter()
                    .map(|name| String::from_utf8_lossy(name).into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
        Ended::Unanswered(what) => (127, Some(format!("this workspace cannot run {what}"))),
    }
}

fn text(data: &[u8], limit: usize) -> (String, bool) {
    let cut = data.len() > limit;
    let kept = &data[..data.len().min(limit)];
    (String::from_utf8_lossy(kept).into_owned(), cut)
}

impl ForegroundExecutor for Workspace {
    fn execute<'a>(
        &'a self,
        command: ForegroundCommand<'a>,
    ) -> ProcessFuture<'a, Result<ForegroundOutput, String>> {
        Box::pin(async move {
            let env = command.session.map_or_else(Vec::new, |session| {
                session
                    .variables()
                    .iter()
                    .map(|(name, value)| (name.as_bytes().to_vec(), value.as_bytes().to_vec()))
                    .collect()
            });
            let finished = self
                .run(Command {
                    script: command.command.to_owned(),
                    cwd: absolute(command.workspace)?,
                    env,
                    timeout: Some(command.timeout),
                })
                .await?;
            let (exit_code, note) = status(&finished);
            let mut stderr_bytes = finished.ran.stderr;
            if let Some(note) = note {
                stderr_bytes.extend_from_slice(note.as_bytes());
                stderr_bytes.push(b'\n');
            }
            let (stdout, cut_out) = text(&finished.ran.stdout, command.max_stdout);
            let (stderr, cut_err) = text(&stderr_bytes, command.max_stderr);
            if let Some(output) = command.output {
                output.write(stdout.as_bytes());
                output.write(stderr.as_bytes());
            }
            Ok(ForegroundOutput {
                stdout,
                stderr,
                exit_code,
                truncated: finished.ran.truncated || cut_out || cut_err,
            })
        })
    }
}

impl ShellExecutor for Workspace {
    fn execute<'a>(
        &'a self,
        command: ExecCommand,
        workspace: &'a Path,
    ) -> ProcessFuture<'a, ExecCommandResult> {
        Box::pin(async move {
            let started = nanocode_platform::now();
            let cwd = command.workdir.as_deref().map_or_else(
                || workspace.to_path_buf(),
                |workdir| workspace.join(workdir),
            );
            let cwd = match absolute(&cwd) {
                Ok(cwd) => cwd,
                Err(error) => return ExecCommandResult::failed(started.elapsed(), error),
            };
            let run = self.run(Command {
                script: command.script,
                cwd,
                env: Vec::new(),
                timeout: None,
            });
            let finished = match run.await {
                Ok(finished) => finished,
                Err(error) => return ExecCommandResult::failed(started.elapsed(), error),
            };
            let (exit_code, note) = status(&finished);
            let mut output = finished.ran.stdout;
            output.extend_from_slice(&finished.ran.stderr);
            if let Some(note) = note {
                output.extend_from_slice(note.as_bytes());
                output.push(b'\n');
            }
            let output = String::from_utf8_lossy(&output).into_owned();
            let limit = effective_token_limit(command.max_output_tokens);
            let (output, _) = redact_and_limit_in(output, &[], limit, command.output_unit);
            ExecCommandResult {
                error: None,
                chunk_id: None,
                wall_time_seconds: started.elapsed().as_secs_f64(),
                exit_code: Some(exit_code),
                session_id: None,
                original_token_count: None,
                output,
                time_left_seconds: None,
            }
        })
    }

    fn write_stdin(&self, _request: WriteStdin) -> ProcessFuture<'_, ExecCommandResult> {
        Box::pin(async {
            ExecCommandResult::failed(
                Duration::ZERO,
                "this workspace runs each command to its end and keeps no session to write to"
                    .to_owned(),
            )
        })
    }

    fn terminate_all(&self) -> ProcessFuture<'_, ()> {
        Box::pin(async {})
    }

    fn keeps_sessions(&self) -> bool {
        false
    }
}

impl ProcessExecutors for Workspace {
    fn shell(&self, environment: Vec<(OsString, OsString)>) -> Arc<dyn ShellExecutor> {
        let env = environment
            .into_iter()
            .map(|(name, value)| {
                (
                    name.as_encoded_bytes().to_vec(),
                    value.as_encoded_bytes().to_vec(),
                )
            })
            .collect();
        Arc::new(self.with_env(env))
    }

    fn foreground(&self) -> Arc<dyn ForegroundExecutor> {
        Arc::new(self.clone())
    }
}
