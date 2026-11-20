pub use nanocode_board_fold::steer::*;

use crate::{Applied, Board, BoardError, ClaimId, Op, TaskId, tasks::Lease};

impl Board {
    pub async fn steer(
        &mut self,
        at_ms: u64,
        task: TaskId,
        body: impl Into<PromptInput>,
    ) -> Result<Applied, BoardError> {
        let send = SteerOp::Send {
            task,
            claim: None,
            body: body.into(),
        };
        self.submit(at_ms, Op::Steer(send)).await
    }

    pub async fn steer_claim(
        &mut self,
        at_ms: u64,
        task: TaskId,
        claim: ClaimId,
        body: impl Into<PromptInput>,
    ) -> Result<Applied, BoardError> {
        let send = SteerOp::Send {
            task,
            claim: Some(claim),
            body: body.into(),
        };
        self.submit(at_ms, Op::Steer(send)).await
    }

    pub async fn take_steers(&mut self, lease: &Lease) -> Result<Vec<Steer>, BoardError> {
        self.refresh().await?;
        let steers = self
            .fold
            .steers_for(&lease.task, &lease.claim)
            .map_err(BoardError::Rejected)?;
        Ok(steers.cloned().collect())
    }

    pub async fn heed(
        &mut self,
        at_ms: u64,
        lease: &Lease,
        upto: u64,
    ) -> Result<Applied, BoardError> {
        let heed = SteerOp::Heed {
            task: lease.task.clone(),
            claim: lease.claim.clone(),
            upto,
        };
        self.submit(at_ms, Op::Steer(heed)).await
    }
}
