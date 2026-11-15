use super::*;
use crate::client::FunctionOutputBody;
use nanocode_agent_core::{SideAnswer, backend::side_question_prompt};
use std::collections::HashSet;

const RUNNING_TOOL_OUTPUT: &str =
    "This tool call is still running in the main conversation; its result is not available yet.";

fn running_outputs(history: &[ResponseItem]) -> Vec<ResponseItem> {
    let answered = history
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCallOutput { call_id, .. }
            | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id.as_ref()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let running = || FunctionOutputBody::Text(RUNNING_TOOL_OUTPUT.into());
    history
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } if !answered.contains(call_id.as_ref()) => Some(ResponseItem::function_call_output(
                call_id.to_string(),
                running(),
            )),
            ResponseItem::CustomToolCall { call_id, name, .. }
                if !answered.contains(call_id.as_ref()) =>
            {
                Some(ResponseItem::custom_tool_output(
                    call_id.to_string(),
                    Some(name.to_string()),
                    running(),
                ))
            }
            _ => None,
        })
        .collect()
}

pub(crate) enum SideAsk {
    Question(String),
    Call(String),
}

impl SideAsk {
    const fn purpose(&self) -> &'static str {
        match self {
            Self::Question(_) => "side question",
            Self::Call(_) => "model call",
        }
    }
}

impl ModelCheckpoint {
    #[allow(
        clippy::too_many_arguments,
        reason = "a side question takes each request setting from the live thread"
    )]
    pub(crate) async fn side_question(
        &self,
        config: &ModelConfig,
        images: &ImageHistory,
        mut client: ResponsesClientBox,
        model: Model,
        thinking: Thinking,
        service_tier: ServiceTier,
        thread_id: &str,
        ask: &SideAsk,
    ) -> Result<SideAnswer> {
        let (events, _events) = EventSink::channel(thread_id.to_owned());
        let factory = ResponsesAttemptFactory::new(
            RequestProfile::new(
                self.provider_session_id.to_string(),
                self.prompt_cache_key.to_string(),
                Arc::clone(&self.request_prefix),
            )
            .with_thread_id(thread_id),
            events,
            Arc::new(TransportStats::default()),
        );
        let mut conversation = self.conversation.clone();
        conversation.reset_for_full_request();
        let message = |text: String| {
            ResponseItem::message(
                MessageRole::User,
                [ContentItem::InputText {
                    text: text.into_boxed_str(),
                }],
            )
        };
        let history = match ask {
            SideAsk::Question(question) => {
                let running = running_outputs(&conversation.flattened_history());
                conversation.append_client(running);
                conversation.append_client([message(side_question_prompt(question))]);
                conversation.managed.generation_request()
            }
            SideAsk::Call(prompt) => {
                let mut history = conversation.managed.generation_request();
                history.replace_from(0, vec![message(prompt.clone())], false);
                history
            }
        };
        let history = evict_images(&history, model);
        let request = factory.generation(
            1,
            &inline_images(images, history).await?,
            model,
            conversation.reasoning.request_effort(
                model,
                thinking,
                config.supports_reasoning_effort_updates(model),
            ),
            service_tier,
        );
        let success = client.execute(request).await?;
        let ResponsesOutput::Generation(response) = success.into_output() else {
            return Err(NanocodeError::InvalidAttemptState {
                detail: match ask {
                    SideAsk::Question(_) => "side question returned a non-generation response",
                    SideAsk::Call(_) => "model call returned a non-generation response",
                },
            });
        };
        let text = response
            .final_message
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                let purpose = ask.purpose();
                NanocodeError::InvalidRequest(if response.code_calls.is_empty() {
                    format!("the {purpose} response contained no text")
                } else {
                    format!("the {purpose} response called a tool instead of answering directly")
                })
            })?;
        let mut stats = RunStats::default();
        if let Some(usage) = &response.usage {
            stats.usage.add(usage, model, service_tier);
        }
        Ok(SideAnswer::new(text, Some(stats.turn_usage())))
    }
}
