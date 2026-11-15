use nanocode_backend_claude_client::ToolResultContent;
pub use nanocode_tool_host::deadline::Deadline;
pub(crate) use nanocode_tool_host::deadline::{Cap, Reminders};
use serde_json::Value;

pub(crate) fn cap(deadline: Deadline, name: &str, input: &Value) -> Cap<Value> {
    match name {
        "Bash" => deadline.cap("timeout", input),
        "wait" => deadline.cap("yield_time_ms", input),
        "exec" if input.get("yield_time_ms").and_then(Value::as_f64).is_some() => {
            deadline.cap("yield_time_ms", input)
        }
        "exec" => {
            let (Some(object), Some(code)) =
                (input.as_object(), input.get("code").and_then(Value::as_str))
            else {
                return Cap::Unchanged;
            };
            match deadline.cap_source(code) {
                Cap::Capped {
                    input: code,
                    notice,
                } => {
                    let mut object = object.clone();
                    object.insert("code".into(), code.into());
                    Cap::Capped {
                        input: Value::Object(object),
                        notice,
                    }
                }
                Cap::Expired(text) => Cap::Expired(text),
                Cap::Unchanged => Cap::Unchanged,
            }
        }
        _ => Cap::Unchanged,
    }
}

pub(crate) fn append(content: &mut ToolResultContent, notice: &str) {
    match content {
        ToolResultContent::Text(text) => {
            text.push_str("\n\n");
            text.push_str(notice);
        }
        ToolResultContent::Blocks(blocks) => {
            blocks.push(serde_json::json!({"type": "text", "text": notice}));
        }
    }
}
