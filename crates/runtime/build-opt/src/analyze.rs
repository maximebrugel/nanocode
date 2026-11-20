use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::graph::Graph;

#[derive(Debug, PartialEq, Eq)]
pub enum AnalyzeError {
    Json(String),
    Field(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub name: String,
    pub version: String,
    pub mode: String,
    pub target: String,
    pub features: Vec<String>,
    pub start: f64,
    pub duration: f64,
    pub unblocks: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Library,
    Other,
    ScriptCompile,
    ScriptRun,
}

impl Unit {
    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    pub fn role(&self) -> Role {
        if self.mode == "run-custom-build" {
            Role::ScriptRun
        } else if self.target.contains("build-script") {
            Role::ScriptCompile
        } else if self.target.is_empty() || self.target == " (check)" {
            Role::Library
        } else {
            Role::Other
        }
    }

    fn key(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub member: bool,
    pub proc_macro: bool,
    pub normal: BTreeSet<String>,
    pub build: BTreeSet<String>,
    pub dev: BTreeSet<String>,
    pub test_binaries: usize,
    pub integration_tests: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    ColdBuild,
    Commit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub basis: Basis,
    pub summary: String,
    pub saved_wall_s: f64,
    pub saved_cpu_s: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub wall_s: f64,
    pub cpu_s: f64,
    pub critical_s: f64,
    pub critical_path: Vec<String>,
    pub units: usize,
    pub commits: usize,
    pub proc_macros: usize,
    pub build_scripts: usize,
    pub duplicated: usize,
    pub recompiled: usize,
    pub test_binaries: usize,
    pub changes: Vec<Change>,
}

fn field<'a>(value: &'a Value, name: &'static str) -> Result<&'a Value, AnalyzeError> {
    value.get(name).ok_or(AnalyzeError::Field(name))
}

fn text(value: &Value, name: &'static str) -> Result<String, AnalyzeError> {
    field(value, name)?
        .as_str()
        .map(str::to_owned)
        .ok_or(AnalyzeError::Field(name))
}

fn array<'a>(value: &'a Value, name: &'static str) -> Result<&'a Vec<Value>, AnalyzeError> {
    field(value, name)?
        .as_array()
        .ok_or(AnalyzeError::Field(name))
}

fn json(text: &str) -> Result<Value, AnalyzeError> {
    serde_json::from_str(text).map_err(|error| AnalyzeError::Json(error.to_string()))
}

pub fn parse_timings(html: &str) -> Result<Vec<Unit>, AnalyzeError> {
    let marker = "const UNIT_DATA = ";
    let at = html.find(marker).ok_or(AnalyzeError::Field("UNIT_DATA"))? + marker.len();
    let end = html[at..]
        .find("\n];")
        .ok_or(AnalyzeError::Field("UNIT_DATA"))?;
    let data = json(&html[at..at + end + 2])?;
    let entries = data.as_array().ok_or(AnalyzeError::Field("UNIT_DATA"))?;
    let index: BTreeMap<u64, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(at, entry)| entry.get("i").and_then(Value::as_u64).map(|i| (i, at)))
        .collect();
    entries
        .iter()
        .map(|entry| {
            let number = |name: &'static str| {
                field(entry, name)?
                    .as_f64()
                    .ok_or(AnalyzeError::Field(name))
            };
            let mut unblocks = Vec::new();
            for name in ["unblocked_units", "unblocked_rmeta_units"] {
                for i in entry
                    .get(name)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(at) = i.as_u64().and_then(|i| index.get(&i)) {
                        unblocks.push(*at);
                    }
                }
            }
            Ok(Unit {
                name: text(entry, "name")?,
                version: text(entry, "version")?,
                mode: text(entry, "mode")?,
                target: text(entry, "target")?,
                features: entry
                    .get("features")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                start: number("start")?,
                duration: number("duration")?,
                unblocks,
            })
        })
        .collect()
}

pub fn parse_crates(metadata: &str) -> Result<BTreeMap<String, Crate>, AnalyzeError> {
    let metadata = json(metadata)?;
    let members: BTreeSet<String> = array(&metadata, "workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let mut crates = BTreeMap::new();
    for package in array(&metadata, "packages")? {
        let targets = array(package, "targets")?;
        let kinds = |target: &Value| -> Vec<String> {
            target
                .get("kind")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        };
        let id = text(package, "id")?;
        crates.insert(
            id.clone(),
            Crate {
                name: text(package, "name")?,
                version: text(package, "version")?,
                member: members.contains(&id),
                proc_macro: targets
                    .iter()
                    .any(|target| kinds(target).iter().any(|kind| kind == "proc-macro")),
                test_binaries: targets
                    .iter()
                    .filter(|target| target.get("test").and_then(Value::as_bool) == Some(true))
                    .count(),
                integration_tests: targets
                    .iter()
                    .filter(|target| kinds(target).iter().any(|kind| kind == "test"))
                    .count(),
                ..Crate::default()
            },
        );
    }
    let nodes = field(&metadata, "resolve")?
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or(AnalyzeError::Field("nodes"))?;
    for node in nodes {
        let id = text(node, "id")?;
        let Some(krate) = crates.get_mut(&id) else {
            continue;
        };
        for dependency in node
            .get("deps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let package = text(dependency, "pkg")?;
            for kind in dependency
                .get("dep_kinds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let set = match kind.get("kind").and_then(Value::as_str) {
                    Some("build") => &mut krate.build,
                    Some("dev") => &mut krate.dev,
                    _ => &mut krate.normal,
                };
                set.insert(package.clone());
            }
        }
    }
    Ok(crates)
}

pub fn edits(graph: &Graph, log: &str) -> (usize, BTreeMap<String, usize>) {
    let mut counts = BTreeMap::new();
    let mut commits = 0;
    for commit in log
        .split('\u{1}')
        .filter(|commit| !commit.trim().is_empty())
    {
        commits += 1;
        let touched: BTreeSet<&str> = commit
            .lines()
            .filter_map(|path| graph.owner(path.trim()))
            .map(|package| package.name.as_str())
            .collect();
        for name in touched {
            *counts.entry(name.to_owned()).or_insert(0) += 1;
        }
    }
    (commits, counts)
}

struct Dag<'a> {
    units: &'a [Unit],
    order: Vec<usize>,
    preds: Vec<BTreeSet<usize>>,
}

impl<'a> Dag<'a> {
    fn new(units: &'a [Unit], crates: &BTreeMap<String, Crate>) -> Self {
        let mut by_key: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (at, unit) in units.iter().enumerate() {
            by_key.entry(unit.key()).or_default().push(at);
        }
        let keyed: BTreeMap<String, &Crate> = crates
            .values()
            .map(|krate| (format!("{} {}", krate.name, krate.version), krate))
            .collect();
        let of_role = |key: &str, role: Role| -> Vec<usize> {
            by_key
                .get(key)
                .into_iter()
                .flatten()
                .copied()
                .filter(|at| units[*at].role() == role)
                .collect()
        };
        let libraries = |ids: &BTreeSet<String>| -> Vec<usize> {
            ids.iter()
                .filter_map(|id| crates.get(id))
                .flat_map(|krate| {
                    of_role(&format!("{} {}", krate.name, krate.version), Role::Library)
                })
                .collect()
        };
        let mut preds: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); units.len()];
        for (at, unit) in units.iter().enumerate() {
            for next in &unit.unblocks {
                preds[*next].insert(at);
            }
            let key = unit.key();
            let Some(krate) = keyed.get(&key) else {
                continue;
            };
            let candidates = match unit.role() {
                Role::ScriptRun => of_role(&key, Role::ScriptCompile),
                Role::ScriptCompile => libraries(&krate.build),
                Role::Library => {
                    let mut all = libraries(&krate.normal);
                    all.extend(of_role(&key, Role::ScriptRun));
                    all
                }
                Role::Other => {
                    let mut all = libraries(&krate.normal);
                    all.extend(libraries(&krate.dev));
                    all.extend(of_role(&key, Role::ScriptRun));
                    all.extend(of_role(&key, Role::Library));
                    all
                }
            };
            preds[at].extend(candidates);
        }
        for (at, unit) in units.iter().enumerate() {
            preds[at].retain(|pred| *pred != at && units[*pred].end() <= unit.start + 0.01);
        }
        let mut order: Vec<usize> = (0..units.len()).collect();
        order.sort_by(|a, b| units[*a].start.total_cmp(&units[*b].start));
        Self {
            units,
            order,
            preds,
        }
    }

    fn longest(&self, weight: impl Fn(usize) -> f64) -> (f64, Vec<usize>) {
        let mut finish = vec![0.0f64; self.units.len()];
        let mut via: Vec<Option<usize>> = vec![None; self.units.len()];
        for at in &self.order {
            let best = self.preds[*at]
                .iter()
                .copied()
                .max_by(|a, b| finish[*a].total_cmp(&finish[*b]));
            finish[*at] = weight(*at) + best.map_or(0.0, |pred| finish[pred]);
            via[*at] = best;
        }
        let Some(mut last) = (0..self.units.len()).max_by(|a, b| finish[*a].total_cmp(&finish[*b]))
        else {
            return (0.0, Vec::new());
        };
        let total = finish[last];
        let mut path = vec![last];
        while let Some(pred) = via[last] {
            path.push(pred);
            last = pred;
        }
        path.reverse();
        (total, path)
    }

    fn within(&self, kept: &BTreeSet<usize>) -> f64 {
        self.longest(|at| {
            if kept.contains(&at) {
                self.units[at].duration
            } else {
                0.0
            }
        })
        .0
    }
}

fn cpu(units: &[Unit], set: &BTreeSet<usize>) -> f64 {
    set.iter().map(|at| units[*at].duration).sum()
}

fn version_rank(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

struct Candidate {
    summary: String,
    set: BTreeSet<usize>,
    factor: f64,
    saves_cpu: bool,
}

fn rank(units: &[Unit], dag: &Dag, candidates: Vec<Candidate>, rounds: usize) -> Vec<Change> {
    let mut weights: Vec<f64> = units.iter().map(|unit| unit.duration).collect();
    let mut current = dag.longest(|at| weights[at]).0;
    let mut left = candidates;
    let mut ranked = Vec::new();
    while ranked.len() < rounds && !left.is_empty() {
        let scored: Vec<(f64, f64)> = left
            .iter()
            .map(|candidate| {
                let wall = current
                    - dag
                        .longest(|at| {
                            if candidate.set.contains(&at) {
                                weights[at] * candidate.factor
                            } else {
                                weights[at]
                            }
                        })
                        .0;
                let cpu = if candidate.saves_cpu {
                    candidate
                        .set
                        .iter()
                        .map(|at| weights[*at] * (1.0 - candidate.factor))
                        .sum()
                } else {
                    0.0
                };
                (wall, cpu)
            })
            .collect();
        let Some(best) = (0..left.len()).max_by(|a, b| {
            scored[*a]
                .0
                .total_cmp(&scored[*b].0)
                .then(scored[*a].1.total_cmp(&scored[*b].1))
        }) else {
            break;
        };
        let (wall, cpu) = scored[best];
        if wall <= 0.0 && cpu <= 0.0 {
            break;
        }
        let candidate = left.swap_remove(best);
        for at in &candidate.set {
            weights[*at] *= candidate.factor;
        }
        current -= wall;
        ranked.push(Change {
            basis: Basis::ColdBuild,
            summary: candidate.summary,
            saved_wall_s: wall,
            saved_cpu_s: cpu,
        });
    }
    ranked
}

pub fn analyze(
    units: &[Unit],
    crates: &BTreeMap<String, Crate>,
    graph: &Graph,
    commits: usize,
    edits: &BTreeMap<String, usize>,
    rounds: usize,
) -> Report {
    let dag = Dag::new(units, crates);
    let (critical_s, path) = dag.longest(|at| units[at].duration);
    let all_cpu: f64 = units.iter().map(|unit| unit.duration).sum();
    let wall_s = units.iter().map(Unit::end).fold(0.0, f64::max);
    let mut candidates = Vec::new();
    let mut by_name: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    let mut by_key: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for (at, unit) in units.iter().enumerate() {
        by_name.entry(unit.name.as_str()).or_default().insert(at);
        by_key.entry(unit.key()).or_default().insert(at);
    }
    let mut build_scripts = 0;
    for (key, set) in &by_key {
        let scripts: BTreeSet<usize> = set
            .iter()
            .copied()
            .filter(|at| matches!(units[*at].role(), Role::ScriptRun | Role::ScriptCompile))
            .collect();
        if scripts.is_empty() {
            continue;
        }
        build_scripts += 1;
        let run: f64 = scripts
            .iter()
            .filter(|at| units[**at].role() == Role::ScriptRun)
            .map(|at| units[*at].duration)
            .sum();
        candidates.push(Candidate {
            summary: format!(
                "cache or drop the build script of {key} (run {run:.1}s, compile {:.1}s)",
                cpu(units, &scripts) - run
            ),
            set: scripts,
            factor: 0.0,
            saves_cpu: true,
        });
    }
    let mut proc_macros = 0;
    for krate in crates.values().filter(|krate| krate.proc_macro) {
        let key = format!("{} {}", krate.name, krate.version);
        let Some(set) = by_key.get(&key) else {
            continue;
        };
        proc_macros += 1;
        let library: BTreeSet<usize> = set
            .iter()
            .copied()
            .filter(|at| units[*at].role() == Role::Library)
            .collect();
        candidates.push(Candidate {
            summary: format!("replace proc-macro {key} ({:.1}s)", cpu(units, &library)),
            set: library,
            factor: 0.0,
            saves_cpu: true,
        });
    }
    let mut versions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for krate in crates.values() {
        versions
            .entry(krate.name.as_str())
            .or_default()
            .insert(krate.version.as_str());
    }
    let mut duplicated = 0;
    for (name, found) in versions.iter().filter(|(_, found)| found.len() > 1) {
        let Some(newest) = found.iter().max_by_key(|version| version_rank(version)) else {
            continue;
        };
        let older: BTreeSet<usize> = by_name
            .get(name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|at| units[*at].version != *newest)
            .collect();
        if older.is_empty() {
            continue;
        }
        duplicated += 1;
        let listed: Vec<&str> = found.iter().copied().collect();
        candidates.push(Candidate {
            summary: format!("unify {name} on {newest} (built {})", listed.join(", ")),
            set: older,
            factor: 0.0,
            saves_cpu: true,
        });
    }
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (at, unit) in units.iter().enumerate() {
        if unit.role() == Role::Library {
            groups
                .entry((unit.key(), unit.target.clone()))
                .or_default()
                .push(at);
        }
    }
    let mut recompiled = 0;
    for ((key, _), copies) in groups.iter().filter(|(_, copies)| copies.len() > 1) {
        let Some(keep) = copies
            .iter()
            .copied()
            .max_by(|a, b| units[*a].duration.total_cmp(&units[*b].duration))
        else {
            continue;
        };
        recompiled += 1;
        let sets: BTreeSet<&Vec<String>> = copies.iter().map(|at| &units[*at].features).collect();
        candidates.push(Candidate {
            summary: format!(
                "unify features of {key} (compiled {} times, {} feature sets)",
                copies.len(),
                sets.len()
            ),
            set: copies.iter().copied().filter(|at| *at != keep).collect(),
            factor: 0.0,
            saves_cpu: true,
        });
    }
    let members: BTreeSet<String> = crates
        .values()
        .filter(|krate| krate.member)
        .map(|krate| format!("{} {}", krate.name, krate.version))
        .collect();
    let test_binaries = crates
        .values()
        .filter(|krate| krate.member)
        .map(|krate| krate.test_binaries)
        .sum();
    for (at, unit) in units.iter().enumerate() {
        if !members.contains(&unit.key()) {
            continue;
        }
        let set = BTreeSet::from([at]);
        match unit.role() {
            Role::Other if unit.target.contains("test)") => candidates.push(Candidate {
                summary: format!(
                    "drop the test harness build of {}{} ({:.1}s): test = false when it holds no unit tests, else move them to one tests/ binary",
                    unit.name, unit.target, unit.duration
                ),
                set,
                factor: 0.0,
                saves_cpu: true,
            }),
            Role::Library | Role::Other => candidates.push(Candidate {
                summary: format!(
                    "split {}{} ({:.1}s) into two crates that build in parallel",
                    unit.name, unit.target, unit.duration
                ),
                set,
                factor: 0.5,
                saves_cpu: false,
            }),
            Role::ScriptCompile | Role::ScriptRun => {}
        }
    }
    let mut changes = rank(units, &dag, candidates, rounds);
    let dependents = graph.dependents();
    for (name, count) in edits {
        let reached = graph.with_dependents(&BTreeSet::from([name.as_str()]));
        let below = reached.len() - 1;
        if below == 0 || commits == 0 {
            continue;
        }
        let rebuild: BTreeSet<usize> = (0..units.len())
            .filter(|at| reached.contains(units[*at].name.as_str()))
            .collect();
        let own: BTreeSet<usize> = rebuild
            .iter()
            .copied()
            .filter(|at| units[*at].name == *name)
            .collect();
        let share = *count as f64 / commits as f64;
        let per_edit = dag.within(&rebuild) - dag.within(&own);
        let direct = dependents.get(name.as_str()).map_or(0, BTreeSet::len);
        if per_edit > 0.0 {
            changes.push(Change {
                basis: Basis::Commit,
                summary: format!(
                    "narrow what depends on {name}: edited in {count}/{commits} commits, each edit rebuilds {below} dependents ({direct} direct) for {per_edit:.1}s of critical path, {:.1}s cpu",
                    cpu(units, &rebuild) - cpu(units, &own)
                ),
                saved_wall_s: share * per_edit,
                saved_cpu_s: share * (cpu(units, &rebuild) - cpu(units, &own)),
            });
        }
    }
    changes.sort_by(|a, b| {
        b.saved_wall_s
            .total_cmp(&a.saved_wall_s)
            .then(b.saved_cpu_s.total_cmp(&a.saved_cpu_s))
    });
    Report {
        wall_s,
        cpu_s: all_cpu,
        critical_s,
        critical_path: path
            .iter()
            .map(|at| {
                format!(
                    "{}{} {:.1}s",
                    units[*at].name, units[*at].target, units[*at].duration
                )
            })
            .collect(),
        units: units.len(),
        commits,
        proc_macros,
        build_scripts,
        duplicated,
        recompiled,
        test_binaries,
        changes,
    }
}
