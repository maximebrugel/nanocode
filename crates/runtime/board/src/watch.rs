use std::{collections::BTreeSet, future::Future, pin::Pin};

pub use nanocode_board_fold::watch::*;

use crate::{
    Applied, Board, BoardError, Entry, EventId, Op, Rejection, TaskId,
    sim::{Mutation, mutated},
    wake::{Timer, Wake},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    pub key: Key,
    pub from: u64,
    pub to: u64,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub watcher: String,
    pub thread: TaskId,
    pub at: u64,
    pub upto: u64,
    pub id: EventId,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runnable {
    Take { thread: TaskId, at: u64, upto: u64 },
    Bury { thread: TaskId, at: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub id: String,
    pub send: Op,
    pub first_ms: u64,
    pub every_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub times: Option<u64>,
}

pub type Handled<'a> = Pin<Box<dyn Future<Output = Result<Vec<Op>, String>> + 'a>>;

pub trait Handler {
    fn run<'a>(&'a self, lease: &'a Lease) -> Handled<'a>;
}

pub struct Registered<'h> {
    pub name: String,
    pub filter: Filter,
    pub policy: Policy,
    pub lease_ms: u64,
    pub handler: Box<dyn Handler + 'h>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Served {
    pub acked: u64,
    pub failed: u64,
    pub buried: u64,
    pub lost: u64,
}

impl Board {
    async fn step(&mut self, at_ms: u64, step: Step) -> Result<Applied, BoardError> {
        self.submit(at_ms, Op::Watch(step)).await
    }

    pub fn pending(&self, actor: &str, scope: Option<&TaskId>, filter: &Filter) -> Batch {
        let key = Key::Cursor {
            actor: actor.to_owned(),
            scope: scope.cloned(),
        };
        let fold = &self.fold;
        let from = fold.position(&key);
        let start = fold.entries().partition_point(|entry| entry.seq <= from);
        let entries = fold.entries()[start..]
            .iter()
            .filter(|entry| fold.access().reads(self.identity.actor(), &entry.task.0))
            .filter(|entry| scope.is_none_or(|above| under(fold.tasks(), &entry.task, above)))
            .filter(|entry| filter.matches(fold.tasks(), entry))
            .cloned()
            .collect();
        Batch {
            key,
            from,
            to: fold.next() - 1,
            entries,
        }
    }

    pub async fn consume(
        &mut self,
        at_ms: u64,
        batch: &Batch,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        self.commit(
            at_ms,
            batch.key.clone(),
            None,
            batch.from,
            batch.to,
            effects,
        )
        .await
    }

    async fn commit(
        &mut self,
        at_ms: u64,
        key: Key,
        lease: Option<EventId>,
        from: u64,
        to: u64,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        if mutated(Mutation::ConsumeSplits) {
            for op in &effects {
                self.submit(at_ms, op.clone()).await?;
            }
            let step = Step::Consume {
                key,
                lease,
                from,
                to,
                effects: Vec::new(),
            };
            return self.step(at_ms, step).await;
        }
        let step = Step::Consume {
            key,
            lease,
            from,
            to,
            effects,
        };
        self.step(at_ms, step).await
    }

    pub async fn watch(
        &mut self,
        at_ms: u64,
        watcher: &str,
        filter: &Filter,
        policy: Policy,
    ) -> Result<Option<Applied>, BoardError> {
        self.refresh().await?;
        let current = self.fold.marks().watchers().get(watcher);
        if current.is_some_and(|found| found.filter == *filter && found.policy == policy) {
            return Ok(None);
        }
        let step = Step::Watch {
            watcher: watcher.to_owned(),
            filter: filter.clone(),
            policy,
        };
        self.step(at_ms, step).await.map(Some)
    }

    pub async fn unwatch(&mut self, at_ms: u64, watcher: &str) -> Result<Applied, BoardError> {
        let step = Step::Unwatch {
            watcher: watcher.to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub fn runnable(&self, watcher: &str, at_ms: u64) -> Vec<Runnable> {
        let Some(found) = self.fold.marks().watchers().get(watcher) else {
            return Vec::new();
        };
        let now = self.fold.now_ms().max(at_ms);
        found
            .lanes
            .iter()
            .filter_map(|(thread, lane)| {
                let at = lane.head()?;
                let thread = thread.clone();
                let tries = lane.attempt.tries();
                match &lane.attempt {
                    Attempt::Leased { until_ms, .. } if *until_ms > now => None,
                    Attempt::Idle { not_before_ms, .. } if *not_before_ms > now => None,
                    _ if tries >= found.policy.tries => Some(Runnable::Bury { thread, at }),
                    _ if lane.single(at) => Some(Runnable::Take {
                        thread,
                        at,
                        upto: at,
                    }),
                    _ => {
                        let last = lane.pending.iter().take(BATCH).copied().max().unwrap_or(at);
                        Some(Runnable::Take {
                            thread,
                            at,
                            upto: last.max(at),
                        })
                    }
                }
            })
            .collect()
    }

    pub async fn take(
        &mut self,
        at_ms: u64,
        watcher: &str,
        thread: &TaskId,
        (at, upto): (u64, u64),
        lease_ms: u64,
    ) -> Result<Lease, BoardError> {
        let step = Step::Take {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
            upto,
            lease_ms,
        };
        let applied = self.step(at_ms, step).await?;
        let fold = &self.fold;
        // The lane indexed these rows under the thread they had then; a later
        // move must not drop them from the lease, so read the lane, not roots.
        // A take read back after another worker consumed the rows is stale.
        let lane = fold
            .marks()
            .watchers()
            .get(watcher)
            .and_then(|found| found.lanes.get(thread));
        let rows: BTreeSet<u64> = lane
            .map(|lane| {
                lane.pending
                    .iter()
                    .copied()
                    .filter(|seq| (at..=upto).contains(seq))
                    .collect()
            })
            .unwrap_or_default();
        if !rows.contains(&at) {
            let head = lane.and_then(Lane::head);
            return Err(BoardError::Rejected(Rejection::Watch(Refusal::NotHead {
                head,
            })));
        }
        let start = fold.entries().partition_point(|entry| entry.seq < at);
        let entries = fold.entries()[start..]
            .iter()
            .take_while(|entry| entry.seq <= upto)
            .filter(|entry| rows.contains(&entry.seq))
            .cloned()
            .collect();
        Ok(Lease {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
            upto,
            id: applied.event,
            entries,
        })
    }

    pub async fn ack(
        &mut self,
        at_ms: u64,
        lease: &Lease,
        effects: Vec<Op>,
    ) -> Result<Applied, BoardError> {
        let key = Key::Watcher {
            watcher: lease.watcher.clone(),
            thread: lease.thread.clone(),
        };
        self.refresh().await?;
        let from = self.fold.position(&key);
        self.commit(
            at_ms,
            key,
            Some(lease.id.clone()),
            from,
            lease.upto,
            effects,
        )
        .await
    }

    pub async fn fail(
        &mut self,
        at_ms: u64,
        lease: &Lease,
        error: &str,
    ) -> Result<Applied, BoardError> {
        let step = Step::Fail {
            watcher: lease.watcher.clone(),
            thread: lease.thread.clone(),
            lease: lease.id.clone(),
            error: error.to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub async fn bury(
        &mut self,
        at_ms: u64,
        watcher: &str,
        thread: &TaskId,
        at: u64,
    ) -> Result<Applied, BoardError> {
        let step = Step::Bury {
            watcher: watcher.to_owned(),
            thread: thread.clone(),
            at,
        };
        self.step(at_ms, step).await
    }

    pub async fn schedule(&mut self, at_ms: u64, plan: Plan) -> Result<Applied, BoardError> {
        let step = Step::Schedule {
            id: plan.id,
            by: self.writer().to_owned(),
            send: Box::new(plan.send),
            first_ms: plan.first_ms,
            every_ms: plan.every_ms,
            until_ms: plan.until_ms,
            times: plan.times,
        };
        self.step(at_ms, step).await
    }

    pub async fn unschedule(&mut self, at_ms: u64, id: &str) -> Result<Applied, BoardError> {
        let step = Step::Unschedule {
            id: id.to_owned(),
            by: self.writer().to_owned(),
        };
        self.step(at_ms, step).await
    }

    pub async fn fire_due(&mut self, at_ms: u64) -> Result<u64, BoardError> {
        let mut fired = 0;
        loop {
            self.refresh().await?;
            let now = self.fold.now_ms().max(at_ms);
            let due = self
                .fold
                .marks()
                .schedules()
                .iter()
                .find(|(_, schedule)| schedule.due_ms().is_some_and(|due| due <= now))
                .map(|(id, schedule)| (id.clone(), schedule.sent));
            let Some((id, n)) = due else {
                return Ok(fired);
            };
            let key = format!("fire:{id}:{n}");
            match self
                .submit_keyed(&key, at_ms, Op::Watch(Step::Fire { id, n }))
                .await
            {
                Ok(_) => fired += 1,
                Err(BoardError::Rejected(_)) => return Ok(fired),
                Err(error) => return Err(error),
            }
        }
    }

    pub async fn wait(
        &mut self,
        wake: &dyn Wake,
        timer: &dyn Timer,
        scope: Option<&TaskId>,
        filter: &Filter,
        after: u64,
        until_ms: u64,
    ) -> Result<Vec<Entry>, BoardError> {
        loop {
            self.fire_due(timer.now_ms()).await?;
            let fold = &self.fold;
            let start = fold.entries().partition_point(|entry| entry.seq <= after);
            let found = fold.entries()[start..]
                .iter()
                .filter(|entry| fold.access().reads(self.identity.actor(), &entry.task.0))
                .filter(|entry| scope.is_none_or(|above| under(fold.tasks(), &entry.task, above)))
                .filter(|entry| filter.matches(fold.tasks(), entry))
                .cloned()
                .collect::<Vec<_>>();
            let now = timer.now_ms();
            if !found.is_empty() || now >= until_ms {
                return Ok(found);
            }
            let head = fold.next() - 1;
            let wake_at = self
                .due_ms()
                .filter(|due| *due > now)
                .map_or(until_ms, |due| due.min(until_ms));
            wake.wait(&self.scope, head, wake_at).await;
        }
    }

    pub async fn serve(
        &mut self,
        timer: &dyn Timer,
        watcher: &Registered<'_>,
    ) -> Result<Served, BoardError> {
        let mut served = Served::default();
        self.watch(
            timer.now_ms(),
            &watcher.name,
            &watcher.filter,
            watcher.policy,
        )
        .await?;
        self.fire_due(timer.now_ms()).await?;
        loop {
            let runnable = self.runnable(&watcher.name, timer.now_ms());
            if runnable.is_empty() {
                return Ok(served);
            }
            let before = served;
            self.serve_once(timer, watcher, runnable, &mut served)
                .await?;
            if served.acked + served.failed + served.buried
                == before.acked + before.failed + before.buried
            {
                return Ok(served);
            }
        }
    }

    async fn serve_once(
        &mut self,
        timer: &dyn Timer,
        watcher: &Registered<'_>,
        runnable: Vec<Runnable>,
        served: &mut Served,
    ) -> Result<(), BoardError> {
        for runnable in runnable {
            let (thread, at) = match runnable {
                Runnable::Bury { thread, at } => {
                    match self.bury(timer.now_ms(), &watcher.name, &thread, at).await {
                        Ok(_) => served.buried += 1,
                        Err(BoardError::Rejected(_)) => served.lost += 1,
                        Err(error) => return Err(error),
                    }
                    continue;
                }
                Runnable::Take { thread, at, upto } => (thread, (at, upto)),
            };
            let lease = match self
                .take(timer.now_ms(), &watcher.name, &thread, at, watcher.lease_ms)
                .await
            {
                Ok(lease) => lease,
                Err(BoardError::Rejected(_)) => {
                    served.lost += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let settled = match watcher.handler.run(&lease).await {
                Ok(effects) => self
                    .ack(timer.now_ms(), &lease, effects)
                    .await
                    .map(|_| true),
                Err(error) => self
                    .fail(timer.now_ms(), &lease, &error)
                    .await
                    .map(|_| false),
            };
            match settled {
                Ok(true) => served.acked += 1,
                Ok(false) => served.failed += 1,
                Err(BoardError::Rejected(_)) => served.lost += 1,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}
