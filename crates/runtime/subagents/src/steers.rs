use super::{AgentId, BoardCoordination, Inner, Mutation, ReadFuture, mutated};

impl BoardCoordination {
    pub fn steer<'a>(
        &'a self,
        root_session_id: &'a str,
        id: AgentId,
        body: String,
    ) -> ReadFuture<'a, Result<(), String>> {
        let root = root_session_id.to_owned();
        Box::pin(async move {
            self.call(move |inner| Box::pin(async move { inner.steer(&root, id, body).await }))
                .await
                .unwrap_or_else(|| Err("the board coordination task stopped".to_owned()))
        })
    }

    pub(super) fn boundary<'a>(
        &'a self,
        session: &'a str,
    ) -> ReadFuture<'a, Result<Vec<String>, String>> {
        let session = session.to_owned();
        Box::pin(async move {
            self.call(move |inner| Box::pin(async move { inner.boundary(&session).await }))
                .await
                .unwrap_or_else(|| Err("the board coordination task stopped".to_owned()))
        })
    }
}

impl Inner {
    async fn steer(&mut self, root: &str, id: AgentId, body: String) -> Result<(), String> {
        let session = self
            .session_of(root, id)
            .await
            .ok_or_else(|| format!("steer: agent {id} has no board session"))?;
        let session = session.as_str();
        let task = self
            .sessions
            .get(session)
            .map(|state| state.task(session))
            .ok_or_else(|| format!("steer: session {session} has no board task"))?;
        let at = self.tick();
        let board = self
            .dispatcher_board()
            .await
            .ok_or_else(|| "steer: the board is not open".to_owned())?;
        board
            .steer(at, task.clone(), body)
            .await
            .map_err(|error| format!("steer {task}: {error}"))?;
        if let Some(state) = self.sessions.get_mut(session) {
            state.sends.push(state.boundaries);
        }
        Ok(())
    }

    async fn boundary(&mut self, session: &str) -> Result<Vec<String>, String> {
        let Some(state) = self.sessions.get(session) else {
            return Ok(Vec::new());
        };
        let Some(claim) = state.claim.clone() else {
            return Ok(Vec::new());
        };
        let task = state.task(session);
        let heard = if mutated(Mutation::SteersTakenMidTurn) {
            None
        } else {
            state.heard
        };
        if let Some(state) = self.sessions.get_mut(session) {
            state.boundaries += 1;
        }
        if mutated(Mutation::SteersNeverDelivered) {
            return Ok(Vec::new());
        }
        let at = self.tick();
        let Some(board) = self.worker(session).await else {
            return Err(format!("steers for {task}: the worker board is not open"));
        };
        let steers = match board.boundary(at, &task, &claim, heard).await {
            Ok(steers) => steers,
            Err(error) => {
                let error = format!("steers for {task}: {error}");
                self.fail(error.clone());
                return Err(error);
            }
        };
        if let Some(state) = self.sessions.get_mut(session) {
            state.heard = steers.last().map(|steer| steer.seq);
            let heard = steers.len().min(state.sends.len());
            state.sends.drain(..heard);
        }
        self.stats.delivered += u64::try_from(steers.len()).expect("a steer count fits in u64");
        Ok(steers.into_iter().map(|steer| steer.body).collect())
    }

    pub(super) async fn heed_heard(&mut self, session: &str) -> Result<(), String> {
        let Some(state) = self.sessions.get(session) else {
            return Ok(());
        };
        let Some(claim) = state.claim.clone() else {
            return Ok(());
        };
        let heard = state.heard;
        let task = state.task(session);
        let at = self.tick();
        let Some(board) = self.worker(session).await else {
            return Err(format!("heed {task}: the worker board is not open"));
        };
        let mut upto = heard;
        let mut unheard = 0;
        loop {
            let steers = board
                .boundary(at, &task, &claim, upto.take())
                .await
                .map_err(|error| format!("heed {task}: {error}"))?;
            let Some(last) = steers.last() else {
                break;
            };
            upto = Some(last.seq);
            unheard += steers.len();
        }
        self.stats.unheard += u64::try_from(unheard).expect("a steer count fits in u64");
        if let Some(state) = self.sessions.get_mut(session) {
            state.heard = None;
            let unheard = unheard.min(state.sends.len());
            let missed = state
                .sends
                .drain(..unheard)
                .filter(|sent_at| *sent_at < state.boundaries)
                .count();
            self.stats.missed += u64::try_from(missed).expect("a steer count fits in u64");
        }
        Ok(())
    }
}
