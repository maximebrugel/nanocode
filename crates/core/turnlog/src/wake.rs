use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, value::RawValue};

use crate::Record;

pub mod sim {
    use std::sync::atomic::{AtomicU8, Ordering};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum Mutation {
        None,
        FiredApart,
        SnapshotDropsWakes,
        IndexSkipsHear,
        Unbounded,
    }

    impl Mutation {
        pub const ALL: [Self; 4] = [
            Self::FiredApart,
            Self::SnapshotDropsWakes,
            Self::IndexSkipsHear,
            Self::Unbounded,
        ];

        #[must_use]
        pub const fn name(self) -> &'static str {
            match self {
                Self::None => "none",
                Self::FiredApart => "fired_apart",
                Self::SnapshotDropsWakes => "snapshot_drops_wakes",
                Self::IndexSkipsHear => "index_skips_hear",
                Self::Unbounded => "unbounded",
            }
        }

        #[must_use]
        pub fn parse(name: &str) -> Option<Self> {
            std::iter::once(Self::None)
                .chain(Self::ALL)
                .find(|mutation| mutation.name() == name)
        }
    }

    static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

    pub fn mutate(mutation: Mutation) {
        MUTATION.store(mutation as u8, Ordering::SeqCst);
    }

    #[must_use]
    pub fn mutated(mutation: Mutation) -> bool {
        MUTATION.load(Ordering::Relaxed) == mutation as u8
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Trigger {
    pub filter: BTreeMap<String, Vec<Value>>,
    pub heard_max: u64,
    pub topic: String,
}

impl Trigger {
    #[must_use]
    pub fn matches(&self, topic: &str, fields: &Map<String, Value>) -> bool {
        self.topic == topic
            && self.filter.iter().all(|(field, values)| {
                fields
                    .get(field)
                    .is_some_and(|value| values.contains(value))
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Wake {
    pub due_at: Option<u64>,
    pub on: Option<Trigger>,
    pub payload: Box<RawValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Unfit {
    Deaf,
    Unawaited,
    UnheardField(String),
    UnheardTopic,
}

impl std::fmt::Display for Unfit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deaf => formatter.write_str("a trigger needs room for at least one event"),
            Self::Unawaited => formatter.write_str("a wake needs a due time, a trigger or both"),
            Self::UnheardField(field) => {
                write!(formatter, "filter field {field} lists no value to match")
            }
            Self::UnheardTopic => formatter.write_str("a trigger needs a topic"),
        }
    }
}

impl std::error::Error for Unfit {}

impl Wake {
    pub fn fit(&self) -> Result<(), Unfit> {
        let Some(on) = &self.on else {
            return match self.due_at {
                Some(_) => Ok(()),
                None => Err(Unfit::Unawaited),
            };
        };
        if on.topic.is_empty() {
            return Err(Unfit::UnheardTopic);
        }
        if on.heard_max == 0 {
            return Err(Unfit::Deaf);
        }
        match on.filter.iter().find(|(_, values)| values.is_empty()) {
            Some((field, _)) => Err(Unfit::UnheardField(field.clone())),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ArmedWake {
    pub generation: u64,
    pub heard: Vec<Box<RawValue>>,
    pub wake: Wake,
}

impl ArmedWake {
    #[must_use]
    pub const fn due_at(&self) -> Option<u64> {
        if self.heard.is_empty() {
            self.wake.due_at
        } else {
            Some(0)
        }
    }

    #[must_use]
    pub fn due(&self, now: u64) -> bool {
        self.due_at().is_some_and(|at| at <= now)
    }

    #[must_use]
    pub fn hears(&self, topic: &str, fields: &Map<String, Value>) -> bool {
        self.wake
            .on
            .as_ref()
            .is_some_and(|on| on.matches(topic, fields))
    }

    #[must_use]
    pub fn full(&self) -> bool {
        self.wake
            .on
            .as_ref()
            .is_some_and(|on| self.heard.len() as u64 >= on.heard_max)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Wakes {
    generation: u64,
    held: BTreeMap<String, ArmedWake>,
}

impl Wakes {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ArmedWake> {
        self.held.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ArmedWake)> {
        self.held.iter().map(|(key, held)| (key.as_str(), held))
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    #[must_use]
    pub fn next_due(&self) -> Option<u64> {
        self.held.values().filter_map(ArmedWake::due_at).min()
    }

    pub fn due(&self, now: u64) -> impl Iterator<Item = (&str, &ArmedWake)> {
        self.iter().filter(move |(_, held)| held.due(now))
    }

    #[must_use]
    pub const fn next_generation(&self) -> u64 {
        self.generation + 1
    }

    pub(crate) fn restore(&mut self, key: &str, held: Option<ArmedWake>) {
        match held {
            Some(held) => {
                assert!(
                    held.generation <= self.generation,
                    "a restored wake is below the generation floor"
                );
                self.held.insert(key.to_owned(), held);
            }
            None => {
                self.held.remove(key);
            }
        }
    }

    pub(crate) fn apply(&mut self, record: &Record) -> Result<(), String> {
        match record {
            Record::Armed {
                generation,
                key,
                wake,
            } => self.arm(*generation, key, wake),
            Record::Heard {
                event,
                generation,
                key,
            } => {
                self.held_mut(key, *generation)?.heard.push(event.clone());
                Ok(())
            }
            Record::Disarmed { generation, key }
            | Record::Fired {
                generation, key, ..
            } => {
                self.held_mut(key, *generation)?;
                self.held.remove(key);
                Ok(())
            }
            Record::Woken { generation } => {
                if *generation < self.generation {
                    return Err(format!(
                        "wake floor {generation} is below generation {}",
                        self.generation
                    ));
                }
                self.generation = *generation;
                Ok(())
            }
            _ => unreachable!("only wake records reach the wakes"),
        }
    }

    fn arm(&mut self, generation: u64, key: &str, wake: &Wake) -> Result<(), String> {
        if generation <= self.generation {
            return Err(format!(
                "wake {key} generation {generation} does not follow {}",
                self.generation
            ));
        }
        wake.fit()
            .map_err(|unfit| format!("wake {key} is unfit: {unfit}"))?;
        let heard = self
            .held
            .remove(key)
            .map(|held| held.heard)
            .unwrap_or_default();
        self.generation = generation;
        self.held.insert(
            key.to_owned(),
            ArmedWake {
                generation,
                heard,
                wake: wake.clone(),
            },
        );
        Ok(())
    }

    fn held_mut(&mut self, key: &str, generation: u64) -> Result<&mut ArmedWake, String> {
        match self.held.get_mut(key) {
            Some(held) if held.generation == generation => Ok(held),
            Some(held) => Err(format!(
                "wake {key} is at generation {}, not {generation}",
                held.generation
            )),
            None => Err(format!("wake {key} is not armed")),
        }
    }

    pub(crate) fn records<E>(
        &self,
        mut take: impl FnMut(Record) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut held = self.held.iter().collect::<Vec<_>>();
        held.sort_by_key(|(_, held)| held.generation);
        if sim::mutated(sim::Mutation::SnapshotDropsWakes) {
            held.clear();
        }
        for (key, held) in held {
            take(Record::Armed {
                generation: held.generation,
                key: key.clone(),
                wake: held.wake.clone(),
            })?;
            for event in &held.heard {
                take(Record::Heard {
                    event: event.clone(),
                    generation: held.generation,
                    key: key.clone(),
                })?;
            }
        }
        if self.generation > 0 {
            take(Record::Woken {
                generation: self.generation,
            })?;
        }
        Ok(())
    }
}
