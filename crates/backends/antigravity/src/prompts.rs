use std::path::Path;

pub const SYSTEM: &str = include_str!("../prompts/system.md");
pub const SKILLS: &str = include_str!("../prompts/skills.md");
pub const SEARCH: &str = include_str!("../prompts/search_web.md");
const LOGS: &str = "<appDataDir>/brain/<conversation-id>/.system_generated/logs";
pub const COMPACTION: &str = include_str!("../prompts/compaction.md");

pub struct Environment<'a> {
    pub os: &'a str,
    pub workspace: &'a Path,
    pub app_data: &'a Path,
    pub conversation: &'a str,
    pub skills: &'a [(String, String, String)],
}

pub fn system(environment: &Environment<'_>) -> String {
    let skills = match environment.skills.is_empty() {
        true => String::new(),
        false => SKILLS.replace(
            "{{skill_list}}",
            &environment
                .skills
                .iter()
                .map(|(name, path, description)| format!("- {name} ({path}): {description}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    };
    let artifacts = environment
        .app_data
        .join("brain")
        .join(environment.conversation);
    SYSTEM
        .replace("{{skills}}", &skills)
        .replace("{{os}}", environment.os)
        .replace(
            "{{workspaces}}",
            &format!("- {}", environment.workspace.display()),
        )
        .replace("{{cwd}}", &environment.workspace.display().to_string())
        .replace("{{app_data}}", &environment.app_data.display().to_string())
        .replace("{{conversation_id}}", environment.conversation)
        .replace("{{artifact_dir}}", &artifacts.display().to_string())
}

pub fn user_request(text: &str, local_time: &str, settings_change: Option<&str>) -> String {
    let mut out = format!(
        "<USER_REQUEST>\n{text}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: {local_time}.\n</ADDITIONAL_METADATA>"
    );
    if let Some(model) = settings_change {
        out.push_str(&format!(
            "\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to {model}. No need to comment on this change if the user doesn't ask about it. If reporting what model you are, please use a human readable name instead of the exact string.\n</USER_SETTINGS_CHANGE>"
        ));
    }
    out
}

pub fn context_summary(
    generated: &str,
    requests: &[String],
    summary: &str,
    tasks: &[String],
    _logs: &Path,
) -> String {
    let requests = requests
        .iter()
        .enumerate()
        .map(|(index, request)| format!("{}. {request}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!(
        "<CONTEXT_SUMMARY>\nThe following is a summary of the conversation history that has been truncated to fit within the context window:\n\nThis summary was generated at {generated}.\n\n# User Requests\nThe following were the most recent user requests in chronological order:\n{requests}\n\n# Previous Session Summary:\n{summary}\n"
    );
    if !tasks.is_empty() {
        out.push_str(&format!(
            "\n# Running Background Tasks\nThe following background tasks were still running when this summary was generated:\n{}\n",
            tasks.join("\n")
        ));
    }
    out.push_str(&format!(
        "\n# Conversation Logs\nStart with the compact transcript; if a step has `truncated_fields`, read only that specific line in `transcript_full.jsonl`:\n- `{}/transcript.jsonl`\n- `{}/transcript_full.jsonl`\n</CONTEXT_SUMMARY>",
        LOGS,
        LOGS
    ));
    out
}
