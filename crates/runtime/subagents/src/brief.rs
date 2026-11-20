use nanocode_board::brief::BriefPolicy;

use super::{BoardCoordination, Inner, ReadFuture};

impl BoardCoordination {
    pub fn set_brief(&self, policy: Option<BriefPolicy>) {
        self.send(Box::new(move |inner| {
            Box::pin(async move {
                inner.brief = policy;
            })
        }));
    }

    pub(super) fn briefed<'a>(
        &'a self,
        session: &'a str,
    ) -> ReadFuture<'a, Result<Option<String>, String>> {
        let session = session.to_owned();
        Box::pin(async move {
            self.call(move |inner| Box::pin(async move { inner.brief_text(&session).await }))
                .await
                .unwrap_or_else(|| Err("the board coordination task stopped".to_owned()))
        })
    }
}

impl Inner {
    async fn brief_text(&mut self, session: &str) -> Result<Option<String>, String> {
        let Some(policy) = self.brief else {
            return Ok(None);
        };
        let task = self
            .sessions
            .get(session)
            .map(|state| state.task(session))
            .ok_or_else(|| format!("subagent brief: session {session} has no board task"))?;
        let board = self
            .dispatcher_board()
            .await
            .ok_or_else(|| "subagent brief: the board is not open".to_owned())?;
        if let Err(error) = board.refresh().await {
            return Err(format!("subagent brief: board refresh: {error}"));
        }
        board
            .fold()
            .brief(&task, &policy)
            .map(|brief| Some(brief.text))
            .map_err(|error| format!("subagent brief for {}: {error:?}", task.0))
    }
}
