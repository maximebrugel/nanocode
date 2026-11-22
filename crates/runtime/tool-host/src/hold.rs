use std::sync::Arc;

use nanocode_tools::{
    Tool, ToolContext, ToolDefinition, ToolError, ToolInput, ToolOutput, ToolResult,
};
pub use nanocode_turnlog::{Answer, Answered, Hold, HoldError, Resumption};
use serde_json::value::RawValue;

use crate::embedded::HostFuture;

pub struct HoldCall<'a> {
    pub call_id: &'a str,
    pub input: &'a RawValue,
    pub session_id: &'a str,
    pub tool: &'a str,
    pub turn_id: Option<&'a str>,
}

pub enum Judgment {
    Run,
    Refuse {
        message: String,
    },
    Hold {
        card: Box<RawValue>,
        key: String,
        message: String,
    },
}

pub trait HoldPolicy: Send + Sync + 'static {
    fn judge<'a>(&'a self, call: HoldCall<'a>) -> HostFuture<'a, Judgment>;
}

#[derive(Clone, Debug)]
pub enum LedgerError {
    Hold(HoldError),
    Journal(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hold(error) => error.fmt(formatter),
            Self::Journal(error) => write!(formatter, "the hold journal was not saved: {error}"),
        }
    }
}

impl std::error::Error for LedgerError {}

pub struct Running {
    pub input: Box<RawValue>,
    pub key: String,
}

pub trait HoldLedger: Send + Sync + 'static {
    fn hold<'a>(
        &'a self,
        key: &'a str,
        call: HoldCall<'a>,
        card: &'a RawValue,
    ) -> HostFuture<'a, Result<(), LedgerError>>;

    fn resolve<'a>(
        &'a self,
        key: &'a str,
        answer: Answer,
    ) -> HostFuture<'a, Result<Resumption, LedgerError>>;

    fn running<'a>(&'a self, call_id: &'a str) -> HostFuture<'a, Option<Running>>;

    fn release<'a>(&'a self, key: &'a str) -> HostFuture<'a, Result<(), LedgerError>>;
}

#[derive(Clone)]
pub struct Holds {
    ledger: Arc<dyn HoldLedger>,
    policy: Arc<dyn HoldPolicy>,
}

impl Holds {
    #[must_use]
    pub fn new(policy: Arc<dyn HoldPolicy>, ledger: Arc<dyn HoldLedger>) -> Self {
        Self { ledger, policy }
    }

    #[must_use]
    pub const fn ledger(&self) -> &Arc<dyn HoldLedger> {
        &self.ledger
    }

    #[must_use]
    #[cfg(not(target_family = "wasm"))]
    pub fn wrap_provider(
        &self,
        inner: Arc<dyn nanocode_tool_provider::DynamicToolProvider>,
    ) -> Arc<dyn nanocode_tool_provider::DynamicToolProvider> {
        Arc::new(provider::HeldProvider {
            holds: self.clone(),
            inner,
        })
    }

    #[must_use]
    pub fn wrap(&self, inner: Arc<dyn Tool>) -> Arc<dyn Tool> {
        let name = Arc::from(inner.definition().name());
        Arc::new(Holding {
            holds: self.clone(),
            inner,
            name,
        })
    }
}

struct Holding {
    holds: Holds,
    inner: Arc<dyn Tool>,
    name: Arc<str>,
}

pub fn raw_input(input: &ToolInput) -> Result<Box<RawValue>, String> {
    match input {
        ToolInput::Function(arguments) => Ok(arguments.clone()),
        ToolInput::Freeform(text) => {
            serde_json::value::to_raw_value(text).map_err(|error| error.to_string())
        }
    }
}

enum Admission {
    Run,
    Resume { key: String },
    Answer(ToolOutput),
}

fn same_input(given: &RawValue, approved: &RawValue) -> bool {
    given.get() == approved.get()
        || matches!(
            (
                serde_json::from_str::<serde_json::Value>(given.get()),
                serde_json::from_str::<serde_json::Value>(approved.get()),
            ),
            (Ok(given), Ok(approved)) if given == approved
        )
}

impl Holds {
    async fn admit(&self, name: &str, raw: &RawValue, context: &ToolContext<'_>) -> Admission {
        if let Some(Running {
            input: approved,
            key,
        }) = self.ledger.running(context.call_id()).await
        {
            if same_input(raw, &approved)
                || crate::sim::mutated(crate::sim::Mutation::ResumeIgnoresAnswer)
            {
                return Admission::Resume { key };
            }
            return Admission::Answer(ToolOutput::error(format!(
                "tool `{name}` was not run: its input differs from the input answered on hold `{key}`"
            )));
        }
        let call = || HoldCall {
            call_id: context.call_id(),
            input: raw,
            session_id: context.session_id(),
            tool: name,
            turn_id: context.turn_id(),
        };
        match self.policy.judge(call()).await {
            Judgment::Run => Admission::Run,
            Judgment::Refuse { message } => Admission::Answer(ToolOutput::error(message)),
            Judgment::Hold { card, key, message } => {
                Admission::Answer(match self.ledger.hold(&key, call(), &card).await {
                    Ok(()) => ToolOutput::text(message),
                    Err(error) => ToolOutput::error(format!(
                        "tool `{name}` was not run because its hold could not be recorded: {error}"
                    )),
                })
            }
        }
    }

    async fn released(&self, name: &str, key: &str) {
        if let Err(error) = self.ledger.release(key).await {
            nanocode_trace::warn!(%error, key, tool = %name, "a resumed held call ran but its release was not saved; it reads as uncertain until settled");
        }
    }
}

fn unreadable(name: &str, error: impl std::fmt::Display) -> ToolOutput {
    ToolOutput::error(format!(
        "tool `{name}` was not run: its input could not be read: {error}"
    ))
}

impl Holding {
    async fn judged(&self, input: ToolInput, context: ToolContext<'_>) -> ToolResult {
        let name = &self.name;
        let raw = match raw_input(&input) {
            Ok(raw) => raw,
            Err(error) => return Ok(unreadable(name, error)),
        };
        match self.holds.admit(name, &raw, &context).await {
            Admission::Run => self.inner.execute(input, context).await,
            Admission::Resume { key } => {
                let output = self.inner.execute(input, context).await;
                self.holds.released(name, &key).await;
                output
            }
            Admission::Answer(output) => Ok(output),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
mod provider {
    use std::sync::Arc;

    use nanocode_tool_provider::{DynamicToolProvider, SessionEnvironment, ToolExposure};
    use nanocode_tools::{Tool, ToolContext, ToolDefinition, ToolOutput};
    use serde_json::Value;

    use super::{Admission, Holds, unreadable};
    use crate::embedded::HostFuture;

    pub(super) struct HeldProvider {
        pub(super) holds: Holds,
        pub(super) inner: Arc<dyn DynamicToolProvider>,
    }

    impl HeldProvider {
        async fn judged(
            &self,
            name: &str,
            input: Value,
            context: ToolContext<'_>,
        ) -> Option<ToolOutput> {
            let raw = match serde_json::value::to_raw_value(&input) {
                Ok(raw) => raw,
                Err(error) => return Some(unreadable(name, error)),
            };
            match self.holds.admit(name, &raw, &context).await {
                Admission::Run => self.inner.execute(name, input, context).await,
                Admission::Resume { key } => {
                    let output = self.inner.execute(name, input, context).await;
                    self.holds.released(name, &key).await;
                    output
                }
                Admission::Answer(output) => Some(output),
            }
        }
    }

    impl DynamicToolProvider for HeldProvider {
        fn start(&self) {
            self.inner.start();
        }

        fn bind_session(&self, session: &SessionEnvironment) {
            self.inner.bind_session(session);
        }

        fn direct_tools(&self) -> Vec<Arc<dyn Tool>> {
            self.inner.direct_tools()
        }

        fn direct_tools_for_exposure(&self, exposure: ToolExposure) -> Vec<Arc<dyn Tool>> {
            self.inner.direct_tools_for_exposure(exposure)
        }

        fn available_definitions(&self) -> Vec<ToolDefinition> {
            self.inner.available_definitions()
        }

        fn model_catalog_visible(&self) -> bool {
            self.inner.model_catalog_visible()
        }

        fn catalog_revision(&self) -> Option<u64> {
            self.inner.catalog_revision()
        }

        fn code_mode_tool_summaries(&self) -> Vec<(String, String)> {
            self.inner.code_mode_tool_summaries()
        }

        fn contains(&self, name: &str) -> bool {
            self.inner.contains(name)
        }

        fn supports_parallel_tool_calls(&self, name: &str) -> bool {
            self.inner.supports_parallel_tool_calls(name)
        }

        fn resolve<'life0, 'life1, 'async_trait>(
            &'life0 self,
            name: &'life1 str,
        ) -> HostFuture<'async_trait, bool>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            Self: 'async_trait,
        {
            self.inner.resolve(name)
        }

        fn execute<'life0, 'life1, 'life2, 'async_trait>(
            &'life0 self,
            name: &'life1 str,
            input: Value,
            context: ToolContext<'life2>,
        ) -> HostFuture<'async_trait, Option<ToolOutput>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            'life2: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(self.judged(name, input, context))
        }

        fn resumes(&self, name: &str) -> bool {
            self.inner.resumes(name)
        }

        fn recover<'life0, 'life1, 'life2, 'async_trait>(
            &'life0 self,
            name: &'life1 str,
            input: Value,
            context: ToolContext<'life2>,
        ) -> HostFuture<'async_trait, Option<ToolOutput>>
        where
            'life0: 'async_trait,
            'life1: 'async_trait,
            'life2: 'async_trait,
            Self: 'async_trait,
        {
            self.inner.recover(name, input, context)
        }
    }
}

impl Tool for Holding {
    fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.inner.supports_parallel_tool_calls()
    }

    fn running_tasks(&self) -> Vec<nanocode_tools::RunningTask> {
        self.inner.running_tasks()
    }

    fn end_turn<'life0, 'life1, 'life2, 'life3, 'async_trait>(
        &'life0 self,
        session_id: &'life1 str,
        turn_id: &'life2 str,
        hook_event_name: &'life3 str,
    ) -> HostFuture<'async_trait, Result<(), ToolError>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        Self: 'async_trait,
    {
        self.inner.end_turn(session_id, turn_id, hook_event_name)
    }

    fn execute<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> HostFuture<'async_trait, ToolResult>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(self.judged(input, context))
    }

    fn recover<'life0, 'life1, 'async_trait>(
        &'life0 self,
        input: ToolInput,
        context: ToolContext<'life1>,
    ) -> Option<HostFuture<'async_trait, Option<ToolOutput>>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        self.inner.recover(input, context)
    }
}
