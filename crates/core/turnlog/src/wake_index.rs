use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use nanocode_durability_core::{Sql, SqlAccess, SqlStatement, StoreError, StoreFuture};

use crate::{Wakes, host::host_id};

type Dialects = [&'static str; 2];

const PREPARE_LOCK: Dialects = [
    "SELECT NULL",
    "SELECT CAST(pg_advisory_xact_lock(7_061_636_194) AS TEXT)",
];
const CREATE_DUE: Dialects = [
    "CREATE TABLE IF NOT EXISTS nanocode_wake_due (scope TEXT PRIMARY KEY, due_ms INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS nanocode_wake_due (scope TEXT PRIMARY KEY, due_ms BIGINT NOT NULL)",
];
const INDEX_DUE: Dialects = [
    "CREATE INDEX IF NOT EXISTS nanocode_wake_due_at ON nanocode_wake_due (due_ms, scope)",
    "CREATE INDEX IF NOT EXISTS nanocode_wake_due_at ON nanocode_wake_due (due_ms, scope)",
];
const CREATE_TOPIC: Dialects = [
    "CREATE TABLE IF NOT EXISTS nanocode_wake_topic (topic TEXT NOT NULL, scope TEXT NOT NULL, PRIMARY KEY (topic, scope)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS nanocode_wake_topic (topic TEXT NOT NULL, scope TEXT NOT NULL, PRIMARY KEY (topic, scope))",
];
const INDEX_TOPIC: Dialects = [
    "CREATE INDEX IF NOT EXISTS nanocode_wake_topic_scope ON nanocode_wake_topic (scope)",
    "CREATE INDEX IF NOT EXISTS nanocode_wake_topic_scope ON nanocode_wake_topic (scope)",
];
const CLEAR_DUE: Dialects = [
    "DELETE FROM nanocode_wake_due WHERE scope = ?1",
    "DELETE FROM nanocode_wake_due WHERE scope = $1::text",
];
const SET_DUE: Dialects = [
    "INSERT INTO nanocode_wake_due (scope, due_ms) SELECT ?1, CAST(?2 AS INTEGER) WHERE ?2 <> ''",
    "INSERT INTO nanocode_wake_due (scope, due_ms) SELECT $1::text, CAST(NULLIF($2::text, '') AS BIGINT) WHERE $2::text <> ''",
];
const CLEAR_TOPICS: Dialects = [
    "DELETE FROM nanocode_wake_topic WHERE scope = ?1",
    "DELETE FROM nanocode_wake_topic WHERE scope = $1::text",
];
const SET_TOPICS: Dialects = [
    "INSERT INTO nanocode_wake_topic (topic, scope) SELECT value, ?1 FROM json_each(?2)",
    "INSERT INTO nanocode_wake_topic (topic, scope) SELECT topic, $1::text FROM jsonb_array_elements_text(CAST($2::text AS jsonb)) AS listened(topic)",
];
const DUE: Dialects = [
    "SELECT scope, CAST(due_ms AS TEXT) FROM nanocode_wake_due WHERE due_ms <= CAST(?1 AS INTEGER) ORDER BY due_ms, scope LIMIT CAST(?2 AS INTEGER)",
    "SELECT scope, CAST(due_ms AS TEXT) FROM nanocode_wake_due WHERE due_ms <= CAST($1::text AS BIGINT) ORDER BY due_ms, scope LIMIT CAST($2::text AS BIGINT)",
];
const NEXT_DUE: Dialects = [
    "SELECT CAST(due_ms AS TEXT) FROM nanocode_wake_due ORDER BY due_ms LIMIT 1",
    "SELECT CAST(due_ms AS TEXT) FROM nanocode_wake_due ORDER BY due_ms LIMIT 1",
];
const LISTENING: Dialects = [
    "SELECT scope FROM nanocode_wake_topic WHERE topic = ?1 AND scope > ?2 ORDER BY scope LIMIT CAST(?3 AS INTEGER)",
    "SELECT scope FROM nanocode_wake_topic WHERE topic = $1::text AND scope > $2::text ORDER BY scope LIMIT CAST($3::text AS BIGINT)",
];

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WakeSummary {
    pub next_due: Option<u64>,
    pub topics: Vec<String>,
}

impl Wakes {
    #[must_use]
    pub fn summary(&self) -> WakeSummary {
        let mut topics = self
            .iter()
            .filter(|(_, armed)| !armed.full())
            .filter_map(|(_, armed)| armed.wake.on.as_ref().map(|on| on.topic.clone()))
            .collect::<Vec<_>>();
        topics.sort();
        topics.dedup();
        WakeSummary {
            next_due: self.next_due(),
            topics,
        }
    }
}

pub type Statements = Vec<(&'static str, Vec<String>)>;

pub trait WakeRider: Send + Sync {
    fn host(&self) -> usize;

    fn statements(&self, scope: &str, summary: &WakeSummary) -> Statements;

    fn prepare(&self) -> StoreFuture<'_, Result<(), StoreError>>;

    fn write<'a>(
        &'a self,
        scope: &'a str,
        summary: &'a WakeSummary,
    ) -> StoreFuture<'a, Result<(), StoreError>>;
}

pub struct WakeIndex<H: ?Sized> {
    prepared: AtomicBool,
    host: Arc<H>,
}

fn invalid(detail: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(format!("wake index is invalid: {detail}"))
}

fn number(text: Option<&String>, column: &str) -> Result<u64, StoreError> {
    text.ok_or_else(|| invalid(format!("{column} is null")))?
        .parse()
        .map_err(|error| invalid(format!("{column} is not a number: {error}")))
}

impl<H: Sql + ?Sized> WakeIndex<H> {
    pub const fn new(host: Arc<H>) -> Self {
        Self {
            prepared: AtomicBool::new(false),
            host,
        }
    }

    fn pick(&self, statements: &Dialects) -> &'static str {
        self.host.dialect().pick(statements)
    }

    async fn read(
        &self,
        operation: &'static str,
        sql: &Dialects,
        params: Vec<&str>,
    ) -> Result<Vec<Vec<Option<String>>>, StoreError> {
        let statements = [SqlStatement {
            sql: self.pick(sql),
            params,
        }];
        self.host
            .run(SqlAccess::Read, operation, &statements)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| invalid(format!("{operation} returned no result")))
    }

    pub async fn due(&self, now_ms: u64, limit: usize) -> Result<Vec<(String, u64)>, StoreError> {
        let (now, limit) = (now_ms.to_string(), limit.to_string());
        self.read("wake_index_due", &DUE, vec![&now, &limit])
            .await?
            .into_iter()
            .map(|row| {
                let scope = row
                    .first()
                    .cloned()
                    .flatten()
                    .ok_or_else(|| invalid("a due row has no scope"))?;
                Ok((scope, number(row.get(1).and_then(Option::as_ref), "due_ms")?))
            })
            .collect()
    }

    pub async fn next_due(&self) -> Result<Option<u64>, StoreError> {
        self.read("wake_index_next_due", &NEXT_DUE, Vec::new())
            .await?
            .first()
            .map(|row| number(row.first().and_then(Option::as_ref), "due_ms"))
            .transpose()
    }

    pub async fn listening(
        &self,
        topic: &str,
        after: &str,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let limit = limit.to_string();
        self.read("wake_index_listening", &LISTENING, vec![topic, after, &limit])
            .await?
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .next()
                    .flatten()
                    .ok_or_else(|| invalid("a listening row has no scope"))
            })
            .collect()
    }
}

impl<H: Sql + Send + Sync + ?Sized + 'static> WakeRider for WakeIndex<H> {
    fn host(&self) -> usize {
        host_id(&self.host)
    }

    fn statements(&self, scope: &str, summary: &WakeSummary) -> Statements {
        let due = summary.next_due.map(|at| at.to_string()).unwrap_or_default();
        let topics = serde_json::to_string(&summary.topics)
            .expect("a list of topics encodes as JSON");
        vec![
            (self.pick(&CLEAR_DUE), vec![scope.to_owned()]),
            (self.pick(&SET_DUE), vec![scope.to_owned(), due]),
            (self.pick(&CLEAR_TOPICS), vec![scope.to_owned()]),
            (self.pick(&SET_TOPICS), vec![scope.to_owned(), topics]),
        ]
    }

    fn prepare(&self) -> StoreFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            if self.prepared.load(Ordering::Acquire) {
                return Ok(());
            }
            let statements = [
                &PREPARE_LOCK,
                &CREATE_DUE,
                &INDEX_DUE,
                &CREATE_TOPIC,
                &INDEX_TOPIC,
            ]
            .map(|statement| SqlStatement {
                sql: self.pick(statement),
                params: Vec::new(),
            });
            self.host
                .transaction("wake_index_prepare", &statements)
                .await?;
            self.prepared.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn write<'a>(
        &'a self,
        scope: &'a str,
        summary: &'a WakeSummary,
    ) -> StoreFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let held = self.statements(scope, summary);
            let statements = held
                .iter()
                .map(|(sql, params)| SqlStatement {
                    sql,
                    params: params.iter().map(String::as_str).collect(),
                })
                .collect::<Vec<_>>();
            self.host
                .transaction("wake_index_write", &statements)
                .await
                .map(drop)
        })
    }
}
