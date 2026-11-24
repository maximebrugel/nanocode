use std::sync::Arc;

use nanocode_turnlog::{
    ArmedWake, Lease, Point, Record, Rider, Unfit, Wake, WakeRider,
    wake_sim::{Mutation, mutated},
};
use serde_json::{Map, Value, value::RawValue};

use super::{Lifecycle, OneLog, invalid, logged, raw, record_of};
use crate::{
    durability::{SqlStatement, StoreError},
    session_log::{LoggedEvent, Submission, TurnRecord, now, submission_digest},
};

#[derive(Debug)]
pub enum WakeError {
    Full { max: usize },
    Store(StoreError),
    Unfit(Unfit),
}

impl std::fmt::Display for WakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full { max } => write!(
                formatter,
                "this session already holds its {max} wakes: disarm one or raise the policy's wakes_max"
            ),
            Self::Store(error) => error.fmt(formatter),
            Self::Unfit(unfit) => unfit.fmt(formatter),
        }
    }
}

impl std::error::Error for WakeError {}

impl From<StoreError> for WakeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Heard {
    pub full: Vec<(String, u64)>,
    pub heard: Vec<(String, u64)>,
}

#[derive(Clone, Debug)]
pub enum Fire {
    Fired {
        event: LoggedEvent,
        next: Option<u64>,
        record: TurnRecord,
    },
    NotDue,
    Stale,
    Taken(TurnRecord),
}

impl OneLog {
    pub async fn index_wakes(&self, index: Arc<dyn WakeRider>) -> Result<(), StoreError> {
        self.log
            .answered_within("wake index prepare", index.prepare())
            .await?;
        let lease = self.log.lease_within().await?;
        let summary = lease.fold().wakes().summary();
        self.log
            .answered_within("wake index write", index.write(self.scope(), &summary))
            .await?;
        self.wake_index.set(Some(index));
        drop(lease);
        Ok(())
    }

    async fn commit_wakes(&self, lease: &mut Lease<'_>, point: Point) -> Result<(), StoreError> {
        let Some(index) = self.wake_index.read(Clone::clone) else {
            lease.commit(point).await?;
            return Ok(());
        };
        let held = index.statements(self.scope(), &lease.fold().wakes().summary());
        let statements = held
            .iter()
            .map(|(sql, params)| SqlStatement {
                sql,
                params: params.iter().map(String::as_str).collect(),
            })
            .collect::<Vec<_>>();
        let rider = Rider {
            host: index.host(),
            statements: &statements,
        };
        let (_, carried) = lease.commit_carrying(point, rider).await?;
        // An error from here on would make the caller unwind a commit that is already durable, so
        // a rider the log's host did not carry is written on its own instead, and its failure
        // only leaves the index stale until the next index_wakes.
        if !carried {
            let summary = lease.fold().wakes().summary();
            let written = self
                .log
                .answered_within("wake index write", index.write(self.scope(), &summary))
                .await;
            if let Err(error) = written {
                nanocode_trace::warn!(
                    %error,
                    scope = self.scope(),
                    "turn log committed its wakes, but its host did not carry the wake index's rider and writing the index on its own failed; it stays stale until index_wakes"
                );
            }
        }
        Ok(())
    }

    pub async fn arm(&self, key: &str, wake: Wake) -> Result<u64, WakeError> {
        wake.fit().map_err(WakeError::Unfit)?;
        let mut lease = self.log.lease_within().await?;
        let prior = lease.fold().wakes().get(key).cloned();
        if let Some(max) = self.wakes_max
            && prior.is_none()
            && lease.fold().wakes().len() >= max
            && !mutated(Mutation::Unbounded)
        {
            return Err(WakeError::Full { max });
        }
        let generation = lease.fold().wakes().next_generation();
        lease.push(Record::Armed {
            generation,
            key: key.to_owned(),
            wake,
        })?;
        if let Err(error) = self.commit_wakes(&mut lease, Point::Progress).await {
            lease.unwake(key, prior);
            return Err(error.into());
        }
        assert_eq!(
            lease.fold().wakes().get(key).map(|armed| armed.generation),
            Some(generation),
            "an armed wake holds the generation it was armed at"
        );
        Ok(generation)
    }

    pub async fn disarm(&self, key: &str) -> Result<Option<u64>, StoreError> {
        let mut lease = self.log.lease_within().await?;
        let Some(prior) = lease.fold().wakes().get(key).cloned() else {
            return Ok(None);
        };
        let generation = prior.generation;
        lease.push(Record::Disarmed {
            generation,
            key: key.to_owned(),
        })?;
        if let Err(error) = self.commit_wakes(&mut lease, Point::Progress).await {
            lease.unwake(key, Some(prior));
            return Err(error);
        }
        assert!(
            lease.fold().wakes().get(key).is_none(),
            "a disarmed wake is gone"
        );
        Ok(Some(generation))
    }

    pub async fn hear(
        &self,
        topic: &str,
        fields: &Map<String, Value>,
        event: Box<RawValue>,
    ) -> Result<Heard, StoreError> {
        let mut lease = self.log.lease_within().await?;
        let mut heard = Heard::default();
        let mut priors = Vec::new();
        for (key, armed) in lease.fold().wakes().iter() {
            if !armed.hears(topic, fields) {
                continue;
            }
            assert!(armed.wake.on.is_some(), "a wake that hears has a trigger");
            let found = (key.to_owned(), armed.generation);
            if armed.full() {
                heard.full.push(found);
            } else {
                priors.push((key.to_owned(), armed.clone()));
                heard.heard.push(found);
            }
        }
        if heard.heard.is_empty() {
            return Ok(heard);
        }
        for (key, generation) in &heard.heard {
            lease.push(Record::Heard {
                event: event.clone(),
                generation: *generation,
                key: key.clone(),
            })?;
        }
        let committed = if mutated(Mutation::IndexSkipsHear) {
            lease.commit(Point::Progress).await.map(drop)
        } else {
            self.commit_wakes(&mut lease, Point::Progress).await
        };
        if let Err(error) = committed {
            for (key, prior) in priors {
                lease.unwake(&key, Some(prior));
            }
            return Err(error);
        }
        Ok(heard)
    }

    pub async fn wake(&self, key: &str) -> Option<ArmedWake> {
        self.log.read(|fold| fold.wakes().get(key).cloned()).await
    }

    pub async fn due_wakes(&self, now: u64) -> Vec<(String, ArmedWake)> {
        self.log
            .read(|fold| {
                fold.wakes()
                    .due(now)
                    .map(|(key, armed)| (key.to_owned(), armed.clone()))
                    .collect()
            })
            .await
    }

    pub async fn next_due(&self) -> Option<u64> {
        self.log.read(|fold| fold.wakes().next_due()).await
    }

    pub async fn fire(
        &self,
        key: &str,
        generation: u64,
        now_ms: u64,
        submission: &Submission,
        next: Option<Wake>,
    ) -> Result<Fire, WakeError> {
        if let Some(next) = &next {
            next.fit().map_err(WakeError::Unfit)?;
        }
        let id = submission.request_id.as_str();
        let digest = submission_digest(&submission.prompt, submission.hand.as_deref())?;
        let mut lease = self.log.lease_within().await?;
        let prior = match lease.fold().wakes().get(key) {
            Some(armed) if armed.generation == generation => armed.clone(),
            _ => return Ok(Fire::Stale),
        };
        if !prior.due(now_ms) {
            return Ok(Fire::NotDue);
        }
        if let Some(turn) = lease.fold().turn(id) {
            return Ok(Fire::Taken(record_of(turn)?));
        }
        let first = lease.next_event();
        let at = now();
        let body = raw(&Lifecycle {
            kind: "queued",
            request_id: id,
        })?;
        let fired = self
            .fired(
                &mut lease, key, generation, submission, digest, first, at, &body, next,
            )
            .await;
        let next = match fired {
            Ok(next) => next,
            Err(error) => {
                lease.unadmit(id, first);
                lease.unwake(key, Some(prior));
                return Err(error.into());
            }
        };
        let record = record_of(
            lease
                .fold()
                .turn(id)
                .ok_or_else(|| invalid(format!("fired turn {id} is missing")))?,
        )?;
        assert_eq!(
            lease.fold().wakes().get(key).map(|armed| armed.generation),
            next,
            "a fired wake is gone or rearmed at its next generation"
        );
        Ok(Fire::Fired {
            event: logged(first, Some(id), at, body),
            next,
            record,
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one fire's parts, pushed under one lease"
    )]
    async fn fired(
        &self,
        lease: &mut Lease<'_>,
        key: &str,
        generation: u64,
        submission: &Submission,
        digest: String,
        first: u64,
        at: u64,
        body: &RawValue,
        next: Option<Wake>,
    ) -> Result<Option<u64>, StoreError> {
        let id = submission.request_id.as_str();
        lease.push(Record::Admitted {
            digest,
            first,
            hand: submission.hand.clone(),
            metadata: submission.metadata.clone(),
            prompt: raw(&submission.prompt)?,
            turn: id.to_owned(),
        })?;
        lease.push(Record::Event {
            at,
            event: body.to_owned(),
            seq: first,
            turn: Some(id.to_owned()),
        })?;
        if mutated(Mutation::FiredApart) {
            lease.commit(Point::Admission).await?;
        }
        lease.push(Record::Fired {
            generation,
            key: key.to_owned(),
            turn: id.to_owned(),
        })?;
        let rearmed = match next {
            Some(wake) => {
                let generation = lease.fold().wakes().next_generation();
                lease.push(Record::Armed {
                    generation,
                    key: key.to_owned(),
                    wake,
                })?;
                Some(generation)
            }
            None => None,
        };
        self.commit_wakes(lease, Point::Admission).await?;
        Ok(rearmed)
    }
}
