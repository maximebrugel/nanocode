use nanocode_turnlog::{Point, Record};

use super::{OneLog, owned, stored_key};
use crate::durability::{StoreError, StoreRecord};

pub mod sim {
    use std::sync::atomic::{AtomicU8, Ordering};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(u8)]
    pub enum Mutation {
        None,
        HeldSkipsCommit,
        ReimportAdvances,
        ExportSkipsCommit,
        ExportSkipsKeys,
    }

    impl Mutation {
        pub const ALL: [Self; 4] = [
            Self::HeldSkipsCommit,
            Self::ReimportAdvances,
            Self::ExportSkipsCommit,
            Self::ExportSkipsKeys,
        ];

        #[must_use]
        pub const fn name(self) -> &'static str {
            match self {
                Self::None => "none",
                Self::HeldSkipsCommit => "held_skips_commit",
                Self::ReimportAdvances => "reimport_advances",
                Self::ExportSkipsCommit => "export_skips_commit",
                Self::ExportSkipsKeys => "export_skips_keys",
            }
        }

        #[must_use]
        pub fn parse(name: &str) -> Option<Self> {
            std::iter::once(Self::None)
                .chain(Self::ALL)
                .find(|mutation| mutation.name() == name)
        }
    }

    static MUTATION: AtomicU8 = AtomicU8::new(Mutation::None as u8);

    pub fn mutate(mutation: Mutation) {
        MUTATION.store(mutation as u8, Ordering::SeqCst);
    }

    pub fn mutated(mutation: Mutation) -> bool {
        MUTATION.load(Ordering::Relaxed) == mutation as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Imported {
    Copied,
    Held,
}

impl OneLog {
    pub async fn import_state(
        &self,
        state_id: &str,
        revision: u64,
        payload: &str,
        records: &[StoreRecord],
    ) -> Result<Imported, StoreError> {
        if revision == 0 {
            return Err(StoreError::NotCommitted(format!(
                "state {state_id} is imported at revision 0, which holds no checkpoint"
            )));
        }
        // Checked under the lease, which acquire also holds while it adds an owner, so no session
        // can take the state between this check and the import's commit.
        let mut lease = self.log.lease().await;
        if self.owners.read(|owners| owners.get(state_id).is_some()) {
            return Err(StoreError::Fenced);
        }
        lease.load_owners(&owned(state_id, records)).await?;
        let fold = lease.fold();
        let unsaved = records
            .iter()
            .filter(|record| {
                fold.stored(&stored_key(state_id, &record.key)) != Some(&*record.value)
            })
            .collect::<Vec<_>>();
        let (imported, written) = match fold.checkpoint(state_id) {
            None => (Imported::Copied, revision),
            Some(held) if held.revision != revision => {
                return Err(StoreError::Conflict {
                    expected: revision,
                    actual: held.revision,
                });
            }
            Some(held) if held.payload != payload => {
                return Err(StoreError::NotCommitted(format!(
                    "turn log {} holds state {state_id} at revision {revision} with a different payload",
                    self.scope()
                )));
            }
            Some(_) if !unsaved.is_empty() => {
                return Err(StoreError::NotCommitted(format!(
                    "turn log {} holds state {state_id} at revision {revision} with a different record {}",
                    self.scope(),
                    unsaved[0].key
                )));
            }
            Some(_) if sim::mutated(sim::Mutation::ReimportAdvances) => {
                (Imported::Copied, revision + 1)
            }
            Some(_) => (Imported::Held, revision),
        };
        let pushed = imported == Imported::Copied;
        if pushed {
            let unsaved = unsaved
                .into_iter()
                .map(|record| Record::Stored {
                    key: stored_key(state_id, &record.key),
                    value: record.value.to_string(),
                })
                .collect::<Vec<_>>();
            for record in unsaved {
                lease.push(record)?;
            }
            lease.push(Record::Checkpoint {
                payload: payload.to_owned(),
                revision: written,
                state: state_id.to_owned(),
            })?;
        }
        let owed = lease.buffered() > 0;
        assert!(
            !pushed || owed,
            "an import that pushed records has them buffered"
        );
        if owed && (pushed || !sim::mutated(sim::Mutation::HeldSkipsCommit)) {
            lease.commit(Point::Progress).await?;
        }
        Ok(imported)
    }
}
