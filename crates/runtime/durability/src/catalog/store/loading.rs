use super::*;
use crate::format::FormatFuture;

#[inline(never)]
pub(super) fn load<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<StoredSession>> {
    Box::pin(async move {
        let state = this.require(id).await?;
        let (checkpoint, checkpoint_payload, hydrated) =
            match this.loaded_checkpoint(id, &state).await? {
                Some((checkpoint, payload, records, hydrated)) => (
                    Some(checkpoint),
                    Some((payload, Arc::new(records))),
                    hydrated,
                ),
                None => (None, None, None),
            };
        let record = match (state.session(), &checkpoint) {
            (Some(record), _) => record.clone(),
            (None, Some(checkpoint)) => this.infer_record(id, checkpoint)?,
            (None, None) => {
                return Err(Error::SessionNotFound {
                    session_id: id.to_owned(),
                });
            }
        };
        let turns = this.state_turns(id, &state).await?;
        let admitted = admitted_turns(&turns, &state);
        let progress = this.progress(id, &state, record.family()).await?;
        let transcript = match (&progress, &checkpoint) {
            (Some((_, view, steers)), _) => {
                let mut transcript = this.transcript(view, record.family(), &admitted)?;
                transcript.extend(steers.iter().cloned().map(TranscriptItem::User));
                transcript
            }
            (None, Some(checkpoint)) => this.transcript(checkpoint, record.family(), &admitted)?,
            (None, None) => Vec::new(),
        };
        let preview = turns
            .iter()
            .find_map(|turn| turn.preview.clone())
            .or_else(|| {
                transcript.iter().find_map(|item| match item {
                    TranscriptItem::User(text) => Some(text.clone()),
                    _ => None,
                })
            });
        Ok(StoredSession {
            summary: SessionSummary { record, preview },
            transcript,
            turns,
            in_progress: progress.map(|(operation, _, _)| operation),
            checkpoint,
            formats: this.formats.clone(),
            loaded: Loaded {
                store: this.store.downgrade(),
                state_id: id.to_owned(),
                checkpoint_key: state
                    .latest_checkpoint()
                    .map(|payload| payload.key().to_owned()),
                checkpoint_payload,
                hydrated,
                record_inferred: state.session().is_none(),
            },
        })
    })
}

#[inline(never)]
pub(super) fn turns<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Vec<StoredTurn>>> {
    Box::pin(async move {
        let state = this.require(id).await?;
        this.state_turns(id, &state).await
    })
}

#[inline(never)]
pub(super) fn pending_turns<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Vec<StoredTurn>>> {
    Box::pin(async move {
        let state = this.require_head(id).await?;
        this.stored_turns(id, state.pending_operations(), false)
            .await
    })
}

#[inline(never)]
pub(super) fn resumable<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Option<Resumable>>> {
    Box::pin(async move {
        let Some(state) = this.peek_head(id).await? else {
            return Ok(None);
        };
        let Some(record) = state.session().cloned() else {
            return match this.load(id).await {
                Ok(stored) => Ok(Some(Resumable {
                    record: stored.summary.record,
                    pending: stored
                        .turns
                        .into_iter()
                        .filter(|turn| turn.status == TurnStatus::Pending)
                        .collect(),
                    checkpointed: stored.checkpoint.is_some(),
                })),
                Err(Error::SessionNotFound { .. }) => Ok(None),
                Err(error) => Err(error),
            };
        };
        Ok(Some(Resumable {
            pending: this
                .stored_turns(id, state.pending_operations(), false)
                .await?,
            checkpointed: state.latest_checkpoint().is_some(),
            record,
        }))
    })
}

#[inline(never)]
pub(super) fn turn<'a>(
    this: &'a SessionStore,
    id: &'a str,
    request_id: &'a str,
) -> FormatFuture<'a, Result<Option<StoredTurn>>> {
    Box::pin(async move {
        let mut state = this.require_head(id).await?;
        if state.operation(request_id).is_none() {
            state
                .hydrate(&mut this.store.clone(), id, &mut HashSet::new())
                .await?;
        }
        let Some(operation) = state.operation(request_id) else {
            return Ok(None);
        };
        Ok(this
            .stored_turns(id, vec![(request_id, operation)], false)
            .await?
            .pop())
    })
}
