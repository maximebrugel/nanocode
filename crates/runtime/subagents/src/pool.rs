use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use nanocode_agent_core::AgentHandle;
use nanocode_board::sim::{Mutation, mutated};
use nanocode_dispatch::{Agent, AgentPhase};
use nanocode_dispatcher::{Pool, PoolError, PoolFuture, Start};
use nanocode_sync::Atom;
use serde_json::Value;

use crate::{AgentId, AgentStatus, AgentTask, Registry, start_agent};

pub type PoolClock = Arc<dyn Fn() -> u64 + Send + Sync>;

#[derive(Clone)]
struct Placed {
    id: AgentId,
    agent: Agent,
    idle_since_ms: Option<u64>,
    ended: Option<Ended>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    Completed(String),
    Failed(String),
    Stopped,
}

impl Placed {
    fn listed(&self) -> Agent {
        let mut agent = self.agent.clone();
        if mutated(Mutation::PoolForgetsIntent) {
            agent.intent = None;
        }
        agent
    }
}

pub struct SubagentPool {
    parent: AgentHandle,
    registry: Weak<Registry>,
    session: String,
    now_ms: PoolClock,
    placed: Atom<BTreeMap<String, Placed>>,
}

impl SubagentPool {
    pub fn new(
        parent: AgentHandle,
        registry: Arc<Registry>,
        session: impl Into<String>,
        now_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> Self {
        Self::clocked(parent, &registry, session, Arc::new(now_ms))
    }

    pub fn clocked(
        parent: AgentHandle,
        registry: &Arc<Registry>,
        session: impl Into<String>,
        now_ms: PoolClock,
    ) -> Self {
        Self {
            parent,
            registry: Arc::downgrade(registry),
            session: session.into(),
            now_ms,
            placed: Atom::new(BTreeMap::new()),
        }
    }

    pub fn ended(&self, agent: &str) -> Option<Ended> {
        self.placed
            .read(|placed| placed.get(agent).and_then(|placed| placed.ended.clone()))
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    fn registry(&self) -> Result<Arc<Registry>, PoolError> {
        self.registry
            .upgrade()
            .ok_or_else(|| PoolError::Unknown("the subagent registry stopped".to_owned()))
    }

    pub fn agent_id(&self, agent: &str) -> Option<AgentId> {
        self.placed
            .read(|placed| placed.get(agent).map(|placed| placed.id))
    }

    async fn place(&self, start: Start) -> Result<(), PoolError> {
        if self.placed.read(|placed| placed.contains_key(&start.agent)) {
            return Ok(());
        }
        let registry = self.registry()?;
        let task = AgentTask {
            role: start.spawn.domain.role.clone(),
            task: start.brief.text.clone(),
            output_schema: Value::Object(serde_json::Map::new()),
        };
        let report = start_agent(&self.parent, &registry, &self.session, task)
            .await
            .map_err(|error| PoolError::Placement(error.to_string()))?;
        let agent = Agent {
            id: start.agent.clone(),
            domain: start.spawn.domain,
            cost_nano_usd: start.spawn.cost_nano_usd,
            intent: Some(start.spawn.intent),
            phase: AgentPhase::Starting,
        };
        let placed = Placed {
            id: report.agent_id,
            agent,
            idle_since_ms: None,
            ended: None,
        };
        self.placed.update(|all| {
            all.insert(start.agent.clone(), placed.clone());
        });
        Ok(())
    }

    async fn list(&self) -> Vec<Agent> {
        let listed = match self.registry() {
            Ok(registry) => registry.directory(&self.session, true, false).await.ok(),
            Err(_) => None,
        };
        let Some(listed) = listed else {
            return self
                .placed
                .read(|placed| placed.values().map(Placed::listed).collect());
        };
        let now = (self.now_ms)();
        self.placed.update(|placed| {
            placed.retain(|_, placed| {
                let Some(entry) = listed.iter().find(|entry| entry.agent_id == placed.id) else {
                    return false;
                };
                placed.agent.phase = match &entry.status {
                    AgentStatus::Pending => AgentPhase::Starting,
                    AgentStatus::Running => {
                        placed.idle_since_ms = None;
                        placed.ended = None;
                        AgentPhase::Busy
                    }
                    AgentStatus::Closing => AgentPhase::Retiring,
                    AgentStatus::Closed => return false,
                    AgentStatus::Completed { output } => {
                        placed.ended = Some(Ended::Completed(output.to_string()));
                        idle(placed, now)
                    }
                    AgentStatus::Failed { error } => {
                        placed.ended = Some(Ended::Failed(error.clone()));
                        idle(placed, now)
                    }
                    AgentStatus::Interrupted => {
                        placed.ended = Some(Ended::Stopped);
                        idle(placed, now)
                    }
                };
                true
            });
            placed.values().map(Placed::listed).collect()
        })
    }

    async fn close(&self, agent: &str) -> Result<(), PoolError> {
        let Some(id) = self.agent_id(agent) else {
            return Ok(());
        };
        if !mutated(Mutation::PoolRetireKeepsAgent) {
            self.registry()?
                .close(&self.session, id)
                .await
                .map_err(|error| PoolError::Unknown(error.to_string()))?;
        }
        self.placed.update(|placed| {
            placed.remove(agent);
        });
        Ok(())
    }
}

fn idle(placed: &mut Placed, now: u64) -> AgentPhase {
    AgentPhase::Idle {
        since_ms: *placed.idle_since_ms.get_or_insert(now),
    }
}

impl Pool for SubagentPool {
    fn agents(&self) -> PoolFuture<'_, Vec<Agent>> {
        Box::pin(self.list())
    }

    fn start(&self, start: Start) -> PoolFuture<'_, Result<(), PoolError>> {
        Box::pin(self.place(start))
    }

    fn retire<'a>(&'a self, agent: &'a str) -> PoolFuture<'a, Result<(), PoolError>> {
        Box::pin(self.close(agent))
    }
}
