use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use nanocode_board::{
    Board, BoardError, ClaimId, NewTask, Op, Outcome, Phase, Rejection, Task, TaskId,
    access::{Can, Grant, Identity, Verifier, Who},
    tasks::{OnFail, TaskOp, Terms},
};
use nanocode_turnlog::{FileLog, FileOptions};
use serde_json::{Value, json};

const SCOPE: &str = "board";

const SPREAD: usize = 8;

const HELP: &str = "nc-board: a nanocode task board kept in a directory
usage: nc-board [--dir DIR] [--as ACTOR] COMMAND
  DIR defaults to $NC_BOARD_DIR, ACTOR to $NC_BOARD_AS; ACTOR is a principal such as agent:b1, optionally with +profile
  init                                     charter the board as ACTOR and let everyone read, post, claim and manage
  post ID [--parent ID] [--label K=V]... [--needs ID]... [--assignee ACTOR] [--on-fail cascade|wait] SPEC
  list [--label K=V]... [--status open|claimed|done|failed|cancelled|blocked|ready]...
  show ID
  claim ID --lease SECS
  claim-next [--label K=V]... [--spread K] --lease SECS  claim a ready task matching every label: best rank, then ACTOR's own,
                                           then one of the K (default 8) oldest; retries the oldest after a lost race; exits 2 when none is ready
  renew ID --lease SECS [--claim CLAIM]
  finish ID [--claim CLAIM] DETAIL
  fail ID [--claim CLAIM] DETAIL
  release ID [--claim CLAIM] NOTE
  block ID KIND REASON
  unblock ID
  reopen ID
  cancel ID REASON
  reply ID BODY
every command prints JSON: a task, or a list of tasks; CLAIM defaults to ACTOR's current claim on the task";

type Failure = Box<dyn std::error::Error>;

struct Parsed {
    flags: BTreeMap<String, Vec<String>>,
    words: Vec<String>,
}

impl Parsed {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, Failure> {
        let mut flags = BTreeMap::<String, Vec<String>>::new();
        let mut words = Vec::new();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            if argument == "--" {
                words.extend(arguments.by_ref());
            } else if matches!(argument.as_str(), "-h" | "--help") {
                words.insert(0, "help".to_owned());
            } else if let Some(name) = argument.strip_prefix("--") {
                let value = arguments
                    .next()
                    .ok_or_else(|| format!("--{name} needs a value"))?;
                flags.entry(name.to_owned()).or_default().push(value);
            } else {
                words.push(argument);
            }
        }
        Ok(Self { flags, words })
    }

    fn allow(&self, names: &[&str]) -> Result<(), Failure> {
        match self
            .flags
            .keys()
            .find(|name| !names.contains(&name.as_str()) && !matches!(name.as_str(), "dir" | "as"))
        {
            Some(name) => Err(format!("--{name} does not apply here").into()),
            None => Ok(()),
        }
    }

    fn one(&self, name: &str) -> Result<Option<String>, Failure> {
        match self.flags.get(name).map(Vec::as_slice) {
            None => Ok(None),
            Some([value]) => Ok(Some(value.clone())),
            Some(_) => Err(format!("--{name} is given more than once").into()),
        }
    }

    fn many(&self, name: &str) -> Vec<String> {
        self.flags.get(name).cloned().unwrap_or_default()
    }

    fn words<const N: usize>(&self) -> Result<[String; N], Failure> {
        <[String; N]>::try_from(self.words[1..].to_vec()).map_err(|words| {
            format!(
                "{} takes {N} arguments, not {}; see nc-board --help",
                self.words[0],
                words.len()
            )
            .into()
        })
    }

    fn labels(&self) -> Result<BTreeMap<String, String>, Failure> {
        self.many("label")
            .iter()
            .map(|pair| {
                pair.split_once('=')
                    .filter(|(key, _)| !key.is_empty())
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .ok_or_else(|| format!("label {pair} is not KEY=VALUE").into())
            })
            .collect()
    }

    fn lease_ms(&self) -> Result<u64, Failure> {
        let secs = self.one("lease")?.ok_or("--lease SECS is required")?;
        let secs = secs
            .parse::<u64>()
            .map_err(|error| format!("--lease {secs}: {error}"))?;
        Ok(secs
            .checked_mul(1000)
            .ok_or("--lease overflows milliseconds")?)
    }

    fn setting(&self, flag: &str, variable: &str) -> Result<String, Failure> {
        match self.one(flag)? {
            Some(value) => Ok(value),
            None => std::env::var(variable)
                .map_err(|_| format!("give --{flag} or set {variable}").into()),
        }
    }
}

struct Trusted;

impl Verifier for Trusted {
    fn verify(&self, credential: &str) -> Option<String> {
        Some(credential.to_owned())
    }
}

fn identity(actor: &str) -> Result<Identity, Failure> {
    let (principal, profile) = match actor.split_once('+') {
        Some((principal, profile)) => (principal, Some(profile)),
        None => (actor, None),
    };
    let identity =
        Identity::verify(&Trusted, principal).map_err(|denied| format!("{actor}: {denied:?}"))?;
    match profile {
        Some(profile) => identity
            .profile(profile)
            .map_err(|denied| format!("{actor}: {denied:?}").into()),
        None => Ok(identity),
    }
}

fn now_ms() -> Result<u64, Failure> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

const fn status(task: &Task) -> &'static str {
    match task.phase {
        Phase::Open => "open",
        Phase::Claimed { .. } => "claimed",
        Phase::Done { .. } => "done",
        Phase::Failed { .. } => "failed",
        Phase::Cancelled { .. } => "cancelled",
        Phase::Blocked { .. } => "blocked",
    }
}

fn render(board: &Board, id: &TaskId, task: &Task, at_ms: u64) -> Value {
    let mut out = json!({
        "id": id,
        "parent": task.spec.parent,
        "spec": task.spec.spec,
        "labels": task.spec.labels,
        "needs": task.spec.needs,
        "assignee": task.assignee,
        "status": status(task),
        "ready": board.fold().claimable(id, at_ms).is_ok(),
        "attempts": task.attempts,
        "replies": task.replies.iter().map(|(by, body)| json!({"by": by, "body": body})).collect::<Vec<_>>(),
    });
    let phase = match &task.phase {
        Phase::Open => json!({}),
        Phase::Claimed {
            claim,
            worker,
            until_ms,
        } => {
            json!({"claim": claim, "worker": worker, "until_ms": until_ms, "expired": *until_ms <= at_ms})
        }
        Phase::Done { result, .. } => json!({"result": result}),
        Phase::Failed { error, .. } => json!({"error": error}),
        Phase::Cancelled { reason } => json!({"reason": reason}),
        Phase::Blocked { kind, reason } => json!({"kind": kind, "reason": reason}),
    };
    if let (Value::Object(out), Value::Object(phase)) = (&mut out, phase) {
        out.extend(phase);
    }
    out
}

fn shown(board: &Board, id: &TaskId, at_ms: u64) -> Result<Value, Failure> {
    let task = board
        .task(id)
        .ok_or_else(|| format!("no task {id} on this board"))?;
    Ok(render(board, id, task, at_ms))
}

fn held(board: &Board, id: &TaskId, given: Option<String>) -> Result<ClaimId, Failure> {
    if let Some(claim) = given {
        return Ok(ClaimId(claim));
    }
    match board.task(id).map(|task| &task.phase) {
        Some(Phase::Claimed { claim, worker, .. }) if *worker == board.writer() => {
            Ok(claim.clone())
        }
        Some(Phase::Claimed { worker, .. }) => Err(format!(
            "{id} is claimed by {worker}, not {}; pass --claim",
            board.writer()
        )
        .into()),
        Some(_) => Err(format!("{id} is not claimed").into()),
        None => Err(format!("no task {id} on this board").into()),
    }
}

async fn submit(board: &mut Board, at_ms: u64, op: Op) -> Result<(), Failure> {
    board
        .submit(at_ms, op)
        .await
        .map(drop)
        .map_err(|error| match error {
            BoardError::Unsettled { .. } => {
                format!("{error}; run show to see whether it landed").into()
            }
            error => error.to_string().into(),
        })
}

async fn run(parsed: &Parsed, board: &mut Board) -> Result<Option<Value>, Failure> {
    let at_ms = now_ms()?;
    let command = parsed.words[0].as_str();
    let id = |word: &str| TaskId(word.to_owned());
    let written = match command {
        "init" => {
            parsed.allow(&[])?;
            parsed.words::<0>()?;
            board
                .charter(at_ms)
                .await
                .map_err(|error| error.to_string())?;
            let grant = Grant {
                who: vec![Who::Everyone],
                can: BTreeMap::from([("*".to_owned(), Can::ALL.to_vec())]),
            };
            board
                .grant("everyone", at_ms, grant)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(Some(
                json!({"owner": board.writer(), "granted": "everyone may do everything"}),
            ));
        }
        "list" => {
            parsed.allow(&["label", "status"])?;
            parsed.words::<0>()?;
            let labels = parsed.labels()?;
            let statuses = parsed.many("status");
            let listed = board
                .tasks()
                .filter(|(_, task)| {
                    labels
                        .iter()
                        .all(|(key, value)| task.spec.labels.get(key) == Some(value))
                })
                .map(|(id, task)| render(board, id, task, at_ms))
                .filter(|task| {
                    statuses.is_empty()
                        || statuses.iter().any(|wanted| {
                            task["status"] == wanted.as_str()
                                || (wanted == "ready" && task["ready"] == true)
                        })
                })
                .collect::<Vec<_>>();
            return Ok(Some(Value::Array(listed)));
        }
        "show" => {
            parsed.allow(&[])?;
            let [task] = parsed.words()?;
            return shown(board, &id(&task), at_ms).map(Some);
        }
        "claim-next" => {
            parsed.allow(&["label", "lease", "spread"])?;
            parsed.words::<0>()?;
            let lease_ms = parsed.lease_ms()?;
            let labels = parsed.labels()?;
            let spread = match parsed.one("spread")? {
                Some(spread) => spread
                    .parse::<usize>()
                    .ok()
                    .filter(|spread| *spread > 0)
                    .ok_or(format!("--spread {spread} is not a positive count"))?,
                None => SPREAD,
            };
            let Some(task) = board
                .fold()
                .spread(board.writer(), &labels, at_ms, spread)
                .cloned()
            else {
                return Ok(None);
            };
            let claim = |task: &TaskId| Op::Claim {
                task: task.clone(),
                lease_ms,
                fence: None,
                cap: None,
            };
            match board.submit(at_ms, claim(&task)).await {
                Err(BoardError::Rejected(Rejection::Held { .. } | Rejection::Terminal))
                    if spread > 1 =>
                {
                    let Some(head) = board.fold().pick(board.writer(), &labels, at_ms).cloned()
                    else {
                        return Ok(None);
                    };
                    submit(board, at_ms, claim(&head)).await?;
                    head
                }
                landed => {
                    landed.map_err(|error| error.to_string())?;
                    task
                }
            }
        }
        "post" => {
            parsed.allow(&["parent", "label", "needs", "assignee", "on-fail"])?;
            let [task, spec] = parsed.words()?;
            let on_fail = match parsed.one("on-fail")?.as_deref() {
                None | Some("cascade") => OnFail::Cascade,
                Some("wait") => OnFail::Wait,
                Some(other) => {
                    return Err(format!("--on-fail {other} is not cascade or wait").into());
                }
            };
            let new = NewTask {
                id: id(&task),
                parent: parsed.one("parent")?.map(|parent| id(&parent)),
                spec,
                labels: parsed.labels()?,
                needs: parsed.many("needs").iter().map(|need| id(need)).collect(),
                terms: Terms {
                    assignee: parsed.one("assignee")?,
                    on_fail,
                    ..Terms::default()
                },
            };
            submit(board, at_ms, Op::Post { task: new }).await?;
            id(&task)
        }
        "claim" => {
            parsed.allow(&["lease"])?;
            let [task] = parsed.words()?;
            let lease_ms = parsed.lease_ms()?;
            submit(
                board,
                at_ms,
                Op::Claim {
                    task: id(&task),
                    lease_ms,
                    fence: None,
                    cap: None,
                },
            )
            .await?;
            id(&task)
        }
        "renew" => {
            parsed.allow(&["lease", "claim"])?;
            let [task] = parsed.words()?;
            let lease_ms = parsed.lease_ms()?;
            let claim = held(board, &id(&task), parsed.one("claim")?)?;
            submit(
                board,
                at_ms,
                Op::Renew {
                    task: id(&task),
                    claim,
                    lease_ms,
                },
            )
            .await?;
            id(&task)
        }
        "finish" | "fail" => {
            parsed.allow(&["claim"])?;
            let [task, detail] = parsed.words()?;
            let claim = held(board, &id(&task), parsed.one("claim")?)?;
            let outcome = if command == "finish" {
                Outcome::Done(detail)
            } else {
                Outcome::Failed(detail)
            };
            submit(
                board,
                at_ms,
                Op::Finish {
                    task: id(&task),
                    claim,
                    outcome,
                },
            )
            .await?;
            id(&task)
        }
        "release" => {
            parsed.allow(&["claim"])?;
            let [task, note] = parsed.words()?;
            let claim = held(board, &id(&task), parsed.one("claim")?)?;
            submit(
                board,
                at_ms,
                Op::Release {
                    task: id(&task),
                    claim,
                    note,
                },
            )
            .await?;
            id(&task)
        }
        "block" => {
            parsed.allow(&[])?;
            let [task, kind, reason] = parsed.words()?;
            let claim = match board.task(&id(&task)).map(|task| &task.phase) {
                Some(Phase::Claimed { claim, worker, .. }) if *worker == board.writer() => {
                    Some(claim.clone())
                }
                _ => None,
            };
            submit(
                board,
                at_ms,
                Op::Tasks(TaskOp::Block {
                    task: id(&task),
                    claim,
                    kind,
                    reason,
                }),
            )
            .await?;
            id(&task)
        }
        "unblock" | "reopen" => {
            parsed.allow(&[])?;
            let [task] = parsed.words()?;
            let op = if command == "unblock" {
                TaskOp::Unblock { task: id(&task) }
            } else {
                TaskOp::Reopen { task: id(&task) }
            };
            submit(board, at_ms, Op::Tasks(op)).await?;
            id(&task)
        }
        "cancel" => {
            parsed.allow(&[])?;
            let [task, reason] = parsed.words()?;
            submit(
                board,
                at_ms,
                Op::Cancel {
                    task: id(&task),
                    reason,
                },
            )
            .await?;
            id(&task)
        }
        "reply" => {
            parsed.allow(&[])?;
            let [task, body] = parsed.words()?;
            submit(
                board,
                at_ms,
                Op::Reply {
                    task: id(&task),
                    body,
                },
            )
            .await?;
            id(&task)
        }
        other => return Err(format!("unknown command {other}; see nc-board --help").into()),
    };
    shown(board, &written, now_ms()?).map(Some)
}

fn open(dir: &Path) -> Result<std::fs::File, Failure> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("board.lock"))
        .map_err(|error| format!("{}: {error}", dir.join("board.lock").display()))?;
    lock.lock()
        .map_err(|error| format!("{}: {error}", dir.join("board.lock").display()))?;
    Ok(lock)
}

fn session(parsed: &Parsed) -> Result<Option<Value>, Failure> {
    let dir = PathBuf::from(parsed.setting("dir", "NC_BOARD_DIR")?);
    let actor = match parsed.words[0].as_str() {
        "list" | "show" => parsed
            .setting("as", "NC_BOARD_AS")
            .unwrap_or_else(|_| "agent:reader".to_owned()),
        _ => parsed.setting("as", "NC_BOARD_AS")?,
    };
    let identity = identity(&actor)?;
    let lock = open(&dir)?;
    let runtime = nanocode_executor::Runtime::new()?;
    let result = runtime.block_on(async {
        let host = Arc::new(FileLog::open(
            &dir.join("board.log"),
            FileOptions {
                scope: SCOPE,
                sync: true,
            },
        )?);
        let mut board = Board::open(host, SCOPE, identity)
            .await
            .map_err(|error| error.to_string())?;
        let reader = parsed.words[0] == "list" || parsed.words[0] == "show";
        if reader && board.fold().access().owner().is_some() && !board.may("*", Can::Read) {
            return Err(format!("{actor} may not read this board").into());
        }
        run(parsed, &mut board).await
    });
    drop(lock);
    result
}

pub fn main() -> ExitCode {
    let parsed = match Parsed::parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("nc-board: {error}");
            return ExitCode::FAILURE;
        }
    };
    if parsed.words.first().is_none_or(|word| word == "help") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    match session(&parsed) {
        Ok(Some(value)) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("nc-board: no ready task matches");
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("nc-board: {error}");
            ExitCode::FAILURE
        }
    }
}
