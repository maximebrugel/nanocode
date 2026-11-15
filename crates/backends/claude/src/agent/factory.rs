use super::*;

pub(super) fn spawn(
    this: &ClaudeNativeFactory,
    parent: AgentHandle,
    options: SpawnOptions,
    host_context: Option<Arc<str>>,
) -> BackendFuture<Result<(Agent, AgentEvents)>> {
    let state = this.owner();
    let recipe = this.recipe();
    let journal_backed = parent.child_journal().is_some();
    Box::pin(async move {
        let state = state?;
        let native_model = state.model();
        let host_context = host_context.or_else(|| state.host_context.clone());
        let mut recipe = recipe;
        let lineage =
            Lineage::child_of(&state.lineage, state.session_id.as_str(), Origin::Subagent);
        let child_id = options
            .session_id
            .clone()
            .unwrap_or_else(|| nanocode_platform::uuid_v7().to_string());
        state.durable_child(&mut recipe, &child_id, &lineage, "spawn", journal_backed)?;
        recipe.session_id = Some(child_id);
        recipe.lineage = Some(lineage);

        if options.selected_harness_model().is_none()
            && options
                .selected_family()
                .is_none_or(|family| family == HarnessFamily::Claude)
        {
            let mut recipe = recipe;
            recipe.claude.model = native_model;
            recipe.effort = state.effort();
            recipe.thinking_mode = state.thinking_mode.get();
            recipe.fast_mode = state.fast_mode.load(Ordering::SeqCst);
            if let Some(thinking) = options.selected_thinking() {
                recipe = recipe.thinking(thinking)?;
            }
            state.initialize_child_workspace(&mut recipe)?;
            state.share_system_context(&mut recipe);
            return Agent::persist_created(recipe.host_context(host_context).build()).await;
        }

        let model = claude_selection(&state.model());
        let thinking = state.selected_thinking(&model);
        let options = options.resolve(model, thinking)?;
        let selected = options.selected_harness_model().expect("resolved model");
        if selected.family() != HarnessFamily::Claude {
            return Err(NanocodeError::UnsupportedCapability {
                capability: "cross_family_spawn",
            });
        }
        let mut recipe = recipe;
        recipe.claude.model = selected.as_str().into();
        if let Some(system) = recipe
            .model_systems
            .iter()
            .find(|(model, _)| model == selected.as_str())
            .map(|(_, instructions)| instructions.to_string())
        {
            recipe = recipe.system(system);
        }

        recipe.fast_mode = state.fast_mode.load(Ordering::SeqCst) && selected.supports_fast_mode();
        state.initialize_child_workspace(&mut recipe)?;
        state.share_system_context(&mut recipe);
        let child = recipe
            .thinking(options.selected_thinking().expect("resolved thinking"))?
            .host_context(host_context)
            .build();
        Agent::persist_created(child).await
    })
}

pub(super) fn fork(
    this: &ClaudeNativeFactory,
    _parent: AgentHandle,
    request: ForkRequest,
) -> BackendFuture<Result<(Agent, AgentEvents)>> {
    let state = this.owner();
    let mut recipe = this.recipe();
    Box::pin(async move {
        let state = state?;
        let (point, origin) = request.into_parts();
        if !matches!(origin, Origin::Fork | Origin::SideConversation) {
            return Err(NanocodeError::InvalidRequest(
                "a fork must be a fork or a side conversation".into(),
            ));
        }
        let mut snapshot = state.fork_point(point).await?;
        if !snapshot.has_conversation() {
            return Err(NanocodeError::ForkBeforeCompletedTurn);
        }

        snapshot.tasks = None;
        snapshot.conversation.pending_continuation = false;
        snapshot.conversation.previous_message_id = None;
        snapshot.conversation.container = None;
        snapshot.conversation.lifecycle_started = false;
        snapshot.conversation.fallback_latch = FallbackLatch::Main;
        if origin == Origin::SideConversation {
            snapshot.conversation.auto_compaction_suppressed = true;
        }
        recipe.claude.model = state.model();
        recipe.effort = state.effort();
        recipe.thinking_mode = state.thinking_mode.get();
        recipe.fast_mode = state.fast_mode.load(Ordering::SeqCst);
        let child_id = nanocode_platform::uuid_v7().to_string();
        let lineage = Lineage::child_of(&state.lineage, state.session_id.as_str(), origin);
        snapshot.lineage = Some(lineage.clone());
        snapshot.conversation_id = Some(state.conversation_id.clone());

        if let Some(parent) = &state.policy {
            let child = nanocode_agent_core::SessionInfo::new(
                child_id.clone(),
                HarnessFamily::Claude,
                lineage.clone(),
            );

            let policy = parent
                .branch(&child)?
                .ok_or(NanocodeError::ExecutionPolicyBranchUnsupported { operation: "fork" })?;
            {
                if policy.state_id() != child_id {
                    return Err(NanocodeError::InvalidRequest(
                        "durable branch state ID must equal the child session ID".into(),
                    ));
                }
                policy
                    .checkpoint(serde_json::to_value(&snapshot).map_err(provider_error)?)
                    .await?;
                recipe.policy = Some(policy);
            }
        }
        recipe.session_id = Some(child_id);
        recipe.restored = Some(snapshot);
        recipe.lineage = Some(lineage);
        recipe.conversation_id = Some(state.conversation_id.clone());
        state.initialize_child_workspace(&mut recipe)?;
        state.share_system_context(&mut recipe);
        Agent::persist_created(recipe.build()).await
    })
}

pub(super) fn restore(
    this: &ClaudeNativeFactory,
    parent: AgentHandle,
    checkpoint: SessionCheckpoint,
    host_context: Option<Arc<str>>,
) -> BackendFuture<Result<(Agent, AgentEvents)>> {
    let available = this.owner();
    let recipe = this.recipe();
    let journal_backed = parent.child_journal().is_some();
    Box::pin(async move {
        let state = available?;
        let mut recipe = recipe.resume(checkpoint)?;
        if let Some(kept) = recipe
            .restored
            .as_mut()
            .and_then(|restored| restored.context.as_mut())
        {
            state.lend_instructions(kept);
        }

        if let (Some(child_id), Some(lineage)) = (recipe.session_id.clone(), recipe.lineage.clone())
        {
            state.durable_child(&mut recipe, &child_id, &lineage, "restore", journal_backed)?;
        }
        state.initialize_child_workspace(&mut recipe)?;
        state.share_system_context(&mut recipe);
        Agent::persist_created(recipe.host_context(host_context).build()).await
    })
}
