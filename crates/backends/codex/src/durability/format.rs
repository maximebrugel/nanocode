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
            Ok((summary(&hydrated.snapshot)?, Some(Hydrated::new(hydrated))))
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
            transcript_item(item, &mut items);
        }
        items
    }

    fn typed_session_checkpoint(
        &self,
        record: &SessionRecord,
        _checkpoint: &Value,
        hydrated: &Hydrated,
    ) -> Option<Result<SessionCheckpoint>> {
        let hydrated = hydrated.get::<context::HydratedSnapshot>()?;
        Some(
            SessionCheckpoint::codex(
                record.session_id.clone(),
                record.lineage.clone(),
                record.model.default_thinking(),
                hydrated.snapshot.clone(),
            )
            .map_err(|error| Error::InvalidState(error.to_string())),
        )
    }

    fn typed_transcript(
        &self,
        _checkpoint: &Value,
        hydrated: &Hydrated,
        _turns: &[AdmittedTurn<'_>],
    ) -> Option<Vec<TranscriptItem>> {
        let hydrated = hydrated.get::<context::HydratedSnapshot>()?;
        let mut items = Vec::new();
        for item in hydrated.snapshot.history_from(0) {
            let item = serde_json::to_value(item).ok()?;
            transcript_item(&item, &mut items);
        }
        Some(items)
    }
}

fn summary(snapshot: &SessionSnapshot) -> Result<Value> {
    serde_json::to_value(snapshot.without_context()).map_err(Error::InvalidPayload)
}

fn transcript_item(item: &Value, items: &mut Vec<TranscriptItem>) {
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
    let saved: context::Snapshot = serde_json::from_value(stored).map_err(Error::InvalidPayload)?;
    let Some(earlier) = previous
        .hydrated
        .and_then(Hydrated::take::<context::HydratedSnapshot>)
    else {
        let (hydrated, keys) = context::load_hydrated_with_keys(reader, saved).await?;
        read.extend(keys);
        let value = summary(&hydrated.snapshot)?;
        return Ok(Advanced::whole(value, Some(Hydrated::new(hydrated))));
    };
    let advanced = context::advance_snapshot(reader, saved, earlier).await?;
    read.extend(advanced.keys);
    Ok(Advanced {
        checkpoint: summary(&advanced.hydrated.snapshot)?,
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
