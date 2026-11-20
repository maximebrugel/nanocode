use crate::json::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FunctionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: Value,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
    #[serde(default)]
    pub thoughts_token_count: u64,
    #[serde(default)]
    pub total_token_count: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub usage_metadata: Option<Usage>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub prompt_feedback: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub grounding_metadata: Option<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub parts: Vec<Part>,
    pub usage: Usage,
    pub finish_reason: Option<String>,
    pub model_version: Option<String>,
    pub grounding: Option<Value>,
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    pub fn response(name: impl Into<String>, key: &str, body: impl Into<String>) -> Self {
        Self {
            function_response: Some(FunctionResponse {
                id: None,
                name: name.into(),
                response: serde_json::json!({ key: body.into() }),
            }),
            ..Self::default()
        }
    }

    pub fn is_thought(&self) -> bool {
        self.thought == Some(true)
    }

    pub fn to_json(&self) -> Json {
        let mut fields = Vec::new();
        if let Some(text) = &self.text {
            fields.push(("text".to_owned(), Json::str(text.clone())));
        }
        if let Some(call) = &self.function_call {
            let mut call_fields = Vec::new();
            if let Some(id) = &call.id {
                call_fields.push(("id".to_owned(), Json::str(id.clone())));
            }
            call_fields.push(("name".to_owned(), Json::str(call.name.clone())));
            call_fields.push(("args".to_owned(), Json::sorted(&call.args)));
            fields.push(("functionCall".to_owned(), Json::Object(call_fields)));
        }
        if let Some(response) = &self.function_response {
            let mut response_fields = Vec::new();
            if let Some(id) = &response.id {
                response_fields.push(("id".to_owned(), Json::str(id.clone())));
            }
            response_fields.push(("name".to_owned(), Json::str(response.name.clone())));
            response_fields.push(("response".to_owned(), Json::sorted(&response.response)));
            fields.push(("functionResponse".to_owned(), Json::Object(response_fields)));
        }
        if let Some(thought) = self.thought {
            fields.push(("thought".to_owned(), Json::Bool(thought)));
        }
        if let Some(signature) = &self.thought_signature {
            fields.push(("thoughtSignature".to_owned(), Json::str(signature.clone())));
        }
        Json::Object(fields)
    }
}

impl Content {
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: "user".to_owned(),
            parts,
        }
    }

    pub fn model(parts: Vec<Part>) -> Self {
        Self {
            role: "model".to_owned(),
            parts,
        }
    }

    pub fn to_json(&self) -> Json {
        Json::object([
            ("role", Json::str(self.role.clone())),
            (
                "parts",
                Json::Array(self.parts.iter().map(Part::to_json).collect()),
            ),
        ])
    }

    pub fn calls(&self) -> impl Iterator<Item = &FunctionCall> {
        self.parts
            .iter()
            .filter_map(|part| part.function_call.as_ref())
    }
}

pub fn merge_parts(chunks: Vec<Part>) -> Vec<Part> {
    let mut merged: Vec<Part> = Vec::new();
    for chunk in chunks {
        let joins = chunk.function_call.is_none()
            && chunk.function_response.is_none()
            && chunk.text.is_some()
            && merged.last().is_some_and(|last| {
                last.function_call.is_none()
                    && last.function_response.is_none()
                    && last.text.is_some()
                    && last.is_thought() == chunk.is_thought()
                    && last.thought_signature.is_none()
            });
        match (joins, merged.last_mut()) {
            (true, Some(last)) => {
                if let (Some(text), Some(more)) = (last.text.as_mut(), chunk.text.as_ref()) {
                    text.push_str(more);
                }
                if chunk.thought_signature.is_some() {
                    last.thought_signature = chunk.thought_signature;
                }
            }
            _ => {
                if chunk.text.as_deref() == Some("")
                    && chunk.function_call.is_none()
                    && chunk.thought_signature.is_some()
                    && let Some(last) = merged.last_mut()
                    && last.thought_signature.is_none()
                {
                    last.thought_signature = chunk.thought_signature;
                    continue;
                }
                merged.push(chunk);
            }
        }
    }
    merged
}
