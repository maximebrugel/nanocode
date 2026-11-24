use crate::postgres::{PgConnection, PgError, PgPool};
use nanocode_durability_core::{
    Settle, Sql, SqlAccess, SqlDialect, SqlRow, SqlStatement, StoreError, StoreFuture,
};
use nanocode_sync::Owned;
use std::{sync::Arc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PgTimes {
    pub waited: Duration,
    pub ran: Duration,
}

pub struct PgSql {
    writes: PgPool,
    reads: PgPool,
    timed: Option<fn(&'static str, PgTimes)>,
}

impl PgSql {
    #[must_use]
    pub const fn reading(writes: PgPool, reads: PgPool) -> Self {
        Self {
            writes,
            reads,
            timed: None,
        }
    }

    #[must_use]
    pub fn timed(mut self, timed: fn(&'static str, PgTimes)) -> Self {
        self.timed = Some(timed);
        self
    }

    async fn transact(
        &self,
        pool: &PgPool,
        operation: &'static str,
        settle: Settle,
        statements: &[SqlStatement<'_>],
    ) -> Result<Vec<Vec<SqlRow>>, StoreError> {
        let started = nanocode_platform::now();
        let mut connection = pool.acquire().await.map_err(backend)?;
        let acquired = nanocode_platform::now();
        let result = connection
            .transaction(settle, statements)
            .await
            .map_err(backend);
        if let Some(timed) = self.timed {
            timed(
                operation,
                PgTimes {
                    waited: acquired - started,
                    ran: nanocode_platform::elapsed(acquired),
                },
            );
        }
        result
    }
}

impl Sql for PgSql {
    fn transaction<'a>(
        &'a self,
        operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        Box::pin(self.transact(&self.writes, operation, Settle::Durable, statements))
    }

    fn call_bound(&self) -> Option<Duration> {
        None
    }

    fn run<'a>(
        &'a self,
        access: SqlAccess,
        operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        let pool = match access {
            SqlAccess::Read => &self.reads,
            SqlAccess::Write | SqlAccess::Schema => &self.writes,
        };
        Box::pin(self.transact(pool, operation, Settle::Durable, statements))
    }

    fn write<'a>(
        &'a self,
        settle: Settle,
        operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        Box::pin(self.transact(&self.writes, operation, settle, statements))
    }

    fn dialect(&self) -> SqlDialect {
        SqlDialect::Postgres
    }

    fn concurrent(self: Arc<Self>) -> Option<Arc<dyn Sql>> {
        (self.writers() > 1 || self.readers() > 1).then_some(self as Arc<dyn Sql>)
    }

    fn writers(&self) -> usize {
        self.writes.connections()
    }

    fn readers(&self) -> usize {
        self.reads.connections()
    }
}

pub struct PgTransaction<'c>(Owned<&'c mut PgConnection>);

impl<'c> PgTransaction<'c> {
    pub fn new(connection: &'c mut PgConnection) -> Self {
        Self(Owned::new(connection))
    }
}

impl Sql for PgTransaction<'_> {
    fn transaction<'a>(
        &'a self,
        _operation: &'static str,
        statements: &'a [SqlStatement<'a>],
    ) -> StoreFuture<'a, Result<Vec<Vec<SqlRow>>, StoreError>> {
        Box::pin(async move {
            self.0
                .acquire()
                .await
                .run(statements)
                .await
                .map_err(backend)
        })
    }

    fn call_bound(&self) -> Option<Duration> {
        None
    }

    fn dialect(&self) -> SqlDialect {
        SqlDialect::Postgres
    }
}

fn backend(error: PgError) -> StoreError {
    StoreError::Backend(error.to_string())
}
