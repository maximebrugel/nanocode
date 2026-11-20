use std::{
    cell::Cell, collections::BTreeMap, future::Future, num::NonZeroUsize, ops::Range,
    path::PathBuf, pin::Pin, process::ExitCode, rc::Rc, sync::Arc, time::Duration,
};

use nanocode_code_mode::embedded::HandCalls;
use nanocode_code_mode::{ToolContext, ToolOutput, ToolOutputBody};
use nanocode_hands::{ChunkError, Chunks, HostFuture, Sha256Digest, Workspace as HandWorkspace};
use nanocode_shell::{
    Commands, FileSystem, HostInfo, Identity, Limits, Machine, MemFs, NoCommands,
};
use nanocode_sync::block_on;
use nanocode_workspace::{
    Command, Config, Workspace, WorkspaceHost,
    sim::{self as workspace_sim, Mutation as Rule},
};
use serde_json::{Value, json};

use crate::rng::{FNV_OFFSET, Rng, fnv};

const ROOT: &str = "/w";
const HAND: &str = "workspace";
const SESSION: &str = "session";
const FILES: [&str; 5] = ["a.txt", "b.txt", "d/c.txt", "d/e/f.txt", "n.txt"];
const SHELL_FILES: [&str; 3] = ["a.txt", "b.txt", "n.txt"];
const PIECES: [&str; 5] = ["a", "b", "ab", "\n", "c"];
const SLICE: u32 = 4096;
const DEPTH: u32 = nanocode_shell::DEFAULT_DEPTH;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceHostMutation {
    None,
    ReadTruncates,
    RunsTwice,
    ExitIgnored,
    WriteSkipsParents,
}

impl WorkspaceHostMutation {
    const ALL: [Self; 5] = [
        Self::None,
        Self::ReadTruncates,
        Self::RunsTwice,
        Self::ExitIgnored,
        Self::WriteSkipsParents,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mutation| mutation.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadTruncates => "read_truncates",
            Self::RunsTwice => "runs_twice",
            Self::ExitIgnored => "exit_ignored",
            Self::WriteSkipsParents => "write_skips_parents",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .skip(1)
            .map(|mutation| mutation.name())
            .collect::<Vec<_>>()
            .join("|")
    }

    const fn rule(self) -> Rule {
        match self {
            Self::None => Rule::None,
            Self::ReadTruncates => Rule::ReadTruncates,
            Self::RunsTwice => Rule::RunsTwice,
            Self::ExitIgnored => Rule::ExitIgnored,
            Self::WriteSkipsParents => Rule::WriteSkipsParents,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceHostStats {
    pub seeds: u64,
    pub writes: u64,
    pub edits: u64,
    pub edits_refused: u64,
    pub scripts: u64,
    pub scripts_failed: u64,
    pub steps: u64,
    pub puts: u64,
    pub puts_refused: u64,
    pub gets: u64,
}

impl WorkspaceHostStats {
    pub const fn add(&mut self, other: &Self) {
        self.seeds += other.seeds;
        self.writes += other.writes;
        self.edits += other.edits;
        self.edits_refused += other.edits_refused;
        self.scripts += other.scripts;
        self.scripts_failed += other.scripts_failed;
        self.steps += other.steps;
        self.puts += other.puts;
        self.puts_refused += other.puts_refused;
        self.gets += other.gets;
    }
}

pub struct WorkspaceHostOutcome {
    digest: u64,
    wrong: Option<String>,
    stats: WorkspaceHostStats,
    shape: String,
}

struct Clock {
    ticks: Cell<u64>,
    seed: u64,
}

impl Machine for Clock {
    fn now(&self) -> Duration {
        let ticks = self.ticks.get() + 1;
        self.ticks.set(ticks);
        Duration::from_millis(ticks)
    }

    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::pending())
    }

    fn random(&self, bytes: &mut [u8]) {
        let ticks = self.ticks.get() + 1;
        self.ticks.set(ticks);
        let mut rng = Rng::derive(self.seed ^ ticks, "workspace_random");
        for byte in bytes {
            *byte = rng.draw() as u8;
        }
    }
}

struct Slices {
    bytes: Vec<u8>,
    sent: usize,
    size: usize,
}

impl Chunks for Slices {
    fn next(&mut self) -> HostFuture<'_, Result<Option<Vec<u8>>, ChunkError>> {
        let end = self.bytes.len().min(self.sent + self.size);
        let chunk = (self.sent < end).then(|| self.bytes[self.sent..end].to_vec());
        self.sent = end;
        Box::pin(async move { Ok(chunk) })
    }
}

fn text(output: &ToolOutput) -> String {
    match &output.output {
        ToolOutputBody::Text(text) => text.clone(),
        body => serde_json::to_string(body).unwrap_or_default(),
    }
}

fn absolute(path: &str) -> String {
    format!("{ROOT}/{path}")
}

fn words(rng: &mut Rng, most: u64) -> String {
    (0..rng.range(0, most + 1))
        .map(|_| PIECES[rng.pick(PIECES.len())])
        .collect()
}

fn quoted(content: &str) -> String {
    format!("'{}'", content.replace('\'', "'\\''"))
}

fn start(seed: u64) -> Workspace {
    let clock = move || -> Rc<Clock> {
        Rc::new(Clock {
            ticks: Cell::new(0),
            seed,
        })
    };
    Workspace::start(Config {
        files: Arc::new(move || -> Rc<dyn FileSystem> { Rc::new(MemFs::new(clock())) }),
        machine: Arc::new(move || -> Rc<dyn Machine> { clock() }),
        fetch: None,
        credentials: None,
        commands: Arc::new(|| -> Rc<dyn Commands> { Rc::new(NoCommands) }),
        identity: Identity {
            info: HostInfo::default(),
            pid: 1,
            user: b"user".to_vec(),
            group: b"user".to_vec(),
            home: ROOT.as_bytes().to_vec(),
        },
        limits: Limits {
            fuel: u64::MAX,
            output: u64::MAX,
            file: u64::MAX,
            operations: u64::MAX,
            depth: DEPTH,
        },
        env: Vec::new(),
        slice: SLICE,
    })
    .expect("the workspace's worker starts")
}

fn simulate(seed: u64, mutation: WorkspaceHostMutation) -> WorkspaceHostOutcome {
    let mut rng = Rng::derive(seed, "workspace_host");
    let ops = rng.range(8, 40);
    let bytes_max = rng.range(1, 24);
    let shape = format!("ops={ops} bytes_max={bytes_max}");
    let mut stats = WorkspaceHostStats {
        seeds: 1,
        ..WorkspaceHostStats::default()
    };
    let mut digest = FNV_OFFSET;
    let files = start(seed);
    let wrong = block_on(async {
        files.write(b"/w/.keep", b"").await.map_err(|error| format!("setup: {error}"))?;
        let deep = Rng::derive(seed, "workspace_host_depth").range(1, u64::from(DEPTH) - 1);
        let nested = files
            .run(Command {
                script: format!("g(){{ (( $1 <= 0 )) && return; g $(( $1 - 1 )); }}; g {deep}; echo deep"),
                cwd: ROOT.as_bytes().to_vec(),
                env: Vec::new(),
                timeout: None,
            })
            .await?;
        if nested.ran.stdout != b"deep\n" {
            return Err(format!(
                "{deep} nested calls under a limit of {DEPTH}: {:?} {}",
                nested.ran.ended,
                String::from_utf8_lossy(&nested.ran.stderr)
            ));
        }
        let built = nanocode::claude::host_tools(
            PathBuf::from(ROOT),
            Arc::new(files.clone()),
            Arc::new(files.clone()),
        )
        .map_err(|error| format!("host tools: {error:?}"))?;
        let host = WorkspaceHost::new(
            HAND,
            ROOT,
            files.clone(),
            bytes_max,
            built.registered_tools().cloned(),
        );
        workspace_sim::mutate(mutation.rule());
        let mut model: BTreeMap<&'static str, String> = BTreeMap::new();
        for op in 0..ops {
            let step = format!("step-{op}");
            let context = ToolContext::new("", SESSION, &step, &[], usize::MAX);
            let said = match rng.pick(5) {
                0 => {
                    let path = FILES[rng.pick(FILES.len())];
                    let content = words(&mut rng, 6);
                    let input = json!({"file_path": absolute(path), "content": content});
                    let output = host.call("Write", input, context).await;
                    stats.writes += 1;
                    if !output.success {
                        return Err(format!("op {op}: Write {path} failed: {}", text(&output)));
                    }
                    model.insert(path, content);
                    text(&output)
                }
                1 if !model.is_empty() => {
                    let paths: Vec<&'static str> = model.keys().copied().collect();
                    let path = paths[rng.pick(paths.len())];
                    let old = PIECES[rng.pick(PIECES.len())];
                    let new = PIECES[rng.pick(PIECES.len())];
                    let before = model[path].clone();
                    let unique = old != new && before.matches(old).count() == 1;
                    let input = json!({
                        "file_path": absolute(path),
                        "old_string": old,
                        "new_string": new,
                    });
                    let output = host.call("Edit", input, context).await;
                    stats.edits += 1;
                    if output.success != unique {
                        return Err(format!(
                            "op {op}: Edit {old:?} -> {new:?} on {before:?} succeeded={} but one match is {unique}: {}",
                            output.success,
                            text(&output)
                        ));
                    }
                    match unique {
                        true => {
                            model.insert(path, before.replacen(old, new, 1));
                        }
                        false => stats.edits_refused += 1,
                    }
                    text(&output)
                }
                2 => {
                    let mut lines = Vec::new();
                    let mut status = 0;
                    for _ in 0..rng.range(1, 5) {
                        let path = SHELL_FILES[rng.pick(SHELL_FILES.len())];
                        let line = match rng.pick(4) {
                            0 => {
                                let content = words(&mut rng, 3);
                                let line = format!("printf '%s' {} > {}", quoted(&content), absolute(path));
                                if status == 0 {
                                    model.insert(path, content);
                                }
                                line
                            }
                            1 => {
                                let content = words(&mut rng, 3);
                                let line = format!("printf '%s' {} >> {}", quoted(&content), absolute(path));
                                if status == 0 {
                                    model.entry(path).or_default().push_str(&content);
                                }
                                line
                            }
                            2 => {
                                if status == 0 {
                                    model.remove(path);
                                }
                                format!("rm -f {}", absolute(path))
                            }
                            _ => {
                                let code = rng.range(1, 5);
                                if status == 0 {
                                    status = code;
                                }
                                format!("exit {code}")
                            }
                        };
                        lines.push(line);
                    }
                    let output = host
                        .call("Bash", json!({"command": lines.join("\n")}), context)
                        .await;
                    stats.scripts += 1;
                    let said = text(&output);
                    let reported = serde_json::from_str::<Value>(&said)
                        .ok()
                        .and_then(|said| said.get("exit_code").and_then(Value::as_u64))
                        == Some(status);
                    if !reported {
                        return Err(format!(
                            "op {op}: script {lines:?} exited {status} but the Bash tool said {said:?}"
                        ));
                    }
                    if status != 0 {
                        stats.scripts_failed += 1;
                    }
                    said
                }
                3 => {
                    let input = json!({"command": format!("printf x >> {}", absolute("n.txt"))});
                    let output = match rng.pick(2) {
                        0 => HandCalls::call(&host, HAND, "Bash", input, context).await,
                        _ => HandWorkspace::run(&host, SESSION, "Bash", input, context).await,
                    };
                    stats.steps += 1;
                    model.entry("n.txt").or_default().push('x');
                    text(&output)
                }
                _ => {
                    let path = FILES[rng.pick(FILES.len())];
                    let content = words(&mut rng, 8);
                    let honest = rng.pick(3) != 0;
                    let sha256 = match honest {
                        true => Sha256Digest::of(content.as_bytes()),
                        false => Sha256Digest::of(format!("{content}!").as_bytes()),
                    };
                    let size = rng.range(1, 6) as usize;
                    let chunks = Box::new(Slices {
                        bytes: content.clone().into_bytes(),
                        sent: 0,
                        size,
                    });
                    let put = HandWorkspace::put(&host, SESSION, path, chunks, sha256).await;
                    stats.puts += 1;
                    let fits = content.len() as u64 <= bytes_max;
                    if put.is_ok() != (honest && fits) {
                        return Err(format!(
                            "op {op}: put of {} bytes (honest={honest}, max={bytes_max}) gave {put:?}",
                            content.len()
                        ));
                    }
                    match put.is_ok() {
                        true => {
                            model.insert(path, content);
                        }
                        false => stats.puts_refused += 1,
                    }
                    if let Some(expected) = model.get(path) {
                        let chunk = NonZeroUsize::new(size).unwrap_or(NonZeroUsize::MIN);
                        let mut fetched = HandWorkspace::get(&host, SESSION, path, chunk)
                            .await
                            .map_err(|error| format!("op {op}: get {path}: {error:?}"))?;
                        let mut got = Vec::new();
                        while let Some(piece) = fetched
                            .chunks
                            .next()
                            .await
                            .map_err(|error| format!("op {op}: get {path} chunk: {error:?}"))?
                        {
                            got.extend(piece);
                        }
                        stats.gets += 1;
                        if got != expected.as_bytes()
                            || fetched.sha256 != Sha256Digest::of(&got)
                            || fetched.size_bytes != got.len() as u64
                        {
                            return Err(format!("op {op}: get {path} gave {got:?}, the model holds {expected:?}"));
                        }
                    }
                    format!("{put:?}")
                }
            };
            digest = fnv(digest, said.as_bytes());
            for path in FILES {
                let held = files
                    .read(absolute(path).as_bytes())
                    .await
                    .map_err(|error| format!("op {op}: read {path}: {error}"))?;
                let expected = model.get(path).map(|content| content.as_bytes().to_vec());
                if held != expected {
                    return Err(format!(
                        "op {op}: {path} holds {:?}, the model holds {:?}",
                        held.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                        model.get(path)
                    ));
                }
                digest = fnv(digest, held.as_deref().unwrap_or(b"-"));
            }
        }
        Ok(())
    })
    .err();
    workspace_sim::mutate(Rule::None);
    WorkspaceHostOutcome {
        digest,
        wrong,
        stats,
        shape,
    }
}

fn run_seed(seed: u64, mutation: WorkspaceHostMutation) -> WorkspaceHostOutcome {
    std::panic::catch_unwind(|| simulate(seed, mutation)).unwrap_or_else(|panic| {
        workspace_sim::mutate(Rule::None);
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default();
        WorkspaceHostOutcome {
            digest: 0,
            wrong: Some(format!("panic: {message}")),
            stats: WorkspaceHostStats::default(),
            shape: String::new(),
        }
    })
}

pub fn judge(
    seed: u64,
    mutation: WorkspaceHostMutation,
    twice: bool,
    verbose: bool,
    totals: &mut WorkspaceHostStats,
) -> Option<String> {
    let first = run_seed(seed, mutation);
    totals.add(&first.stats);
    if verbose {
        println!(
            "seed {seed} digest {:016x} {} stats={:?}",
            first.digest, first.shape, first.stats
        );
    }
    if first.wrong.is_some() || !twice {
        return first
            .wrong
            .map(|message| format!("{} :: {message}", first.shape));
    }
    let second = run_seed(seed, mutation);
    (second.digest != first.digest || second.wrong.is_some()).then(|| {
        format!(
            "{} :: nondeterminism: digest {:016x} then {:016x}",
            first.shape, first.digest, second.digest
        )
    })
}

pub fn replay(seed: u64, mutation: WorkspaceHostMutation) -> String {
    let mut command = format!("nanocode-sim --scenario workspace_host --seed {seed}");
    if mutation != WorkspaceHostMutation::None {
        command.push_str(" --mutation ");
        command.push_str(mutation.name());
    }
    command
}

pub fn report(
    seeds: &Range<u64>,
    passed: u64,
    failed: u64,
    twice: bool,
    mutation: WorkspaceHostMutation,
    totals: &WorkspaceHostStats,
) -> ExitCode {
    println!(
        "RESULT seeds={}..{} scenario=workspace_host mutation={} passed={passed} failed={failed} twice={twice} stats={totals:?}",
        seeds.start,
        seeds.end,
        mutation.name()
    );
    match mutation {
        WorkspaceHostMutation::None if failed == 0 => ExitCode::SUCCESS,
        WorkspaceHostMutation::None => ExitCode::FAILURE,
        _ if failed > 0 => {
            println!("MUTATION {} caught by {failed} seeds", mutation.name());
            ExitCode::SUCCESS
        }
        _ => {
            println!("MUTATION {} NOT caught", mutation.name());
            ExitCode::FAILURE
        }
    }
}
