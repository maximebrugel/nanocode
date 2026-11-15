use super::*;
use crate::format::FormatFuture;

#[inline(never)]
pub(super) fn session<'a>(
    this: &'a SessionStore,
    record: SessionRecord,
) -> FormatFuture<'a, Result<DurableSession>> {
    Box::pin(async move {
        let session = DurableSession::open_shared_described(
            this.store.clone(),
            record.session_id.clone(),
            None,
            Some(record),
        )
        .await?;
        this.publish_opened(session).await
    })
}

#[inline(never)]
pub(super) fn recovery<'a>(
    this: &'a SessionStore,
    journal: nanocode_agent_core::SessionCheckpoint,
) -> FormatFuture<'a, Result<(nanocode_agent_core::SessionCheckpoint, Option<String>)>> {
    Box::pin(async move {
        journal
            .validate()
            .map_err(|error| Error::InvalidState(error.to_string()))?;
        let id = journal.session_id();
        let Some(state) = this.peek_head(id).await? else {
            return Ok((journal, None));
        };
        let checkpoint = this.loaded_checkpoint(id, &state).await?;
        let record = match (state.session(), &checkpoint) {
            (Some(record), _) => record.clone(),
            (None, Some((checkpoint, ..))) => this.infer_record(id, checkpoint)?,
            (None, None) => return Ok((journal, None)),
        };
        if record.family() != journal.family() || record.lineage != *journal.lineage() {
            return Err(Error::InvalidState(
                "child journal identity differs from its durable session".into(),
            ));
        }
        let provider = is_listed(&state).then(|| record.provider.clone()).flatten();
        let checkpoint = match checkpoint {
            Some((checkpoint, ..)) => session_checkpoint(&this.formats, &record, &checkpoint)?,
            None => journal,
        };
        Ok((checkpoint, provider))
    })
}

#[inline(never)]
pub(super) fn resume<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<DurableSession>> {
    Box::pin(async move {
        let record = this.resume_record(id, None).await?;
        let session =
            DurableSession::open_shared_described(this.store.clone(), id.to_owned(), None, record)
                .await?;
        this.publish_opened(session).await
    })
}

#[inline(never)]
pub(super) fn prepare_resume<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<DurableSession>> {
    Box::pin(async move { this.prepare_resume_with(id, None).await })
}

#[inline(never)]
pub(super) fn prepare_resume_loaded<'a>(
    this: &'a SessionStore,
    stored: &'a StoredSession,
) -> FormatFuture<'a, Result<DurableSession>> {
    Box::pin(async move {
        if !stored.loaded.store.is(&this.store) {
            return Err(Error::InvalidState(
                "the stored session was loaded from a different session store".to_owned(),
            ));
        }
        let id = &stored.summary.record.session_id;
        if *id != stored.loaded.state_id {
            return Err(Error::InvalidState(
                "the stored session identity changed after loading".to_owned(),
            ));
        }
        let state = if stored.loaded.record_inferred {
            let inferred = match (&stored.loaded.checkpoint_key, &stored.checkpoint) {
                (Some(key), Some(checkpoint)) => Some((key.as_str(), checkpoint)),
                _ => None,
            };
            this.prepare_resume_with(id, inferred).await?
        } else {
            DurableSession::prepare_shared_described(this.store.clone(), id.clone(), None, None)
                .await?
        };
        if let (Some(key), Some(checkpoint)) = (&stored.loaded.checkpoint_key, &stored.checkpoint) {
            state.preload_checkpoint(
                key.clone(),
                checkpoint.clone(),
                stored.loaded.checkpoint_payload.clone(),
                stored.loaded.hydrated.clone(),
            );
        }
        Ok(state)
    })
}

#[inline(never)]
pub(super) fn branch<'a>(
    this: &'a SessionStore,
    id: &'a str,
    at: BranchPoint,
    workspace: Option<PathBuf>,
) -> FormatFuture<'a, Result<SessionSummary>> {
    Box::pin(async move {
        this.branch_with(id, at, workspace, |selected, _| Ok(selected))
            .await
    })
}
