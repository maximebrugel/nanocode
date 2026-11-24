use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    #[default]
    Prompt,
    Reminder,
    Summary,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
}

impl Usage {
    pub(crate) fn parse(value: &Value) -> Option<Self> {
        let usage = value.as_object()?;
        let number = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
        Some(Self {
            prompt_tokens: number(usage.get("prompt_tokens")),
            completion_tokens: number(usage.get("completion_tokens")),
            cached_tokens: number(
                usage
                    .get("cached_tokens")
                    .or_else(|| value.pointer("/prompt_tokens_details/cached_tokens")),
            ),
            reasoning_tokens: number(value.pointer("/completion_tokens_details/reasoning_tokens")),
        })
    }

    #[must_use]
    pub const fn total(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: String,
        origin: Origin,
    },
    Assistant {
        content: String,
        reasoning: String,
        tool_calls: Vec<ToolCall>,
        usage: Option<Usage>,
    },
    Tool {
        tool_call_id: String,
        content: String,
        #[serde(default)]
        failed: bool,
        #[serde(default)]
        note: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
}

const TOOL_ERROR_STATUS: &str = "<system>ERROR: Tool execution failed.</system>";
const TOOL_EMPTY_STATUS: &str = "<system>Tool output is empty.</system>";
const TOOL_EMPTY_ERROR_STATUS: &str =
    "<system>ERROR: Tool execution failed. Tool output is empty.</system>";
pub const TOOL_OUTPUT_EMPTY: &str = "Tool output is empty.";

#[must_use]
pub fn render_tool_output(output: &str, failed: bool) -> String {
    match failed {
        true if output.is_empty() => TOOL_EMPTY_ERROR_STATUS.to_owned(),
        true => format!("{TOOL_ERROR_STATUS}\n{output}"),
        false if output.trim().is_empty() || output.trim() == TOOL_OUTPUT_EMPTY => {
            TOOL_EMPTY_STATUS.to_owned()
        }
        false => output.to_owned(),
    }
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>, origin: Origin) -> Self {
        Self::User {
            content: content.into(),
            origin,
        }
    }

    fn wire(&self) -> Value {
        match self {
            Self::User { content, .. } => json!({"role": "user", "content": content}),
            Self::Tool {
                tool_call_id,
                content,
                failed,
                note,
                notice,
                ..
            } => {
                let text = render_tool_output(content, *failed) + note;
                json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": match notice {
                        Some(notice) => json!([{"type": "text", "text": text}, {"type": "text", "text": notice}]),
                        None => json!(text),
                    },
                })
            }
            Self::Assistant {
                content,
                reasoning,
                tool_calls,
                ..
            } => {
                let mut message = Map::new();
                message.insert("role".to_owned(), json!("assistant"));
                if tool_calls.is_empty() || !content.trim().is_empty() {
                    message.insert("content".to_owned(), json!(content));
                }
                if !tool_calls.is_empty() {
                    let calls = tool_calls
                        .iter()
                        .map(|call| {
                            json!({
                                "id": call.id,
                                "type": "function",
                                "function": {"name": call.name, "arguments": call.arguments},
                            })
                        })
                        .collect::<Vec<_>>();
                    message.insert("tool_calls".to_owned(), Value::Array(calls));
                }
                message.insert("reasoning_content".to_owned(), json!(reasoning));
                Value::Object(message)
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

pub struct Request<'a> {
    pub model: &'a str,
    pub system: &'a str,
    pub messages: &'a [Message],
    pub extra: Option<&'a Message>,
    pub tools: &'a [Value],
    pub max_completion_tokens: u64,
    pub prompt_cache_key: &'a str,
    pub effort: &'a str,
}

impl Request<'_> {
    #[must_use]
    pub fn encode(&self) -> String {
        let mut messages = vec![json!({"role": "system", "content": self.system})];
        messages.extend(self.messages.iter().chain(self.extra).map(Message::wire));
        let mut body = Map::new();
        body.insert("model".to_owned(), json!(self.model));
        body.insert("messages".to_owned(), Value::Array(messages));
        if !self.tools.is_empty() {
            body.insert("tools".to_owned(), Value::Array(self.tools.to_vec()));
        }
        body.insert(
            "max_completion_tokens".to_owned(),
            json!(self.max_completion_tokens),
        );
        body.insert("prompt_cache_key".to_owned(), json!(self.prompt_cache_key));
        body.insert(
            "thinking".to_owned(),
            json!({"type": "enabled", "effort": self.effort, "keep": "all"}),
        );
        body.insert("stream".to_owned(), json!(true));
        body.insert("stream_options".to_owned(), json!({"include_usage": true}));
        Value::Object(body).to_string()
    }
}

#[must_use]
pub fn estimate_text(text: &str) -> u64 {
    let (ascii, other) = text.chars().fold((0_u64, 0_u64), |(ascii, other), char| {
        if char.is_ascii() {
            (ascii + 1, other)
        } else {
            (ascii, other + 1)
        }
    });
    ascii.div_ceil(4) + other
}

#[must_use]
pub fn estimate_message(message: &Message) -> u64 {
    estimate(message, |arguments| {
        estimate_text(&Value::String(arguments.to_owned()).to_string())
    })
}

#[must_use]
pub fn estimate_request_message(message: &Message) -> u64 {
    estimate(message, estimate_text)
}

fn estimate(message: &Message, arguments: impl Fn(&str) -> u64) -> u64 {
    match message {
        Message::User { content, .. } => estimate_text("user") + estimate_text(content),
        Message::Tool {
            content,
            note,
            notice,
            ..
        } => {
            estimate_text("tool")
                + estimate_text(&format!("{content}{note}"))
                + notice.as_deref().map_or(0, estimate_text)
        }
        Message::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
        } => {
            let calls = tool_calls
                .iter()
                .map(|call| estimate_text(&call.name) + arguments(&call.arguments))
                .sum::<u64>();
            estimate_text("assistant") + estimate_text(reasoning) + estimate_text(content) + calls
        }
    }
}

#[must_use]
pub fn sanitize_call_id(id: &str) -> String {
    let sanitized = id
        .chars()
        .map(|char| match char {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' => char,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "tool_call".to_owned()
    } else {
        sanitized
    }
}
