use super::*;
use crate::runtime::RegistryFuture;

#[inline(never)]
pub(super) fn accept_delivery<'a>(
    this: &'a mut Harness,
    mut command: DeliveryCommand,
    priority: MessagePriority,
) -> RegistryFuture<'a, ()> {
    Box::pin(async move {
        if !command.wait_for_commit().await {
            return;
        }
        let steer = if priority == MessagePriority::Urgent && this.active.is_some() {
            match this.registry.upgrade() {
                Some(registry) => {
                    registry
                        .begin_turn_steer(&this.root_session_id, this.id)
                        .await
                }
                None => None,
            }
        } else {
            None
        };
        if let Some(steer) = steer {
            let delegation = this.begin_delegation(command.message.id).await;
            let prompt = format!("{}\n\n{}", command.message.prompt(), completion_reminder());
            let result = this
                .active
                .as_ref()
                .expect("steering requires an active turn")
                .control
                .steer(Prompt::new(prompt).with_instruction_revision(steer.revision()))
                .await;
            if let Some(registry) = this.registry.upgrade() {
                registry
                    .finish_turn_steer(&this.root_session_id, steer, result.is_ok())
                    .await;
            }
            match result {
                Ok(()) => {
                    this.admit(
                        command.message.id,
                        command.response,
                        MessageDisposition::Steered,
                    )
                    .await;
                    return;
                }
                Err(NanocodeError::TurnNotSteerable | NanocodeError::TurnStopped) => {
                    this.rollback_delegation(delegation).await;
                }
                Err(error) => {
                    this.rollback_delegation(delegation).await;
                    this.reject(
                        command,
                        format!("could not urgently message agent {}: {error}", this.id),
                    )
                    .await;
                    return;
                }
            }
        }

        if this.active.is_none()
            && this.pending_deferred.is_empty()
            && this.pending_urgent.is_empty()
            && let Ok(capacity) = this.capacity.reserve()
        {
            let delegation = this.begin_delegation(command.message.id).await;
            let from_peers = from_peers(std::slice::from_ref(&command.message));
            match this
                .start_turn(command.message.prompt(), capacity, from_peers)
                .await
            {
                Ok(Started::Prompt) => {
                    this.admit(
                        command.message.id,
                        command.response,
                        MessageDisposition::Started,
                    )
                    .await;
                }
                Ok(Started::RecoveryOnly) if resumes(&command.message) => {
                    this.rollback_delegation(delegation).await;
                    this.admit(
                        command.message.id,
                        command.response,
                        MessageDisposition::Started,
                    )
                    .await;
                }
                Ok(Started::RecoveryOnly) => {
                    this.rollback_delegation(delegation).await;
                    this.queue_delivery(command, priority).await;
                }
                Err(error) => {
                    this.rollback_delegation(delegation).await;
                    this.reject(command, error.to_string()).await;
                }
            }
            return;
        }

        this.queue_delivery(command, priority).await;
    })
}

#[inline(never)]
pub(super) fn start_turn<'a>(
    this: &'a mut Harness,
    prompt: String,
    capacity: TurnCapacity,
    from_peers: bool,
) -> RegistryFuture<'a, std::io::Result<Started>> {
    Box::pin(async move {
        if this.active.is_some() {
            return Err(std::io::Error::other(format!(
                "agent {} is not idle",
                this.id
            )));
        }
        let registry = this
            .registry
            .upgrade()
            .ok_or_else(|| std::io::Error::other("subagent runtime is closed"))?;
        let agent = this
            .agent
            .as_ref()
            .ok_or_else(|| std::io::Error::other(format!("agent {} is closed", this.id)))?;
        let pending = registry
            .pending_turn(&this.root_session_id, this.id, agent.session_id())
            .await?;
        let pending = match pending {
            Some((id, input)) => Some((
                id,
                agent
                    .decode_durable_prompt(input)
                    .map_err(std::io::Error::other)?,
            )),
            None => None,
        };
        let recovering = pending.is_some();
        let recovery_request_id = pending.as_ref().map(|(id, _)| id.clone());
        if !recovering {
            let (model, thinking, reasoning_mode) =
                agent.settings().await.map_err(std::io::Error::other)?;
            this.context = Some(AgentContext {
                model,
                thinking,
                reasoning_mode,
            });
        }
        let Some(started) = registry
            .harness_turn_started(
                &this.root_session_id,
                this.id,
                pending
                    .as_ref()
                    .map(|(_, recovered)| recovered.instruction_revision().unwrap_or(0)),
                if recovering {
                    None
                } else {
                    this.context.as_ref()
                },
            )
            .await
        else {
            return Err(std::io::Error::other(format!(
                "agent {} cannot start another turn",
                this.id
            )));
        };
        let instruction_revision = started.revision;

        let (prompt, contract) = match &this.contract {
            Contract::Pending {
                assignment: Some(assignment),
            } => (
                format!("{assignment}\n\n{prompt}"),
                completion_instructions(&this.output_schema),
            ),
            Contract::Pending { assignment: None } => {
                (prompt, completion_instructions(&this.output_schema))
            }
            Contract::Delivered if from_peers && started.keeps_result => {
                (prompt, follow_up_reminder())
            }
            Contract::Delivered => (prompt, completion_reminder()),
        };
        let prompt = format!("{prompt}\n\n{contract}");
        let request = match pending {
            Some((id, recovered)) => {
                nanocode_agent_core::PromptRequest::new(recovered).request_id(id)
            }
            None => {
                let prompt =
                    Prompt::new(prompt.clone()).with_instruction_revision(instruction_revision);
                let prompt = match &this.context {
                    Some(context) => context.prompt(prompt),
                    None => prompt,
                };
                nanocode_agent_core::PromptRequest::new(prompt)
            }
        };
        agent.set_parking(registry.hibernates() && !recovering);
        let turn = match agent.prompt(request).await {
            Ok(turn) => turn,
            Err(error) => {
                let error = format!("could not start agent {}: {error}", this.id);
                registry
                    .harness_turn_start_failed(&this.root_session_id, this.id, error.clone())
                    .await;
                return Err(std::io::Error::other(error));
            }
        };
        this.contract = Contract::Delivered;
        let control = turn.control();
        let result = platform::spawn(turn.result());
        this.active = Some(ActiveTurn {
            control: control.clone(),
            result,
            _capacity: capacity,
        });
        if let Some(request_id) = recovery_request_id {
            let prompt = Prompt::new(prompt).with_instruction_revision(instruction_revision);
            let prompt = match &this.context {
                Some(context) => context.prompt(prompt),
                None => prompt,
            };
            let encoded = serde_json::to_string(&prompt).map_err(std::io::Error::other)?;
            let message_id = format!(
                "subagent-recovery:{request_id}:{}",
                crate::durable::checkpoint_key(&encoded)
            );
            return match control.steer_with_id(message_id, prompt).await {
                Ok(()) => Ok(Started::Prompt),
                Err(NanocodeError::TurnNotSteerable | NanocodeError::TurnStopped) => {
                    Ok(Started::RecoveryOnly)
                }
                Err(error) => Err(std::io::Error::other(error)),
            };
        }
        Ok(Started::Prompt)
    })
}
