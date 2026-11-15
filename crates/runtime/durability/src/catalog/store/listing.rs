use super::*;
use crate::format::FormatFuture;

#[inline(never)]
pub(super) fn list<'a>(this: &'a SessionStore) -> FormatFuture<'a, Result<Vec<SessionSummary>>> {
    Box::pin(async move { this.list_page(None, usize::MAX, |_| true).await })
}

#[inline(never)]
pub(super) fn summarize<'a>(
    this: &'a SessionStore,
    session_ids: &'a [String],
) -> FormatFuture<'a, Result<()>> {
    Box::pin(async move {
        let mut known = Vec::new();
        for state_id in session_ids.iter().filter(|id| !is_auxiliary_state(id)) {
            let Some((revision, summary)) = this.revised_summary(state_id).await? else {
                continue;
            };
            known.push(KnownSummary {
                state_id: state_id.clone(),
                revision,
                summary: summary.as_ref().map(listed_summary).transpose()?,
            });
        }
        if !known.is_empty() {
            this.store.clone().remember_summaries(known).await?;
        }
        Ok(())
    })
}

#[inline(never)]
pub(super) fn revised_summary<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Option<(u64, Option<SessionSummary>)>>> {
    Box::pin(async move {
        let Some(state) = this.peek(id).await? else {
            return Ok(None);
        };
        let revision = state.revision();
        if !is_listed(&state) {
            return Ok(Some((revision, None)));
        }
        let preview = this.first_prompt(id, &state).await?;
        let record = match state.session() {
            Some(record) => record.clone(),
            None => {
                let Some(checkpoint) = this.native_checkpoint(id, &state).await? else {
                    return Ok(Some((revision, None)));
                };
                this.infer_record(id, &checkpoint)?
            }
        };
        let preview = preview.or_else(|| record.event_preview.clone());
        Ok(Some((revision, Some(SessionSummary { record, preview }))))
    })
}

#[inline(never)]
pub(super) fn summary<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Option<SessionSummary>>> {
    Box::pin(async move {
        Ok(this
            .revised_summary(id)
            .await?
            .and_then(|(_, summary)| summary))
    })
}

#[inline(never)]
pub(super) fn record_and_base<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Option<(SessionRecord, Option<String>)>>> {
    Box::pin(async move {
        let Some(state) = this.peek(id).await? else {
            return Ok(None);
        };
        if !is_listed(&state) {
            return Ok(None);
        }
        let base = state
            .latest_checkpoint()
            .map(|latest| latest.key().to_owned());
        let record = match state.session() {
            Some(record) => record.clone(),
            None => match this.native_checkpoint(id, &state).await? {
                Some(checkpoint) => this.infer_record(id, &checkpoint)?,
                None => return Ok(None),
            },
        };
        Ok(Some((record, base)))
    })
}

#[inline(never)]
pub(super) fn latest_checkpoint_key<'a>(
    this: &'a SessionStore,
    id: &'a str,
) -> FormatFuture<'a, Result<Option<String>>> {
    Box::pin(async move {
        let Some(state) = this.peek_head(id).await? else {
            return Ok(None);
        };
        Ok(is_listed(&state)
            .then(|| {
                state
                    .latest_checkpoint()
                    .map(|latest| latest.key().to_owned())
            })
            .flatten())
    })
}

#[inline(never)]
pub(super) fn summaries<'a>(
    this: &'a SessionStore,
) -> FormatFuture<'a, Result<Vec<SessionSummary>>> {
    Box::pin(async move {
        let ids = this.store.clone().list_states(usize::MAX).await?;
        Ok(nanocode_sync::try_join_in_order(
            ids.iter()
                .filter(|id| !is_auxiliary_state(id))
                .map(|id| this.summary(id)),
        )
        .await?
        .into_iter()
        .flatten()
        .collect())
    })
}

#[inline(never)]
pub(super) fn candidates<'a>(
    this: &'a SessionStore,
    listed: Vec<ListedState>,
    workspace: Option<&'a Path>,
) -> FormatFuture<'a, Result<Vec<Candidate>>> {
    Box::pin(async move {
        let elsewhere =
            |scope: &str| workspace.is_some_and(|workspace| Path::new(scope) != workspace);
        let mut candidates = Vec::with_capacity(listed.len());
        let mut unknown = Vec::new();
        for state in listed {
            if is_auxiliary_state(&state.state_id) {
                continue;
            }
            match state.summary {
                ListedSummary::Listed(summary)
                    if summary.scope.as_deref().is_some_and(elsewhere) => {}
                ListedSummary::Listed(summary) => candidates.push(Candidate::Listed(summary)),
                ListedSummary::Unlisted => {}
                ListedSummary::Unknown => unknown.push(state.state_id),
            }
        }
        let mut known = Vec::new();
        for state_id in unknown {
            let Some((revision, summary)) = this.revised_summary(&state_id).await? else {
                continue;
            };
            known.push(KnownSummary {
                state_id,
                revision,
                summary: summary.as_ref().map(listed_summary).transpose()?,
            });
            candidates.extend(summary.map(Candidate::Read));
        }
        if !known.is_empty() {
            this.store.clone().remember_summaries(known).await?;
        }
        candidates.sort_by_cached_key(|candidate| std::cmp::Reverse(candidate.updated_at_ms()));
        Ok(candidates)
    })
}
