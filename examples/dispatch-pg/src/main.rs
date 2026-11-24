//! dispatch-pg: `Dispatcher::pass` on a real Postgres board (PartedLog::fenced over SqlParts, one
//! partition), to count the log commits and SQL transactions the dispatcher's own writes take.
//!   dispatch-pg postgres://nc@127.0.0.1:5544/db tasks=2000 members=16 per_member=16
//! Posts `tasks` code tasks over `members` members and projects, then steps: each agent claims a
//! task on one step and finishes it on the next, and the dispatcher passes (spawns, starts,
//! settles, retires idle agents) until every task is done. Only what happens inside
//! `Dispatcher::pass` is counted.
use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use nanocode_board::{
    Board, ClaimId, NewTask, Op, Outcome, Phase, TaskId, Write,
    access::{Identity, Verifier},
};
use nanocode_dispatch::{Agent, AgentPhase, Budgets, Policy, Role};
use nanocode_dispatcher::{BRIEF, Dispatcher, Pool, PoolError, PoolFuture, Start};
use nanocode_durability_core::{Sql, SqlDialect, SqlRow, SqlStatement, StoreError, StoreFuture};
use nanocode_partitions::Partition;
use nanocode_store_sql::{PgOptions, PgPool, PgSql};
use nanocode_turnlog::{
    LogHost,
    parted::{PartedLog, SqlParts},
};

const OWNER: &str = "agent:lead";
const POSTER: &str = "agent:lead+poster";
const SCOPE: &str = "board";
const LEASE_MS: u64 = 3_600_000;
const STEP_MS: u64 = 50;

/// Counts transactions, each one pipelined round trip on PgSql.
struct Counted {
    inner: PgSql,
    trips: AtomicU64,
}

impl Sql for Counted {
    fn call_bound(&self) -> Option<std::time::Duration> {
        self.inner.call_bound()
    }

    fn dialect(&self) -> SqlDialect {
        self.inner.dialect()
    }

    fn transaction<'a>(
        &'a self,
        operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        self.trips.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move { self.inner.transaction(operation, statements).await })
    }
}

struct Named;

impl Verifier for Named {
    fn verify(&self, credential: &str) -> Option<String> {
        Some(credential.to_owned())
    }
}

fn identity(actor: &str) -> Identity {
    let (principal, profile) = actor
        .split_once('+')
        .map_or((actor, None), |(p, f)| (p, Some(f)));
    let Ok(owner) = Identity::verify(&Named, principal) else {
        panic!("{principal} does not verify");
    };
    match profile {
        Some(profile) => owner
            .profile(profile)
            .unwrap_or_else(|denied| panic!("{actor}: {denied:?}")),
        None => owner,
    }
}

struct Bot {
    agent: Agent,
    worker: String,
    work: Option<(TaskId, ClaimId)>,
}

#[derive(Default)]
struct Bots {
    now: u64,
    bots: Vec<Bot>,
}

struct BenchPool(Mutex<Bots>);

impl BenchPool {
    fn bots(&self) -> MutexGuard<'_, Bots> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Pool for BenchPool {
    fn agents(&self) -> PoolFuture<'_, Vec<Agent>> {
        let agents = self
            .bots()
            .bots
            .iter()
            .map(|bot| bot.agent.clone())
            .collect();
        Box::pin(std::future::ready(agents))
    }

    fn start(&self, start: Start) -> PoolFuture<'_, Result<(), PoolError>> {
        let mut bots = self.bots();
        let now = bots.now;
        bots.bots.push(Bot {
            agent: Agent {
                id: start.agent,
                domain: start.spawn.domain.clone(),
                cost_nano_usd: start.spawn.cost_nano_usd,
                intent: Some(start.spawn.intent.clone()),
                phase: AgentPhase::Idle { since_ms: now },
            },
            worker: start.worker,
            work: None,
        });
        Box::pin(std::future::ready(Ok(())))
    }

    fn retire<'a>(&'a self, agent: &'a str) -> PoolFuture<'a, Result<(), PoolError>> {
        self.bots().bots.retain(|bot| bot.agent.id != agent);
        Box::pin(std::future::ready(Ok(())))
    }
}

fn number(values: &BTreeMap<String, u64>, key: &str, default: u64) -> u64 {
    values.get(key).copied().unwrap_or(default)
}

fn fail(what: &str) -> std::process::ExitCode {
    eprintln!("dispatch-pg: {what}");
    std::process::ExitCode::FAILURE
}

#[allow(clippy::too_many_lines)]
fn main() -> std::process::ExitCode {
    let mut url = "postgres://nc@127.0.0.1:5544/dispatch".to_owned();
    let mut values = BTreeMap::new();
    for arg in std::env::args().skip(1) {
        match arg.split_once('=') {
            Some((key, value)) if !arg.starts_with("postgres") => {
                let Ok(value) = value.parse() else {
                    return fail(&format!("bad number in {arg}"));
                };
                values.insert(key.to_owned(), value);
            }
            _ => url = arg,
        }
    }
    let tasks = number(&values, "tasks", 2_000);
    let members = number(&values, "members", 16).max(1);
    let per_member = number(&values, "per_member", 16).max(1);
    let Ok(runtime) = nanocode_executor::Builder::new_multi_thread()
        .worker_threads(4)
        .build()
    else {
        return fail("no runtime");
    };
    let pg = match PgOptions::parse(&url) {
        Ok(pg) => pg,
        Err(error) => return fail(&format!("bad url: {error}")),
    };
    let pool = PgPool::new(pg, 8);
    let sql = Arc::new(Counted {
        inner: PgSql::reading(pool.clone(), pool),
        trips: AtomicU64::new(0),
    });
    let store = Arc::new(SqlParts::new(Arc::clone(&sql)));
    runtime.spawn(store.notifier());
    let Some(one) = NonZeroU32::new(1) else {
        return fail("one partition");
    };
    let log = Arc::new(PartedLog::fenced(Arc::clone(&store), one, "bench"));
    let host: Arc<dyn LogHost> = log.clone();
    let policy = Policy {
        roles: vec![Role {
            name: "coder".to_owned(),
            labels: [("kind".to_owned(), "code".to_owned())]
                .into_iter()
                .collect(),
            cost_nano_usd: 1,
            placement: "coder-pool".to_owned(),
        }],
        member_label: "member".to_owned(),
        project_label: "project".to_owned(),
        intent_prefix: "dispatch/".to_owned(),
        budgets: Budgets {
            agents_per_member_max: per_member,
            agents_per_project_max: per_member,
            nano_usd_max: u64::MAX / 4,
        },
        idle_retire_ms: STEP_MS,
        start_timeout_ms: 60_000,
        start_attempts_max: 3,
    };
    let result = runtime.block_on(async {
        log.prepare().await.map_err(|error| format!("{error}"))?;
        log.claim(Partition(0), 0, u64::MAX / 4, false)
            .await
            .map_err(|error| format!("{error}"))?;
        let shown = |error: nanocode_board::BoardError| format!("{error:?}");
        let open = |actor: &str| Board::open(host.clone(), SCOPE, identity(actor));
        let mut lead = open(OWNER).await.map_err(|error| format!("stage{}: {}", 1, shown(error)))?;
        lead.charter(1_000).await.map_err(|error| format!("stage{}: {}", 2, shown(error)))?;
        let mut poster = open(POSTER).await.map_err(|error| format!("stage{}: {}", 3, shown(error)))?;
        let posts: Vec<_> = (0..tasks)
            .map(|index| {
                let member = format!("m{}", index % members);
                Write {
                    by: None,
                    key: Some(format!("post:t{index}")),
                    at_ms: 1_000,
                    op: Op::Post {
                        task: NewTask {
                            id: TaskId(format!("t{index}")),
                            parent: None,
                            spec: format!("task {index}"),
                            labels: [
                                ("kind".to_owned(), "code".to_owned()),
                                ("member".to_owned(), member.clone()),
                                ("project".to_owned(), member.replace('m', "p")),
                            ]
                            .into_iter()
                            .collect(),
                            needs: Vec::new(),
                            terms: Default::default(),
                        },
                    },
                }
            })
            .collect();
        for chunk in posts.chunks(256) {
            let (verdicts, _) = poster.submit_group(chunk.to_vec()).await;
            if let Some(Err(error)) = verdicts.into_iter().find(Result::is_err) {
                return Err(format!("post: {error:?}"));
            }
        }
        let mut dispatcher = Dispatcher::new(
            open(OWNER).await.map_err(|error| format!("stage{}: {}", 4, shown(error)))?,
            policy.clone(),
            BRIEF,
        );
        let pool = BenchPool(Mutex::new(Bots {
            now: 2_000,
            bots: Vec::new(),
        }));
        let mut workers: BTreeMap<String, Board> = BTreeMap::new();
        let (mut done, mut steps) = (0u64, 0u64);
        let (mut pass_s, mut pass_trips, mut pass_commits, mut pass_appends, mut passes) =
            (0f64, 0u64, 0u64, 0u64, 0u64);
        let started = Instant::now();
        while done < tasks && steps < 200_000 {
            steps += 1;
            let now = {
                let mut bots = pool.bots();
                bots.now += STEP_MS;
                bots.now
            };
            let count = pool.bots().bots.len();
            for index in 0..count {
                let (phase, worker, work, domain) = {
                    let bots = pool.bots();
                    let bot = &bots.bots[index];
                    (bot.agent.phase, bot.worker.clone(), bot.work.clone(), bot.agent.domain.clone())
                };
                if !workers.contains_key(&worker) {
                    workers.insert(worker.clone(), open(&worker).await.map_err(|error| format!("stage{}: {}", 5, shown(error)))?);
                }
                let Some(board) = workers.get_mut(&worker) else {
                    return Err("worker board vanished".to_owned());
                };
                board.refresh().await.map_err(|error| format!("stage{}: {}", 6, shown(error)))?;
                match (phase, work) {
                    (AgentPhase::Idle { .. }, None) => {
                        let labels = [
                            ("kind".to_owned(), "code".to_owned()),
                            ("member".to_owned(), domain.member.clone()),
                            ("project".to_owned(), domain.project.clone()),
                        ]
                        .into_iter()
                        .collect();
                        let Some(task) = board.fold().pick(&worker, &labels, now).cloned() else {
                            continue;
                        };
                        let claim = Op::Claim {
                            task: task.clone(),
                            lease_ms: LEASE_MS,
                            fence: None,
                            cap: None,
                        };
                        if board.submit(now, claim).await.is_err() {
                            continue;
                        }
                        if let Some(Phase::Claimed { claim, .. }) =
                            board.fold().task(&task).map(|task| task.phase.clone())
                        {
                            let mut bots = pool.bots();
                            bots.bots[index].work = Some((task, claim));
                            bots.bots[index].agent.phase = AgentPhase::Busy;
                        }
                    }
                    (AgentPhase::Busy, Some((task, claim))) => {
                        let finish = Op::Finish {
                            task,
                            claim,
                            outcome: Outcome::Done("ok".to_owned()),
                        };
                        done += u64::from(board.submit(now, finish).await.is_ok());
                        let mut bots = pool.bots();
                        bots.bots[index].work = None;
                        bots.bots[index].agent.phase = AgentPhase::Idle { since_ms: now };
                    }
                    _ => {}
                }
            }
            let (trips, stats, began) = (sql.trips.load(Ordering::Relaxed), log.stats(), Instant::now());
            dispatcher.pass(&pool, now).await.map_err(|error| format!("pass: {error:?}"))?;
            pass_s += began.elapsed().as_secs_f64();
            passes += 1;
            pass_trips += sql.trips.load(Ordering::Relaxed) - trips;
            let after = log.stats();
            pass_commits += after.commits - stats.commits;
            pass_appends += after.appends - stats.appends;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let counters = dispatcher.counters();
        let starts = counters.starts.max(1) as f64;
        println!(
            "METRIC tasks={tasks} members={members} per_member={per_member} done={done} steps={steps} passes={passes} starts={} retires={} pass_commits_per_start={:.2} pass_appends_per_start={:.2} pass_txns_per_start={:.2} starts_per_pass_s={:.0} pass_s={pass_s:.2} total_s={elapsed:.2}",
            counters.starts,
            counters.retires,
            pass_commits as f64 / starts,
            pass_appends as f64 / starts,
            pass_trips as f64 / starts,
            starts / pass_s.max(1e-9),
        );
        Ok::<(), String>(())
    });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}
