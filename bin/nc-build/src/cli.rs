use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Read, Seek, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use nanocode_build_opt::{
    ab::{Comparison, Side, Verdict},
    graph::Graph,
    idle,
    plan::{self, Rules, Scope},
    schedule::{self, Job, Shard},
    slots::{self, Machine, Observation, Pin, Placement, Pool, Refusal, State, Want},
};

const USAGE: &str = "usage:
  nc-build affected [--base REF]      crates the diff since REF (and untracked files) touches, with their dependents; all when a manifest, the lockfile or .cargo changed
  nc-build analyze [--timings DIR|FILE] [--since 30.days] [--manifest-path M] [--top 10]
                                      ranks structural build changes by predicted seconds saved, from cargo metadata, git log and cargo --timings
  nc-build shard K/N [--durations F]  reads job keys on stdin and prints this shard's keys, longest first (F: lines of key and milliseconds)
  nc-build slots take --cores N --label L [--kind K] [--pool large|small] [--machine M] [--minutes T] [--wait S] [--env]
                                      takes N cores on a machine with room and prints the lease as JSON, whose env (all --env prints) is a line to eval;
                                      room means leased cores + unleased busy cores (the latest observation's runnable threads outside SCHED_IDLE
                                      on cpus no lease holds) + N <= the machine's cores, so a take never over-commits: without room it fails, or
                                      with --wait S retries for S seconds; leased cores count cpus, each once: a cpu is leased when any live lease
                                      or reservation covers it, so leases nested in a reservation or overlapping each other add nothing; leases
                                      on machines no longer in 'slots boxes' (removed or expired) are given back by take, list and observe
                                      (NC_BUILD_LEASE, NC_BUILD_LEASE_CORES and CARGO_BUILD_JOBS, which caps rustc at N, and NC_BUILD_PREFIX); among machines with room it picks
                                      the one with the fewest cores leased to kind K (default build), so kinds mix and boxes stay full, then
                                      the pool order: the large pool (default, nanocode work) the largest boxes first, the small pool
                                      (pyramid2 work) the smaller boxes and dev-georgios first; gak-9 only with --machine gak-9;
                                      without --machine it takes on this box, the name in $NC_BUILD_MACHINE or ~/.config/nc-build/machine,
                                      when that is set, so only an explicit --machine places elsewhere; a lease on a machine that is
                                      neither that name nor this hostname prints a warning to stderr;
                                      a label containing fill (any case) is refused: filler runs SCHED_IDLE in fill.slice and never holds a lease;
                                      stop only jobs under your own lease: by its systemd unit or cgroup, or by a PID file you wrote; never pkill
                                      or killall by process name, which also kills other lanes' jobs that share the name;
                                      a label starting with a standing --label-prefix reservation's prefix places inside its cpus when they have room;
                                      a take whose label and kind a lane cap covers is refused (and with --wait retried) when that lane's
                                      leased cores across all machines plus N would exceed the cap; takes placed inside a standing
                                      --label-prefix reservation neither count nor are refused
  (a take refuses a box whose lease would run into its last 15 minutes before expiry, saying when the box closes (PST);
   use a shorter --minutes or another box; a take --wait without --machine, on this box by default, moves to the other boxes then)
  (fair queue: a take --wait is recorded in the state as a waiter with its request time, cores and --machine while it waits;
   the oldest live waiter on each machine holds the window of its N contiguous cpus that frees soonest, and every other
   take sees that window's free cpus as busy, except takes inside a standing reservation matching their label and a
   backfill whose --minutes ends before the window frees; a waiter is dropped once it gets its lease, its --wait
   runs out, or it has not retried for 2 minutes; 'slots list' shows waiters and their holds)
  nc-build slots give LEASE | slots renew LEASE [--minutes T]
  nc-build slots reserve --machine M --cpus FIRST-LAST --until 'YYYY-MM-DD HH:MM' --label L [--kind reserved] [--overlap]
  nc-build slots reserve --machine M --cpus FIRST-LAST --until 'YYYY-MM-DD HH:MM' --label-prefix P [--overlap]
                                      holds those cpus until that time (PST) so other takes never place there; with --kind reserved the
                                      first take with the same label (and fitting --cores) gets that lease, which give then frees;
                                      with --label-prefix P it is a standing reservation (kind reserved, label P*): every take whose label
                                      starts with P places inside those cpus while they have room, as its own lease, and the reservation
                                      stays until it expires or is given; other takes see the cpus as busy;
                                      --overlap places it over leases that hold those cpus now, which keep them until they end
  nc-build slots observe MACHINE [--local] [--check] [--idle-below P] [--idle-minutes M]
                                      reads the output of 'nproc --all; cat /proc/loadavg /proc/meminfo; ps -eLo policy=,stat=,psr=' from that machine
                                      on stdin (--local runs it here); busy cores are its runnable threads outside SCHED_IDLE, so preemptible
                                      filler never blocks a take, or without the ps lines the 1-minute load average; appends a sample line
                                      to machines.log, and an alarm line when busy exceeds cores, which --check turns into exit status 2
                                      with --local, a lease on MACHINE whose cpus stayed under P% (default 10) busy for M minutes
                                      (default 10), from /proc/stat deltas between observations with nice time not busy, is given back
                                      and logged as a reap line with its machine, cpus, kind and label; leases of kind reserved,
                                      closed-load or bench and labels starting box-closing-, nf-deploy-, nf-fan-, p2live-,
                                      lander-p2live or boardrel-red- are never reaped
  nc-build slots caps [--set]         the lane caps; --set replaces them with the JSON array read on stdin, each entry
                                      {lane: L, prefixes: [P, ...], cores: N, kinds: [K, ...]}: a lane covers takes whose label
                                      starts with one of its prefixes and whose kind is listed (without kinds, every kind but bench), and
                                      holds the cores of its live leases on every machine except kinds reserved and closed-load, labels
                                      starting box-closing- and leases inside a standing --label-prefix reservation; [] removes them all
  nc-build slots list                 each machine's busy and leased cores (counted per cpu, as take counts them), load, memory and expiry,
                                      also appended to machines.log, a total over the machines in 'slots boxes',
                                      and a line 'lane L held X/N' per lane cap
  nc-build slots boxes [--set]        the machines placement uses; --set stores the boxes.json array read on stdin in the state;
                                      an entry with a kinds list, such as [bench], takes only leases of those kinds
  nc-build slots serve ADDR [--path P]
                                      serves the state over HTTP for clients that set $NC_BUILD_SLOTS_URL
  nc-build ab --a CMD --b CMD [--runs N] [--setup CMD] [--label L] [--post TASK [--proposal TEXT] [--board-dir D] [--board-as A]]
                                      runs A and B interleaved N times (default 5) with setup before each run, prints medians, min, max,
                                      the delta and a verdict (faster when the ranges do not overlap, else noise) as text and a JSON line,
                                      and appends it to $NC_BUILD_AB_LOG (~/swarm/ab.jsonl); --post files the winner as a board task
                                      through $NC_BOARD_BIN (nc-board) and posts nothing on noise
  state: $NC_BUILD_SLOTS_URL or ~/.config/nc-build/slots-url (an nc-build slots serve endpoint, reached with curl); else
  $NC_BUILD_SLOTS_GIT or ~/.config/nc-build/slots-git, a git remote whose branch $NC_BUILD_SLOTS_BRANCH (nc-build-slots) holds
  slots.json as one parentless commit, replaced by a push with --force-with-lease on the fetched commit that retries when
  another push won; else the file $NC_BUILD_SLOTS
  (~/swarm/slots.json); lane caps live apart from it, where clients older than caps never write: caps.json on the branch
  NC_BUILD_SLOTS_BRANCH-caps (nc-build-slots-caps), else slots.caps.json beside the file; caps still found in slots.json
  move there on first use; machines: the stored boxes, else $NC_BUILD_BOXES (~/swarm/boxes.json), each until it expires;
  dev-georgios only with a fresh observation, gak-9 by name; log: $NC_BUILD_MACHINES_LOG (~/swarm/machines.log)";

pub fn main() -> ExitCode {
    let started = std::time::Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("check") => {
            let code = crate::check::main(&args[1..]);
            if !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "--plan" | "-h" | "--help"))
            {
                metric(&args, started.elapsed().as_millis(), code, compiled());
            }
            return ExitCode::from(code);
        }
        Some("affected") => affected(&args[1..]),
        Some("shard") => shard(&args[1..]),
        Some("slots") => slots_command(&args[1..]),
        Some("ab") => ab_command(&args[1..]),
        Some("analyze") => crate::analyze::analyze(&args[1..]),
        _ => Err(USAGE.to_owned()),
    };
    let code = match result {
        Ok(()) => 0,
        Err(message) => {
            let _ = writeln!(io::stderr(), "{message}");
            2
        }
    };
    metric(&args, started.elapsed().as_millis(), code, None);
    ExitCode::from(code)
}

/// After a check: whether cargo compiled nothing, from the first lines of each output/check log, where
/// cargo prints its Compiling lines; None when no log is there.
fn compiled() -> Option<bool> {
    let logs = std::fs::read_dir("output/check").ok()?;
    let mut any = false;
    for entry in logs.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "log") {
            continue;
        }
        any = true;
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        if io::BufReader::new(file)
            .lines()
            .take(40)
            .map_while(Result::ok)
            .any(|line| {
                line.starts_with("Compiling") || line.trim_start().starts_with("Compiling ")
            })
        {
            return Some(false);
        }
    }
    any.then_some(true)
}

/// Appends one point for this invocation to the swarm dashboard's feed, in its ingest format:
/// `$NC_METRICS_LOG` (`off`: none), else ~/swarm/dash/feeds/BUILD.jsonl where that directory
/// exists, else ~/swarm/metrics/BUILD.jsonl. Best effort: it reads HEAD from .git without
/// running git, and ignores any error, so it never changes the result.
fn metric(args: &[String], wall_ms: u128, exit: u8, cached: Option<bool>) {
    let started = std::time::Instant::now();
    let path = match std::env::var_os("NC_METRICS_LOG") {
        Some(path) if path == "off" => return,
        Some(path) => PathBuf::from(path),
        None if home().join("swarm/dash/feeds").is_dir() => {
            home().join("swarm/dash/feeds/BUILD.jsonl")
        }
        None => home().join("swarm/metrics/BUILD.jsonl"),
    };
    let cmd = match args {
        [] => return,
        [group, sub, ..] if group == "slots" => format!("nc-build slots {sub}"),
        // the series scripts/check wrote before it ran this engine
        [sub, rest @ ..] if sub == "check" => {
            if rest.iter().any(|arg| arg == "--full") {
                "check --full".to_owned()
            } else {
                "check".to_owned()
            }
        }
        [sub, ..] => format!("nc-build {sub}"),
    };
    let (commit, repo) = git_head().unwrap_or_default();
    let host = hostname();
    let point = serde_json::json!({
        "name": format!("build {repo} {cmd}"),
        "value": wall_ms,
        "unit": "ms",
        "better": "down",
        "source": "measured",
        "at": now_ms(),
        "commit": commit,
        "note": format!("{host} exit={exit}"),
        "repo": repo,
        "cmd": cmd,
        "box": host,
        "exit": exit,
        "noop": false,
        "cached": cached,
        "metric_us": started.elapsed().as_micros(),
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(format!("{point}\n").as_bytes());
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{program}: {error}"))
}

fn affected(args: &[String]) -> Result<(), String> {
    let base = option(args, "--base").unwrap_or("HEAD");
    let metadata = run(
        "cargo",
        &["metadata", "--format-version", "1", "--locked", "--no-deps"],
    )?;
    let graph = Graph::from_metadata(&metadata).map_err(|error| format!("{error:?}"))?;
    let mut changed: BTreeSet<String> = BTreeSet::new();
    for listing in [
        run("git", &["diff", "--name-only", base, "--"])?,
        run("git", &["ls-files", "--others", "--exclude-standard"])?,
    ] {
        changed.extend(
            listing
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
    }
    let changed: Vec<String> = changed.into_iter().collect();
    let mut out = io::stdout().lock();
    match plan::affected(&graph, &Rules::cargo(), &changed) {
        Scope::Everything => writeln!(out, "all"),
        Scope::Crates(crates) => crates.iter().try_for_each(|name| writeln!(out, "{name}")),
    }
    .map_err(|error| error.to_string())
}

fn shard(args: &[String]) -> Result<(), String> {
    let shard = args
        .first()
        .map(|text| Shard::parse(text))
        .transpose()
        .map_err(|error| format!("{error:?}"))?
        .unwrap_or(Shard::WHOLE);
    let durations = match option(args, "--durations") {
        Some(path) => std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))?,
        None => String::new(),
    };
    let expected = |key: &str| {
        durations.lines().find_map(|line| {
            let (name, ms) = line.rsplit_once(char::is_whitespace)?;
            (name.trim() == key)
                .then(|| ms.trim().parse().ok())
                .flatten()
        })
    };
    let jobs: Vec<Job> = io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .map(|key| Job {
            expected_ms: expected(&key),
            key,
        })
        .collect();
    let mut out = io::stdout().lock();
    schedule::plan(jobs, shard)
        .iter()
        .try_for_each(|job| writeln!(out, "{}", job.key))
        .map_err(|error| error.to_string())
}

/// The current directory's checkout: HEAD's commit (12 hex digits) and the repository's name,
/// read from .git (a worktree's gitdir, its commondir, loose refs and packed-refs) without running
/// git, which costs more than the rest of the point.
fn git_head() -> Option<(String, String)> {
    let cwd = std::env::current_dir().ok()?;
    let dot = cwd
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|dot| dot.exists())?;
    let gitdir = if dot.is_file() {
        let text = std::fs::read_to_string(&dot).ok()?;
        dot.parent()?.join(text.trim().strip_prefix("gitdir: ")?)
    } else {
        dot
    };
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .map_or_else(|_| gitdir.clone(), |text| gitdir.join(text.trim()));
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let commit = match head.strip_prefix("ref: ") {
        None => head.to_owned(),
        Some(name) => std::fs::read_to_string(gitdir.join(name))
            .or_else(|_| std::fs::read_to_string(common.join(name)))
            .ok()
            .or_else(|| {
                let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
                packed.lines().find_map(|line| {
                    let (sha, reference) = line.split_once(' ')?;
                    (reference == name).then(|| sha.to_owned())
                })
            })?,
    };
    let commit: String = commit.trim().chars().take(12).collect();
    let common = std::fs::canonicalize(&common).unwrap_or(common);
    let base = if common.ends_with(".git") {
        common.parent()?
    } else {
        common.as_path()
    };
    let name = base.file_name()?.to_str()?;
    let name = name.strip_suffix(".git").unwrap_or(name);
    Some((commit, name.to_owned()))
}

fn print(value: &serde_json::Value) -> Result<(), String> {
    writeln!(io::stdout().lock(), "{value}").map_err(|error| error.to_string())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

const NO_ROOM: &str = "no machine has";
const KILL_RULE: &str = "; stop only jobs under your own lease (its systemd unit or cgroup, or a PID file you wrote), never pkill or killall by process name";

const OVER_CAP: &str = "over its cap:";

const PST_OFFSET_MINUTES: i64 = -8 * 60;

/// No lease may run into a box's last 15 minutes: the box is swapped then.
const CLOSE_WINDOW_MS: u64 = slots::CLOSE_WINDOW_MS;

fn fits_before_close(expires_ms: Option<u64>, now: u64, ttl: u64) -> bool {
    expires_ms.is_none_or(|expires| expires.saturating_sub(CLOSE_WINDOW_MS) >= now + ttl)
}

fn machines(pool: Pool, state: &State, now: u64) -> Vec<Machine> {
    let boxes = match &state.boxes {
        Some(boxes) => boxes.to_string(),
        None => std::fs::read_to_string(
            std::env::var_os("NC_BUILD_BOXES")
                .map_or_else(|| home().join("swarm/boxes.json"), PathBuf::from),
        )
        .unwrap_or_default(),
    };
    let gak = Machine {
        name: "gak-9".to_owned(),
        cores: 6,
        expires_ms: None,
        pin: Pin::Nice,
        placement: Placement::Named,
        kinds: None,
    };
    let dev = Machine {
        name: "dev-georgios".to_owned(),
        cores: 32,
        expires_ms: None,
        pin: Pin::Taskset,
        placement: Placement::LastResort,
        kinds: None,
    };
    let mut machines = slots::machines_from_boxes(&boxes, pool, &[gak], &[dev]);
    machines.retain(|machine| machine.expires_ms.is_none_or(|expires| expires > now));
    machines
}

fn reap_gone(state: &mut State, machines: &[Machine]) -> Vec<slots::Lease> {
    if machines
        .iter()
        .any(|machine| machine.placement == Placement::Fill)
    {
        state.reap_gone(machines)
    } else {
        Vec::new()
    }
}

fn log_gone(gone: &[slots::Lease]) {
    for lease in gone {
        log_line(&format!(
            "reap {} {} cpus {} {} {} (box gone)",
            lease.id,
            lease.machine,
            lease.cpus(),
            lease.kind,
            lease.label
        ));
    }
}

fn config(name: &str, file: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| std::fs::read_to_string(home().join(".config/nc-build").join(file)).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn run_input(program: &str, args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| format!("{program}: no stdin"))?
        .write_all(input.as_bytes())
        .map_err(|error| format!("{program}: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{program}: {error}"))
}

fn slots_branch() -> String {
    config("NC_BUILD_SLOTS_BRANCH", "slots-branch").unwrap_or_else(|| "nc-build-slots".to_owned())
}

fn with_git_file<T>(
    remote: &str,
    branch: &str,
    file: &str,
    mut change: impl FnMut(Option<&str>) -> Result<(Option<String>, T), String>,
) -> Result<T, String> {
    let dir = home().join(".cache/nc-build/slots.git");
    let dir = dir.to_str().ok_or("cache path is not UTF-8")?.to_owned();
    std::fs::create_dir_all(&dir).map_err(|error| format!("{dir}: {error}"))?;
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(&dir).join("nc-build.lock"))
        .map_err(|error| format!("{dir}: {error}"))?;
    lock.lock().map_err(|error| error.to_string())?;
    if !Path::new(&dir).join("HEAD").exists() {
        run("git", &["init", "-q", "--bare", &dir])?;
    }
    let git = |args: &[&str], input: &str| {
        let mut all = vec![
            "--git-dir",
            dir.as_str(),
            "-c",
            "user.name=nc-build",
            "-c",
            "user.email=nc-build@localhost",
        ];
        all.extend_from_slice(args);
        run_input("git", &all, input)
    };
    loop {
        let parent = match git(&["fetch", "-q", "--depth", "1", remote, branch], "") {
            Ok(_) => Some(git(&["rev-parse", "FETCH_HEAD"], "")?.trim().to_owned()),
            Err(error) if error.contains("couldn't find remote ref") => None,
            Err(error) => return Err(error),
        };
        let text = match &parent {
            Some(parent) => Some(git(&["show", &format!("{parent}:{file}")], "")?),
            None => None,
        };
        let (after, result) = change(text.as_deref())?;
        let Some(after) = after else {
            return Ok(result);
        };
        let blob = git(&["hash-object", "-w", "--stdin"], &after)?;
        let tree = git(
            &["mktree"],
            &format!("100644 blob {}\t{file}\n", blob.trim()),
        )?;
        let commit = git(&["commit-tree", tree.trim(), "-m", "slots"], "")?;
        let lease = format!(
            "--force-with-lease=refs/heads/{branch}:{}",
            parent.as_deref().unwrap_or_default()
        );
        match git(
            &[
                "push",
                "-q",
                &lease,
                remote,
                &format!("{}:refs/heads/{branch}", commit.trim()),
            ],
            "",
        ) {
            Ok(_) => return Ok(result),
            Err(error) if error.contains("stale info") || error.contains("rejected") => {
                std::thread::sleep(std::time::Duration::from_millis(now_ms() % 500));
            }
            Err(error) => return Err(error),
        }
    }
}

fn with_git_state<T>(remote: &str, mut change: impl FnMut(&mut State) -> T) -> Result<T, String> {
    let branch = slots_branch();
    with_git_file(remote, &branch, "slots.json", |json| {
        let mut state = State::from_json(json.unwrap_or_default())
            .ok_or_else(|| format!("{remote} {branch}: not a slots state"))?;
        let before = state.to_json();
        let result = change(&mut state);
        let after = state.to_json();
        Ok(((json.is_none() || after != before).then_some(after), result))
    })
}

fn slots_path() -> PathBuf {
    std::env::var_os("NC_BUILD_SLOTS")
        .map_or_else(|| home().join("swarm/slots.json"), PathBuf::from)
}

fn parse_caps(text: &str, from: &str) -> Result<Vec<slots::Cap>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("{from}: {error}"))?;
    slots::caps_from_json(&value).map_err(|error| format!("{from}: {error}"))
}

fn caps_text(caps: &[slots::Cap]) -> String {
    serde_json::Value::from(caps.iter().map(slots::Cap::to_json).collect::<Vec<_>>()).to_string()
}

fn stored_caps(write: Option<&[slots::Cap]>) -> Result<Option<Vec<slots::Cap>>, String> {
    if let Some(remote) = config("NC_BUILD_SLOTS_GIT", "slots-git") {
        let branch = format!("{}-caps", slots_branch());
        let from = format!("{remote} {branch} caps.json");
        return with_git_file(&remote, &branch, "caps.json", |text| {
            let read = text.map(|text| parse_caps(text, &from)).transpose()?;
            Ok((
                write.map(caps_text),
                write.map(<[slots::Cap]>::to_vec).or(read),
            ))
        });
    }
    let path = slots_path().with_extension("caps.json");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.lock().map_err(|error| error.to_string())?;
    if let Some(caps) = write {
        file.set_len(0).map_err(|error| error.to_string())?;
        file.write_all(caps_text(caps).as_bytes())
            .map_err(|error| error.to_string())?;
        return Ok(Some(caps.to_vec()));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    parse_caps(&text, &path.display().to_string()).map(Some)
}

fn with_state<T>(mut change: impl FnMut(&mut State) -> T) -> Result<T, String> {
    let stored = stored_caps(None)?;
    let mut caps = Vec::new();
    let mut idle = Vec::new();
    let result = with_slots_state(|state| {
        idle = state.give_back_idle(now_ms());
        let legacy = std::mem::take(&mut state.caps);
        state.caps = stored.clone().unwrap_or(legacy);
        let result = change(state);
        caps = std::mem::take(&mut state.caps);
        result
    })?;
    log_idle(&idle, now_ms());
    if stored.as_ref() != Some(&caps) && (stored.is_some() || !caps.is_empty()) {
        stored_caps(Some(&caps))?;
    }
    Ok(result)
}

fn idle_line(lease: &slots::Lease, now: u64) -> String {
    let idle = lease
        .used_ms
        .map_or(0, |used| now.saturating_sub(used) / 60_000);
    format!(
        "reservation {} {} cpus {} {} idle {idle}m, given back",
        lease.id,
        lease.machine,
        lease.cpus(),
        lease.label
    )
}

fn log_idle(idle: &[slots::Lease], now: u64) {
    for lease in idle {
        log_line(&idle_line(lease, now));
    }
}

fn with_slots_state<T>(mut change: impl FnMut(&mut State) -> T) -> Result<T, String> {
    if let Some(remote) = config("NC_BUILD_SLOTS_GIT", "slots-git") {
        return with_git_state(&remote, change);
    }
    let path = slots_path();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.lock().map_err(|error| error.to_string())?;
    let mut json = String::new();
    file.read_to_string(&mut json)
        .map_err(|error| error.to_string())?;
    let mut state =
        State::from_json(&json).ok_or_else(|| format!("{}: not a slots state", path.display()))?;
    let result = change(&mut state);
    let json = state.to_json();
    file.set_len(0).map_err(|error| error.to_string())?;
    file.rewind().map_err(|error| error.to_string())?;
    file.write_all(json.as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(result)
}

fn minutes(args: &[String], default: u64) -> Result<u64, String> {
    option(args, "--minutes")
        .map_or(Ok(default), str::parse)
        .map(|minutes: u64| minutes * 60_000)
        .map_err(|error| format!("--minutes: {error}"))
}

fn without(args: &[String], flag: &str, takes_value: bool) -> Vec<String> {
    let mut kept = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if arg == flag {
            skip = takes_value;
        } else {
            kept.push(arg.clone());
        }
    }
    kept
}

fn idle_leases(args: &[String], url: Option<&str>) -> Result<(), String> {
    let below: u64 = option(args, "--idle-below")
        .map_or(Ok(IDLE_BELOW_PERCENT), str::parse)
        .map_err(|error| format!("--idle-below: {error}"))?;
    let idle_ms: u64 = option(args, "--idle-minutes")
        .map_or(Ok(IDLE_MINUTES), str::parse::<u64>)
        .map_err(|error| format!("--idle-minutes: {error}"))?
        * 60_000;
    let name = args.get(1).ok_or("observe MACHINE")?;
    let stat =
        std::fs::read_to_string("/proc/stat").map_err(|error| format!("/proc/stat: {error}"))?;
    let dir = home().join(".cache/nc-build");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let stat_path = dir.join(format!("stat.{name}"));
    let since_path = dir.join(format!("idle-since.{name}"));
    let before = std::fs::read_to_string(&stat_path).unwrap_or_default();
    let before_ms = std::fs::metadata(&stat_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map(|at| u64::try_from(at.as_millis()).unwrap_or(u64::MAX));
    std::fs::write(&stat_path, &stat).map_err(|error| error.to_string())?;
    let Some(before_ms) = before_ms.filter(|_| !before.is_empty()) else {
        return Ok(());
    };
    let now = now_ms();
    let call = |words: &[&str]| {
        let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        match url {
            Some(url) => remote(url, &words, ""),
            None => slots_once(&words, ""),
        }
    };
    let leases = idle::leases_on(&call(&["list"])?, name);
    let mut since: std::collections::BTreeMap<String, u64> = std::fs::read_to_string(&since_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (id, at) = line.split_once(' ')?;
            Some((id.to_owned(), at.parse().ok()?))
        })
        .collect();
    let idle = idle::idle_step(
        &mut since,
        &leases,
        (&idle::cpu_times(&before), before_ms),
        (&idle::cpu_times(&stat), now),
        below,
        idle_ms,
    );
    for lease in idle {
        let what = format!(
            "{} {name} cpus {} {} {} (under {below}% busy for {}m, {}% now)",
            lease.id,
            lease.cpus,
            lease.kind,
            lease.label,
            lease.idle_ms / 60_000,
            lease.busy_percent
        );
        match call(&["give", &lease.id]) {
            Ok(_) => log_line(&format!("reap {what}")),
            Err(error) => log_line(&format!("WARN reap failed {what}: {error}")),
        }
        since.remove(&lease.id);
    }
    let since: String = since
        .iter()
        .map(|(id, at)| format!("{id} {at}\n"))
        .collect();
    std::fs::write(&since_path, since).map_err(|error| error.to_string())
}

const IDLE_BELOW_PERCENT: u64 = 10;

const IDLE_MINUTES: u64 = 10;

fn read_stdin() -> Result<String, String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| error.to_string())?;
    Ok(input)
}

fn slots_command(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("serve") {
        return serve(&args[1..]);
    }
    let url = config("NC_BUILD_SLOTS_URL", "slots-url");
    let command = args.first().map(String::as_str);
    let local = args.iter().any(|arg| arg == "--local");
    if command == Some("observe") && local {
        idle_leases(args, url.as_deref())?;
    }
    let input = match command {
        Some("observe") if local => run("sh", &["-c", PROBE])?,
        Some("observe") => read_stdin()?,
        Some("boxes" | "caps") if args.iter().any(|arg| arg == "--set") => read_stdin()?,
        _ => String::new(),
    };
    let wait_ms = option(args, "--wait")
        .map_or(Ok(0), str::parse::<u64>)
        .map_err(|error| format!("--wait: {error}"))?
        * 1000;
    let args = without(&without(args, "--local", false), "--wait", true);
    let take = command == Some("take");
    let env = take && args.iter().any(|arg| arg == "--env");
    let local = config("NC_BUILD_MACHINE", "machine");
    let started = now_ms();
    let args = if take {
        queued_take(
            &local_take(&without(&args, "--env", false), local.as_deref()),
            started,
            wait_ms,
            &hostname(),
        )
    } else {
        args
    };
    let once = || match &url {
        Some(url) => remote(url, &args, &input),
        None => slots_once(&args, &input),
    };
    loop {
        match once() {
            Ok(out) if take => {
                let lease: serde_json::Value =
                    serde_json::from_str(&out).map_err(|error| format!("take: {error}: {out}"))?;
                let host = hostname();
                let hosts: Vec<&str> = [Some(host.as_str()), local.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter(|name| !name.is_empty())
                    .collect();
                if let Some(warning) =
                    placement_warning(lease["machine"].as_str().unwrap_or_default(), &hosts)
                {
                    let _ = writeln!(io::stderr(), "{warning}");
                }
                let out = if env {
                    format!("{}\n", lease["env"].as_str().unwrap_or_default())
                } else {
                    out
                };
                return io::stdout()
                    .lock()
                    .write_all(out.as_bytes())
                    .map_err(|error| error.to_string());
            }
            Ok(out) => {
                return io::stdout()
                    .lock()
                    .write_all(out.as_bytes())
                    .map_err(|error| error.to_string());
            }
            Err(message)
                if (message.starts_with(NO_ROOM) || message.starts_with(OVER_CAP))
                    && now_ms().saturating_sub(started) < wait_ms =>
            {
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
            Err(message) => return Err(message),
        }
    }
}

fn local_take(args: &[String], local: Option<&str>) -> Vec<String> {
    let mut args = args.to_vec();
    if let Some(local) = local
        && option(&args, "--machine").is_none()
    {
        args.extend([
            "--machine".to_owned(),
            local.to_owned(),
            "--machine-default".to_owned(),
        ]);
    }
    args
}

/// HH:MM of a time in ms, in PST.
fn clock(ms: u64) -> String {
    let minutes =
        (i64::try_from(ms / 60_000).unwrap_or(0) + PST_OFFSET_MINUTES).rem_euclid(24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// Takes a lease on the machines whose close window it fits before. A take on a box whose lease
/// would run into its last 15 minutes is refused with the time the box closes, so callers
/// re-target; a take --wait on this box by default (no --machine given) moves to the other boxes
/// instead, as a waiter for any machine.
fn take_fitting(
    state: &mut State,
    mut machines: Vec<Machine>,
    want: &Want,
    implicit: bool,
    now: u64,
) -> Result<(serde_json::Value, slots::Lease), String> {
    let mut closing = None;
    machines.retain(|machine| {
        let fits = fits_before_close(machine.expires_ms, now, want.ttl_ms);
        if !fits && want.only.as_deref() == Some(machine.name.as_str()) {
            closing = machine.expires_ms;
        }
        fits
    });
    let moved;
    let want = match (closing, implicit, &want.queue) {
        (Some(_), true, Some(_)) => {
            moved = Want {
                only: None,
                ..want.clone()
            };
            closing = None;
            &moved
        }
        _ => want,
    };
    let cores = want.cores;
    let taken = state.take(&machines, want, now);
    if let (Err(_), Some(expires), Some(name)) = (&taken, closing, &want.only) {
        if let Some(queue) = &want.queue {
            state.waiters.retain(|waiter| waiter.id != queue.id);
        }
        let left = expires.saturating_sub(now) / 60_000;
        let room = expires.saturating_sub(now + CLOSE_WINDOW_MS) / 60_000;
        return Err(format!(
            "{name} box closes at {} PST (expires at {} PST, in {left}m) and no lease may run into its last {}m; ask for --minutes {room} or less, or take on another box",
            clock(expires.saturating_sub(CLOSE_WINDOW_MS)),
            clock(expires),
            CLOSE_WINDOW_MS / 60_000
        ));
    }
    let lease = taken.map_err(|refusal| match (refusal, &want.only) {
        (Refusal::NoRoom, Some(name)) => format!("{NO_ROOM} {cores} free cores on {name}{KILL_RULE}"),
        (Refusal::NoRoom, None) => format!("{NO_ROOM} {cores} free cores{KILL_RULE}"),
        (Refusal::OverCap { lane, held, cap }, _) => format!(
            "{OVER_CAP} lane {lane} holds {held}/{cap} cores across the fleet, so --label {} cannot take {cores} more",
            want.label
        ),
    })?;
    let machine = machines
        .iter()
        .find(|machine| machine.name == lease.machine);
    Ok((lease.to_json(machine), lease))
}

fn placement_warning(machine: &str, hosts: &[&str]) -> Option<String> {
    (!hosts.contains(&machine)).then(|| {
        format!(
            "warning: lease is on {machine}, not this box ({}): run the build there or give the lease back",
            hosts.join(", ")
        )
    })
}

fn remote(url: &str, args: &[String], input: &str) -> Result<String, String> {
    let request = serde_json::json!({ "args": args, "stdin": input }).to_string();
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "--data-binary",
            "@-",
            "-H",
            "content-type: application/json",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("curl: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("curl: no stdin")?
        .write_all(request.as_bytes())
        .map_err(|error| format!("curl: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("curl: {error}"))?;
    let body = String::from_utf8_lossy(&output.stdout);
    let reply: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        format!(
            "slots server {url}: {}{body}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    let out = reply["out"].as_str().unwrap_or_default().to_owned();
    if reply["ok"].as_bool() == Some(true) {
        Ok(out)
    } else {
        Err(out)
    }
}

fn serve(args: &[String]) -> Result<(), String> {
    let listen = args.first().ok_or("serve ADDR [--path P]")?;
    let path = option(args, "--path").unwrap_or("/").to_owned();
    let listener = TcpListener::bind(listen).map_err(|error| format!("{listen}: {error}"))?;
    writeln!(io::stderr(), "nc-build slots serving on {listen}")
        .map_err(|error| error.to_string())?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let path = path.clone();
        std::thread::spawn(move || {
            let _ = answer(stream, &path);
        });
    }
    Ok(())
}

fn answer(stream: TcpStream, path: &str) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let (status, reply) = if target != path {
        (
            "404 Not Found",
            serde_json::json!({ "ok": false, "out": "not found" }),
        )
    } else {
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let args: Vec<String> = request["args"]
            .as_array()
            .map(|args| {
                args.iter()
                    .filter_map(|arg| arg.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let input = request["stdin"].as_str().unwrap_or_default();
        let result = if args.first().map(String::as_str) == Some("serve") {
            Err(USAGE.to_owned())
        } else {
            slots_once(&args, input)
        };
        let (ok, out) = match result {
            Ok(out) => (true, out),
            Err(out) => (false, out),
        };
        ("200 OK", serde_json::json!({ "ok": ok, "out": out }))
    };
    let reply = reply.to_string();
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    )
}

fn want(args: &[String], cores: u32, ttl_ms: u64) -> Result<Want, String> {
    Ok(Want {
        cores,
        kind: option(args, "--kind")
            .unwrap_or(slots::DEFAULT_KIND)
            .to_owned(),
        label: option(args, "--label").ok_or("--label L")?.to_owned(),
        only: option(args, "--machine").map(str::to_owned),
        ttl_ms,
        queue: queue(args)?,
    })
}

/// The queue a take --wait sends with each retry: --queue-id ID --queue-since MS --queue-wait MS.
fn queue(args: &[String]) -> Result<Option<slots::Queue>, String> {
    let Some(id) = option(args, "--queue-id") else {
        return Ok(None);
    };
    let ms = |name: &str| {
        option(args, name)
            .ok_or_else(|| format!("--queue-id needs {name}"))?
            .parse::<u64>()
            .map_err(|error| format!("{name}: {error}"))
    };
    Ok(Some(slots::Queue {
        id: id.to_owned(),
        since_ms: ms("--queue-since")?,
        wait_ms: ms("--queue-wait")?,
    }))
}

/// A take --wait carries its place in line on every retry, so the state can record it as a waiter.
fn queued_take(args: &[String], started: u64, wait_ms: u64, host: &str) -> Vec<String> {
    let mut args = args.to_vec();
    if wait_ms > 0 {
        args.extend([
            "--queue-id".to_owned(),
            format!("{host}-{}-{started}", std::process::id()),
            "--queue-since".to_owned(),
            started.to_string(),
            "--queue-wait".to_owned(),
            wait_ms.to_string(),
        ]);
    }
    args
}

fn slots_once(args: &[String], input: &str) -> Result<String, String> {
    let pool = match option(args, "--pool") {
        None | Some("large" | "nanocode") => Pool::Large,
        Some("small" | "pyramid2") => Pool::Small,
        Some(other) => return Err(format!("--pool {other}: large or small")),
    };
    let now = now_ms();
    match args.first().map(String::as_str) {
        Some("take") => {
            let cores: u32 = option(args, "--cores")
                .ok_or("--cores N")?
                .parse()
                .map_err(|error| format!("--cores: {error}"))?;
            let ttl = minutes(args, 60)?;
            let want = want(args, cores, ttl)?;
            if want.label.to_ascii_lowercase().contains("fill") {
                return Err(format!(
                    "--label {}: a filler label (contains fill) never takes a lease; filler runs SCHED_IDLE in fill.slice on cores leases leave free",
                    want.label
                ));
            }
            let implicit = args.iter().any(|arg| arg == "--machine-default");
            let (taken, gone) = with_state(|state| {
                let machines = machines(pool, state, now);
                let gone = reap_gone(state, &machines);
                (take_fitting(state, machines, &want, implicit, now), gone)
            })?;
            log_gone(&gone);
            let (json, lease) = taken?;
            log_line(&format!(
                "take {} {} {} cores {} {} cpus {}",
                lease.id,
                lease.machine,
                lease.cores,
                lease.kind,
                lease.label,
                lease.cpus()
            ));
            if args.iter().any(|arg| arg == "--env") {
                return Ok(format!("{}\n", json["env"].as_str().unwrap_or_default()));
            }
            Ok(format!("{json}\n"))
        }
        Some("reserve") => {
            let machine = option(args, "--machine").ok_or("--machine M")?;
            let (first, last) = option(args, "--cpus")
                .and_then(|cpus| cpus.split_once('-'))
                .and_then(|(first, last)| {
                    Some((first.parse::<u32>().ok()?, last.parse::<u32>().ok()?))
                })
                .filter(|(first, last)| first <= last)
                .ok_or("--cpus FIRST-LAST")?;
            let until = option(args, "--until")
                .and_then(|until| slots::parse_local_minutes(until, PST_OFFSET_MINUTES))
                .ok_or("--until 'YYYY-MM-DD HH:MM' (PST)")?;
            let want = match option(args, "--label-prefix") {
                Some(_)
                    if option(args, "--label").is_some() || option(args, "--kind").is_some() =>
                {
                    return Err("--label-prefix takes no --label or --kind".to_owned());
                }
                Some(prefix) if !prefix.is_empty() => Want {
                    cores: last - first + 1,
                    kind: slots::RESERVED_KIND.to_owned(),
                    label: format!("{prefix}{}", slots::PREFIX_MARK),
                    only: Some(machine.to_owned()),
                    ttl_ms: until.saturating_sub(now),
                    queue: None,
                },
                Some(_) => return Err("--label-prefix P: P is empty".to_owned()),
                None => want(args, last - first + 1, until.saturating_sub(now))?,
            };
            let overlap = args.iter().any(|arg| arg == "--overlap");
            let lease =
                with_state(|state| state.reserve(machine, first, &want, now, until, overlap))?
                    .ok_or("those cpus overlap a lease")?;
            log_line(&format!(
                "reserve {} {} cpus {} {} until {}",
                lease.id,
                lease.machine,
                lease.cpus(),
                lease.label,
                lease.until_ms / 1000
            ));
            Ok(format!("{}\n", lease.to_json(None)))
        }
        Some("give") => {
            let id = args.get(1).ok_or("give LEASE")?;
            let lease = with_state(|state| state.give(id))?.ok_or("no such lease")?;
            log_line(&format!(
                "give {} {} {} cores {} {} after {}s",
                lease.id,
                lease.machine,
                lease.cores,
                lease.kind,
                lease.label,
                now.saturating_sub(lease.taken_ms) / 1000
            ));
            Ok(String::new())
        }
        Some("renew") => {
            let id = args.get(1).ok_or("renew LEASE")?;
            let until = now + minutes(args, 60)?;
            let renewed = with_state(|state| {
                let machines = machines(pool, state, now);
                let lease = state.renew(id, until).cloned()?;
                let machine = machines
                    .iter()
                    .find(|machine| machine.name == lease.machine);
                Some(lease.to_json(machine))
            })?
            .ok_or("no such lease")?;
            Ok(format!("{renewed}\n"))
        }
        Some("observe") => {
            let name = args.get(1).ok_or("observe MACHINE")?;
            let observation = parse_observation(input, now)
                .ok_or("expected nproc, /proc/loadavg, /proc/meminfo and optionally ps -eLo policy=,stat=[,psr=] on stdin")?;
            let (state, gone) = with_state(|state| {
                state.observations.insert(name.clone(), observation.clone());
                state.expire(now);
                let gone = reap_gone(state, &machines(pool, state, now));
                (state.clone(), gone)
            })?;
            log_gone(&gone);
            let machines = machines(pool, &state, now);
            let mut out = list(&machines, &state, now);
            let leased = state.leased_cpus(name, observation.cores);
            let nearest = state
                .leases
                .iter()
                .map(|lease| lease.until_ms)
                .min()
                .map_or("none".to_owned(), |until| {
                    format!("{}s", until.saturating_sub(now) / 1000)
                });
            let sample = format!(
                "sample {name} load {:.0}/{} cores, mem {}% free, slots {leased}/{} leased here, {} leases in all, nearest lease expiry {nearest}",
                observation.load,
                observation.cores,
                observation.mem_available_kb * 100 / observation.mem_total_kb.max(1),
                observation.cores,
                state.leases.len()
            );
            log_line(&sample);
            out.push_str(&sample);
            out.push('\n');
            if observation.load > f64::from(observation.cores) {
                let alarm = format!(
                    "alarm {name} load {:.0} exceeds {} cores",
                    observation.load, observation.cores
                );
                log_line(&alarm);
                out.push_str(&alarm);
                out.push('\n');
                if args.iter().any(|arg| arg == "--check") {
                    return Err(out);
                }
            }
            Ok(out)
        }
        Some("boxes") => {
            if args.iter().any(|arg| arg == "--set") {
                let boxes: serde_json::Value =
                    serde_json::from_str(input).map_err(|error| format!("boxes: {error}"))?;
                if !boxes.is_array() {
                    return Err("boxes: expected a JSON array like boxes.json".to_owned());
                }
                with_state(|state| state.boxes = Some(boxes.clone()))?;
            }
            let state = with_state(|state| state.clone())?;
            Ok(machines(pool, &state, now)
                .iter()
                .map(|machine| {
                    format!(
                        "{} {} cores{}\n",
                        machine.name,
                        machine.cores,
                        machine.expires_ms.map_or(String::new(), |expires| format!(
                            ", expires in {}m",
                            expires.saturating_sub(now) / 60_000
                        ))
                    )
                })
                .collect())
        }
        Some("caps") => {
            if args.iter().any(|arg| arg == "--set") {
                let caps: serde_json::Value =
                    serde_json::from_str(input).map_err(|error| format!("caps: {error}"))?;
                let caps = slots::caps_from_json(&caps)?;
                with_state(|state| state.caps.clone_from(&caps))?;
            }
            let state = with_state(|state| state.clone())?;
            let caps: Vec<serde_json::Value> = state.caps.iter().map(slots::Cap::to_json).collect();
            Ok(format!("{}\n", serde_json::Value::from(caps)))
        }
        Some("list") => {
            let (state, gone) = with_state(|state| {
                state.expire(now);
                let gone = reap_gone(state, &machines(pool, state, now));
                (state.clone(), gone)
            })?;
            log_gone(&gone);
            Ok(list(&machines(pool, &state, now), &state, now))
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn parse_observation(input: &str, at_ms: u64) -> Option<Observation> {
    let mut cores = None;
    let mut load = None;
    let mut runnable = None;
    let mut per_cpu: Option<Vec<u32>> = None;
    let mut total = None;
    let mut available = None;
    for line in input.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [count] if cores.is_none() => cores = count.parse().ok(),
            [one, _, _, _, _] if load.is_none() => load = one.parse().ok(),
            ["MemTotal:", kb, ..] => total = kb.parse().ok(),
            ["MemAvailable:", kb, ..] => available = kb.parse().ok(),
            [policy, stat] if POLICIES.contains(policy) => {
                let count = runnable.get_or_insert(0_u32);
                if *policy != "IDL" && stat.starts_with('R') {
                    *count += 1;
                }
            }
            [policy, stat, cpu] if POLICIES.contains(policy) => {
                let cpu: usize = cpu.parse().ok()?;
                let count = runnable.get_or_insert(0_u32);
                let counts = per_cpu.get_or_insert_with(Vec::new);
                if counts.len() <= cpu {
                    counts.resize(cpu + 1, 0);
                }
                if *policy != "IDL" && stat.starts_with('R') {
                    *count += 1;
                    counts[cpu] += 1;
                }
            }
            _ => {}
        }
    }
    Some(Observation {
        load: runnable.map_or(load?, f64::from),
        cores: cores?,
        mem_total_kb: total?,
        mem_available_kb: available?,
        at_ms,
        runnable_cpus: per_cpu,
    })
}

const POLICIES: [&str; 7] = ["TS", "FF", "RR", "B", "ISO", "IDL", "DLN"];

const PROBE: &str = "nproc --all; cat /proc/loadavg /proc/meminfo; ps -eLo policy=,stat=,psr=";

fn log_line(line: &str) {
    let path = std::env::var_os("NC_BUILD_MACHINES_LOG")
        .map_or_else(|| home().join("swarm/machines.log"), PathBuf::from);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{} {line}", now_ms() / 1000);
    }
}

fn list(machines: &[Machine], state: &State, now: u64) -> String {
    let mut out = String::new();
    let (mut busy_total, mut leased_total, mut cores_total) = (0, 0, 0);
    for machine in machines {
        let usage = state.usage(machine, now);
        let observation = state.observations.get(&machine.name);
        let memory = observation.map_or("mem ?".to_owned(), |observation| {
            format!(
                "mem {}% free",
                observation.mem_available_kb * 100 / observation.mem_total_kb.max(1)
            )
        });
        let load = observation.map_or("load ?".to_owned(), |observation| {
            format!(
                "load {:.1}{}",
                observation.load,
                if usage.observed { "" } else { " (stale)" }
            )
        });
        let expires = machine
            .expires_ms
            .map_or("no expiry".to_owned(), |expires| {
                format!("expires in {}m", expires.saturating_sub(now) / 60_000)
            });
        let line = format!(
            "{} {}/{} cores busy, {} leased, {load}, {memory}, {expires}",
            machine.name, usage.busy, usage.cores, usage.leased
        );
        out.push_str(&line);
        out.push('\n');
        log_line(&format!("machine {line}"));
        busy_total += usage.busy;
        leased_total += usage.leased;
        cores_total += usage.cores;
    }
    let total = format!("total {busy_total}/{cores_total} cores busy, {leased_total} leased");
    log_line(&total);
    for lease in &state.leases {
        out.push_str(&format!(
            "lease {} {} cpus {} {} {} until {}s\n",
            lease.id,
            lease.machine,
            lease.cpus(),
            lease.kind,
            lease.label,
            lease.until_ms.saturating_sub(now) / 1000
        ));
    }
    let holds = state.holds(machines, now);
    for waiter in state.waiters.iter().filter(|waiter| waiter.live(now)) {
        let hold =
            holds
                .iter()
                .find(|hold| hold.waiter == waiter.id)
                .map_or(String::new(), |hold| {
                    format!(
                        ", holds {} cpus {}-{} free in {}s",
                        hold.machine,
                        hold.first_cpu,
                        hold.end_cpu - 1,
                        hold.ready_ms.saturating_sub(now) / 1000
                    )
                });
        out.push_str(&format!(
            "waiter {} {} {} cores {} {} waiting {}s, {}s left{hold}\n",
            waiter.id,
            waiter.machine.as_deref().unwrap_or("any"),
            waiter.cores,
            waiter.kind,
            waiter.label,
            now.saturating_sub(waiter.since_ms) / 1000,
            waiter.until_ms.saturating_sub(now) / 1000
        ));
    }
    for cap in &state.caps {
        out.push_str(&format!(
            "lane {} held {}/{}\n",
            cap.lane,
            state.held(cap),
            cap.cores
        ));
    }
    out.push_str(&total);
    out.push('\n');
    out
}

fn shell(command: &str) -> Result<std::time::Duration, String> {
    let started = std::time::Instant::now();
    let output = Command::new("sh")
        .args(["-c", command])
        .output()
        .map_err(|error| format!("sh -c {command}: {error}"))?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        return Err(format!(
            "{command}: {}\n{}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(elapsed)
}

fn ab_command(args: &[String]) -> Result<(), String> {
    let a = option(args, "--a").ok_or("--a CMD")?;
    let b = option(args, "--b").ok_or("--b CMD")?;
    let runs: usize = option(args, "--runs")
        .map_or(Ok(5), str::parse)
        .map_err(|error| format!("--runs: {error}"))?;
    if runs == 0 {
        return Err("--runs must be at least 1".to_owned());
    }
    let setup = option(args, "--setup");
    let label = option(args, "--label").unwrap_or("ab");
    let mut samples = [Vec::with_capacity(runs), Vec::with_capacity(runs)];
    for run in 1..=runs {
        for (index, (side, command)) in [(Side::A, a), (Side::B, b)].into_iter().enumerate() {
            if let Some(setup) = setup {
                shell(setup)
                    .map_err(|error| format!("setup before {} run {run}: {error}", side.name()))?;
            }
            let elapsed =
                shell(command).map_err(|error| format!("{} run {run}: {error}", side.name()))?;
            let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            let _ = writeln!(
                io::stderr(),
                "run {run}/{runs} {} {:.2}s",
                side.name(),
                elapsed.as_secs_f64()
            );
            samples[index].push(ms);
        }
    }
    let comparison = Comparison::of(&samples[0], &samples[1]).ok_or("no samples")?;
    let mut record = comparison.to_json();
    record["label"] = label.into();
    record["a_cmd"] = a.into();
    record["b_cmd"] = b.into();
    record["setup"] = setup.into();
    record["at"] = (now_ms() / 1000).into();
    record["cwd"] = std::env::current_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default()
        .into();
    let path = std::env::var_os("NC_BUILD_AB_LOG")
        .map_or_else(|| home().join("swarm/ab.jsonl"), PathBuf::from);
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{record}"))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    writeln!(io::stdout().lock(), "{label}\n{}", comparison.text())
        .map_err(|error| error.to_string())?;
    if let Some(task) = option(args, "--post") {
        record["posted"] = post(task, args, &comparison, a, b, label)?.into();
    }
    print(&record)
}

fn post(
    task: &str,
    args: &[String],
    comparison: &Comparison,
    a: &str,
    b: &str,
    label: &str,
) -> Result<bool, String> {
    let (winner, loser) = match comparison.verdict {
        Verdict::Faster(Side::B) => (b, a),
        Verdict::Faster(Side::A) => (a, b),
        Verdict::Noise => {
            let _ = writeln!(io::stderr(), "noise: no proposal posted");
            return Ok(false);
        }
    };
    let proposal = option(args, "--proposal").unwrap_or(label);
    let (fast, slow) = match comparison.verdict {
        Verdict::Faster(Side::A) => (&comparison.a, &comparison.b),
        _ => (&comparison.b, &comparison.a),
    };
    let spec = format!(
        "{proposal}\n\nnc-build ab measured {} interleaved runs: `{winner}` median {:.2}s (min {:.2}s, max {:.2}s) vs `{loser}` median {:.2}s (min {:.2}s, max {:.2}s), {:.1}% faster, ranges disjoint, sign test p={:.3}.",
        fast.samples_ms.len(),
        fast.median_ms / 1000.0,
        fast.min_ms as f64 / 1000.0,
        fast.max_ms as f64 / 1000.0,
        slow.median_ms / 1000.0,
        slow.min_ms as f64 / 1000.0,
        slow.max_ms as f64 / 1000.0,
        (slow.median_ms - fast.median_ms) * 100.0 / slow.median_ms.max(1.0),
        comparison.sign_p()
    );
    let board = std::env::var("NC_BOARD_BIN").unwrap_or_else(|_| "nc-board".to_owned());
    let mut command = vec!["post".to_owned(), task.to_owned()];
    for pair in ["kind=build-opt".to_owned(), format!("ab={label}")] {
        command.extend(["--label".to_owned(), pair]);
    }
    for flag in ["--board-dir", "--board-as"] {
        if let Some(value) = option(args, flag) {
            command.extend([flag.replace("board-", ""), value.to_owned()]);
        }
    }
    command.push(spec);
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    let posted = run(&board, &words)?;
    let _ = writeln!(io::stderr(), "posted {task}: {}", posted.trim());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{local_take, parse_observation, placement_warning, queued_take, reap_gone, slots};
    use nanocode_build_opt::slots::{Cap, Lease, Machine, Pin, Placement, Refusal, State, Want};

    const NOW: u64 = 1_000_000;

    fn probe(threads: &[(&str, &str, u32)]) -> String {
        let mut input =
            "48\n1.00 1.00 1.00 1/100 1\nMemTotal: 1000 kB\nMemAvailable: 900 kB\n".to_owned();
        for (policy, stat, cpu) in threads {
            input.push_str(&format!("{policy} {stat} {cpu}\n"));
        }
        input
    }

    fn running(
        policy: &'static str,
        cpus: std::ops::Range<u32>,
    ) -> Vec<(&'static str, &'static str, u32)> {
        cpus.map(|cpu| (policy, "R", cpu)).collect()
    }

    fn machine() -> Machine {
        Machine {
            name: "m".to_owned(),
            cores: 48,
            expires_ms: None,
            pin: Pin::Taskset,
            placement: Placement::Fill,
            kinds: None,
        }
    }

    fn want(cores: u32) -> Want {
        Want {
            cores,
            kind: slots::DEFAULT_KIND.to_owned(),
            label: "t".to_owned(),
            only: None,
            ttl_ms: 60_000,
            queue: None,
        }
    }

    fn state(threads: &[(&str, &str, u32)], leased: &[(u32, u32)]) -> State {
        let mut state = State::default();
        state.observations.insert(
            "m".to_owned(),
            parse_observation(&probe(threads), NOW).unwrap(),
        );
        for (index, (first_cpu, cores)) in leased.iter().enumerate() {
            state.leases.push(Lease {
                id: format!("l{index}"),
                machine: "m".to_owned(),
                first_cpu: *first_cpu,
                cores: *cores,
                kind: slots::DEFAULT_KIND.to_owned(),
                label: "held".to_owned(),
                taken_ms: NOW - 1_000,
                until_ms: NOW + 3_600_000,
                used_ms: None,
            });
        }
        state
    }

    #[test]
    fn observation_counts_runnable_threads_outside_sched_idle_per_cpu() {
        let observation = parse_observation(
            &probe(&[
                ("TS", "R", 3),
                ("TS", "S", 3),
                ("IDL", "R", 3),
                ("B", "R+", 5),
                ("IDL", "R", 7),
            ]),
            NOW,
        )
        .unwrap();
        assert_eq!(observation.load, 2.0);
        assert_eq!(
            observation.runnable_cpus,
            Some(vec![0, 0, 0, 1, 0, 1, 0, 0])
        );
    }

    #[test]
    fn take_refuses_when_leased_plus_unleased_busy_plus_want_exceeds_cores() {
        let threads = running("TS", 12..42);
        let mut state = state(&threads, &[(0, 12)]);
        assert_eq!(state.usage(&machine(), NOW).busy, 42);
        assert!(state.take(&[machine()], &want(7), NOW).is_err());
        assert!(state.take(&[machine()], &want(6), NOW).is_ok());
        assert!(state.take(&[machine()], &want(1), NOW).is_err());
    }

    #[test]
    fn take_counts_idle_leases_and_unleased_threads_together() {
        let threads = running("TS", 24..48);
        let mut state = state(&threads, &[(0, 24)]);
        assert!(state.take(&[machine()], &want(1), NOW).is_err());
    }

    #[test]
    fn threads_inside_a_lease_count_once_as_that_lease() {
        let mut threads = running("TS", 0..12);
        threads.extend(running("TS", 0..12));
        threads.extend(running("TS", 12..32));
        let mut state = state(&threads, &[(0, 12)]);
        assert_eq!(state.usage(&machine(), NOW).busy, 32);
        assert!(state.take(&[machine()], &want(16), NOW).is_ok());
    }

    #[test]
    fn sched_idle_filler_never_blocks_a_take() {
        let mut threads = running("IDL", 0..48);
        threads.extend(running("IDL", 0..48));
        let mut state = state(&threads, &[]);
        assert_eq!(state.usage(&machine(), NOW).busy, 0);
        assert!(state.take(&[machine()], &want(48), NOW).is_ok());
    }

    #[test]
    fn runnable_cpus_survive_the_state_round_trip() {
        let state = state(&running("TS", 2..4), &[]);
        assert_eq!(State::from_json(&state.to_json()), Some(state));
    }

    #[test]
    fn an_observation_without_cpus_counts_load_beyond_leased_cores() {
        let mut state = state(&[], &[(0, 12)]);
        state.observations.get_mut("m").unwrap().load = 40.0;
        assert_eq!(state.usage(&machine(), NOW).busy, 40);
        assert!(state.take(&[machine()], &want(9), NOW).is_err());
        assert!(state.take(&[machine()], &want(8), NOW).is_ok());
    }

    fn lane_cap(cores: u32) -> Cap {
        Cap {
            lane: "nf".to_owned(),
            prefixes: vec!["nf-".to_owned(), "lane-perf-".to_owned()],
            cores,
            kinds: None,
        }
    }

    fn lease(id: &str, first_cpu: u32, cores: u32, kind: &str, label: &str) -> Lease {
        Lease {
            id: id.to_owned(),
            machine: "m".to_owned(),
            first_cpu,
            cores,
            kind: kind.to_owned(),
            label: label.to_owned(),
            taken_ms: NOW - 1_000,
            until_ms: NOW + 3_600_000,
            used_ms: None,
        }
    }

    fn labelled(cores: u32, kind: &str, label: &str) -> Want {
        Want {
            kind: kind.to_owned(),
            label: label.to_owned(),
            ..want(cores)
        }
    }

    #[test]
    fn a_lane_never_holds_more_cores_than_its_cap() {
        let mut state = state(&[], &[]);
        state.caps.push(lane_cap(16));
        state.leases.push(lease("a", 0, 8, "test", "nf-a"));
        let mut other = machine();
        other.name = "o".to_owned();
        let machines = [machine(), other];
        assert!(
            state
                .take(&machines, &labelled(4, "build", "lane-perf-b"), NOW)
                .is_ok()
        );
        assert_eq!(
            state.take(&machines, &labelled(8, "sim", "nf-c"), NOW),
            Err(Refusal::OverCap {
                lane: "nf".to_owned(),
                held: 12,
                cap: 16
            })
        );
        assert!(
            state
                .take(&machines, &labelled(4, "sim", "nf-c"), NOW)
                .is_ok()
        );
        assert!(
            state
                .take(&machines, &labelled(1, "build", "nf-d"), NOW)
                .is_err()
        );
        assert!(
            state
                .take(&machines, &labelled(1, "bench", "nf-e"), NOW)
                .is_ok()
        );
        assert!(
            state
                .take(&machines, &labelled(8, "build", "other"), NOW)
                .is_ok()
        );
        assert_eq!(state.held(&lane_cap(16)), 16);
    }

    #[test]
    fn a_cap_covers_only_its_kinds() {
        let mut state = state(&[], &[]);
        let mut cap = lane_cap(4);
        cap.kinds = Some(vec!["test".to_owned()]);
        state.caps.push(cap);
        assert!(
            state
                .take(&[machine()], &labelled(8, "build", "nf-a"), NOW)
                .is_ok()
        );
        assert!(
            state
                .take(&[machine()], &labelled(4, "test", "nf-b"), NOW)
                .is_ok()
        );
        assert!(
            state
                .take(&[machine()], &labelled(1, "test", "nf-c"), NOW)
                .is_err()
        );
    }

    #[test]
    fn reservations_closing_load_and_standing_takes_do_not_count_against_a_cap() {
        let mut state = state(&[], &[]);
        let mut cap = lane_cap(4);
        cap.prefixes.push("box-".to_owned());
        state.caps.push(cap.clone());
        state
            .leases
            .push(lease("r", 0, 8, slots::RESERVED_KIND, "nf-held"));
        state
            .leases
            .push(lease("c", 8, 8, slots::CLOSED_LOAD_KIND, "nf-load"));
        state
            .leases
            .push(lease("x", 16, 8, "build", "box-closing-nf"));
        state
            .leases
            .push(lease("s", 24, 16, slots::RESERVED_KIND, "nf-*"));
        assert_eq!(state.held(&cap), 0);
        assert!(
            state
                .take(&[machine()], &labelled(12, "build", "nf-in"), NOW)
                .is_ok()
        );
        assert_eq!(state.held(&cap), 0);
        assert!(
            state
                .take(&[machine()], &labelled(4, "build", "lane-perf-a"), NOW)
                .is_ok()
        );
        assert!(
            state
                .take(&[machine()], &labelled(1, "build", "lane-perf-b"), NOW)
                .is_err()
        );
        assert!(
            state
                .take(&[machine()], &labelled(4, "build", "nf-in"), NOW)
                .is_ok()
        );
    }

    #[test]
    fn caps_live_apart_from_the_state_and_reject_bad_entries() {
        let mut state = state(&[], &[]);
        state.caps.push(lane_cap(16));
        let mut cap = lane_cap(8);
        cap.lane = "sim".to_owned();
        cap.kinds = Some(vec!["sim".to_owned()]);
        state.caps.push(cap);
        let caps = state.caps.clone();
        assert_eq!(
            super::parse_caps(&super::caps_text(&caps), "caps"),
            Ok(caps.clone())
        );
        let json = state.to_json();
        assert!(!json.contains("\"caps\""));
        assert!(State::from_json(&json).unwrap().caps.is_empty());
        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy["caps"] = serde_json::from_str(&super::caps_text(&caps)).unwrap();
        assert_eq!(State::from_json(&legacy.to_string()).unwrap().caps, caps);
        for bad in [
            r#"{"lane":"a","prefixes":["a-"],"cores":1}"#,
            r#"[{"lane":"","prefixes":["a-"],"cores":1}]"#,
            r#"[{"lane":"a","prefixes":[],"cores":1}]"#,
            r#"[{"lane":"a","prefixes":["a-"]}]"#,
            r#"[{"lane":"a","prefixes":["a-"],"cores":1,"kinds":[]}]"#,
            r#"[{"lane":"a","prefixes":["a-"],"cores":1},{"lane":"a","prefixes":["b-"],"cores":1}]"#,
        ] {
            assert!(slots::caps_from_json(&serde_json::from_str(bad).unwrap()).is_err());
        }
    }

    #[test]
    fn a_reservation_with_nested_leases_counts_its_cpus_once() {
        let mut state = state(&[], &[]);
        let mut cap = lane_cap(4);
        cap.prefixes.push("py-".to_owned());
        state.caps.push(cap.clone());
        state
            .leases
            .push(lease("r", 0, 16, slots::RESERVED_KIND, "py-*"));
        state.leases.push(lease("a", 0, 4, "test", "py-a"));
        state.leases.push(lease("b", 4, 8, "build", "py-b"));
        state.leases.push(lease("o", 16, 32, "build", "other"));
        let usage = state.usage(&machine(), NOW);
        assert_eq!((usage.leased, usage.busy), (48, 48));
        assert_eq!(state.held(&cap), 0);
        assert_eq!(
            state.take(&[machine()], &labelled(1, "build", "else"), NOW),
            Err(Refusal::NoRoom)
        );
        let inside = state
            .take(&[machine()], &labelled(4, "build", "py-c"), NOW)
            .unwrap();
        assert_eq!(inside.cpus(), "12-15");
        assert_eq!(state.usage(&machine(), NOW).leased, 48);
        assert_eq!(state.held(&cap), 0);
    }

    #[test]
    fn overlapping_leases_count_each_cpu_once() {
        let mut overlapping = state(&[], &[(0, 12), (8, 12)]);
        assert_eq!(overlapping.usage(&machine(), NOW).leased, 20);
        assert!(overlapping.take(&[machine()], &want(28), NOW).is_ok());
        assert!(overlapping.take(&[machine()], &want(1), NOW).is_err());
        let mut closed = state(&[], &[]);
        closed
            .leases
            .push(lease("c", 0, 48, slots::CLOSED_LOAD_KIND, "closed-load"));
        closed.leases.push(lease("n", 4, 4, "build", "nf-x"));
        closed
            .leases
            .push(lease("p", 16, 8, slots::RESERVED_KIND, "py-*"));
        closed
            .leases
            .push(lease("q", 44, 8, "build", "past-the-end"));
        let usage = closed.usage(&machine(), NOW);
        assert_eq!((usage.leased, usage.busy), (48, 48));
    }

    #[test]
    fn a_deleted_boxs_leases_do_not_count_and_are_reaped() {
        let mut state = state(&[], &[(0, 8)]);
        state.caps.push(lane_cap(16));
        let mut gone = lease("g", 0, 48, "build", "nf-gone");
        gone.machine = "gone".to_owned();
        state.leases.push(gone);
        assert_eq!(state.held(&lane_cap(16)), 48);
        assert_eq!(state.usage(&machine(), NOW).leased, 8);
        let machines = [machine()];
        let reaped = reap_gone(&mut state, &machines);
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].machine, "gone");
        assert_eq!(state.leases.len(), 1);
        assert_eq!(state.held(&lane_cap(16)), 0);
        assert!(
            state
                .take(&machines, &labelled(16, "build", "nf-a"), NOW)
                .is_ok()
        );
        let mut named = machine();
        named.placement = Placement::Named;
        let mut unknown = state.clone();
        unknown.leases.push(lease("k", 0, 4, "build", "kept"));
        unknown.leases[1].machine = "elsewhere".to_owned();
        assert!(reap_gone(&mut unknown, &[named]).is_empty());
        assert_eq!(unknown.leases.len(), 3);
    }

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn take_without_machine_defaults_to_this_box() {
        let take = words(&["take", "--cores", "8", "--label", "t"]);
        assert_eq!(
            local_take(&take, Some("b1")),
            words(&[
                "take",
                "--cores",
                "8",
                "--label",
                "t",
                "--machine",
                "b1",
                "--machine-default"
            ])
        );
        assert_eq!(local_take(&take, None), take);
    }

    #[test]
    fn an_explicit_machine_overrides_this_box() {
        let take = words(&["take", "--cores", "8", "--machine", "b2", "--label", "t"]);
        assert_eq!(local_take(&take, Some("b1")), take);
    }

    #[test]
    fn take_on_this_box_never_places_on_another_with_room() {
        let mut other = machine();
        other.name = "other".to_owned();
        let mut state = state(&running("TS", 0..44), &[]);
        let mut only = want(8);
        only.only = Some("m".to_owned());
        assert!(state.take(&[other.clone(), machine()], &only, NOW).is_err());
        assert_eq!(
            state
                .take(&[other, machine()], &want(8), NOW)
                .unwrap()
                .machine,
            "other"
        );
    }

    #[test]
    fn a_lease_on_another_box_warns() {
        assert_eq!(placement_warning("b1", &["ip-10-0-0-1", "b1"]), None);
        assert_eq!(placement_warning("b1", &["b1"]), None);
        assert!(
            placement_warning("b2", &["ip-10-0-0-1", "b1"])
                .is_some_and(|warning| warning.contains("b2") && warning.contains("b1"))
        );
    }

    #[test]
    fn take_refuses_leases_that_run_into_the_last_fifteen_minutes() {
        use super::fits_before_close;
        let now = 1_000_000_000;
        let expires = now + 40 * 60_000;
        assert!(fits_before_close(Some(expires), now, 25 * 60_000));
        assert!(!fits_before_close(Some(expires), now, 26 * 60_000));
        assert!(fits_before_close(None, now, 600 * 60_000));
    }

    const MIN: u64 = 60_000;

    /// A full 48-core box of twelve 4-core leases, lease i on cpus 4i..4i+4 until NOW + 5(i+1) minutes.
    fn staggered() -> State {
        let mut state = State::default();
        for i in 0..12_u32 {
            let mut held = lease(&format!("s{i}"), 4 * i, 4, "build", "small");
            held.until_ms = NOW + 5 * MIN * u64::from(i + 1);
            state.leases.push(held);
        }
        state
    }

    fn waiting(cores: u32, id: &str, since_ms: u64, wait_ms: u64, ttl_ms: u64) -> Want {
        let mut waiting = want(cores);
        waiting.only = Some("m".to_owned());
        waiting.ttl_ms = ttl_ms;
        waiting.queue = Some(slots::Queue {
            id: id.to_owned(),
            since_ms,
            wait_ms,
        });
        waiting
    }

    fn small(ttl_ms: u64) -> Want {
        let mut small = want(4);
        small.only = Some("m".to_owned());
        small.ttl_ms = ttl_ms;
        small
    }

    #[test]
    fn a_sixteen_core_waiter_gets_cores_before_later_four_core_takes() {
        let machines = [machine()];
        let mut state = staggered();
        let big = waiting(16, "big", NOW, 120 * MIN, 60 * MIN);
        assert_eq!(state.take(&machines, &big, NOW), Err(Refusal::NoRoom));
        assert_eq!(state.waiters.len(), 1);
        let holds = state.holds(&machines, NOW);
        assert_eq!(holds.len(), 1);
        assert_eq!((holds[0].first_cpu, holds[0].end_cpu), (0, 16));
        assert_eq!(holds[0].ready_ms, NOW + 20 * MIN);
        let later = waiting(4, "later", NOW + MIN, 120 * MIN, 60 * MIN);
        for at in [5, 10, 15] {
            let now = NOW + at * MIN;
            // The waiter retries every few seconds; between its retries the freed cpus stay held.
            assert_eq!(
                state.take(&machines, &big, now),
                Err(Refusal::NoRoom),
                "big still waits at {at}m"
            );
            // Without the waiter the 4-core take would win the freed cpus.
            let mut unfair = state.clone();
            unfair.waiters.clear();
            assert!(unfair.take(&machines, &small(60 * MIN), now).is_ok());
            assert_eq!(
                state.take(&machines, &small(60 * MIN), now),
                Err(Refusal::NoRoom)
            );
            assert_eq!(state.take(&machines, &later, now), Err(Refusal::NoRoom));
        }
        let got = state.take(&machines, &big, NOW + 20 * MIN).unwrap();
        assert_eq!((got.cpus(), got.cores), ("0-15".to_owned(), 16));
        assert!(state.waiters.iter().all(|waiter| waiter.id != "big"));
        // The 4-core waiter is next in line: it holds the next window, the 4 cpus that free first.
        let lease = state.take(&machines, &later, NOW + 25 * MIN).unwrap();
        assert_eq!(lease.cpus(), "16-19");
        assert!(state.waiters.is_empty());
    }

    #[test]
    fn a_short_take_backfills_cpus_held_for_a_waiter_only_if_it_ends_first() {
        let machines = [machine()];
        let mut state = staggered();
        let big = waiting(16, "big", NOW, 120 * MIN, 60 * MIN);
        assert!(state.take(&machines, &big, NOW).is_err());
        let now = NOW + 5 * MIN;
        assert!(state.take(&machines, &big, now).is_err());
        // cpus 0-3 freed at 5m; the waiter's window 0-15 is free at 20m.
        assert_eq!(
            state.take(&machines, &small(16 * MIN), now),
            Err(Refusal::NoRoom)
        );
        let backfill = state.take(&machines, &small(15 * MIN), now).unwrap();
        assert_eq!(backfill.cpus(), "0-3");
        assert_eq!(state.holds(&machines, now)[0].ready_ms, NOW + 20 * MIN);
        assert!(state.take(&machines, &big, NOW + 20 * MIN).is_ok());
        // Cpus outside a waiter's window are not held at all.
        let mut wide = self::state(&[], &[(0, 24)]);
        wide.leases[0].until_ms = NOW + 30 * MIN;
        let mut held = lease("h", 24, 8, "build", "small");
        held.until_ms = NOW + 10 * MIN;
        wide.leases.push(held);
        let big = waiting(24, "big", NOW, 120 * MIN, 60 * MIN);
        assert!(wide.take(&machines, &big, NOW).is_err());
        let hold = &wide.holds(&machines, NOW)[0];
        assert_eq!((hold.first_cpu, hold.end_cpu), (24, 48));
        assert_eq!(
            wide.take(&machines, &small(60 * MIN), NOW),
            Err(Refusal::NoRoom)
        );
        let narrow = waiting(8, "narrow", NOW, 120 * MIN, 60 * MIN);
        let mut other = self::state(&[], &[(0, 24), (24, 8)]);
        other.leases[0].until_ms = NOW + 30 * MIN;
        other.leases[1].until_ms = NOW + 10 * MIN;
        other.waiters.push(slots::Waiter {
            id: "narrow".to_owned(),
            machine: Some("m".to_owned()),
            cores: 8,
            kind: slots::DEFAULT_KIND.to_owned(),
            label: "t".to_owned(),
            ttl_ms: 60 * MIN,
            since_ms: NOW,
            until_ms: NOW + 120 * MIN,
            seen_ms: NOW,
        });
        // The 8-core waiter holds 32-39 (free now); 40-47 stay open to a 60-minute take.
        assert_eq!(
            other.take(&machines, &small(60 * MIN), NOW).unwrap().cpus(),
            "40-43"
        );
        assert_eq!(other.take(&machines, &narrow, NOW).unwrap().cpus(), "32-39");
        assert!(other.waiters.is_empty());
    }

    #[test]
    fn a_stale_waiter_is_dropped() {
        let machines = [machine()];
        let mut state = staggered();
        let big = waiting(16, "big", NOW, 2 * MIN, 60 * MIN);
        assert!(state.take(&machines, &big, NOW).is_err());
        assert_eq!(
            State::from_json(&state.to_json()).unwrap().waiters,
            state.waiters
        );
        assert_eq!(
            state.take(&machines, &small(60 * MIN), NOW + MIN),
            Err(Refusal::NoRoom)
        );
        // Its --wait (2 minutes) ran out: the 4 cpus freed at 5m go to the next take.
        let lease = state
            .take(&machines, &small(60 * MIN), NOW + 5 * MIN)
            .unwrap();
        assert_eq!(lease.cpus(), "0-3");
        assert!(state.waiters.is_empty());
        // A waiter whose client stopped retrying is dropped after 2 minutes, however long its --wait.
        let mut gone = staggered();
        let big = waiting(16, "gone", NOW, 120 * MIN, 60 * MIN);
        assert!(gone.take(&machines, &big, NOW).is_err());
        assert!(
            gone.take(&machines, &small(60 * MIN), NOW + MIN + 1)
                .is_err()
        );
        assert!(
            gone.take(&machines, &small(60 * MIN), NOW + 5 * MIN)
                .is_ok()
        );
        assert!(gone.waiters.is_empty());
        // A take whose wait already ran out is not recorded.
        let mut late = staggered();
        assert!(
            late.take(
                &machines,
                &waiting(16, "late", NOW - 3 * MIN, MIN, 60 * MIN),
                NOW
            )
            .is_err()
        );
        assert!(late.waiters.is_empty());
    }

    #[test]
    fn a_take_inside_a_matching_standing_reservation_ignores_holds() {
        let machines = [machine()];
        let mut state = staggered();
        let mut reservation = lease("r", 0, 8, slots::RESERVED_KIND, "py-*");
        reservation.until_ms = NOW + 60 * MIN;
        state.leases.retain(|held| held.first_cpu >= 8);
        state.leases.push(reservation);
        let big = waiting(16, "big", NOW, 120 * MIN, 60 * MIN);
        assert!(state.take(&machines, &big, NOW).is_err());
        let mut inside = small(60 * MIN);
        inside.label = "py-x".to_owned();
        assert_eq!(state.take(&machines, &inside, NOW).unwrap().cpus(), "0-3");
    }

    #[test]
    fn take_wait_sends_its_place_in_line() {
        let take = words(&["take", "--cores", "16", "--label", "t"]);
        assert_eq!(queued_take(&take, 5, 0, "h"), take);
        let queued = queued_take(&take, 5, 9_000, "h");
        let want = super::want(&queued, 16, MIN).unwrap();
        let queue = want.queue.unwrap();
        assert_eq!((queue.since_ms, queue.wait_ms), (5, 9_000));
        assert!(queue.id.starts_with("h-"));
        assert_eq!(super::want(&take, 16, MIN).unwrap().queue, None);
    }

    #[test]
    fn a_waiter_on_a_closing_box_by_default_moves_to_another_box() {
        let mut closing = machine();
        closing.name = "c".to_owned();
        closing.expires_ms = Some(NOW + 30 * MIN);
        let mut other = machine();
        other.name = "o".to_owned();
        let mut state = State::default();
        let mut on_closing = waiting(8, "w", NOW, 60 * MIN, 60 * MIN);
        on_closing.only = Some("c".to_owned());
        let machines = vec![closing, other];
        // By default (no --machine given), a take --wait moves to the box with room.
        let (_, lease) =
            super::take_fitting(&mut state, machines.clone(), &on_closing, true, NOW).unwrap();
        assert_eq!(lease.machine, "o");
        assert!(state.waiters.is_empty());
        // With no room anywhere it waits for any machine, not for the closing box.
        let mut full = State::default();
        full.leases.push(lease_on("o", 48));
        let error =
            super::take_fitting(&mut full, machines.clone(), &on_closing, true, NOW).unwrap_err();
        assert!(error.starts_with(super::NO_ROOM), "{error}");
        assert_eq!(full.waiters.len(), 1);
        assert_eq!(full.waiters[0].machine, None);
        // An explicit --machine is refused at once, with when the box closes.
        let mut explicit = State::default();
        let error = super::take_fitting(&mut explicit, machines.clone(), &on_closing, false, NOW)
            .unwrap_err();
        assert!(!error.starts_with(super::NO_ROOM), "{error}");
        assert!(
            error.contains(&format!(
                "box closes at {} PST",
                super::clock(NOW + 15 * MIN)
            )),
            "{error}"
        );
        assert!(error.contains("--minutes 15 or less"), "{error}");
        assert!(explicit.waiters.is_empty());
        // A take without --wait on this box by default is refused the same way.
        let mut once = on_closing.clone();
        once.queue = None;
        assert!(
            super::take_fitting(&mut State::default(), machines, &once, true, NOW)
                .unwrap_err()
                .contains("box closes at")
        );
    }

    fn standing(state: &mut State, prefix: &str, first_cpu: u32, at: u64) -> Lease {
        let want = Want {
            cores: 8,
            kind: slots::RESERVED_KIND.to_owned(),
            label: format!("{prefix}{}", slots::PREFIX_MARK),
            only: Some("m".to_owned()),
            ttl_ms: 600 * MIN,
            queue: None,
        };
        state
            .reserve("m", first_cpu, &want, at, at + 600 * MIN, false)
            .unwrap()
    }

    #[test]
    fn an_idle_standing_reservation_is_given_back_after_15_minutes() {
        let mut state = State::default();
        let reserved = standing(&mut state, "py-", 0, NOW);
        assert_eq!(reserved.used_ms, Some(NOW));
        assert!(state.give_back_idle(NOW + 14 * MIN).is_empty());
        assert_eq!(state.leases.len(), 1);
        let given = state.give_back_idle(NOW + 15 * MIN);
        assert_eq!(given.len(), 1);
        assert!(state.leases.is_empty());
        assert_eq!(
            super::idle_line(&given[0], NOW + 15 * MIN),
            format!(
                "reservation {} m cpus 0-7 py-* idle 15m, given back",
                reserved.id
            )
        );
    }

    #[test]
    fn exempt_standing_reservations_are_never_given_back_for_idling() {
        let mut state = State::default();
        for (at, prefix) in ["p2live-api-", "nf-deploy-", "lander-p2live", "py-"]
            .iter()
            .enumerate()
        {
            standing(&mut state, prefix, 8 * u32::try_from(at).unwrap(), NOW);
        }
        let given = state.give_back_idle(NOW + 600 * MIN - 1);
        assert_eq!(
            given
                .iter()
                .map(|lease| lease.label.as_str())
                .collect::<Vec<_>>(),
            ["py-*"]
        );
        assert_eq!(state.leases.len(), 3);
        assert!(slots::idle_exempt("lander-p2live-x"));
        assert!(!slots::idle_exempt("p2-"));
    }

    #[test]
    fn a_take_inside_a_standing_reservation_keeps_it_alive() {
        let machines = [machine()];
        let mut state = State::default();
        let reserved = standing(&mut state, "py-", 0, NOW);
        let mut inside = labelled(4, slots::DEFAULT_KIND, "py-build");
        inside.ttl_ms = MIN;
        let at = NOW + 10 * MIN;
        let lease = state.take(&machines, &inside, at).unwrap();
        assert_eq!(lease.cpus(), "0-3");
        assert_eq!(state.leases[0].used_ms, Some(at));
        // The take's own lease ended at 11m; the reservation counts from the take at 10m.
        assert!(state.give_back_idle(at + 14 * MIN).is_empty());
        assert_eq!(state.leases[0].id, reserved.id);
        assert_eq!(state.give_back_idle(at + 15 * MIN).len(), 1);
        // A lease still running inside a reservation keeps it, however long ago it was taken.
        let mut busy = State::default();
        standing(&mut busy, "py-", 0, NOW);
        inside.ttl_ms = 120 * MIN;
        busy.take(&machines, &inside, NOW).unwrap();
        assert!(busy.give_back_idle(NOW + 60 * MIN).is_empty());
        assert_eq!(busy.leases.len(), 2);
    }

    #[test]
    fn reservations_from_older_state_files_load_and_start_counting_when_read() {
        let mut state = State::default();
        standing(&mut state, "py-", 0, NOW);
        let mut old: serde_json::Value = serde_json::from_str(&state.to_json()).unwrap();
        old["leases"][0].as_object_mut().unwrap().remove("used_ms");
        let mut old = State::from_json(&old.to_string()).unwrap();
        assert_eq!(old.leases[0].used_ms, None);
        assert!(old.give_back_idle(NOW + 60 * MIN).is_empty());
        assert_eq!(old.leases[0].used_ms, Some(NOW + 60 * MIN));
        let mut round = State::from_json(&old.to_json()).unwrap();
        assert_eq!(round.leases, old.leases);
        assert_eq!(round.give_back_idle(NOW + 75 * MIN).len(), 1);
    }

    fn lease_on(machine: &str, cores: u32) -> Lease {
        let mut held = lease("f", 0, cores, "build", "full");
        held.machine = machine.to_owned();
        held
    }

    #[test]
    fn clock_prints_pst() {
        // 20:05 UTC is 12:05 PST; 03:00 UTC is 19:00 PST the day before.
        let day = 20_000 * 86_400_000;
        assert_eq!(super::clock(day + 20 * 3_600_000 + 5 * 60_000), "12:05");
        assert_eq!(super::clock(day + 3 * 3_600_000), "19:00");
    }
}
