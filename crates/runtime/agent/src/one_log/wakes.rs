use nanocode_turnlog::{
    ArmedWake, Point, Record, Unfit, Wake,
    wake_sim::{Mutation, mutated},
};
use serde_json::{Map, Value, value::RawValue};

use super::{Lifecycle, OneLog, invalid, logged, raw, record_of};
use crate::{
    durability::StoreError,
    session_log::{LoggedEvent, Submission, TurnRecord, now, submission_digest},
};

#[derive(Debug)]
pub enum WakeError {
    Store(StoreError),
    Unfit(Unfit),
}

impl std::fmt::Display for WakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
    pub async fn arm(&self, key: &str, wake: Wake) -> Result<u64, WakeError> {
        wake.fit().map_err(WakeError::Unfit)?;
        let mut lease = self.log.lease().await;
        let prior = lease.fold().wakes().get(key).cloned();
        let generation = lease.fold().wakes().next_generation();
        lease.push(Record::Armed {
            generation,
            key: key.to_owned(),
            wake,
        })?;
        if let Err(error) = lease.commit(Point::Progress).await {
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
        let mut lease = self.log.lease().await;
        let Some(prior) = lease.fold().wakes().get(key).cloned() else {
            return Ok(None);
        };
        let generation = prior.generation;
        lease.push(Record::Disarmed {
            generation,
            key: key.to_owned(),
        })?;
        if let Err(error) = lease.commit(Point::Progress).await {
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
        let mut lease = self.log.lease().await;
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
        if let Err(error) = lease.commit(Point::Progress).await {
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
        let mut lease = self.log.lease().await;
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
        let fired = Self::fired(
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
        lease: &mut nanocode_turnlog::Lease<'_>,
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
        lease.commit(Point::Admission).await?;
        Ok(rearmed)
    }
}
