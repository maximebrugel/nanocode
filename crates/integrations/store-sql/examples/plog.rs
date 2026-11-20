//! The partitioned log (nanocode_turnlog::parted) on a real Postgres 16+: a conformance pass over
//! SqlParts, then measured appends/s and claims/s at several concurrencies.
//!
//! cargo run --release -p nanocode-store-sql --example plog -- postgres://nc@127.0.0.1:5544/plog
//!
//! Prints one RESULT line per check and one METRIC line per measurement.

use std::{
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use nanocode_durability_core::{Sql, StoreError};
use nanocode_partitions::Partition;
use nanocode_store_sql::{PgOptions, PgPool, PgSql, log_wake};
use nanocode_turnlog::{
    LogHost,
    parted::{PartedLog, SqlParts},
};

static FIRST_ERROR: Mutex<Option<String>> = Mutex::new(None);

type Log = PartedLog<SqlParts<Counted>>;

/// Counts transactions, each one pipelined round trip on PgSql.
struct Counted {
    inner: PgSql,
    trips: AtomicU64,
}

impl Sql for Counted {
    fn dialect(&self) -> nanocode_durability_core::SqlDialect {
        self.inner.dialect()
    }

    fn transaction<'a>(
        &'a self,
        operation: &'static str,
        statements: &'a [nanocode_durability_core::SqlStatement<'a>],
    ) -> nanocode_durability_core::StoreFuture<
        'a,
        Result<Vec<Vec<nanocode_durability_core::SqlRow>>, StoreError>,
    > {
        self.trips.fetch_add(1, Ordering::Relaxed);
        self.inner.transaction(operation, statements)
    }
}

fn check(name: &str, ok: bool, detail: &str, failed: &mut u32) {
    println!(
        "RESULT {} {name} {detail}",
        if ok { "pass" } else { "FAIL" }
    );
    if !ok {
        *failed += 1;
    }
}

async fn conformance(sql: Arc<Counted>, failed: &mut u32) -> Result<(), StoreError> {
    let count = NonZeroU32::new(4).ok_or(StoreError::Fenced)?;
    let a = PartedLog::fenced(
        Arc::new(SqlParts::new(Arc::clone(&sql))),
        count,
        &format!("node-a-{}", std::process::id()),
    );
    let b = PartedLog::fenced(
        Arc::new(SqlParts::new(Arc::clone(&sql))),
        count,
        &format!("node-b-{}", std::process::id()),
    );
    a.prepare().await?;
    let scope = format!("conf/{}", std::process::id());
    let part = a.partition(&scope);
    check(
        "unclaimed append is fenced",
        matches!(
            a.append(&scope, &[(0, "d0".into())]).await,
            Err(StoreError::Fenced)
        ),
        "",
        failed,
    );
    let other = (0..)
        .map(|index| format!("{scope}/other{index}"))
        .find(|other| a.partition(other) == part)
        .unwrap_or_default();
    let epoch = a.claim(part, 0, 60_000, true).await?;
    a.append(&scope, &[(0, "d0:1".into()), (1, "d1:2".into())])
        .await?;
    let conflict = a
        .append(&scope, &[(1, "dX:9".into()), (2, "dY:9".into())])
        .await;
    let rows = a.scan(&scope, 0, u64::MAX).await?;
    check(
        "conflicting seq rejects only its append, all-or-nothing",
        matches!(conflict, Err(StoreError::Fenced)) && rows.len() == 2,
        &format!("rows={}", rows.len()),
        failed,
    );
    let (x, y, z) = futures_join(
        a.append(&scope, &[(2, "d2:1".into())]),
        a.append(&scope, &[(2, "d2b:1".into())]),
        a.append(&other, &[(0, "d0:1".into())]),
    )
    .await;
    check(
        "one group commit: one of two same-seq appends lands, the other scope lands",
        (x.is_ok() ^ y.is_ok()) && z.is_ok(),
        &format!("{:?} {:?} {:?}", x.is_ok(), y.is_ok(), z.is_ok()),
        failed,
    );
    let taken = b.claim(part, 1, 60_000, true).await;
    check(
        "live holder keeps the partition",
        taken.is_err(),
        "",
        failed,
    );
    let b_epoch = b.claim(part, 120_000, 60_000, true).await;
    check(
        "expired holder is taken over with a higher epoch",
        b_epoch.as_ref().is_ok_and(|next| *next > epoch),
        &format!("{b_epoch:?}"),
        failed,
    );
    let stale = a.append(&scope, &[(3, "d3:1".into())]).await;
    check(
        "old owner is fenced after takeover",
        matches!(stale, Err(StoreError::Fenced)),
        "",
        failed,
    );
    b.append(&scope, &[(3, "s5".into()), (4, "d4:1".into())])
        .await?;
    let loaded = b.load(&scope).await?;
    check(
        "load starts at the latest snapshot",
        loaded
            .first()
            .is_some_and(|(seq, body)| *seq == 3 && body == "s5")
            && loaded.len() == 2,
        &format!("{loaded:?}"),
        failed,
    );
    let entries = b.follow(part, 0, 1_000_000).await?;
    let mine: Vec<_> = entries
        .iter()
        .filter(|entry| entry.scope.as_ref() == scope)
        .collect();
    let ordered = mine
        .windows(2)
        .all(|pair| pair[0].pos < pair[1].pos && pair[0].seq < pair[1].seq);
    check(
        "follow returns the partition in position order",
        ordered && mine.len() == 5,
        &format!("rows={}", mine.len()),
        failed,
    );
    let floor = mine.get(1).map_or(0, |entry| entry.pos);
    b.cursor(part, &format!("reader-{scope}"), floor).await?;
    let dropped = b.prune(&scope, 4).await?;
    let left = b.scan(&scope, 0, u64::MAX).await?;
    check(
        "prune keeps rows a cursor has not read and the snapshot",
        dropped == 2 && left.first().is_some_and(|(seq, _)| *seq == 2),
        &format!("dropped={dropped} first={:?}", left.first()),
        failed,
    );
    let more: Vec<(u64, String)> = (5..=14).map(|seq| (seq, format!("d{seq}:1"))).collect();
    b.append(&scope, &more).await?;
    let scanned = b.scan(&scope, 0, u64::MAX).await?;
    let loaded = b.load(&scope).await?;
    let in_order = |rows: &[(u64, String)]| rows.windows(2).all(|pair| pair[1].0 == pair[0].0 + 1);
    check(
        "scan and load return more than nine rows in seq order",
        in_order(&scanned)
            && scanned.last().is_some_and(|(seq, _)| *seq == 14)
            && in_order(&loaded)
            && loaded.len() == 12,
        &format!(
            "scan={:?} load={:?}",
            scanned.iter().map(|row| row.0).collect::<Vec<_>>(),
            loaded.iter().map(|row| row.0).collect::<Vec<_>>()
        ),
        failed,
    );
    b.cursor(part, &format!("reader-{scope}"), u64::MAX / 2)
        .await?;
    b.release(part).await?;
    check(
        "released partition refuses appends",
        matches!(
            b.append(&scope, &[(5, "d5:1".into())]).await,
            Err(StoreError::Fenced)
        ),
        "",
        failed,
    );
    Ok(())
}

async fn futures_join<A: std::future::Future, B: std::future::Future, C: std::future::Future>(
    a: A,
    b: B,
    c: C,
) -> (A::Output, B::Output, C::Output) {
    let (mut a, mut b, mut c) = (Box::pin(a), Box::pin(b), Box::pin(c));
    let (mut ra, mut rb, mut rc) = (None, None, None);
    std::future::poll_fn(|context| {
        use std::task::Poll;
        if ra.is_none()
            && let Poll::Ready(value) = a.as_mut().poll(context)
        {
            ra = Some(value);
        }
        if rb.is_none()
            && let Poll::Ready(value) = b.as_mut().poll(context)
        {
            rb = Some(value);
        }
        if rc.is_none()
            && let Poll::Ready(value) = c.as_mut().poll(context)
        {
            rc = Some(value);
        }
        if ra.is_some() && rb.is_some() && rc.is_some() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    (
        ra.unwrap_or_else(|| unreachable!()),
        rb.unwrap_or_else(|| unreachable!()),
        rc.unwrap_or_else(|| unreachable!()),
    )
}

fn percentile(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index] as f64 / 1000.0
}

#[derive(Clone, Copy)]
enum Kind {
    /// Each worker appends one row to its own scope.
    Append,
    /// Every worker claims the next seq of one shared scope (read head, append there, retry on
    /// conflict): a board claim.
    Claim,
}

fn measure(
    runtime: &nanocode_executor::Runtime,
    log: &Arc<Log>,
    sql: &Arc<Counted>,
    kind: Kind,
    workers: usize,
    seconds: u64,
    run: &str,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let latencies = Arc::new(Mutex::new(Vec::new()));
    let retries = Arc::new(AtomicU64::new(0));
    let trips_before = sql.trips.load(Ordering::Relaxed);
    let stats_before = log.stats();
    let started = Instant::now();
    let handles: Vec<_> = (0..workers)
        .map(|worker| {
            let (log, stop, latencies, retries) = (
                Arc::clone(log),
                Arc::clone(&stop),
                Arc::clone(&latencies),
                Arc::clone(&retries),
            );
            let scope = match kind {
                Kind::Append => format!("bench/{run}/{workers}/{worker}"),
                Kind::Claim => format!("claim/{run}"),
            };
            runtime.spawn(async move {
                let mut mine = Vec::new();
                let mut next = 0_u64;
                while !stop.load(Ordering::Relaxed) {
                    let began = Instant::now();
                    match kind {
                        Kind::Append => {
                            if log
                                .append(&scope, &[(next, format!("d{worker}#{next}:1"))])
                                .await
                                .inspect_err(|error| {
                                    FIRST_ERROR
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .get_or_insert_with(|| error.to_string());
                                })
                                .is_ok()
                            {
                                next += 1;
                            } else {
                                retries.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Kind::Claim => loop {
                            let head = log
                                .scan(&scope, next, u64::MAX)
                                .await
                                .ok()
                                .and_then(|rows| rows.last().map(|(seq, _)| seq + 1))
                                .unwrap_or(next);
                            next = head;
                            if log
                                .append(&scope, &[(head, format!("claim {worker}"))])
                                .await
                                .is_ok()
                            {
                                next = head + 1;
                                break;
                            }
                            retries.fetch_add(1, Ordering::Relaxed);
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                        },
                    }
                    mine.push(u64::try_from(began.elapsed().as_micros()).unwrap_or(u64::MAX));
                }
                latencies
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend(mine);
            })
        })
        .collect();
    runtime.block_on(async {
        nanocode_executor::sleep(Duration::from_secs(seconds)).await;
        stop.store(true, Ordering::Relaxed);
        for handle in handles {
            let _ = handle.await;
        }
    });
    let elapsed = started.elapsed().as_secs_f64();
    let mut all = latencies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    all.sort_unstable();
    let stats = log.stats();
    let commits = stats.commits - stats_before.commits;
    let appends = stats.appends - stats_before.appends;
    let trips = sql.trips.load(Ordering::Relaxed) - trips_before;
    let name = match kind {
        Kind::Append => "appends",
        Kind::Claim => "claims",
    };
    let ops = all.len() as f64;
    println!(
        "METRIC kind={name} concurrency={workers} per_s={:.0} p50_ms={:.2} p99_ms={:.2} round_trips_per_op={:.2} appends_per_commit={:.2} retries={} seconds={elapsed:.1} first_error={:?}",
        ops / elapsed,
        percentile(&all, 0.5),
        percentile(&all, 0.99),
        trips as f64 / ops.max(1.0),
        appends as f64 / (commits as f64).max(1.0),
        retries.load(Ordering::Relaxed),
        FIRST_ERROR
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(),
    );
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let url = args
        .next()
        .unwrap_or_else(|| "postgres://nc@127.0.0.1:5544/plog".to_owned());
    let seconds: u64 = args.next().and_then(|text| text.parse().ok()).unwrap_or(10);
    let connections: usize = args.next().and_then(|text| text.parse().ok()).unwrap_or(16);
    let parts: u32 = args.next().and_then(|text| text.parse().ok()).unwrap_or(16);
    let Ok(runtime) = nanocode_executor::Builder::new_multi_thread()
        .worker_threads(8)
        .build()
    else {
        eprintln!("no runtime");
        return std::process::ExitCode::FAILURE;
    };
    let options = match PgOptions::parse(&url) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("bad url: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let pool = PgPool::new(options.clone(), connections);
    let sql = Arc::new(Counted {
        inner: PgSql::reading(pool.clone(), pool),
        trips: AtomicU64::new(0),
    });
    let mut failed = 0;
    if let Err(error) = runtime.block_on(conformance(Arc::clone(&sql), &mut failed)) {
        println!("RESULT FAIL conformance aborted {error}");
        failed += 1;
    }
    let rung = Arc::new(AtomicU64::new(0));
    let heard = Arc::clone(&rung);
    let wake_options = options;
    runtime.spawn(async move {
        let heard = heard;
        let _ = log_wake::listen(&wake_options, &|| {}, &|_, _| {
            heard.fetch_add(1, Ordering::Relaxed);
        })
        .await;
    });
    let Some(count) = NonZeroU32::new(parts) else {
        return std::process::ExitCode::FAILURE;
    };
    let log = Arc::new(PartedLog::fenced(
        Arc::new(SqlParts::new(Arc::clone(&sql))),
        count,
        "bench",
    ));
    let run = format!(
        "{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    let claimed = runtime.block_on(async {
        log.prepare().await?;
        for part in 0..parts {
            let _ = log.claim(Partition(part), 0, u64::MAX / 4, false).await;
        }
        Ok::<(), StoreError>(())
    });
    if let Err(error) = claimed {
        println!("RESULT FAIL bench setup {error}");
        return std::process::ExitCode::FAILURE;
    }
    let held = (0..parts)
        .filter(|part| log.held(Partition(*part)).is_some())
        .count();
    if held != parts as usize {
        // A previous run holds the rows: take them over as an expired holder would be.
        runtime.block_on(async {
            for part in 0..parts {
                let _ = log
                    .claim(Partition(part), u64::MAX / 2, u64::MAX / 4, false)
                    .await;
            }
        });
    }
    for workers in [1, 16, 64] {
        measure(&runtime, &log, &sql, Kind::Append, workers, seconds, &run);
    }
    for workers in [1, 16, 64] {
        measure(
            &runtime,
            &log,
            &sql,
            Kind::Claim,
            workers,
            seconds,
            &format!("{run}-{workers}"),
        );
    }
    let committed = log.stats().appends;
    std::thread::sleep(Duration::from_millis(500));
    let heard = rung.load(Ordering::Relaxed);
    check(
        "listener heard a wake for committed appends",
        heard > 0,
        &format!("heard={heard} committed={committed}"),
        &mut failed,
    );
    runtime.block_on(async {
        for part in 0..parts {
            let _ = log.release(Partition(part)).await;
        }
    });
    if failed == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
