use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::idle::KEPT_LABEL_PREFIX;

const FRESH_MS: u64 = 15 * 60 * 1000;

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
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub observations: BTreeMap<String, Observation>,
    pub leases: Vec<Lease>,
    pub next: u64,
    pub boxes: Option<Value>,
    pub caps: Vec<Cap>,
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
                json!({
                    "id": lease.id,
                    "machine": lease.machine,
                    "first_cpu": lease.first_cpu,
                    "cores": lease.cores,
                    "kind": lease.kind,
                    "label": lease.label,
                    "taken_ms": lease.taken_ms,
                    "until_ms": lease.until_ms,
                })
            })
            .collect();
        serde_json::to_string_pretty(&json!({
            "observations": observations,
            "leases": leases,
            "next": self.next,
            "boxes": self.boxes,
        }))
        .unwrap_or_default()
    }

    pub fn expire(&mut self, now_ms: u64) -> Vec<Lease> {
        let (expired, live) = self
            .leases
            .drain(..)
            .partition(|lease| lease.until_ms <= now_ms);
        self.leases = live;
        expired
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

    fn gap(&self, machine: &Machine, cores: u32, want: u32) -> Option<u32> {
        let mut used: Vec<(u32, u32)> = self
            .leases
            .iter()
            .filter(|lease| lease.machine == machine.name)
            .map(|lease| (lease.first_cpu, lease.first_cpu + lease.cores))
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

    pub fn take(
        &mut self,
        machines: &[Machine],
        want: &Want,
        now_ms: u64,
    ) -> Result<Lease, Refusal> {
        self.expire(now_ms);
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
            Some((held.machine.clone(), self.gap_within(held, want.cores)?))
        });
        if let Some((machine, first_cpu)) = standing {
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
            if !usage.memory_ok
                || usage.busy + want.cores > usage.cores
                || (machine.placement == Placement::LastResort && !usage.observed)
                || (machine.placement == Placement::Named && want.only.is_none())
            {
                continue;
            }
            let Some(first_cpu) = self.gap(machine, usage.cores, want.cores) else {
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
