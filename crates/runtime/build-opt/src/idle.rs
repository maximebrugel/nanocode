use std::collections::BTreeMap;

pub type CpuTimes = BTreeMap<u32, (u64, u64)>;

pub struct LeaseCpus {
    pub id: String,
    pub first: u32,
    pub last: u32,
    pub kind: String,
    pub label: String,
}

pub struct IdleLease {
    pub id: String,
    pub cpus: String,
    pub kind: String,
    pub label: String,
    pub busy_percent: u64,
    pub idle_ms: u64,
}

pub const KEPT_KINDS: [&str; 3] = ["reserved", "closed-load", "bench"];

/// Labels whose leases are never reaped for idling: box-closing- moves, and long-lived services
/// (Nanofactory planes and node agents, p2live) whose cpus sit idle between scheduled work.
pub const KEPT_LABEL_PREFIX: &str = "box-closing-";

pub const KEPT_LABEL_PREFIXES: [&str; 6] = [
    "box-closing-",
    "nf-deploy-",
    "nf-fan-",
    "p2live-",
    "lander-p2live",
    "boardrel-red-",
];

impl LeaseCpus {
    pub fn kept(&self) -> bool {
        KEPT_KINDS.contains(&self.kind.as_str())
            || KEPT_LABEL_PREFIXES
                .iter()
                .any(|prefix| self.label.starts_with(prefix))
    }
}

pub fn cpu_times(stat: &str) -> CpuTimes {
    stat.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let cpu = fields.next()?.strip_prefix("cpu")?.parse().ok()?;
            let values = fields
                .take(8)
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            let idle = values.get(1)? + values.get(3)? + values.get(4)?;
            Some((cpu, (values.iter().sum(), idle)))
        })
        .collect()
}

pub fn busy_percent(before: &CpuTimes, after: &CpuTimes, first: u32, last: u32) -> Option<u64> {
    let (mut total, mut idle) = (0u64, 0u64);
    for cpu in first..=last {
        let (total_before, idle_before) = before.get(&cpu)?;
        let (total_after, idle_after) = after.get(&cpu)?;
        total += total_after.checked_sub(*total_before)?;
        idle += idle_after.checked_sub(*idle_before)?;
    }
    (total > 0).then(|| total.saturating_sub(idle) * 100 / total)
}

pub fn leases_on(listing: &str, machine: &str) -> Vec<LeaseCpus> {
    listing
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let ["lease", id, on, "cpus", cpus, kind, label, ..] = fields.as_slice() else {
                return None;
            };
            if *on != machine {
                return None;
            }
            let (first, last) = cpus.split_once('-').unwrap_or((cpus, cpus));
            Some(LeaseCpus {
                id: (*id).to_owned(),
                first: first.parse().ok()?,
                last: last.parse().ok()?,
                kind: (*kind).to_owned(),
                label: (*label).to_owned(),
            })
        })
        .collect()
}

pub fn idle_step(
    since: &mut BTreeMap<String, u64>,
    leases: &[LeaseCpus],
    before: (&CpuTimes, u64),
    after: (&CpuTimes, u64),
    below_percent: u64,
    idle_ms: u64,
) -> Vec<IdleLease> {
    since.retain(|id, _| leases.iter().any(|lease| &lease.id == id && !lease.kept()));
    let mut idle = Vec::new();
    for lease in leases.iter().filter(|lease| !lease.kept()) {
        let Some(busy) = busy_percent(before.0, after.0, lease.first, lease.last) else {
            continue;
        };
        if busy >= below_percent {
            since.remove(&lease.id);
            continue;
        }
        let start = *since.entry(lease.id.clone()).or_insert(before.1);
        let for_ms = after.1.saturating_sub(start);
        if for_ms >= idle_ms {
            idle.push(IdleLease {
                id: lease.id.clone(),
                cpus: format!("{}-{}", lease.first, lease.last),
                kind: lease.kind.clone(),
                label: lease.label.clone(),
                busy_percent: busy,
                idle_ms: for_ms,
            });
        }
    }
    idle
}

#[cfg(test)]
mod tests {
    use super::LeaseCpus;

    fn lease(kind: &str, label: &str) -> LeaseCpus {
        LeaseCpus {
            id: "l".to_owned(),
            first: 0,
            last: 3,
            kind: kind.to_owned(),
            label: label.to_owned(),
        }
    }

    #[test]
    fn service_leases_are_never_reaped_for_idling() {
        for label in [
            "nf-deploy-plane",
            "nf-deploy-node-0",
            "nf-fan-node-0",
            "p2live-run",
            "lander-p2live-a",
            "boardrel-red-land38",
            "box-closing-nf",
        ] {
            assert!(lease("build", label).kept(), "{label}");
        }
        assert!(!lease("build", "claims50k-metrics").kept());
        assert!(!lease("test", "nfperf-a").kept());
        assert!(lease("reserved", "anything").kept());
    }
}
