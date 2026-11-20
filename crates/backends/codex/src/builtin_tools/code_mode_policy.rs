use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock},
};

use nanocode_process_contracts::JOB_TOOLS;
use nanocode_tool_host::{
    CodeModeToolObservation, CodeModeToolPolicy, FileAccess, WorkspaceToolRouting,
    contract::ToolOutputContent,
};
use serde_json::Value;

use super::{StandardTool, jobs::JobTool};

static POLICY: LazyLock<Arc<dyn CodeModeToolPolicy>> =
    LazyLock::new(|| Arc::new(StandardToolPolicy));

pub(crate) fn standard_tool_policy() -> Arc<dyn CodeModeToolPolicy> {
    Arc::clone(&POLICY)
}

static JOBS_HIDDEN: LazyLock<Arc<dyn CodeModeToolPolicy>> = LazyLock::new(|| Arc::new(JobsHidden));

pub(crate) fn jobs_hidden_policy() -> Arc<dyn CodeModeToolPolicy> {
    Arc::clone(&JOBS_HIDDEN)
}

struct JobsHidden;

impl CodeModeToolPolicy for JobsHidden {
    fn hidden_host_tool(&self, name: &str) -> bool {
        JOB_TOOLS.contains(&name)
    }
}

struct StandardToolPolicy;

impl CodeModeToolPolicy for StandardToolPolicy {
    fn workspace_routing(&self, name: &str) -> Option<WorkspaceToolRouting> {
        [
            (
                StandardTool::ExecCommand,
                WorkspaceToolRouting::ProcessStart,
            ),
            (StandardTool::WriteStdin, WorkspaceToolRouting::ProcessInput),
            (StandardTool::ViewImage, WorkspaceToolRouting::Path),
            (StandardTool::ApplyPatch, WorkspaceToolRouting::Patch),
        ]
        .into_iter()
        .find_map(|(tool, routing)| (tool.name() == name).then_some(routing))
        .or_else(|| {
            JobTool::named(name).map(|tool| match tool {
                JobTool::Start => WorkspaceToolRouting::ProcessStart,
                JobTool::Wait | JobTool::Kill => WorkspaceToolRouting::ProcessInput,
            })
        })
    }

    fn file_access(&self, name: &str, input: &Value) -> Vec<FileAccess> {
        if StandardTool::ApplyPatch.name() != name {
            return Vec::new();
        }
        super::apply_patch::touched_paths(input)
            .into_iter()
            .map(FileAccess::Write)
            .collect()
    }

    fn direct_host_tool(&self, name: &str) -> bool {
        [
            StandardTool::ExecCommand,
            StandardTool::WriteStdin,
            StandardTool::UpdatePlan,
            StandardTool::ApplyPatch,
            StandardTool::ViewImage,
        ]
        .into_iter()
        .any(|tool| tool.name() == name)
            || JobTool::named(name).is_some()
    }

    fn hidden_host_tool(&self, name: &str) -> bool {
        ["Bash", "Read", "Edit", "Write", "TaskOutput", "TaskStop"].contains(&name)
    }

    fn append_observation_content(
        &self,
        calls: &[CodeModeToolObservation<'_>],
        content: &mut Vec<ToolOutputContent>,
    ) {
        let exec_command = StandardTool::ExecCommand.name();
        let write_stdin = StandardTool::WriteStdin.name();
        let mut running = BTreeSet::new();
        for observed in calls {
            let call = observed.call;
            let job = JobTool::named(&call.name);
            if call.name != exec_command && call.name != write_stdin && job.is_none() {
                continue;
            }
            if (call.name == write_stdin || matches!(job, Some(JobTool::Wait | JobTool::Kill)))
                && let Some(session_id) = call.input.get("session_id").and_then(Value::as_i64)
            {
                running.remove(&session_id);
            }
            if let Some(session_id) = observed
                .process_trace
                .and_then(|trace| trace.session_id)
                .or_else(|| {
                    call.structured_result
                        .get("session_id")
                        .and_then(Value::as_i64)
                })
            {
                running.insert(session_id);
            }
        }
        for session_id in running {
            if content
                .iter()
                .filter_map(|item| match item {
                    ToolOutputContent::InputText { text } => Some(text),
                    _ => None,
                })
                .any(|text| {
                    serde_json::from_str::<Value>(text).is_ok_and(|value| {
                        value.as_i64() == Some(session_id)
                            || value.get("session_id").and_then(Value::as_i64) == Some(session_id)
                    })
                })
            {
                continue;
            }
            content.push(ToolOutputContent::InputText {
                text: format!("Nested shell process is still running with session ID {session_id}. Resume it with tools.{write_stdin}({{ session_id: {session_id}, chars: \"\" }})."),
            });
        }
    }
}
