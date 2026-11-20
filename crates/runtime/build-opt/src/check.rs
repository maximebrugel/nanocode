use std::collections::{BTreeMap, BTreeSet};

use crate::{graph::Graph, schedule::fnv1a};

pub const PASS_UNIT_SEEDS: u64 = 8;
pub const CANARY_SEEDS: u64 = 400;
pub const CANARY_UNIT_SEEDS: u64 = 16;
pub const FIRST_CANARY_UNITS: [u64; 2] = [1, 8];
pub const FAILURE_LINES: usize = 20;
pub const TAIL_LINES: usize = 4;
pub const FAIL_DETAIL_LINES: usize = 3;
pub const ERROR_CONTEXT_LINES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Simulator<'a> {
    pub package: &'a str,
    pub dir: &'a str,
    pub src: &'a str,
    pub program: &'a str,
    pub aliases: &'a [(&'a str, &'a str)],
}

pub type Modules = BTreeMap<String, Vec<(String, String)>>;

#[derive(Debug, PartialEq, Eq)]
pub enum ScenarioError {
    NoMain,
    NoScenarioList,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TableError {
    Budget(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pass,
    Canary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimJob {
    pub name: String,
    pub kind: Kind,
    pub seeds: u64,
    pub flags: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    pub job: usize,
    pub from: u64,
    pub upto: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed { exit: i32, lines: Vec<String> },
    Caught,
    Missed { exit: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub unit: Unit,
    pub outcome: Outcome,
    pub ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub log: Vec<String>,
    pub times: Vec<String>,
    pub green: bool,
}

pub fn libraries(graph: &Graph, crates: &BTreeSet<String>) -> BTreeSet<String> {
    crates
        .iter()
        .filter(|name| {
            graph
                .packages
                .get(*name)
                .is_some_and(|package| package.library)
        })
        .cloned()
        .collect()
}

fn each_word<'a>(text: &'a str, mut found: impl FnMut(&'a str, &'a str)) {
    let mut start = None;
    for (at, c) in text.char_indices().chain([(text.len(), ' ')]) {
        let word = c.is_alphanumeric() || c == '_';
        match (start, word) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                found(&text[from..at], &text[at..]);
                start = None;
            }
            _ => {}
        }
    }
}

pub fn scenario_list(main: &str) -> Option<Vec<String>> {
    main.match_indices("--scenario ").find_map(|(at, marker)| {
        let rest = &main[at + marker.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '|'))
            .unwrap_or(rest.len());
        (end > 0 && rest[end..].starts_with(']'))
            .then(|| rest[..end].split('|').map(str::to_owned).collect())
    })
}

fn main_text(modules: &Modules) -> Result<String, ScenarioError> {
    modules
        .get("main")
        .map(|files| {
            files
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .ok_or(ScenarioError::NoMain)
}

pub fn all_scenarios(modules: &Modules) -> Result<Vec<String>, ScenarioError> {
    scenario_list(&main_text(modules)?).ok_or(ScenarioError::NoScenarioList)
}

pub fn scenarios(
    sim: &Simulator,
    modules: &Modules,
    graph: &Graph,
    reached: &BTreeSet<String>,
    changed: &[String],
) -> Result<Vec<String>, ScenarioError> {
    if !reached.contains(sim.package) {
        return Ok(Vec::new());
    }
    let crate_of: BTreeMap<String, &str> = graph
        .packages
        .values()
        .filter(|package| package.library)
        .map(|package| (package.name.replace('-', "_"), package.name.as_str()))
        .collect();
    let mut uses: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut refs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (module, files) in modules {
        let module_uses = uses.entry(module.as_str()).or_default();
        let module_refs = refs.entry(module.as_str()).or_default();
        for (_, text) in files {
            each_word(text, |word, rest| {
                if word.starts_with("nanocode_")
                    && let Some(name) = crate_of.get(word)
                {
                    module_uses.insert(*name);
                }
                if rest.starts_with("::")
                    && word != module.as_str()
                    && word != "main"
                    && let Some((name, _)) = modules.get_key_value(word)
                {
                    module_refs.insert(name.as_str());
                }
            });
        }
    }
    let names = all_scenarios(modules)?;
    let all_files: BTreeSet<&str> = modules
        .values()
        .flatten()
        .map(|(path, _)| path.as_str())
        .collect();
    let sim_changed: Vec<&str> = changed
        .iter()
        .map(String::as_str)
        .filter(|path| path.starts_with(sim.dir))
        .collect();
    let alias = |name: &str| {
        sim.aliases
            .iter()
            .find(|(scenario, _)| *scenario == name)
            .map(|(_, module)| (*module).to_owned())
    };
    let mut picked = Vec::new();
    for scenario in names {
        let candidates = [
            Some(scenario.replace('_', "")),
            Some(scenario.clone()),
            alias(&scenario),
        ];
        let Some(entry) = candidates
            .into_iter()
            .flatten()
            .find_map(|candidate| modules.get_key_value(candidate.as_str()))
            .map(|(name, _)| name.as_str())
        else {
            picked.push(scenario);
            continue;
        };
        let mut seen: BTreeSet<&str> = BTreeSet::from([entry]);
        let mut todo = vec![entry];
        while let Some(module) = todo.pop() {
            for &next in refs.get(module).into_iter().flatten() {
                if seen.insert(next) {
                    todo.push(next);
                }
            }
        }
        seen.insert("main");
        let files: BTreeSet<&str> = seen
            .iter()
            .filter_map(|module| modules.get(*module))
            .flatten()
            .map(|(path, _)| path.as_str())
            .collect();
        let touched = sim_changed
            .iter()
            .any(|path| files.contains(path) || !all_files.contains(path));
        let reaches = seen
            .iter()
            .filter_map(|module| uses.get(module))
            .flatten()
            .any(|name| reached.contains(*name));
        if touched || reaches {
            picked.push(scenario);
        }
    }
    Ok(picked)
}

pub fn default_table(script: &str) -> String {
    let mut lines = script.lines();
    if !lines
        .by_ref()
        .any(|line| line.starts_with("default_table='"))
    {
        return String::new();
    }
    lines
        .take_while(|line| !line.starts_with('\''))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn sim_jobs(
    table: &str,
    scenarios: &[String],
    pass_seeds: u64,
) -> Result<Vec<SimJob>, TableError> {
    let mut rows: Vec<Vec<String>> = table
        .lines()
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .collect();
    for scenario in scenarios {
        let listed = rows.iter().any(|row| {
            row.windows(2)
                .any(|pair| pair[0] == "--scenario" && pair[1] == *scenario)
        });
        if !listed {
            for (seeds, name, extra) in [
                ("8", scenario.clone(), None),
                ("2", format!("{scenario}_twice"), Some("--twice")),
            ] {
                let mut row = vec![
                    seeds.to_owned(),
                    name,
                    "pass".to_owned(),
                    "--scenario".to_owned(),
                    scenario.clone(),
                ];
                row.extend(extra.map(str::to_owned));
                rows.push(row);
            }
        }
    }
    let wanted: BTreeSet<&str> = scenarios.iter().map(String::as_str).collect();
    let mut jobs = Vec::new();
    for row in &rows {
        if row.len() < 3 {
            continue;
        }
        let scenario = row[3..]
            .windows(2)
            .filter(|pair| pair[0] == "--scenario")
            .map(|pair| pair[1].as_str())
            .next_back();
        if !scenario.is_some_and(|scenario| wanted.contains(scenario)) {
            continue;
        }
        let (kind, seeds) = match row[2].as_str() {
            "pass" => (Kind::Pass, pass_seeds),
            "canary" => (Kind::Canary, CANARY_SEEDS),
            other => match other.strip_prefix("canary:") {
                Some(budget) => (
                    Kind::Canary,
                    budget
                        .parse()
                        .map_err(|_| TableError::Budget(row.join(" ")))?,
                ),
                None => continue,
            },
        };
        jobs.push(SimJob {
            name: row[1].clone(),
            kind,
            seeds,
            flags: row[3..].to_vec(),
        });
    }
    Ok(jobs)
}

pub fn units(jobs: &[SimJob], times: &str) -> Vec<Unit> {
    let known: BTreeMap<(&str, u64, u64), i64> = times
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let from = fields.next()?.parse().ok()?;
            let upto = fields.next()?.parse().ok()?;
            let ms = fields.next()?.parse().ok()?;
            Some(((name, from, upto), ms))
        })
        .collect();
    let late = FIRST_CANARY_UNITS[FIRST_CANARY_UNITS.len() - 1];
    let mut keyed = Vec::new();
    for (at, job) in jobs.iter().enumerate() {
        let mut from = 0;
        let mut index = 0;
        while from < job.seeds {
            let upto = match job.kind {
                Kind::Pass => from + PASS_UNIT_SEEDS,
                Kind::Canary => FIRST_CANARY_UNITS
                    .get(index)
                    .copied()
                    .unwrap_or(from + CANARY_UNIT_SEEDS),
            }
            .min(job.seeds);
            let key = if job.kind == Kind::Canary && from >= late {
                -i64::try_from(from).unwrap_or(i64::MAX)
            } else if known.is_empty() {
                i64::MAX
            } else {
                known
                    .get(&(job.name.as_str(), from, upto))
                    .copied()
                    .unwrap_or(0)
            };
            keyed.push((
                key,
                Unit {
                    job: at,
                    from,
                    upto,
                },
            ));
            from = upto;
            index += 1;
        }
    }
    keyed.sort_by_key(|(key, _)| std::cmp::Reverse(*key));
    keyed.into_iter().map(|(_, unit)| unit).collect()
}

pub fn classify(kind: Kind, exit: i32, output: &str) -> Outcome {
    match kind {
        Kind::Pass if exit == 0 => Outcome::Passed,
        Kind::Pass => Outcome::Failed {
            exit,
            lines: output
                .lines()
                .filter(|line| line.starts_with("FAIL") || line.contains("panicked"))
                .take(FAIL_DETAIL_LINES)
                .map(str::to_owned)
                .collect(),
        },
        Kind::Canary
            if exit == 0
                && output.lines().any(|line| {
                    line.strip_prefix("MUTATION ")
                        .is_some_and(|rest| rest.contains(" caught by"))
                }) =>
        {
            Outcome::Caught
        }
        Kind::Canary => Outcome::Missed { exit },
    }
}

pub fn report(sim: &Simulator, jobs: &[SimJob], finished: &[Finished], units: usize) -> Report {
    let mut log = Vec::new();
    let mut failed_units = 0;
    for done in finished {
        if let Outcome::Failed { exit, lines } = &done.outcome {
            let job = &jobs[done.unit.job];
            failed_units += 1;
            log.push(format!(
                "FAILJOB {} exit={exit} :: {} --seeds {}..{} {}",
                job.name,
                sim.program,
                done.unit.from,
                done.unit.upto,
                job.flags.join(" ")
            ));
            log.extend(lines.iter().cloned());
        }
    }
    let (mut passed, mut caught, mut missed) = (0, 0, 0);
    for (at, job) in jobs.iter().enumerate() {
        let mine = || finished.iter().filter(move |done| done.unit.job == at);
        match job.kind {
            Kind::Pass => {
                if !mine().any(|done| matches!(done.outcome, Outcome::Failed { .. })) {
                    passed += 1;
                    log.push(format!("PASSJOB {}", job.name));
                }
            }
            Kind::Canary => {
                if let Some(first) = mine()
                    .filter(|done| done.outcome == Outcome::Caught)
                    .min_by_key(|done| done.unit.from)
                {
                    caught += 1;
                    log.push(format!(
                        "CAUGHTJOB {} seeds={}..{}",
                        job.name, first.unit.from, first.unit.upto
                    ));
                } else {
                    missed += 1;
                    let exit = mine()
                        .filter_map(|done| match done.outcome {
                            Outcome::Missed { exit } => Some(format!(" exit={exit}")),
                            _ => None,
                        })
                        .next_back()
                        .unwrap_or_default();
                    log.push(format!(
                        "MISSEDJOB {}{exit} :: {} --seeds 0..{} {}",
                        job.name,
                        sim.program,
                        job.seeds,
                        job.flags.join(" ")
                    ));
                }
            }
        }
    }
    log.push(format!(
        "SIM pass_jobs={passed} failed_units={failed_units} canaries_caught={caught} missed={missed} units={units}"
    ));
    let times = finished
        .iter()
        .map(|done| {
            format!(
                "{} {} {} {}",
                jobs[done.unit.job].name, done.unit.from, done.unit.upto, done.ms
            )
        })
        .collect();
    Report {
        log,
        times,
        green: failed_units == 0 && missed == 0 && passed + caught > 0,
    }
}

pub fn line(status: &str, name: &str, secs: f64, detail: &str) -> (String, String) {
    (
        format!("{status:<4}"),
        format!(" {name:<10} {secs:>6.1}s  {detail}"),
    )
}

pub fn failure_lines(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut out: Vec<String> = lines
        .iter()
        .filter(|line| {
            ["error", "FAIL", "MISSEDJOB"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
                || line.contains("panicked")
        })
        .take(FAILURE_LINES)
        .map(|line| format!("     {line}"))
        .collect();
    out.extend(
        lines[lines.len().saturating_sub(TAIL_LINES)..]
            .iter()
            .map(|line| format!("     | {line}")),
    );
    out
}

pub fn error_context(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut out = Vec::new();
    let mut shown_to = 0;
    for (at, line) in lines.iter().enumerate() {
        if !line.starts_with("error") {
            continue;
        }
        let from = at.max(shown_to);
        if shown_to > 0 && from > shown_to {
            out.push("--".to_owned());
        }
        let to = (at + 1 + ERROR_CONTEXT_LINES).min(lines.len());
        out.extend(lines[from..to].iter().map(|line| (*line).to_owned()));
        shown_to = to;
    }
    out.into_iter()
        .take(FAILURE_LINES)
        .map(|line| format!("     | {line}"))
        .collect()
}

pub fn worktree_dir(root: &str) -> String {
    let name = root
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("root");
    format!("{name}-{:016x}", fnv1a(root.as_bytes()))
}
