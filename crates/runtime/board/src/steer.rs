pub use nanocode_board_fold::steer::*;

use crate::{
    Applied, Board, BoardError, ClaimId, Op, TaskId,
    sim::{Mutation, mutated},
};

impl Board {
    pub async fn steer(
        &mut self,
        at_ms: u64,
        task: TaskId,
        body: String,
    ) -> Result<Applied, BoardError> {
        let send = SteerOp::Send {
            task,
            claim: None,
            body,
        };
        self.submit(at_ms, Op::Steer(send)).await
    }

    pub async fn steer_claim(
        &mut self,
        at_ms: u64,
        task: TaskId,
        claim: ClaimId,
        body: String,
    ) -> Result<Applied, BoardError> {
        let send = SteerOp::Send {
            task,
            claim: Some(claim),
            body,
        };
        self.submit(at_ms, Op::Steer(send)).await
    }

    pub async fn boundary(
        &mut self,
        at_ms: u64,
        task: &TaskId,
        claim: &ClaimId,
        heard: Option<u64>,
    ) -> Result<Vec<Steer>, BoardError> {
        if let Some(upto) = heard
            && !mutated(Mutation::StepSteersTwice)
        {
            let upto = if mutated(Mutation::StepHeedsUnseen) {
                self.refresh().await?;
                self.fold
                    .steers_for(task, claim)
                    .map_err(BoardError::Rejected)?
                    .last()
                    .map_or(upto, |steer| steer.seq)
            } else {
                upto
            };
            self.heed(at_ms, task, claim, upto).await?;
        }
        self.take_steers(task, claim).await
    }

    async fn take_steers(
        &mut self,
        task: &TaskId,
        claim: &ClaimId,
    ) -> Result<Vec<Steer>, BoardError> {
        // The heed before this read has landed: failing the boundary on a read that a retry
        // would answer leaves the worker unable to finish.
        self.catch_up().await?;
        let steers = self
            .fold
            .steers_for(task, claim)
            .map_err(BoardError::Rejected)?;
        Ok(steers.cloned().collect())
    }

    async fn heed(
        &mut self,
        at_ms: u64,
        task: &TaskId,
        claim: &ClaimId,
        upto: u64,
    ) -> Result<Applied, BoardError> {
        let heed = SteerOp::Heed {
            task: task.clone(),
            claim: claim.clone(),
            upto,
        };
        self.submit(at_ms, Op::Steer(heed)).await
    }
}
