use std::{collections::BTreeMap, num::NonZeroUsize};

use nanocode_board::{
    deliver::{Delivery, Ingest, Key, Outbox, Refusal},
    sim::Mutation as BoardRule,
    watch::BATCH,
};

use crate::rng::Rng;

pub const ALL: [BoardRule; 2] = [BoardRule::DeliveryIngestedTwice, BoardRule::DeliveryLost];

pub const fn name(rule: BoardRule) -> &'static str {
    match rule {
        BoardRule::DeliveryIngestedTwice => "double_ingest",
        BoardRule::DeliveryLost => "lost_delivery",
        _ => "board",
    }
}

const TAKE: NonZeroUsize = match NonZeroUsize::new(BATCH) {
    Some(take) => take,
    None => panic!("the watch batch is not empty"),
};
const FANOUT_MAX: u64 = 3;

#[derive(Clone, Copy, Debug, Default)]
pub struct DeliverStats {
    pub posted: u64,
    pub ingested: u64,
    pub again: u64,
    pub fenced: u64,
    pub conflicts: u64,
    pub takeovers: u64,
}

struct Batch {
    part: u32,
    epoch: u64,
    taken: Vec<Delivery>,
}

struct Step {
    part: u32,
    epoch: u64,
    source: String,
    seq: u64,
    posts: Vec<Delivery>,
}

struct Db {
    outbox: Outbox,
    epochs: Vec<u64>,
    seqs: BTreeMap<String, u64>,
    logs: BTreeMap<String, Vec<Key>>,
    expected: BTreeMap<Key, String>,
    stats: DeliverStats,
}

impl Db {
    fn fenced(&mut self, part: u32, epoch: u64) -> bool {
        let stale = self.epochs[part as usize] != epoch;
        if stale {
            self.stats.fenced += 1;
        }
        stale
    }

    fn step(&mut self, step: Step) -> Result<(), String> {
        if self.fenced(step.part, step.epoch) {
            return Ok(());
        }
        if self.seqs.get(&step.source).copied().unwrap_or(0) + 1 != step.seq {
            self.stats.conflicts += 1;
            return Ok(());
        }
        let mut outbox = self.outbox.clone();
        for delivery in &step.posts {
            outbox
                .post(delivery.clone())
                .map_err(|refusal| format!("post {:?}: {refusal:?}", delivery.key))?;
        }
        self.outbox = outbox;
        self.seqs.insert(step.source, step.seq);
        for delivery in step.posts {
            self.stats.posted += 1;
            self.expected.insert(delivery.key, delivery.target);
        }
        Ok(())
    }

    fn ingest(&mut self, batch: Batch) -> Result<(), String> {
        if self.fenced(batch.part, batch.epoch) {
            return Ok(());
        }
        let mut outbox = self.outbox.clone();
        let mut landed = Vec::new();
        for delivery in batch.taken {
            if delivery.part != batch.part {
                return Err(format!(
                    "{:?} ingested by partition {}",
                    delivery.key, batch.part
                ));
            }
            match outbox.ingest(&delivery.key) {
                Ok(Ingest::First) => landed.push(delivery),
                Ok(Ingest::Again) => self.stats.again += 1,
                Err(Refusal::Unknown | Refusal::Exists) => {
                    return Err(format!("ingest of unposted {:?}", delivery.key));
                }
            }
        }
        self.outbox = outbox;
        for delivery in landed {
            self.stats.ingested += 1;
            self.logs
                .entry(delivery.target)
                .or_default()
                .push(delivery.key);
        }
        Ok(())
    }

    fn take(&self, part: u32) -> Batch {
        Batch {
            part,
            epoch: self.epochs[part as usize],
            taken: self
                .outbox
                .undelivered(part, TAKE)
                .into_iter()
                .cloned()
                .collect(),
        }
    }

    fn takeover(&mut self, part: u32) -> Result<(), String> {
        self.stats.takeovers += 1;
        self.epochs[part as usize] += 1;
        let body = serde_json::to_string(&self.outbox).map_err(|error| error.to_string())?;
        self.outbox = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn thread(index: u64) -> String {
    format!("thread-{index}")
}

pub fn run(seed: u64) -> Result<DeliverStats, String> {
    let mut rng = Rng::derive(seed, "board deliver");
    let parts = rng.range(2, 5) as u32;
    let threads = rng.range(u64::from(parts) + 1, 13);
    let rounds = rng.range(100, 400);
    let takeover_ppm = rng.range(5_000, 60_000);
    let part = |index: u64| (index % u64::from(parts)) as u32;
    let mut db = Db {
        outbox: Outbox::new(),
        epochs: vec![1; parts as usize],
        seqs: BTreeMap::new(),
        logs: BTreeMap::new(),
        expected: BTreeMap::new(),
        stats: DeliverStats::default(),
    };
    let mut steps: Vec<Step> = Vec::new();
    let mut batches: Vec<Batch> = Vec::new();
    for _ in 0..rounds {
        match rng.below(6) {
            0 | 1 => {
                let source = rng.below(threads);
                let name = thread(source);
                let seq = db.seqs.get(&name).copied().unwrap_or(0) + 1;
                let fanout = rng.range(1, FANOUT_MAX + 1);
                let posts = (0..fanout)
                    .map(|index| {
                        let target = (source + 1 + rng.below(threads - 1)) % threads;
                        Delivery {
                            key: Key {
                                source: name.clone(),
                                seq,
                                index: index as u32,
                            },
                            target: thread(target),
                            part: part(target),
                            payload: format!("{name}/{seq}/{index}"),
                        }
                    })
                    .collect();
                let p = part(source);
                steps.push(Step {
                    part: p,
                    epoch: db.epochs[p as usize],
                    source: name,
                    seq,
                    posts,
                });
            }
            2 if !steps.is_empty() => {
                let step = steps.remove(rng.pick(steps.len()));
                db.step(step)?;
            }
            3 => batches.push(db.take(rng.below(u64::from(parts)) as u32)),
            4 if !batches.is_empty() => {
                let batch = batches.remove(rng.pick(batches.len()));
                db.ingest(batch)?;
            }
            _ => {}
        }
        if rng.chance(takeover_ppm) {
            let lost = rng.below(u64::from(parts)) as u32;
            if rng.chance(500_000) {
                batches.retain(|batch| batch.part != lost);
                steps.retain(|step| step.part != lost);
            }
            db.takeover(lost)?;
        }
    }
    for step in steps.drain(..) {
        db.step(step)?;
    }
    for batch in batches.drain(..) {
        db.ingest(batch)?;
    }
    let owed = db.stats.posted as usize / TAKE + parts as usize + 1;
    for _ in 0..owed {
        for p in 0..parts {
            let batch = db.take(p);
            db.ingest(batch)?;
        }
    }
    if db.outbox.pending() > 0 {
        return Err(format!("{} deliveries never drained", db.outbox.pending()));
    }
    let mut seen: BTreeMap<&Key, u64> = BTreeMap::new();
    for (target, keys) in &db.logs {
        for key in keys {
            match db.expected.get(key) {
                Some(owner) if owner == target => *seen.entry(key).or_default() += 1,
                Some(owner) => return Err(format!("{key:?} for {owner} landed on {target}")),
                None => return Err(format!("{key:?} landed on {target} but no step posted it")),
            }
        }
    }
    for (key, target) in &db.expected {
        match seen.get(key).copied().unwrap_or(0) {
            1 => {}
            0 => return Err(format!("delivery {key:?} to {target} was never ingested")),
            n => {
                return Err(format!(
                    "delivery {key:?} to {target} was ingested {n} times"
                ));
            }
        }
    }
    Ok(db.stats)
}
