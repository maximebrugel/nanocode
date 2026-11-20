use nanocode_backend_antigravity_client::Json;
use nanocode_tool_host::contract::{ToolOutputBody, ToolOutputContent};
use nanocode_tool_host::embedded::{CodeModeObserver, CodeModeUpdate, OwnedToolContext};
use nanocode_tool_host::runtime::ToolRuntime;
use nanocode_tool_host::{ToolContext, ToolDefinition, ToolInput};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct NanoTools {
    runtime: ToolRuntime,
    routes: BTreeMap<String, Route>,
    pub declarations: Vec<String>,
}

#[derive(Clone)]
enum Route {
    Function(String),
    Exec,
    Wait,
}

struct Quiet;

impl CodeModeObserver for Quiet {
    fn update(&mut self, _update: CodeModeUpdate<'_>) {}
}

fn declaration(name: &str, description: &str, schema: Value) -> String {
    Json::object([
        ("name", Json::str(name)),
        ("description", Json::str(description)),
        ("parametersJsonSchema", Json::sorted(&schema)),
    ])
    .encode()
}

fn exec_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": {"type": "string", "description": "JavaScript source to evaluate."},
            "yield_time_ms": {"type": "number", "description": "How long to wait before a script that is still running yields."},
            "max_output_tokens": {"type": "number", "description": "Token budget for this call's direct results."}
        },
        "required": ["code"]
    })
}

impl NanoTools {
    pub fn new(runtime: ToolRuntime, session: &str) -> Self {
        let mut routes = BTreeMap::new();
        let mut declarations = Vec::new();
        let mut add = |name: String, description: &str, schema: Value, route: Route| {
            if nanocode_backend_antigravity_tools::NAMES.contains(&name.as_str())
                || routes.contains_key(&name)
            {
                return;
            }
            declarations.push(declaration(&name, description, schema));
            routes.insert(name, route);
        };
        for definition in flatten(runtime.model_specs(session)) {
            match definition {
                ToolDefinition::Function {
                    name,
                    description,
                    parameters,
                    ..
                } => {
                    let schema =
                        serde_json::to_value(&parameters).unwrap_or(json!({"type": "object"}));
                    let route = match &*name {
                        "wait" => Route::Wait,
                        _ => Route::Function(name.to_string()),
                    };
                    add(name.to_string(), &description, schema, route);
                }
                ToolDefinition::Custom {
                    name, description, ..
                } => {
                    add(name.to_string(), &description, exec_schema(), Route::Exec);
                }
                _ => {}
            }
        }
        Self {
            runtime,
            routes,
            declarations,
        }
    }

    pub fn handles(&self, name: &str) -> bool {
        self.routes.contains_key(name)
    }

    pub async fn call(
        &self,
        name: &str,
        args: &Value,
        context: ToolContext<'_>,
    ) -> std::result::Result<String, String> {
        let route = self
            .routes
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown tool {name:?}"))?;
        match route {
            Route::Exec => {
                let source = args.get("code").and_then(Value::as_str).unwrap_or_default();
                let code = nanocode_tool_host::code_mode::source_with_options(
                    source,
                    args.get("yield_time_ms"),
                    args.get("max_output_tokens"),
                )?;
                let execution = self
                    .runtime
                    .execute_code_owned_with_updates(
                        &code,
                        OwnedToolContext::from_context(context),
                        &mut Quiet,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let mut text = body_text(&execution.output);
                for notification in execution.notifications {
                    text.push_str("\n\n");
                    text.push_str(&notification.text);
                }
                match execution.success {
                    true => Ok(text),
                    false => Err(text),
                }
            }
            Route::Wait => {
                let execution = self
                    .runtime
                    .wait_for_code_with_updates(&args.to_string(), context, &mut Quiet)
                    .await
                    .map_err(|error| error.to_string())?;
                let text = body_text(&execution.output);
                match execution.success {
                    true => Ok(text),
                    false => Err(text),
                }
            }
            Route::Function(target) => {
                let raw =
                    serde_json::value::to_raw_value(args).map_err(|error| error.to_string())?;
                let output = self
                    .runtime
                    .execute_tool(&target, ToolInput::Function(raw), context)
                    .await
                    .map_err(|error| error.to_string())?;
                let text = body_text(&output.output);
                match output.success {
                    true => Ok(text),
                    false => Err(text),
                }
            }
        }
    }
}

fn flatten(definitions: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let mut flat = Vec::new();
    for definition in definitions {
        match definition {
            ToolDefinition::Namespace {
                name: namespace,
                tools,
                ..
            } => {
                for mut tool in tools {
                    match &mut tool {
                        ToolDefinition::Function { name, .. }
                        | ToolDefinition::Custom { name, .. } => {
                            *name = format!("{namespace}__{name}").into();
                        }
                        _ => continue,
                    }
                    flat.push(tool);
                }
            }
            definition => flat.push(definition),
        }
    }
    flat
}

fn body_text(body: &ToolOutputBody) -> String {
    match body {
        ToolOutputBody::Text(text) => text.clone(),
        ToolOutputBody::Content(items) => items
            .iter()
            .filter_map(|item| match item {
                ToolOutputContent::InputText { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
