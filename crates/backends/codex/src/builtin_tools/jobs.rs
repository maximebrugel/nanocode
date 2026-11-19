use std::{collections::HashMap, path::PathBuf, sync::Arc};

use nanocode_process_contracts::{
    ExecCommand, ExecCommandResult, JOB_TOOLS, MAX_POLL_YIELD_MS, MIN_POLL_YIELD_MS, ShellExecutor,
    WriteStdin, effective_token_limit,
};
use nanocode_sync::Atom;
use nanocode_tools::{RunningTask, ToolDefinition};
use serde::Deserialize;
use serde_json::json;

use super::{Tool, ToolContext, ToolInput, ToolOutput, ToolResult, shell::output_limit};

const INTERRUPT: &str = "\u{3}";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobTool {
    Start,
    Wait,
    Kill,
}

impl JobTool {
    pub const ALL: [Self; 3] = [Self::Start, Self::Wait, Self::Kill];

    #[must_use]
    pub const fn name(self) -> &'static str {
        JOB_TOOLS[self as usize]
    }

    #[must_use]
    pub fn definition(self) -> ToolDefinition {
        match self {
            Self::Start => ToolDefinition::function(
                self.name(),
                "Starts a shell command as a background job and returns at once with its session_id. Use it for anything that can run longer than a few seconds: builds and compiles (make, cargo, R package installs), test suites, training or benchmark runs, downloads, video or OCR passes, servers. The job keeps running while you do other work; nothing stops it when your call returns. Check it with job_wait, which waits for the job to exit and returns only the last lines of its output, and stop it with job_kill. Prefer one job_wait with a long yield_time_ms over many short polls, and never restart a job that is still running.",
                nanocode_tools::json_schema!({
                    "type": "object",
                    "properties": {
                        "cmd": { "type": "string", "description": "Shell command to run." },
                        "workdir": { "type": "string", "description": "Working directory for the command. Defaults to the turn cwd." },
                        "tty": { "type": "boolean", "description": "True allocates a PTY so the job can read input later through write_stdin; false or omitted uses plain pipes." }
                    },
                    "required": ["cmd"],
                    "additionalProperties": false
                }),
            ),
            Self::Wait => ToolDefinition::function(
                self.name(),
                format!(
                    "Waits for a background job to exit, up to yield_time_ms, and returns its status with the last lines of its output (the middle of long output is dropped, so the end of a build log or test run is what you see). Returns as soon as the job exits. yield_time_ms defaults to {MAX_POLL_YIELD_MS} ms, its maximum, and is at least {MIN_POLL_YIELD_MS} ms; while the job runs, call it again rather than polling with short waits. A job that is still running reports the latest lines it printed, also when it printed nothing new."
                ),
                nanocode_tools::lazy_json_schema!(json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "number", "description": "The job's session_id from job_start or exec_command." },
                        "yield_time_ms": { "type": "number", "description": format!("How long to wait for the job to exit. Defaults to {MAX_POLL_YIELD_MS} ms; effective range is {MIN_POLL_YIELD_MS}-{MAX_POLL_YIELD_MS} ms.") },
                        "tail_lines": { "type": "number", "description": "Return at most this many trailing lines of output. Defaults to as many as the output budget holds." },
                        "max_output_tokens": { "type": "number", "description": "Output token budget. Defaults to 10000 tokens; larger requests may be capped by policy." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                })),
            ),
            Self::Kill => ToolDefinition::function(
                self.name(),
                format!(
                    "Stops a background job by interrupting its process group, waits up to {MIN_POLL_YIELD_MS} ms for it to exit, and returns its status with the last lines of its output."
                ),
                nanocode_tools::json_schema!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "number", "description": "The job's session_id." }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }),
            ),
        }
    }

    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }
}

#[derive(Clone, Default)]
pub struct JobTails(Arc<Atom<HashMap<i64, String>>>);

impl JobTails {
    fn record(&self, session_id: i64, output: &str, limit: usize) -> String {
        self.0.update(|tails| {
            let tail = tails.entry(session_id).or_default();
            tail.push_str(output);
            if tail.len() > limit {
                let mut cut = tail.len() - limit;
                while !tail.is_char_boundary(cut) {
                    cut += 1;
                }
                tail.drain(..cut);
            }
            tail.clone()
        })
    }

    fn finish(&self, session_id: i64, output: &str, limit: usize) -> String {
        let tail = self.record(session_id, output, limit);
        self.0.update(|tails| {
            tails.remove(&session_id);
        });
        tail
    }
}

pub struct JobHandler {
    kind: JobTool,
    workspace: PathBuf,
    sessions: Arc<dyn ShellExecutor>,
    tails: JobTails,
}

impl JobHandler {
    #[must_use]
    pub fn all(workspace: &std::path::Path, sessions: &Arc<dyn ShellExecutor>) -> Vec<Arc<Self>> {
        let tails = JobTails::default();
        JobTool::ALL
            .into_iter()
            .map(|kind| {
                Arc::new(Self {
                    kind,
                    workspace: workspace.to_path_buf(),
                    sessions: Arc::clone(sessions),
                    tails: tails.clone(),
                })
            })
            .collect()
    }

    #[must_use]
    pub const fn kind(&self) -> JobTool {
        self.kind
    }

    async fn run(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        match self.kind {
            JobTool::Start => {
                let arguments = input.decode_json::<StartArguments>()?;
                let (max_output_tokens, output_unit) = output_limit(None, context);
                let command = ExecCommand::new(
                    arguments.cmd.clone(),
                    arguments.workdir,
                    None,
                    None,
                    arguments.tty,
                    Some(0),
                    Some(max_output_tokens),
                )
                .with_output_unit(output_unit)
                .with_output_token_budget(context.output_token_budget())
                .from_caller(context.session_id());
                let result = self.sessions.execute(command, &self.workspace).await;
                Ok(self.report(
                    &result,
                    max_output_tokens,
                    None,
                    Started::Now(&arguments.cmd),
                ))
            }
            JobTool::Wait => {
                let arguments = input.decode_json::<WaitArguments>()?;
                let (max_output_tokens, output_unit) =
                    output_limit(arguments.max_output_tokens, context);
                let request = WriteStdin::new(
                    arguments.session_id,
                    String::new(),
                    Some(arguments.yield_time_ms.unwrap_or(MAX_POLL_YIELD_MS)),
                    Some(max_output_tokens),
                )
                .with_output_unit(output_unit)
                .from_caller(context.session_id());
                let result = self.sessions.write_stdin(request).await;
                Ok(self.report(
                    &result,
                    max_output_tokens,
                    arguments.tail_lines,
                    Started::Before(i64::from(arguments.session_id)),
                ))
            }
            JobTool::Kill => {
                let arguments = input.decode_json::<KillArguments>()?;
                let (max_output_tokens, output_unit) = output_limit(None, context);
                let request = WriteStdin::new(
                    arguments.session_id,
                    INTERRUPT.to_owned(),
                    Some(MIN_POLL_YIELD_MS),
                    Some(max_output_tokens),
                )
                .with_output_unit(output_unit)
                .from_caller(context.session_id());
                let result = self.sessions.write_stdin(request).await;
                Ok(self.report(
                    &result,
                    max_output_tokens,
                    None,
                    Started::Before(i64::from(arguments.session_id)),
                ))
            }
        }
    }

    fn report(
        &self,
        result: &ExecCommandResult,
        max_output_tokens: usize,
        tail_lines: Option<usize>,
        started: Started<'_>,
    ) -> ToolOutput {
        if let Some(error) = &result.error {
            return ToolOutput::error(error.replacen(
                "write_stdin failed",
                &format!("{} failed", self.kind.name()),
                1,
            ));
        }
        let limit = effective_token_limit(Some(max_output_tokens));
        let tail = match (result.session_id, started) {
            (Some(session_id), _) => self.tails.record(session_id, &result.output, limit),
            (None, Started::Before(session_id)) => {
                self.tails.finish(session_id, &result.output, limit)
            }
            (None, Started::Now(_)) => result.output.clone(),
        };
        let tail = match tail_lines {
            Some(lines) => last_lines(&tail, lines),
            None => tail,
        };
        let status = match (result.session_id, result.exit_code, started) {
            (Some(session_id), _, Started::Now(command)) => format!(
                "Job {session_id} started: `{command}`. It runs in the background; call job_wait({{ session_id: {session_id} }}) to wait for it."
            ),
            (Some(session_id), _, Started::Before(_)) => format!(
                "Job {session_id} is still running after {:.1} s of waiting.",
                result.wall_time_seconds
            ),
            (None, Some(code), _) => format!(
                "Job exited with code {code} after {:.1} s of waiting.",
                result.wall_time_seconds
            ),
            (None, None, _) => "Job ended without an exit code.".to_owned(),
        };
        let status = match result.time_left_seconds {
            Some(time_left_seconds) => format!("{status}\nTime left: {time_left_seconds} seconds"),
            None => status,
        };
        let text = if tail.is_empty() {
            format!("{status}\nNo output yet.")
        } else {
            format!("{status}\nLast output:\n{tail}")
        };
        let mut structured = json!({
            "wall_time_seconds": result.wall_time_seconds,
            "output": tail,
        });
        if let Some(session_id) = result.session_id {
            structured["session_id"] = json!(session_id);
        }
        if let Some(code) = result.exit_code {
            structured["exit_code"] = json!(code);
        }
        ToolOutput::text(text)
            .with_structured_result(structured)
            .with_process_trace(
                result.exit_code,
                result.session_id,
                result.original_token_count,
                result.output.len(),
                result.wall_time_seconds,
            )
    }
}

#[derive(Clone, Copy)]
enum Started<'a> {
    Now(&'a str),
    Before(i64),
}

fn last_lines(text: &str, lines: usize) -> String {
    if lines == 0 {
        return String::new();
    }
    let trimmed = text.trim_end_matches('\n');
    let start = trimmed
        .rmatch_indices('\n')
        .nth(lines - 1)
        .map_or(0, |(index, _)| index + 1);
    trimmed[start..].to_owned()
}

impl Tool for JobHandler {
    fn definition(&self) -> ToolDefinition {
        self.kind.definition()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn running_tasks(&self) -> Vec<RunningTask> {
        match self.kind {
            JobTool::Start => self
                .sessions
                .running()
                .into_iter()
                .map(|process| RunningTask {
                    id: process.session_id.to_string(),
                    label: process.command,
                    silent_for: process.silent_for,
                })
                .collect(),
            JobTool::Wait | JobTool::Kill => Vec::new(),
        }
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> nanocode_tools::ToolCallFuture<'async_trait, ToolResult>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(self.run(input, context))
    }
}

#[derive(Deserialize)]
struct StartArguments {
    cmd: String,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    tty: bool,
}

#[derive(Deserialize)]
struct WaitArguments {
    session_id: i32,
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    tail_lines: Option<usize>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[derive(Deserialize)]
struct KillArguments {
    session_id: i32,
}
