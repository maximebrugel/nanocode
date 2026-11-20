use std::sync::Arc;

use nanocode_sync::{Atom, Map};

use crate::{EncodedPayload, Error, Result, shared_store::SharedStore};

#[derive(Clone)]
enum Kept {
    Keeping(Map<Arc<str>, EncodedPayload>),
    Forgotten,
}

pub(crate) struct PayloadMemo(Atom<Kept>);

impl PayloadMemo {
    pub(crate) fn new() -> Self {
        Self(Atom::new(Kept::Keeping(Map::new())))
    }

    pub(crate) fn forget(&self) {
        self.0.set(Kept::Forgotten);
    }

    pub(crate) fn release<'a>(&self, keys: impl IntoIterator<Item = &'a str>) {
        let keys: Vec<&str> = keys.into_iter().collect();
        if keys.is_empty() {
            return;
        }
        self.0.update(|kept| {
            if let Kept::Keeping(map) = kept {
                for key in &keys {
                    map.remove(*key);
                }
            }
        });
    }

    pub(crate) fn len(&self) -> usize {
        self.0.read(|kept| match kept {
            Kept::Keeping(map) => map.len(),
            Kept::Forgotten => 0,
        })
    }

    fn kept(&self, payloads: &[EncodedPayload]) -> Vec<Option<EncodedPayload>> {
        self.0.read(|kept| match kept {
            Kept::Keeping(map) => payloads
                .iter()
                .map(|payload| map.get(payload.key()).cloned())
                .collect(),
            Kept::Forgotten => vec![None; payloads.len()],
        })
    }

    /// Keeps the payloads just loaded and drops every kept payload that this load
    /// did not ask for and that nothing outside the memo still holds: keeping it
    /// would hold its bytes as a copy no live history shares.
    fn keep(&self, requested: &[EncodedPayload], loaded: &[EncodedPayload]) {
        let requested: std::collections::HashSet<&str> =
            requested.iter().map(EncodedPayload::key).collect();
        self.0.update(|kept| {
            if let Kept::Keeping(map) = kept {
                map.retain(|key, payload| {
                    requested.contains(&**key) || payload.content_held_beyond(1)
                });
                for payload in loaded {
                    map.insert(Arc::from(payload.key()), payload.clone());
                }
            }
        });
    }

    /// Loads payloads the caller decodes into values of its own and then drops:
    /// hands back the copies kept for them and forgets them, and keeps none it
    /// loads, since a kept copy would hold the bytes beside the decoded values.
    pub(crate) async fn take_many(
        &self,
        payloads: &[EncodedPayload],
        store: &SharedStore,
        state_id: &str,
    ) -> Result<Vec<EncodedPayload>> {
        let slots: Vec<Option<EncodedPayload>> = self.0.update(|kept| match kept {
            Kept::Keeping(map) => payloads
                .iter()
                .map(|payload| map.remove(payload.key()))
                .collect(),
            Kept::Forgotten => vec![None; payloads.len()],
        });
        let missing: Vec<EncodedPayload> = payloads
            .iter()
            .zip(&slots)
            .filter(|(_, kept)| kept.is_none())
            .map(|(payload, _)| payload.clone())
            .collect();
        let loaded = if missing.is_empty() {
            Vec::new()
        } else {
            EncodedPayload::load_many(&missing, &mut store.clone(), state_id).await?
        };
        fill(slots, loaded)
    }

    pub(crate) async fn load_many(
        &self,
        payloads: &[EncodedPayload],
        store: &SharedStore,
        state_id: &str,
    ) -> Result<Vec<EncodedPayload>> {
        let slots = self.kept(payloads);
        let missing: Vec<EncodedPayload> = payloads
            .iter()
            .zip(&slots)
            .filter(|(_, kept)| kept.is_none())
            .map(|(payload, _)| payload.clone())
            .collect();
        let loaded = if missing.is_empty() {
            Vec::new()
        } else {
            EncodedPayload::load_many(&missing, &mut store.clone(), state_id).await?
        };
        self.keep(payloads, &loaded);
        fill(slots, loaded)
    }
}

fn fill(
    mut slots: Vec<Option<EncodedPayload>>,
    loaded: Vec<EncodedPayload>,
) -> Result<Vec<EncodedPayload>> {
    let mut loaded = loaded.into_iter();
    for slot in slots.iter_mut().filter(|slot| slot.is_none()) {
        *slot = loaded.next();
    }
    slots
        .into_iter()
        .map(|slot| slot.ok_or_else(|| Error::InvalidState("payload load lost a payload".into())))
        .collect()
}
