use super::*;
use crate::runtime::RegistryFuture;

#[inline(never)]
pub(super) fn interrupt<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
) -> RegistryFuture<'a, std::io::Result<Vec<AgentSummary>>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.interrupt(root_session_id, id).await
    })
}

#[inline(never)]
pub(super) fn close<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
) -> RegistryFuture<'a, std::io::Result<Vec<AgentSummary>>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.close(root_session_id, id).await
    })
}

#[inline(never)]
pub(super) fn set_budget<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
    budget: Option<UsdAmount>,
) -> RegistryFuture<'a, std::io::Result<AgentSummary>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.await_restored(root_session_id).await?;
        this.registry.set_budget(root_session_id, id, budget).await
    })
}

#[inline(never)]
pub(super) fn budgets<'a>(
    this: &'a SubagentControl,
    session_id: &'a str,
) -> RegistryFuture<'a, Vec<(AgentId, UsdAmount)>> {
    Box::pin(async move { this.registry.budgets(session_id).await })
}

#[inline(never)]
pub(super) fn agents<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
) -> RegistryFuture<'a, std::io::Result<Vec<AgentDirectoryEntry>>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.directory(root_session_id, true, false).await
    })
}

#[inline(never)]
pub(super) fn agents_page<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    after: Option<AgentId>,
    limit: std::num::NonZeroUsize,
) -> RegistryFuture<'a, std::io::Result<(Vec<AgentDirectoryEntry>, Option<AgentId>)>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry
            .directory_page(root_session_id, true, false, after, Some(limit))
            .await
    })
}

#[inline(never)]
pub(super) fn active<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
) -> RegistryFuture<'a, std::io::Result<Vec<AgentDirectoryEntry>>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.await_restored(root_session_id).await?;
        this.registry
            .listed(root_session_id, true, false, None)
            .await
    })
}

#[inline(never)]
pub(super) fn session_id<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
) -> RegistryFuture<'a, std::io::Result<String>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.child_session_id(root_session_id, id).await
    })
}

#[inline(never)]
pub(super) fn lease_side_question<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
) -> RegistryFuture<'a, std::io::Result<(HarnessHandle, SideQuestion)>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.await_restored(root_session_id).await?;
        let _target = this
            .registry
            .rehydrate(root_session_id, id, MessagePurpose::Coordinate)
            .await?;
        let root = root_session_id.to_owned();
        let (harness, side_lease) = this
            .registry
            .when_messages_flow(root_session_id, move |state| {
                state.harness_in_scope(&root, id).map(|harness| {
                    let lease = harness.lease_side_question();
                    (harness, lease)
                })
            })
            .await??;
        let lease = SideQuestion {
            lease: Some(side_lease),
            registry: Arc::clone(&this.registry),
            root_session_id: root_session_id.to_owned(),
        };
        Ok((harness, lease))
    })
}

#[inline(never)]
pub(super) fn send_user_message<'a>(
    this: &'a SubagentControl,
    root_session_id: &'a str,
    id: AgentId,
    priority: MessagePriority,
    body: String,
) -> RegistryFuture<'a, std::io::Result<MessageReceipt>> {
    Box::pin(async move {
        this.require_root(root_session_id).await?;
        this.registry.await_restored(root_session_id).await?;
        this.registry
            .deliver_message(
                root_session_id,
                None,
                id,
                priority,
                MessagePurpose::Coordinate,
                None,
                body,
                true,
            )
            .await
    })
}
