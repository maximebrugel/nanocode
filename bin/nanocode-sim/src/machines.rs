use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt::Debug,
    process::ExitCode,
    time::Instant,
};

mod claim;
mod delivery;

pub struct Part {
    pub name: String,
    pub phase: &'static str,
    pub detail: String,
}

pub trait Model {
    type State: Clone;
    type Event: Clone + Debug;

    fn name(&self) -> &'static str;
    fn depth(&self) -> usize;
    fn initial(&self) -> Result<Vec<Self::State>, String>;
    fn events(&self, state: &Self::State) -> Vec<Self::Event>;
    fn label(&self, event: &Self::Event) -> &'static str;
    fn apply(&self, state: &mut Self::State, event: &Self::Event) -> Result<bool, String>;
    fn key(&self, state: &Self::State) -> String;
    fn parts(&self, state: &Self::State) -> Vec<Part>;
    fn moves(&self) -> Vec<(&'static str, &'static str)>;

    fn stays(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    fn addresses(&self, _event: &Self::Event) -> Option<String> {
        None
    }

    fn done(&self, _state: &Self::State) -> bool {
        false
    }

    fn invariant(&self, _state: &Self::State) -> Result<(), String> {
        Ok(())
    }

    fn after(
        &self,
        _before: &Self::State,
        _event: &Self::Event,
        _after: &Self::State,
        _accepted: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    fn idempotent(&self, _event: &Self::Event) -> bool {
        false
    }
}

pub const DEAD_END: &str = "no_dead_end";

#[derive(Clone, Debug)]
pub struct Violation {
    pub property: &'static str,
    pub detail: String,
    pub states: usize,
    pub depth: usize,
}

#[derive(Debug, Default)]
pub struct Report {
    pub model: &'static str,
    pub states: usize,
    pub transitions: usize,
    pub refusals: usize,
    pub depth: usize,
    pub declared: usize,
    pub reached: usize,
    pub violations: Vec<Violation>,
    pub error: Option<String>,
}

struct Walk<'a, M: Model> {
    model: &'a M,
    stop: Option<&'static str>,
    report: Report,
    seen: BTreeMap<String, usize>,
    edges: Vec<BTreeSet<usize>>,
    done: Vec<bool>,
    depths: Vec<usize>,
    reached: BTreeSet<(&'static str, &'static str)>,
    flagged: BTreeSet<&'static str>,
}

impl<M: Model> Walk<'_, M> {
    fn flag(&mut self, property: &'static str, depth: usize, detail: String) {
        if self.flagged.insert(property) {
            self.report.violations.push(Violation {
                property,
                detail,
                states: self.seen.len(),
                depth,
            });
        }
    }

    fn stopped(&self) -> bool {
        self.stop
            .is_some_and(|property| self.flagged.contains(property))
    }

    fn intern(
        &mut self,
        state: &M::State,
        depth: usize,
        queue: &mut VecDeque<(M::State, usize, usize)>,
    ) -> usize {
        let key = self.model.key(state);
        if let Some(index) = self.seen.get(&key) {
            return *index;
        }
        let index = self.seen.len();
        self.seen.insert(key, index);
        self.edges.push(BTreeSet::new());
        self.done.push(self.model.done(state));
        self.depths.push(depth);
        if let Err(detail) = self.model.invariant(state) {
            self.flag(
                "invariant",
                depth,
                format!("{detail} in {}", self.model.key(state)),
            );
        }
        queue.push_back((state.clone(), depth, index));
        index
    }

    fn step(
        &mut self,
        state: &M::State,
        event: &M::Event,
        depth: usize,
    ) -> Result<Option<M::State>, String> {
        let model = self.model;
        let mut next = state.clone();
        let accepted = model.apply(&mut next, event)?;
        let (before, after) = (model.parts(state), model.parts(&next));
        let shown = || format!("{event:?} from {}", model.key(state));
        if !accepted {
            self.report.refusals += 1;
            if model.key(&next) != model.key(state) {
                self.flag(
                    "refused_unchanged",
                    depth,
                    format!("refused {} changed it to {}", shown(), model.key(&next)),
                );
            }
        }
        let terminal = |phase: &'static str| !model.moves().iter().any(|(from, _)| *from == phase);
        for old in &before {
            let new = after.iter().find(|part| part.name == old.name);
            let detail = new.map(|part| (part.phase, part.detail.as_str()));
            if terminal(old.phase) && detail != Some((old.phase, old.detail.as_str())) {
                self.flag(
                    "terminal_frozen",
                    depth,
                    format!("{} left terminal {} on {}", old.name, old.phase, shown()),
                );
            }
            let Some(new) = new else { continue };
            if new.phase != old.phase {
                if model.moves().contains(&(old.phase, new.phase)) {
                    self.reached.insert((old.phase, new.phase));
                } else {
                    self.flag(
                        "declared",
                        depth,
                        format!(
                            "{} moved {} -> {} undeclared on {}",
                            old.name,
                            old.phase,
                            new.phase,
                            shown()
                        ),
                    );
                }
            } else if accepted
                && model.addresses(event).as_deref() == Some(old.name.as_str())
                && !model.stays().contains(&(old.phase, model.label(event)))
            {
                self.flag(
                    "declared",
                    depth,
                    format!(
                        "{} accepted {} in {} without a declared stay",
                        old.name,
                        model.label(event),
                        old.phase
                    ),
                );
            }
        }
        for new in &after {
            if !before.iter().any(|part| part.name == new.name) {
                if model.moves().contains(&("absent", new.phase)) {
                    self.reached.insert(("absent", new.phase));
                } else {
                    self.flag(
                        "declared",
                        depth,
                        format!(
                            "{} created in {} undeclared on {}",
                            new.name,
                            new.phase,
                            shown()
                        ),
                    );
                }
            }
        }
        if let Err(detail) = model.after(state, event, &next, accepted) {
            self.flag("postcondition", depth, format!("{detail} on {}", shown()));
        }
        if accepted && model.idempotent(event) {
            let mut again = next.clone();
            model.apply(&mut again, event)?;
            if model.key(&again) != model.key(&next) {
                self.flag(
                    "idempotent",
                    depth,
                    format!(
                        "repeating {event:?} changed {} to {}",
                        model.key(&next),
                        model.key(&again)
                    ),
                );
            }
        }
        Ok(accepted.then_some(next))
    }

    fn run(&mut self) -> Result<(), String> {
        let mut queue = VecDeque::new();
        for state in self.model.initial()? {
            self.intern(&state, 0, &mut queue);
        }
        while let Some((state, depth, index)) = queue.pop_front() {
            if self.stopped() {
                return Ok(());
            }
            self.report.depth = self.report.depth.max(depth);
            if depth >= self.model.depth() {
                continue;
            }
            for event in self.model.events(&state) {
                if let Some(next) = self.step(&state, &event, depth + 1)? {
                    self.report.transitions += 1;
                    let target = self.intern(&next, depth + 1, &mut queue);
                    self.edges[index].insert(target);
                }
                if self.stopped() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, frontier: &BTreeSet<usize>) {
        let declared: BTreeSet<_> = self.model.moves().into_iter().collect();
        self.report.declared = declared.len();
        self.report.reached = self.reached.len();
        let unreached: Vec<_> = declared
            .difference(&self.reached)
            .map(|(from, to)| format!("{from}->{to}"))
            .collect();
        let depth = self.report.depth;
        if !unreached.is_empty() {
            self.flag(
                "reached",
                depth,
                format!("declared but never taken: {}", unreached.join(", ")),
            );
        }
        let mut live: Vec<bool> = (0..self.done.len())
            .map(|index| self.done[index] || frontier.contains(&index))
            .collect();
        loop {
            let mut grew = false;
            for index in 0..live.len() {
                if !live[index] && self.edges[index].iter().any(|next| live[*next]) {
                    live[index] = true;
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        if let Some(stuck) = live.iter().position(|alive| !alive) {
            let key = self
                .seen
                .iter()
                .find(|(_, index)| **index == stuck)
                .map(|(key, _)| key.clone())
                .unwrap_or_default();
            self.flag(
                DEAD_END,
                depth,
                format!("no resting state reachable from {key}"),
            );
        }
    }
}

pub fn walk<M: Model>(model: &M, stop: Option<&'static str>) -> Report {
    let mut walk = Walk {
        model,
        stop,
        report: Report {
            model: model.name(),
            ..Report::default()
        },
        seen: BTreeMap::new(),
        edges: Vec::new(),
        done: Vec::new(),
        depths: Vec::new(),
        reached: BTreeSet::new(),
        flagged: BTreeSet::new(),
    };
    let started = walk.run();
    walk.report.states = walk.seen.len();
    match started {
        Err(error) => walk.report.error = Some(error),
        Ok(()) if !walk.stopped() => {
            let bound = model.depth();
            let frontier = walk
                .depths
                .iter()
                .enumerate()
                .filter(|(_, depth)| **depth >= bound)
                .map(|(index, _)| index)
                .collect();
            walk.finish(&frontier);
        }
        Ok(()) => {}
    }
    walk.report
}

pub struct Canary {
    pub name: &'static str,
    pub model: &'static str,
    pub property: &'static str,
    pub plant: fn(bool),
}

pub struct Entry {
    pub name: &'static str,
    pub run: fn(bool, Option<&'static str>) -> Report,
}

fn models() -> Vec<Entry> {
    let mut models = delivery::models();
    models.extend(claim::models());
    models
}

fn canaries() -> Vec<Canary> {
    let mut canaries = delivery::canaries();
    canaries.extend(claim::canaries());
    canaries
}

pub struct Options {
    pub deep: bool,
    pub shard: (usize, usize),
    pub canaries: bool,
    pub canary: Option<String>,
}

pub fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options {
        deep: false,
        shard: (0, 1),
        canaries: false,
        canary: None,
    };
    let mut rest = args.iter().skip(1);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--deep" => options.deep = true,
            "--canaries" => options.canaries = true,
            "--canary" => options.canary = Some(rest.next()?.clone()),
            "--shard" => {
                let (index, count) = rest.next()?.split_once('/')?;
                let (index, count): (usize, usize) = (index.parse().ok()?, count.parse().ok()?);
                if count == 0 || index == 0 || index > count {
                    return None;
                }
                options.shard = (index - 1, count);
            }
            _ => return None,
        }
    }
    Some(options)
}

pub fn main(args: &[String]) -> ExitCode {
    let Some(options) = parse(args) else {
        eprintln!(
            "usage: nanocode-sim --machines [--deep] [--shard k/N] [--canaries | --canary NAME]"
        );
        return ExitCode::FAILURE;
    };
    let mine = |index: usize| index % options.shard.1 == options.shard.0;
    let mut failed = false;
    if options.canaries || options.canary.is_some() {
        let all = canaries();
        let picked: Vec<_> = all
            .iter()
            .enumerate()
            .filter(|(index, canary)| match &options.canary {
                Some(name) => canary.name == name,
                None => mine(*index),
            })
            .map(|(_, canary)| canary)
            .collect();
        if picked.is_empty() && options.canary.is_some() {
            eprintln!(
                "MACHINES no canary named {}",
                options.canary.as_deref().unwrap_or_default()
            );
            return ExitCode::FAILURE;
        }
        for canary in picked {
            let Some(entry) = models()
                .into_iter()
                .find(|entry| entry.name == canary.model)
            else {
                println!(
                    "CANARY {} names unknown model {}",
                    canary.name, canary.model
                );
                failed = true;
                continue;
            };
            (canary.plant)(true);
            let started = Instant::now();
            let report = (entry.run)(options.deep, Some(canary.property));
            (canary.plant)(false);
            let caught = report
                .violations
                .iter()
                .find(|violation| violation.property == canary.property);
            match (caught, &report.error) {
                (_, Some(error)) => {
                    println!("CANARY {} {} errored: {error}", canary.name, canary.model);
                    failed = true;
                }
                (Some(violation), None) => println!(
                    "CANARY {} {} {} caught states={} depth={} ms={} ({})",
                    canary.name,
                    canary.model,
                    canary.property,
                    violation.states,
                    violation.depth,
                    started.elapsed().as_millis(),
                    violation.detail
                ),
                (None, None) => {
                    println!(
                        "CANARY {} {} {} MISSED after {} states",
                        canary.name, canary.model, canary.property, report.states
                    );
                    failed = true;
                }
            }
        }
        return if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    for (index, entry) in models().into_iter().enumerate() {
        if !mine(index) {
            continue;
        }
        let started = Instant::now();
        let report = (entry.run)(options.deep, None);
        println!(
            "MACHINE {} states={} transitions={} refusals={} depth={} moves={}/{} ms={}",
            report.model,
            report.states,
            report.transitions,
            report.refusals,
            report.depth,
            report.reached,
            report.declared,
            started.elapsed().as_millis()
        );
        if let Some(error) = &report.error {
            println!("MACHINE {} error: {error}", report.model);
            failed = true;
        }
        for violation in &report.violations {
            println!(
                "MACHINE {} {} at states={} depth={}: {}",
                report.model,
                violation.property,
                violation.states,
                violation.depth,
                violation.detail
            );
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
