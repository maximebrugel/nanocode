use std::collections::HashSet;

use nanocode_agent_core::{
    HarnessFamily, SessionCheckpoint, Thinking,
    session::{ToolOutcome, TranscriptItem},
};
use nanocode_durability::{
    AdmittedTurn, Advanced, CheckpointFormat, EncodedPayload, Error, FormatFuture, Hydrated,
    PayloadReader, Previous, Result, SessionRecord,
};
use serde::Deserialize as _;
use serde_json::Value;

use crate::{CodexCheckpointExt as _, durability::context, session::SessionSnapshot};

pub struct CodexCheckpointFormat;

impl CheckpointFormat for CodexCheckpointFormat {
    fn family(&self) -> HarnessFamily {
        HarnessFamily::Codex
    }

    fn owns(&self, checkpoint: &Value) -> bool {
        checkpoint.get("provider").is_none()
    }

    fn hydrate<'a>(
        &'a self,
        reader: PayloadReader<'a>,
        stored: Value,
    ) -> FormatFuture<'a, Result<Value>> {
        Box::pin(async move { hydrate(reader, stored, &mut HashSet::new()).await })
    }

    fn hydrate_noting<'a>(
        &'a self,
        reader: PayloadReader<'a>,
        stored: Value,
        read: &'a mut HashSet<String>,
    ) -> FormatFuture<'a, Result<Value>> {
        Box::pin(hydrate(reader, stored, read))
    }

    fn hydrate_typed<'a>(
        &'a self,
        reader: PayloadReader<'a>,
        stored: Value,
        read: &'a mut HashSet<String>,
    ) -> FormatFuture<'a, Result<(Value, Option<Hydrated>)>> {
        Box::pin(async move {
            let saved: context::Snapshot =
                serde_json::from_value(stored).map_err(Error::InvalidPayload)?;
            let (hydrated, keys) = context::load_hydrated_with_keys(reader, saved).await?;
            read.extend(keys);
            let value = serde_json::to_value(&hydrated.snapshot).map_err(Error::InvalidPayload)?;
            Ok((value, Some(Hydrated::new(hydrated))))
        })
    }

    fn advance_typed<'a>(
        &'a self,
        reader: PayloadReader<'a>,
        stored: Value,
        read: &'a mut HashSet<String>,
        previous: Previous,
    ) -> FormatFuture<'a, Result<Advanced>> {
        Box::pin(advance(reader, stored, read, previous))
    }

    fn hydrate_continuation<'a>(
        &'a self,
        reader: PayloadReader<'a>,
        continuation: Value,
    ) -> FormatFuture<'a, Result<Option<Value>>> {
        Box::pin(async move {
            let history = context::continuation_history(reader, continuation).await?;
            Ok(Some(serde_json::json!({ "history": history })))
        })
    }

    fn encode(&self, checkpoint: Value) -> Result<EncodedPayload> {
        let snapshot: SessionSnapshot =
            serde_json::from_value(checkpoint).map_err(Error::InvalidPayload)?;
        Ok(context::prepare_snapshot(snapshot, &HashSet::new())?.payload)
    }

    fn session_checkpoint(
        &self,
        record: &SessionRecord,
        checkpoint: Value,
    ) -> Result<SessionCheckpoint> {
        let snapshot: SessionSnapshot =
            serde_json::from_value(checkpoint).map_err(Error::InvalidPayload)?;
        SessionCheckpoint::codex(
            record.session_id.clone(),
            record.lineage.clone(),
            record.model.default_thinking(),
            snapshot,
        )
        .map_err(|error| Error::InvalidState(error.to_string()))
    }

    fn thinking(&self, record: &SessionRecord, checkpoint: &Value) -> Result<Thinking> {
        match checkpoint
            .get("thinking")
            .filter(|thinking| !thinking.is_null())
        {
            Some(thinking) => Thinking::deserialize(thinking).map_err(Error::InvalidPayload),
            None => Ok(record.model.default_thinking()),
        }
    }

    fn transcript(&self, checkpoint: &Value, _turns: &[AdmittedTurn<'_>]) -> Vec<TranscriptItem> {
        let mut items = Vec::new();
        for item in checkpoint["history"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("message") => {
                    let role = item["role"].as_str();
                    for content in item["content"].as_array().into_iter().flatten() {
                        let Some(text) = content["text"].as_str().filter(|text| !text.is_empty())
                        else {
                            continue;
                        };
                        match (role, content["type"].as_str()) {
                            (Some("user"), Some("input_text")) if !is_injected_context(text) => {
                                items.push(TranscriptItem::User(text.into()));
                            }
                            (Some("assistant"), _) => {
                                items.push(TranscriptItem::Assistant(text.into()));
                            }
                            _ => {}
                        }
                    }
                }
                Some("reasoning") => {
                    for summary in item["summary"].as_array().into_iter().flatten() {
                        if let Some(text) = summary["text"].as_str().filter(|text| !text.is_empty())
                        {
                            items.push(TranscriptItem::Reasoning(text.into()));
                        }
                    }
                }
                Some("function_call") => items.push(TranscriptItem::Tool {
                    call_id: item["call_id"].as_str().unwrap_or_default().into(),
                    name: item["name"].as_str().unwrap_or_default().into(),
                    arguments: item["arguments"].as_str().unwrap_or_default().into(),
                    parent_call_id: None,
                }),
                Some("custom_tool_call") => items.push(TranscriptItem::Tool {
                    call_id: item["call_id"].as_str().unwrap_or_default().into(),
                    name: item["name"].as_str().unwrap_or_default().into(),
                    arguments: item["input"].as_str().unwrap_or_default().into(),
                    parent_call_id: None,
                }),
                Some("function_call_output" | "custom_tool_call_output") => {
                    let output = item["output"]
                        .as_str()
                        .map_or_else(|| item["output"].to_string(), str::to_owned);
                    let outcome = if output.contains("outcome unknown") {
                        ToolOutcome::Unknown
                    } else {
                        ToolOutcome::Completed
                    };
                    items.push(TranscriptItem::tool_result(
                        item["call_id"].as_str().unwrap_or_default(),
                        &output,
                        outcome,
                    ));
                }
                _ => {}
            }
        }
        items
    }
}

async fn hydrate(
    reader: PayloadReader<'_>,
    stored: Value,
    read: &mut HashSet<String>,
) -> Result<Value> {
    serde_json::to_value(load(reader, stored, read).await?).map_err(Error::InvalidPayload)
}

async fn load(
    reader: PayloadReader<'_>,
    stored: Value,
    read: &mut HashSet<String>,
) -> Result<SessionSnapshot> {
    let saved: context::Snapshot = serde_json::from_value(stored).map_err(Error::InvalidPayload)?;
    let (snapshot, keys) = context::load_snapshot_with_keys(reader, saved).await?;
    read.extend(keys);
    Ok(snapshot)
}

async fn advance(
    reader: PayloadReader<'_>,
    stored: Value,
    read: &mut HashSet<String>,
    previous: Previous,
) -> Result<Advanced> {
    let Previous {
        checkpoint: mut value,
        hydrated,
    } = previous;
    let earlier = hydrated.and_then(Hydrated::take::<context::HydratedSnapshot>);
    let values = match value.get_mut("history").map(Value::take) {
        Some(Value::Array(values)) => Some(values),
        _ => None,
    };
    let (Some(earlier), Some(mut values)) = (earlier, values) else {
        let saved: context::Snapshot =
            serde_json::from_value(stored).map_err(Error::InvalidPayload)?;
        let (hydrated, keys) = context::load_hydrated_with_keys(reader, saved).await?;
        read.extend(keys);
        let value = serde_json::to_value(&hydrated.snapshot).map_err(Error::InvalidPayload)?;
        return Ok(Advanced::whole(value, Some(Hydrated::new(hydrated))));
    };
    let converted = if values.len() == earlier.snapshot.history_len() {
        values.len()
    } else {
        0
    };
    let earlier_prefix = value.get_mut("request_prefix").map(Value::take);
    let saved: context::Snapshot = serde_json::from_value(stored).map_err(Error::InvalidPayload)?;
    let advanced = context::advance_snapshot(reader, saved, earlier).await?;
    read.extend(advanced.keys);
    let snapshot = &advanced.hydrated.snapshot;
    let reused = advanced.reused.min(converted);
    values.truncate(reused);
    for item in snapshot.history_from(reused) {
        values.push(serde_json::to_value(item).map_err(Error::InvalidPayload)?);
    }
    let mut value =
        serde_json::to_value(snapshot.without_context()).map_err(Error::InvalidPayload)?;
    let Some(object) = value.as_object_mut() else {
        return Err(Error::InvalidPayload(
            <serde_json::Error as serde::de::Error>::custom("a Codex checkpoint is an object"),
        ));
    };
    *object.entry("history").or_insert(Value::Null) = Value::Array(values);
    if let Some(prefix) = snapshot.request_prefix() {
        let prefix = match earlier_prefix {
            Some(kept) if advanced.prefix_kept && !kept.is_null() => kept,
            _ => serde_json::to_value(prefix).map_err(Error::InvalidPayload)?,
        };
        *object.entry("request_prefix").or_insert(Value::Null) = prefix;
    }
    Ok(Advanced {
        checkpoint: value,
        hydrated: Some(Hydrated::new(advanced.hydrated)),
        whole: false,
        reused: advanced.reused,
        decoded: advanced.decoded,
    })
}

fn is_injected_context(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with("<environment_context")
        || text.starts_with("<user_instructions")
        || text.starts_with("# AGENTS.md")
}
