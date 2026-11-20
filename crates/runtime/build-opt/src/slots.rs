use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::idle::KEPT_LABEL_PREFIX;

const FRESH_MS: u64 = 15 * 60 * 1000;

/// No lease may run into a box's last 15 minutes: the box is swapped then.
pub const CLOSE_WINDOW_MS: u64 = 15 * 60_000;

/// A waiter whose take has not retried for this long (its client died) is dropped.
pub const WAITER_GONE_MS: u64 = 2 * 60_000;

/// A standing reservation (label prefix P) with no lease inside it (no take labelled P*) for this
/// long is given back whenever a client reads or writes the state.
pub const RESERVATION_IDLE_MS: u64 = 15 * 60_000;

/// Standing reservations under these label prefixes are never given back for being idle.
pub const IDLE_EXEMPT_PREFIXES: [&str; 4] =
    ["p2live-", "nf-deploy-", "lander-p2live", "boardrel-red-"];

/// Whether a standing reservation with label prefix P is kept however long it sits idle.
pub fn idle_exempt(prefix: &str) -> bool {
    IDLE_EXEMPT_PREFIXES
        .iter()
        .any(|exempt| prefix.starts_with(exempt))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pin {
    Taskset,
    Nice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Fill,
    LastResort,
    Named,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pool {
    Large,
    Small,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    pub cores: u32,
    pub expires_ms: Option<u64>,
    pub pin: Pin,
    pub placement: Placement,
    pub kinds: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub load: f64,
    pub cores: u32,
    pub mem_total_kb: u64,
    pub mem_available_kb: u64,
    pub at_ms: u64,
    pub runnable_cpus: Option<Vec<u32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub id: String,
    pub machine: String,
    pub first_cpu: u32,
    pub cores: u32,
    pub kind: String,
    pub label: String,
    pub taken_ms: u64,
    pub until_ms: u64,
    /// For a standing reservation: when it was made or a lease was last taken inside it. Optional
    /// in the state, so files written before it load; a reservation without it counts from the
    /// first time the state is read.
    pub used_ms: Option<u64>,
}

pub const DEFAULT_KIND: &str = "build";

pub const RESERVED_KIND: &str = "reserved";

pub const PREFIX_MARK: char = '*';

pub const BENCH_KIND: &str = "bench";

pub const CLOSED_LOAD_KIND: &str = "closed-load";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cap {
    pub lane: String,
    pub prefixes: Vec<String>,
    pub cores: u32,
    pub kinds: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    NoRoom,
    OverCap { lane: String, held: u32, cap: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub cores: u32,
    pub kind: String,
    pub label: String,
    pub only: Option<String>,
    pub ttl_ms: u64,
    pub queue: Option<Queue>,
}

/// A take --wait's place in line: its id (stable across its retries), when it first asked and for how long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queue {
    pub id: String,
    pub since_ms: u64,
    pub wait_ms: u64,
}

/// A take --wait still waiting, recorded in the state so cpus can be held for the oldest one per machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Waiter {
    pub id: String,
    pub machine: Option<String>,
    pub cores: u32,
    pub kind: String,
    pub label: String,
    pub ttl_ms: u64,
    pub since_ms: u64,
    pub until_ms: u64,
    pub seen_ms: u64,
}

/// The cpus first_cpu..end_cpu on machine held for waiter, whose N contiguous cpus are all free at ready_ms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    pub machine: String,
    pub waiter: String,
    pub first_cpu: u32,
    pub end_cpu: u32,
    pub ready_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub observations: BTreeMap<String, Observation>,
    pub leases: Vec<Lease>,
    pub next: u64,
    pub boxes: Option<Value>,
    pub caps: Vec<Cap>,
    pub waiters: Vec<Waiter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub cores: u32,
    pub leased: u32,
    pub busy: u32,
    pub memory_ok: bool,
    pub observed: bool,
}

impl Lease {
    pub fn cpus(&self) -> String {
        format!("{}-{}", self.first_cpu, self.first_cpu + self.cores - 1)
    }

    pub fn label_prefix(&self) -> Option<&str> {
        (self.kind == RESERVED_KIND)
            .then(|| self.label.strip_suffix(PREFIX_MARK))
            .flatten()
    }

    const fn end_cpu(&self) -> u32 {
        self.first_cpu + self.cores
    }

    fn within(&self, outer: &Self) -> bool {
        self.id != outer.id
            && self.machine == outer.machine
            && outer.first_cpu <= self.first_cpu
            && self.end_cpu() <= outer.end_cpu()
    }

    pub fn to_json(&self, machine: Option<&Machine>) -> Value {
        let prefix = match machine.map(|machine| machine.pin) {
            Some(Pin::Nice) => "nice -n 19".to_owned(),
            _ => format!("taskset -c {}", self.cpus()),
        };
        json!({
            "lease": self.id,
            "machine": self.machine,
            "hand": self.machine,
            "cores": self.cores,
            "cpus": self.cpus(),
            "prefix": prefix,
            "kind": self.kind,
            "env": format!(
                "export NC_BUILD_LEASE={} NC_BUILD_LEASE_CORES={} CARGO_BUILD_JOBS={} NC_BUILD_PREFIX='{prefix}'",
                self.id, self.cores, self.cores
            ),
            "label": self.label,
            "taken_ms": self.taken_ms,
            "until_ms": self.until_ms,
        })
    }
}

impl Waiter {
    pub const fn live(&self, now_ms: u64) -> bool {
        self.until_ms > now_ms && self.seen_ms + WAITER_GONE_MS > now_ms
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            id: text(value, "id")?,
            machine: text(value, "machine"),
            cores: u32::try_from(number(value, "cores")?).ok()?,
            kind: text(value, "kind").unwrap_or_else(|| DEFAULT_KIND.to_owned()),
            label: text(value, "label")?,
            ttl_ms: number(value, "ttl_ms")?,
            since_ms: number(value, "since_ms")?,
            until_ms: number(value, "until_ms")?,
            seen_ms: number(value, "seen_ms")?,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "machine": self.machine,
            "cores": self.cores,
            "kind": self.kind,
            "label": self.label,
            "ttl_ms": self.ttl_ms,
            "since_ms": self.since_ms,
            "until_ms": self.until_ms,
            "seen_ms": self.seen_ms,
        })
    }
}

impl Cap {
    pub fn covers(&self, kind: &str, label: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| label.starts_with(prefix.as_str()))
            && self.kinds.as_ref().map_or(kind != BENCH_KIND, |kinds| {
                kinds.iter().any(|covered| covered == kind)
            })
    }

    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "lane": self.lane,
            "prefixes": self.prefixes,
            "cores": self.cores,
        });
        if let Some(kinds) = &self.kinds {
            value["kinds"] = json!(kinds);
        }
        value
    }
}

fn strings(value: &Value, what: &str) -> Result<Vec<String>, String> {
    let items = value
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or_else(|| format!("{what}: expected a non-empty array of strings"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{what}: expected non-empty strings"))
        })
        .collect()
}

pub fn caps_from_json(value: &Value) -> Result<Vec<Cap>, String> {
    let entries = value
        .as_array()
        .ok_or("caps: expected a JSON array of {lane, prefixes, cores, kinds?}")?;
    let mut caps: Vec<Cap> = Vec::new();
    for entry in entries {
        let lane = text(entry, "lane")
            .filter(|lane| !lane.is_empty())
            .ok_or("caps: every entry needs a non-empty lane")?;
        if caps.iter().any(|cap| cap.lane == lane) {
            return Err(format!("caps: lane {lane} appears twice"));
        }
        let prefixes = strings(
            entry.get("prefixes").unwrap_or(&Value::Null),
            &format!("caps: lane {lane} prefixes"),
        )?;
        let cores = number(entry, "cores")
            .and_then(|cores| u32::try_from(cores).ok())
            .ok_or_else(|| format!("caps: lane {lane} needs cores, a whole number"))?;
        let kinds = match entry.get("kinds") {
            None | Some(Value::Null) => None,
            Some(kinds) => Some(strings(kinds, &format!("caps: lane {lane} kinds"))?),
        };
        caps.push(Cap {
            lane,
            prefixes,
            cores,
            kinds,
        });
    }
    Ok(caps)
}

fn number(value: &Value, name: &str) -> Option<u64> {
    value.get(name)?.as_u64()
}

fn text(value: &Value, name: &str) -> Option<String> {
    value.get(name)?.as_str().map(str::to_owned)
}

impl State {
    pub fn from_json(json: &str) -> Option<Self> {
        if json.trim().is_empty() {
            return Some(Self::default());
        }
        let value: Value = serde_json::from_str(json).ok()?;
        let mut observations = BTreeMap::new();
        for (name, observation) in value.get("observations")?.as_object()? {
            observations.insert(
                name.clone(),
                Observation {
                    load: observation.get("load")?.as_f64()?,
                    cores: u32::try_from(number(observation, "cores")?).ok()?,
                    mem_total_kb: number(observation, "mem_total_kb")?,
                    mem_available_kb: number(observation, "mem_available_kb")?,
                    at_ms: number(observation, "at_ms")?,
                    runnable_cpus: match observation.get("runnable_cpus") {
                        None | Some(Value::Null) => None,
                        Some(counts) => Some(
                            counts
                                .as_array()?
                                .iter()
                                .map(|count| u32::try_from(count.as_u64()?).ok())
                                .collect::<Option<Vec<u32>>>()?,
                        ),
                    },
                },
            );
        }
        let mut leases = Vec::new();
        for lease in value.get("leases")?.as_array()? {
            leases.push(Lease {
                id: text(lease, "id")?,
                machine: text(lease, "machine")?,
                first_cpu: u32::try_from(number(lease, "first_cpu")?).ok()?,
                cores: u32::try_from(number(lease, "cores")?).ok()?,
                kind: text(lease, "kind").unwrap_or_else(|| DEFAULT_KIND.to_owned()),
                label: text(lease, "label")?,
                taken_ms: number(lease, "taken_ms")?,
                until_ms: number(lease, "until_ms")?,
                used_ms: number(lease, "used_ms"),
            });
        }
        Some(Self {
            observations,
            leases,
            next: number(&value, "next")?,
            boxes: value.get("boxes").filter(|boxes| !boxes.is_null()).cloned(),
            caps: match value.get("caps") {
                None | Some(Value::Null) => Vec::new(),
                Some(caps) => caps_from_json(caps).ok()?,
            },
            waiters: value
                .get("waiters")
                .and_then(Value::as_array)
                .map(|waiters| waiters.iter().filter_map(Waiter::from_json).collect())
                .unwrap_or_default(),
        })
    }

    pub fn to_json(&self) -> String {
        let observations: serde_json::Map<String, Value> = self
            .observations
            .iter()
            .map(|(name, observation)| {
                (
                    name.clone(),
                    json!({
                        "load": observation.load,
                        "cores": observation.cores,
                        "mem_total_kb": observation.mem_total_kb,
                        "mem_available_kb": observation.mem_available_kb,
                        "at_ms": observation.at_ms,
                        "runnable_cpus": observation.runnable_cpus,
                    }),
                )
            })
            .collect();
        let leases: Vec<Value> = self
            .leases
            .iter()
            .map(|lease| {
                let mut value = json!({
                    "id": lease.id,
                    "machine": lease.machine,
                    "first_cpu": lease.first_cpu,
                    "cores": lease.cores,
                    "kind": lease.kind,
                    "label": lease.label,
                    "taken_ms": lease.taken_ms,
                    "until_ms": lease.until_ms,
                });
                if let Some(used_ms) = lease.used_ms {
                    value["used_ms"] = json!(used_ms);
                }
                value
            })
            .collect();
        let mut value = json!({
            "observations": observations,
            "leases": leases,
            "next": self.next,
            "boxes": self.boxes,
        });
        if !self.waiters.is_empty() {
            value["waiters"] = self.waiters.iter().map(Waiter::to_json).collect();
        }
        serde_json::to_string_pretty(&value).unwrap_or_default()
    }

    pub fn expire(&mut self, now_ms: u64) -> Vec<Lease> {
        let (expired, live) = self
            .leases
            .drain(..)
            .partition(|lease| lease.until_ms <= now_ms);
        self.leases = live;
        self.waiters.retain(|waiter| waiter.live(now_ms));
        expired
    }

    /// Gives back every standing reservation that has had no lease inside it for
    /// RESERVATION_IDLE_MS, except those under IDLE_EXEMPT_PREFIXES. A live lease inside one
    /// counts as use now, and one with no recorded use (an older state) starts counting now.
    pub fn give_back_idle(&mut self, now_ms: u64) -> Vec<Lease> {
        let in_use: Vec<String> = self
            .leases
            .iter()
            .filter(|outer| {
                outer.label_prefix().is_some_and(|prefix| {
                    self.leases.iter().any(|lease| {
                        lease.until_ms > now_ms
                            && lease.label.starts_with(prefix)
                            && lease.within(outer)
                    })
                })
            })
            .map(|outer| outer.id.clone())
            .collect();
        for lease in &mut self.leases {
            if lease.label_prefix().is_some()
                && (lease.used_ms.is_none() || in_use.contains(&lease.id))
            {
                lease.used_ms = Some(now_ms);
            }
        }
        let (idle, kept) = self.leases.drain(..).partition(|lease| {
            lease
                .label_prefix()
                .is_some_and(|prefix| !idle_exempt(prefix))
                && lease
                    .used_ms
                    .is_some_and(|used| now_ms.saturating_sub(used) >= RESERVATION_IDLE_MS)
        });
        self.leases = kept;
        idle
    }

    pub fn leased_cpus(&self, name: &str, cores: u32) -> u32 {
        let mut covered = vec![false; cores as usize];
        for lease in self.leases.iter().filter(|lease| lease.machine == name) {
            for cpu in lease.first_cpu..lease.end_cpu().min(cores) {
                covered[cpu as usize] = true;
            }
        }
        covered.iter().map(|&leased| u32::from(leased)).sum()
    }

    pub fn reap_gone(&mut self, machines: &[Machine]) -> Vec<Lease> {
        let (gone, live) = self
            .leases
            .drain(..)
            .partition(|lease| !machines.iter().any(|machine| machine.name == lease.machine));
        self.leases = live;
        gone
    }

    pub fn usage(&self, machine: &Machine, now_ms: u64) -> Usage {
        let leases: Vec<&Lease> = self
            .leases
            .iter()
            .filter(|lease| lease.machine == machine.name)
            .collect();
        let fresh = self
            .observations
            .get(&machine.name)
            .filter(|observation| now_ms.saturating_sub(observation.at_ms) <= FRESH_MS);
        let cores = fresh.map_or(machine.cores, |observation| {
            observation.cores.min(machine.cores)
        });
        let leased = self.leased_cpus(&machine.name, cores);
        let unleased = fresh.map_or(0, |observation| {
            match (machine.pin, &observation.runnable_cpus) {
                (Pin::Taskset, Some(counts)) => (0..)
                    .zip(counts)
                    .filter(|(cpu, _)| {
                        !leases.iter().any(|lease| {
                            lease.taken_ms < observation.at_ms
                                && lease.first_cpu <= *cpu
                                && *cpu < lease.first_cpu + lease.cores
                        })
                    })
                    .map(|(_, count)| count)
                    .sum(),
                _ => (observation.load.ceil() as u32).saturating_sub(leased),
            }
        });
        let memory_ok = fresh.is_none_or(|observation| {
            observation.mem_available_kb.saturating_mul(10) >= observation.mem_total_kb
        });
        Usage {
            cores,
            leased,
            busy: leased + unleased,
            memory_ok,
            observed: fresh.is_some(),
        }
    }

    pub fn held(&self, cap: &Cap) -> u32 {
        self.leases
            .iter()
            .filter(|lease| {
                lease.kind != RESERVED_KIND
                    && lease.kind != CLOSED_LOAD_KIND
                    && !lease.label.starts_with(KEPT_LABEL_PREFIX)
                    && cap.covers(&lease.kind, &lease.label)
                    && !self.leases.iter().any(|outer| {
                        outer
                            .label_prefix()
                            .is_some_and(|prefix| lease.label.starts_with(prefix))
                            && lease.within(outer)
                    })
            })
            .map(|lease| lease.cores)
            .sum()
    }

    fn gap(&self, machine: &Machine, cores: u32, want: u32, held: &[(u32, u32)]) -> Option<u32> {
        let mut used: Vec<(u32, u32)> = self
            .leases
            .iter()
            .filter(|lease| lease.machine == machine.name)
            .map(|lease| (lease.first_cpu, lease.first_cpu + lease.cores))
            .chain(held.iter().copied())
            .collect();
        used.sort_unstable();
        let mut start = 0;
        for (first, end) in used {
            if first >= start + want {
                break;
            }
            start = start.max(end);
        }
        (start + want <= cores).then_some(start)
    }

    fn gap_within(&self, held: &Lease, want: u32) -> Option<u32> {
        let mut used: Vec<(u32, u32)> = self
            .leases
            .iter()
            .filter(|lease| {
                lease.id != held.id
                    && lease.machine == held.machine
                    && lease.first_cpu < held.end_cpu()
                    && held.first_cpu < lease.end_cpu()
            })
            .map(|lease| (lease.first_cpu, lease.end_cpu()))
            .collect();
        used.sort_unstable();
        let mut start = held.first_cpu;
        for (first, end) in used {
            if first >= start + want {
                break;
            }
            start = start.max(end);
        }
        (start + want <= held.end_cpu()).then_some(start)
    }

    /// The window of want contiguous cpus on machine that is all free soonest (now when it is
    /// free now): its first cpu and when it is free, the latest end of the leases over it.
    pub fn window(&self, machine: &str, cores: u32, want: u32, now_ms: u64) -> Option<(u32, u64)> {
        if want == 0 || want > cores {
            return None;
        }
        let leases: Vec<&Lease> = self
            .leases
            .iter()
            .filter(|lease| lease.machine == machine)
            .collect();
        (0..=cores - want)
            .map(|start| {
                let ready = leases
                    .iter()
                    .filter(|lease| lease.first_cpu < start + want && start < lease.end_cpu())
                    .map(|lease| lease.until_ms)
                    .fold(now_ms, u64::max);
                (ready, start)
            })
            .min()
            .map(|(ready, start)| (start, ready))
    }

    fn over_cap(&self, kind: &str, label: &str, cores: u32) -> bool {
        self.caps
            .iter()
            .any(|cap| cap.covers(kind, label) && self.held(cap) + cores > cap.cores)
    }

    /// The cpus held for waiting takes: the oldest live waiter on each machine (by request time;
    /// one without --machine on the machine where its window is free soonest) holds the window of
    /// its N cpus that frees soonest, unless it cannot fit before that machine's close window or
    /// its lane is over its cap, so it could not take there anyway.
    pub fn holds(&self, machines: &[Machine], now_ms: u64) -> Vec<Hold> {
        let mut waiters: Vec<&Waiter> = self
            .waiters
            .iter()
            .filter(|waiter| {
                waiter.live(now_ms) && !self.over_cap(&waiter.kind, &waiter.label, waiter.cores)
            })
            .collect();
        waiters.sort_by(|a, b| (a.since_ms, &a.id).cmp(&(b.since_ms, &b.id)));
        let mut holds: Vec<Hold> = Vec::new();
        for waiter in waiters {
            let soonest = machines
                .iter()
                .filter(|machine| {
                    waiter
                        .machine
                        .as_deref()
                        .is_none_or(|name| name == machine.name)
                        && machine
                            .kinds
                            .as_ref()
                            .is_none_or(|kinds| kinds.contains(&waiter.kind))
                        && !holds.iter().any(|hold| hold.machine == machine.name)
                        && (waiter.machine.is_some() || machine.placement == Placement::Fill)
                })
                .filter_map(|machine| {
                    let cores = self.usage(machine, now_ms).cores;
                    let (first_cpu, ready_ms) =
                        self.window(&machine.name, cores, waiter.cores, now_ms)?;
                    let fits = machine.expires_ms.is_none_or(|expires| {
                        expires.saturating_sub(CLOSE_WINDOW_MS) >= ready_ms + waiter.ttl_ms
                    });
                    fits.then_some((ready_ms, machine, first_cpu))
                })
                .min_by_key(|(ready_ms, _, _)| *ready_ms);
            if let Some((ready_ms, machine, first_cpu)) = soonest {
                holds.push(Hold {
                    machine: machine.name.clone(),
                    waiter: waiter.id.clone(),
                    first_cpu,
                    end_cpu: first_cpu + waiter.cores,
                    ready_ms,
                });
            }
        }
        holds
    }

    fn enqueue(&mut self, want: &Want, now_ms: u64) {
        let Some(queue) = &want.queue else { return };
        let until_ms = queue.since_ms.saturating_add(queue.wait_ms);
        if let Some(waiter) = self.waiters.iter_mut().find(|waiter| waiter.id == queue.id) {
            // A waiter moved off a closing box waits for any machine from then on.
            if waiter.machine != want.only {
                waiter.machine.clone_from(&want.only);
            }
            // Refresh at most every 30s, so a waiter's 5s retries don't rewrite the state each time.
            if now_ms >= waiter.seen_ms + WAITER_GONE_MS / 4 {
                waiter.seen_ms = now_ms;
            }
        } else if until_ms > now_ms {
            self.waiters.push(Waiter {
                id: queue.id.clone(),
                machine: want.only.clone(),
                cores: want.cores,
                kind: want.kind.clone(),
                label: want.label.clone(),
                ttl_ms: want.ttl_ms,
                since_ms: queue.since_ms,
                until_ms,
                seen_ms: now_ms,
            });
        }
    }

    /// Takes a lease; a take with a queue (take --wait) is recorded as a waiter until it gets one or
    /// its wait ends, and every take sees the cpus held for an older waiter as busy, except a
    /// backfill: a take whose lease ends by the time that waiter's window is free.
    pub fn take(
        &mut self,
        machines: &[Machine],
        want: &Want,
        now_ms: u64,
    ) -> Result<Lease, Refusal> {
        self.expire(now_ms);
        self.enqueue(want, now_ms);
        let taken = self.place(machines, want, now_ms);
        if let (Ok(_), Some(queue)) = (&taken, &want.queue) {
            self.waiters.retain(|waiter| waiter.id != queue.id);
        }
        taken
    }

    fn place(&mut self, machines: &[Machine], want: &Want, now_ms: u64) -> Result<Lease, Refusal> {
        let standing = self.leases.iter().find_map(|held| {
            let prefix = held.label_prefix()?;
            if !want.label.starts_with(prefix)
                || want
                    .only
                    .as_deref()
                    .is_some_and(|name| name != held.machine)
            {
                return None;
            }
            Some((
                held.id.clone(),
                held.machine.clone(),
                self.gap_within(held, want.cores)?,
            ))
        });
        if let Some((id, machine, first_cpu)) = standing {
            if let Some(held) = self.leases.iter_mut().find(|held| held.id == id) {
                held.used_ms = Some(now_ms);
            }
            return Ok(self.add(&machine, first_cpu, want, now_ms, now_ms + want.ttl_ms));
        }
        let claim = self.leases.iter().position(|lease| {
            lease.kind == RESERVED_KIND
                && lease.label_prefix().is_none()
                && lease.label == want.label
                && lease.cores >= want.cores
                && want
                    .only
                    .as_deref()
                    .is_none_or(|name| name == lease.machine)
        });
        let cores = claim
            .and_then(|at| self.leases.get(at))
            .map_or(want.cores, |lease| lease.cores);
        if let Some((cap, held)) = self
            .caps
            .iter()
            .filter(|cap| cap.covers(&want.kind, &want.label))
            .map(|cap| (cap, self.held(cap)))
            .find(|(cap, held)| held + cores > cap.cores)
        {
            return Err(Refusal::OverCap {
                lane: cap.lane.clone(),
                held,
                cap: cap.cores,
            });
        }
        if let Some(held) = claim.and_then(|at| self.leases.get_mut(at)) {
            held.kind.clone_from(&want.kind);
            held.taken_ms = now_ms;
            held.until_ms = now_ms + want.ttl_ms;
            return Ok(held.clone());
        }
        let mine = want.queue.as_ref().map(|queue| queue.id.as_str());
        let holds = self.holds(machines, now_ms);
        let mut best: Option<(u32, &Machine, u32)> = None;
        for machine in machines {
            if want
                .only
                .as_deref()
                .is_some_and(|name| name != machine.name)
                || machine
                    .kinds
                    .as_ref()
                    .is_some_and(|kinds| !kinds.contains(&want.kind))
                || machine
                    .expires_ms
                    .is_some_and(|expires| expires <= now_ms + want.ttl_ms.min(10 * 60 * 1000))
            {
                continue;
            }
            let usage = self.usage(machine, now_ms);
            let held: Vec<(u32, u32)> = holds
                .iter()
                .filter(|hold| {
                    hold.machine == machine.name
                        && Some(hold.waiter.as_str()) != mine
                        && now_ms + want.ttl_ms > hold.ready_ms
                })
                .map(|hold| (hold.first_cpu, hold.end_cpu.min(usage.cores)))
                .collect();
            let held_free: u32 = held
                .iter()
                .flat_map(|&(first, end)| first..end)
                .filter(|&cpu| {
                    !self.leases.iter().any(|lease| {
                        lease.machine == machine.name
                            && lease.first_cpu <= cpu
                            && cpu < lease.end_cpu()
                    })
                })
                .map(|_| 1)
                .sum();
            if !usage.memory_ok
                || usage.busy + held_free + want.cores > usage.cores
                || (machine.placement == Placement::LastResort && !usage.observed)
                || (machine.placement == Placement::Named && want.only.is_none())
            {
                continue;
            }
            let Some(first_cpu) = self.gap(machine, usage.cores, want.cores, &held) else {
                continue;
            };
            let same_kind = self
                .leases
                .iter()
                .filter(|lease| lease.machine == machine.name && lease.kind == want.kind)
                .map(|lease| lease.cores)
                .sum();
            if best.is_none_or(|(least, _, _)| same_kind < least) {
                best = Some((same_kind, machine, first_cpu));
            }
        }
        let (_, machine, first_cpu) = best.ok_or(Refusal::NoRoom)?;
        Ok(self.add(&machine.name, first_cpu, want, now_ms, now_ms + want.ttl_ms))
    }

    fn add(
        &mut self,
        machine: &str,
        first_cpu: u32,
        want: &Want,
        now_ms: u64,
        until_ms: u64,
    ) -> Lease {
        self.next += 1;
        let lease = Lease {
            id: format!("{now_ms}-{}", self.next),
            machine: machine.to_owned(),
            first_cpu,
            cores: want.cores,
            kind: want.kind.clone(),
            label: want.label.clone(),
            taken_ms: now_ms,
            until_ms,
            used_ms: None,
        };
        let lease = Lease {
            used_ms: lease.label_prefix().map(|_| now_ms),
            ..lease
        };
        self.leases.push(lease.clone());
        lease
    }

    pub fn reserve(
        &mut self,
        machine: &str,
        first_cpu: u32,
        want: &Want,
        now_ms: u64,
        until_ms: u64,
        overlap: bool,
    ) -> Option<Lease> {
        self.expire(now_ms);
        let end = first_cpu + want.cores;
        let overlaps = !overlap
            && self.leases.iter().any(|lease| {
                lease.machine == machine
                    && lease.first_cpu < end
                    && first_cpu < lease.first_cpu + lease.cores
            });
        (!overlaps).then(|| self.add(machine, first_cpu, want, now_ms, until_ms))
    }

    pub fn give(&mut self, id: &str) -> Option<Lease> {
        let at = self.leases.iter().position(|lease| lease.id == id)?;
        Some(self.leases.remove(at))
    }

    pub fn renew(&mut self, id: &str, until_ms: u64) -> Option<&Lease> {
        let lease = self.leases.iter_mut().find(|lease| lease.id == id)?;
        lease.until_ms = until_ms;
        Some(lease)
    }
}

const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

pub fn parse_local_minutes(text: &str, utc_offset_minutes: i64) -> Option<u64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (hour, minute) = time.split_once(':')?;
    let minutes = days_from_civil(year, month, day) * 1440
        + hour.parse::<i64>().ok()? * 60
        + minute.parse::<i64>().ok()?
        - utc_offset_minutes;
    u64::try_from(minutes * 60_000).ok()
}

pub fn machines_from_boxes(
    json: &str,
    pool: Pool,
    extra: &[Machine],
    last: &[Machine],
) -> Vec<Machine> {
    let boxes: Vec<Machine> = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| {
            Some(Machine {
                name: text(entry, "hand_name").or_else(|| text(entry, "name"))?,
                cores: u32::try_from(number(entry, "cores")?).ok()?,
                expires_ms: text(entry, "expires_at_pst")
                    .and_then(|expires| parse_local_minutes(&expires, -8 * 60)),
                pin: Pin::Taskset,
                placement: Placement::Fill,
                kinds: entry.get("kinds").and_then(Value::as_array).map(|kinds| {
                    kinds
                        .iter()
                        .filter_map(|kind| kind.as_str().map(str::to_owned))
                        .collect()
                }),
            })
        })
        .collect();
    let large = boxes.iter().map(|machine| machine.cores).max().unwrap_or(0);
    let (larges, mut smalls): (Vec<Machine>, Vec<Machine>) = boxes
        .into_iter()
        .partition(|machine| machine.cores == large);
    smalls.sort_by_key(|machine| std::cmp::Reverse(machine.cores));
    let mut ordered = Vec::new();
    match pool {
        Pool::Large => {
            ordered.extend(larges);
            ordered.extend(smalls);
            ordered.extend(last.iter().cloned());
        }
        Pool::Small => {
            ordered.extend(smalls);
            ordered.extend(last.iter().cloned());
            ordered.extend(larges);
        }
    }
    ordered.extend(extra.iter().cloned());
    ordered
}
