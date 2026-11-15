use super::*;
use serde_json::json;

pub(super) fn append_developer_notice<'a>(
    state: &'a State,
    text: String,
    prefix: Option<&'a str>,
) -> AgentFuture<'a, Result<Option<Snapshot>>> {
    Box::pin(async move {
        loop {
            let admission = state.admission.enter().await;
            let notified = state.persistence_ready.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();
            let update = state.persistence_update.enter().await;
            if state.life.stopped() {
                return Err(NanocodeError::AgentStopped);
            }
            let mut snapshot = state.latest_boundary().await?;
            if prefix
                .and_then(|prefix| latest_developer_notice(&snapshot, prefix))
                .is_some_and(|latest| latest.ends_with(&text))
            {
                return Ok(None);
            }
            snapshot
                .developer_notices
                .push((nanocode_platform::uuid_v7().to_string(), text.clone()));
            if let Some(policy) = &state.policy {
                let operation = state
                    .steering
                    .acquire()
                    .await
                    .iter()
                    .filter_map(|(key, turn)| turn.operation.as_ref().map(|id| (key.0, id.clone())))
                    .min_by_key(|(key, _)| *key)
                    .map(|(_, id)| id);
                if let Some(operation) = operation {
                    let notice = snapshot
                        .developer_notices
                        .last()
                        .cloned()
                        .ok_or_else(|| unsupported("Claude developer notice is missing"))?;
                    let step = format!("developer-notice-{}", notice.0);
                    policy
                        .begin_step(
                            operation.clone(),
                            step.clone(),
                            "developer_notice".into(),
                            json!(notice),
                        )
                        .await?;
                    policy.complete_step(operation, step, Value::Null).await?;
                } else if let Err(error) = policy
                    .checkpoint(serde_json::to_value(&snapshot).map_err(provider_error)?)
                    .await
                {
                    if error.execution_policy_disposition()
                        == Some(nanocode_agent_core::ExecutionPolicyDisposition::Retry)
                    {
                        drop(update);
                        drop(admission);
                        notified.await;
                        continue;
                    }
                    return Err(error);
                }
            }
            state
                .developer_notices
                .set(snapshot.developer_notices.clone());
            return Ok(Some(snapshot));
        }
    })
}
