//! The wake half of the partitioned log on Postgres: one LISTEN connection per node on
//! nanocode_log, whose payloads ("scope:head") every commit sends after it lands, in its own
//! unsynced transaction, so a notification arrives only for a committed append. A writer that
//! dies between the two loses that wake, so watchers also re-read on a timer. After every
//! (re)connect the listener calls caught_up first, so watchers re-read what they missed.

use crate::postgres::{PgConnection, PgError, PgOptions};

pub const CHANNEL: &str = "nanocode_log";

/// Splits a nanocode_log payload into its scope and head seq.
#[must_use]
pub fn parse(payload: &str) -> Option<(&str, u64)> {
    let (scope, head) = payload.rsplit_once(':')?;
    Some((scope, head.parse().ok()?))
}

/// Listens until the connection fails: rings every committed head. Call it in a loop that sleeps
/// between reconnects; each call starts with caught_up().
pub async fn listen(
    options: &PgOptions,
    caught_up: &(dyn Fn() + Sync),
    ring: &(dyn Fn(&str, u64) + Sync),
) -> PgError {
    let mut connection = match PgConnection::connect(options).await {
        Ok(connection) => connection,
        Err(error) => return error,
    };
    if let Err(error) = connection.listen(CHANNEL).await {
        return error;
    }
    caught_up();
    loop {
        match connection.notification().await {
            Ok(notification) => {
                if let Some((scope, head)) = parse(&notification.payload) {
                    ring(scope, head);
                }
            }
            Err(error) => return error,
        }
    }
}
