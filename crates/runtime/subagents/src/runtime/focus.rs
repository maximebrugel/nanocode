use super::{AgentDirectoryEntry, AgentSummary, MessageReceipt, Registry, SubagentControl};
use crate::{
    harness::{HarnessHandle, SideQuestionLease},
    model::{AgentId, MessagePriority, MessagePurpose},
    platform,
};
use nanocode_agent_core::{NanocodeError, SideAnswer, UsdAmount};
use std::sync::Arc;

mod control;

struct SideQuestion {
    lease: Option<SideQuestionLease>,
    registry: Arc<Registry>,
    root_session_id: String,
}

impl Drop for SideQuestion {
    fn drop(&mut self) {
        drop(self.lease.take());
        let registry = Arc::clone(&self.registry);
        let root_session_id = std::mem::take(&mut self.root_session_id);
        drop(platform::spawn(async move {
            registry.enforce_resident_limit(&root_session_id).await;
        }));
    }
}

impl SubagentControl {
    pub async fn send_user_message(
        &self,
        root_session_id: &str,
        id: AgentId,
        priority: MessagePriority,
        body: impl Into<String>,
    ) -> std::io::Result<MessageReceipt> {
        control::send_user_message(self, root_session_id, id, priority, body.into()).await
    }

    pub async fn interrupt(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> std::io::Result<Vec<AgentSummary>> {
        control::interrupt(self, root_session_id, id).await
    }

    pub async fn close(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> std::io::Result<Vec<AgentSummary>> {
        control::close(self, root_session_id, id).await
    }

    pub async fn set_budget(
        &self,
        root_session_id: &str,
        id: AgentId,
        budget: Option<UsdAmount>,
    ) -> std::io::Result<AgentSummary> {
        control::set_budget(self, root_session_id, id, budget).await
    }

    pub async fn budgets(&self, session_id: &str) -> Vec<(AgentId, UsdAmount)> {
        control::budgets(self, session_id).await
    }

    pub async fn agents(&self, root_session_id: &str) -> std::io::Result<Vec<AgentDirectoryEntry>> {
        control::agents(self, root_session_id).await
    }

    pub async fn agents_page(
        &self,
        root_session_id: &str,
        after: Option<AgentId>,
        limit: std::num::NonZeroUsize,
    ) -> std::io::Result<(Vec<AgentDirectoryEntry>, Option<AgentId>)> {
        control::agents_page(self, root_session_id, after, limit).await
    }

    pub(super) async fn active(
        &self,
        root_session_id: &str,
    ) -> std::io::Result<Vec<AgentDirectoryEntry>> {
        control::active(self, root_session_id).await
    }

    pub async fn session_id(&self, root_session_id: &str, id: AgentId) -> std::io::Result<String> {
        control::session_id(self, root_session_id, id).await
    }

    pub async fn side_question(
        &self,
        root_session_id: &str,
        id: AgentId,
        question: impl Into<String>,
    ) -> nanocode_agent_core::Result<SideAnswer> {
        let invalid = |error: std::io::Error| NanocodeError::InvalidRequest(error.to_string());
        let (harness, _side_question) = self
            .lease_side_question(root_session_id, id)
            .await
            .map_err(invalid)?;
        let agent = harness.agent().await.map_err(invalid)?;
        agent.side_question(question).await
    }

    async fn lease_side_question(
        &self,
        root_session_id: &str,
        id: AgentId,
    ) -> std::io::Result<(HarnessHandle, SideQuestion)> {
        control::lease_side_question(self, root_session_id, id).await
    }

    async fn require_root(&self, root_session_id: &str) -> std::io::Result<()> {
        if self.registry.is_root_session(root_session_id).await {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "{root_session_id} is not a root session with subagents"
            )))
        }
    }
}
