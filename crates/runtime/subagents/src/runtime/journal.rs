use super::*;

#[inline(never)]
#[allow(clippy::too_many_arguments)]
pub(super) fn write_journal<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
    requests: Vec<JournalRequest>,
    saved: &'a mut Option<String>,
    recorded: &'a mut HashMap<String, Arc<str>>,
    cache: &'a mut durable::JournalCache,
    fenced: bool,
    slot: &'a mut Option<nanocode_agent_core::backend::DeferredJournal>,
    deferrable: &'a mut bool,
    awaiting: &'a mut Vec<oneshot::Sender<std::io::Result<()>>>,
) -> RegistryFuture<'a, (bool, JournalSave)> {
    Box::pin(async move {
        let (restoring, background) = {
            let gate = reg.claims.read(|claims| {
                claims
                    .restored
                    .get(root_session_id)
                    .map(|gate| gate.borrow().clone())
            });
            (
                matches!(
                    gate,
                    Some(RestorationState::Loading | RestorationState::Failed(_))
                ),
                matches!(gate, None | Some(RestorationState::Ready)) && !fenced,
            )
        };
        let mut waiters = Vec::new();
        let mut finals = Vec::new();
        for request in requests {
            match request {
                JournalRequest::Stored(stored) => cache.adopt(*stored),
                JournalRequest::Warmed(agents) => cache.warm(agents),
                JournalRequest::Persist { scope: false, done } if restoring => {
                    let _ = done.send(Err(std::io::Error::other(
                        "subagent task tree is still restoring",
                    )));
                }
                JournalRequest::Persist { scope, done } => waiters.push((scope, done)),
                JournalRequest::Final { write, done } => finals.push((write, done)),
            }
        }
        let Some(store) = reg.store_for(root_session_id) else {
            for done in waiters
                .into_iter()
                .map(|(_, done)| done)
                .chain(finals.into_iter().map(|(_, done)| done))
            {
                let _ = done.send(Err(std::io::Error::other("no subagent store is installed")));
            }
            return (true, JournalSave::Idle);
        };
        let frozen = || {
            Err(std::io::Error::other(
                "subagent journal is frozen for shutdown",
            ))
        };
        let Some(registry) = reg.shared() else {
            return (true, JournalSave::Idle);
        };
        let root = root_session_id.to_owned();
        let mut owned_cache = std::mem::take(cache);
        let defer = *deferrable;
        let (snapshot, finished, returned_waiters, returned_cache, wanted) = reg
            .state
            .call(move |state| {
                let root_session_id = root.as_str();
                let cache = &mut owned_cache;
                let _collect = nanocode_trace::info_span!(
                    "subagent.journal.collect",
                    session.root_id = root_session_id
                )
                .entered();
                let mut wanted = false;
                let (snapshot, finished) = match state.scopes.get_mut(root_session_id) {
                    None => {
                        for (scope, done) in waiters.drain(..) {
                            let _ = done.send(if scope { Ok(()) } else { frozen() });
                        }
                        (None, true)
                    }
                    Some(scope) => {
                        if scope.life.frozen() {
                            let (scoped, unscoped): (Vec<_>, Vec<_>) =
                                waiters.drain(..).partition(|(scope, _)| *scope);
                            waiters = scoped;
                            for (_, done) in unscoped {
                                let _ = done.send(frozen());
                            }
                        }
                        if scope.life.released() {
                            for (_, done) in waiters.drain(..) {
                                let _ = done.send(frozen());
                            }
                        }
                        let write = !waiters.is_empty() || (background && !scope.life.frozen());
                        wanted = write;
                        scope.cool(root_session_id, &registry, cache);
                        (
                            (write && !defer).then(|| {
                                cache.track(
                                    registry
                                        .checkpoint_changes
                                        .update(|changes| changes.remove(root_session_id))
                                        .into_iter()
                                        .flatten(),
                                );
                                cache.track(scope.sessions.take_changed());
                                let checkpoints = registry.checkpoints.get();
                                scope.journal_snapshot(root_session_id, &checkpoints, cache)
                            }),
                            scope.life.frozen(),
                        )
                    }
                };
                (snapshot, finished, waiters, owned_cache, wanted)
            })
            .await;
        let waiters = returned_waiters;
        *cache = returned_cache;
        if defer && wanted {
            let deferred = match slot {
                Some(_) => Ok(true),
                None => match store.save_deferred(root_session_id).await {
                    Ok(Some(open)) => {
                        *slot = Some(open);
                        Ok(true)
                    }
                    Ok(None) => Ok(false),
                    Err(error) => Err(error),
                },
            };
            match deferred {
                Ok(true) => {
                    awaiting.extend(waiters.into_iter().map(|(_, done)| done));
                    let closed = !finals.is_empty();
                    if closed && let Some(open) = slot.take() {
                        let filled = reg
                            .fill_journal(root_session_id, open, saved, recorded, cache)
                            .await;
                        acknowledge_deferred(
                            reg.this.get().cloned().unwrap_or_default(),
                            root_session_id,
                            std::mem::take(awaiting),
                            filled,
                        );
                    }
                    for (write, done) in finals {
                        *saved = None;
                        let _ =
                            done.send(reg.save_write(root_session_id, store.as_ref(), write).await);
                    }
                    return (finished || closed, JournalSave::Saved);
                }
                Ok(false) => {
                    *deferrable = false;
                    let mut requeued = waiters
                        .into_iter()
                        .map(|(scope, done)| JournalRequest::Persist { scope, done })
                        .collect::<Vec<_>>();
                    requeued.extend(
                        finals
                            .into_iter()
                            .map(|(write, done)| JournalRequest::Final { write, done }),
                    );
                    return Box::pin(reg.write_journal(
                        root_session_id,
                        requeued,
                        saved,
                        recorded,
                        cache,
                        fenced,
                        slot,
                        deferrable,
                        awaiting,
                    ))
                    .await;
                }
                Err(error) => {
                    let save = if is_fenced(&error) {
                        JournalSave::Fenced
                    } else {
                        nanocode_trace::warn!(%error, %root_session_id, "could not save subagent journal");
                        JournalSave::Failed
                    };
                    for (_, done) in waiters {
                        let _ = done.send(Err(copy_error(&error)));
                    }
                    let closed = !finals.is_empty();
                    for (write, done) in finals {
                        *saved = None;
                        let _ =
                            done.send(reg.save_write(root_session_id, store.as_ref(), write).await);
                    }
                    return (finished || closed, save);
                }
            }
        }
        let encoded = snapshot.map(|snapshot| {
            snapshot.and_then(|snapshot| {
                let _encode = nanocode_trace::info_span!(
                    "subagent.journal.encode",
                    session.root_id = root_session_id
                )
                .entered();
                snapshot.encode(cache)
            })
        });
        let attempted = encoded.is_some();
        let result = match encoded {
            None => Ok(()),
            Some(Err(error)) => Err(error),
            Some(Ok(encoded)) => {
                let records = &encoded.records;
                if waiters.is_empty() && saved.as_deref() == Some(encoded.payload.as_str()) {
                    reg.acknowledge(root_session_id, &encoded.acknowledged);
                    cache.committed(encoded);
                    Ok(())
                } else {
                    let payload = encoded.payload.clone();
                    let result = store
                        .save(root_session_id, payload.clone(), records.clone())
                        .instrument(nanocode_trace::info_span!(
                            "subagent.journal.save",
                            session.root_id = root_session_id,
                            payload.bytes = payload.len(),
                            record.count = records.len(),
                            record.bytes =
                                records.iter().map(|(_, json)| json.len()).sum::<usize>(),
                            request.count = waiters.len()
                        ))
                        .await;
                    match result {
                        Ok(()) => {
                            reg.acknowledge(root_session_id, &encoded.acknowledged);
                            cache.committed(encoded);
                            if store.records_sessions() {
                                reg.record_children(root_session_id, store.as_ref(), recorded)
                                    .await;
                            }
                            *saved = Some(payload);
                            Ok(())
                        }
                        Err(error) => {
                            *saved = None;
                            Err(error)
                        }
                    }
                }
            }
        };
        if waiters.is_empty()
            && let Err(error) = &result
            && !is_fenced(error)
        {
            nanocode_trace::warn!(%error, %root_session_id, "could not save subagent journal");
        }
        for (_, done) in waiters {
            let _ = done.send(result.as_ref().copied().map_err(copy_error));
        }
        let save = match &result {
            Ok(()) if attempted => JournalSave::Saved,
            Ok(()) => JournalSave::Idle,
            Err(error) if is_fenced(error) => JournalSave::Fenced,
            Err(_) => JournalSave::Failed,
        };
        let closed = !finals.is_empty();
        if closed && let Some(open) = slot.take() {
            let filled = reg
                .fill_journal(root_session_id, open, saved, recorded, cache)
                .await;
            acknowledge_deferred(
                reg.this.get().cloned().unwrap_or_default(),
                root_session_id,
                Vec::new(),
                filled,
            );
        }
        for (write, done) in finals {
            *saved = None;
            let _ = done.send(reg.save_write(root_session_id, store.as_ref(), write).await);
        }
        (finished || closed, save)
    })
}

#[inline(never)]
pub(super) fn journal_snapshot<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
    id: AgentId,
) -> RegistryFuture<'a, std::io::Result<SessionCheckpoint>> {
    Box::pin(async move {
        let (key, pending) = reg
            .checkpoints
            .read(|checkpoints| {
                checkpoints
                    .get(&(root_session_id.to_owned(), id))
                    .map(|checkpoint| (Arc::clone(&checkpoint.key), checkpoint.held().cloned()))
            })
            .ok_or_else(|| {
                std::io::Error::other(format!("subagent {id} has no journaled checkpoint"))
            })?;
        match pending {
            Some(json) => durable::decode_checkpoint(&json, &key),
            None => {
                let store = reg
                    .store_for(root_session_id)
                    .ok_or_else(|| std::io::Error::other("no subagent store is installed"))?;
                store
                    .load_record(root_session_id, &key)
                    .await
                    .and_then(|json| durable::decode_checkpoint(&json, &key))
                    .map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!("subagent {id}'s checkpoint {key} is unreadable: {error}"),
                        )
                    })
            }
        }
    })
}

#[inline(never)]
pub(super) fn fill_with<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
    fill: nanocode_agent_core::backend::JournalFill,
    saved: &'a mut Option<String>,
    recorded: &'a mut HashMap<String, Arc<str>>,
    cache: &'a mut durable::JournalCache,
) -> RegistryFuture<'a, Option<Vec<(AgentId, Arc<str>)>>> {
    Box::pin(async move {
        let (Some(store), Some(registry)) = (reg.store_for(root_session_id), reg.shared()) else {
            return None;
        };
        let root = root_session_id.to_owned();
        let mut owned_cache = std::mem::take(cache);
        let (snapshot, returned_cache) = reg
            .state
            .call(move |state| {
                let root_session_id = root.as_str();
                let cache = &mut owned_cache;
                let snapshot = state.scopes.get_mut(root_session_id).map(|scope| {
                    scope.cool(root_session_id, &registry, cache);
                    cache.track(
                        registry
                            .checkpoint_changes
                            .update(|changes| changes.remove(root_session_id))
                            .into_iter()
                            .flatten(),
                    );
                    cache.track(scope.sessions.take_changed());
                    let checkpoints = registry.checkpoints.get();
                    scope.journal_snapshot(root_session_id, &checkpoints, cache)
                });
                (snapshot, owned_cache)
            })
            .await;
        *cache = returned_cache;
        let encoded = snapshot.map(|snapshot| {
            snapshot.and_then(|snapshot| {
                let _encode = nanocode_trace::info_span!(
                    "subagent.journal.encode",
                    session.root_id = root_session_id
                )
                .entered();
                snapshot.encode(cache)
            })
        });
        let Some(Ok(encoded)) = encoded else {
            if let Some(Err(error)) = encoded {
                nanocode_trace::warn!(%error, %root_session_id, "could not encode subagent journal");
            }
            return None;
        };
        let payload = encoded.payload.clone();
        if fill
            .send((payload.clone(), encoded.records.clone()))
            .is_err()
        {
            return None;
        }
        let saving = encoded.acknowledged.clone();
        reg.settle(root_session_id, &saving, |payload| match payload {
            durable::Payload::Unsaved(json) => durable::Payload::Saving(Arc::clone(json)),
            other => other.clone(),
        });
        cache.committed(encoded);
        if store.records_sessions() {
            reg.record_children(root_session_id, store.as_ref(), recorded)
                .await;
        }
        *saved = Some(payload);
        Some(saving)
    })
}

#[inline(never)]
pub(super) fn record_children<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
    store: &'a dyn SubagentStore,
    recorded: &'a mut HashMap<String, Arc<str>>,
) -> RegistryFuture<'a, ()> {
    Box::pin(async move {
        for (key, checkpoint) in reg.child_checkpoints(root_session_id).await {
            let session_id = checkpoint.session_id().to_owned();
            if recorded.get(&session_id) == Some(&key) {
                continue;
            }
            match store.record_session(root_session_id, checkpoint).await {
                Ok(()) => {
                    reg.release_recorded(root_session_id, &key);
                    recorded.insert(session_id, key);
                }
                Err(error) => nanocode_trace::warn!(
                    %error, %root_session_id, %session_id,
                    "could not record subagent session"
                ),
            }
        }
    })
}

#[inline(never)]
pub(super) fn persist_durably<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
) -> RegistryFuture<'a, std::io::Result<()>> {
    Box::pin(async move {
        reg.request_journal(root_session_id, false)
            .instrument(nanocode_trace::info_span!(
                "subagent.journal.persist",
                session.root_id = root_session_id
            ))
            .await
    })
}

#[inline(never)]
pub(super) fn save_scope<'a>(
    reg: &'a Registry,
    root_session_id: &'a str,
) -> RegistryFuture<'a, std::io::Result<()>> {
    Box::pin(async move {
        reg.request_journal(root_session_id, true)
            .instrument(nanocode_trace::info_span!(
                "subagent.journal.snapshot",
                session.root_id = root_session_id
            ))
            .await
    })
}
