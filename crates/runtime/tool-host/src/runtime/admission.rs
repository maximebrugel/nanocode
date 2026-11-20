use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use nanocode_sync::{Atom, Signal};

use crate::FileAccess;

type OriginKey = Option<Arc<str>>;

#[derive(Clone, Copy, Default)]
struct Origin {
    count: usize,
    epoch: u64,
    yielded: bool,
}

#[derive(Clone, Default)]
struct Path {
    exclusive: Option<u64>,
    shared: usize,
    holders: HashMap<OriginKey, usize>,
    waiting: VecDeque<Waiter>,
}

#[derive(Clone, Default)]
struct GateState {
    paths: HashMap<Arc<str>, Path>,
    origins: HashMap<OriginKey, Origin>,
    granted: HashMap<u64, u64>,
    epochs: u64,
}

#[derive(Clone)]
struct Waiter {
    token: u64,
    exclusive: bool,
    origin: OriginKey,
    admitted: Arc<Signal>,
}

#[derive(Default)]
pub(super) struct AdmissionGate {
    state: Atom<GateState>,
    tokens: AtomicU64,
}

pub(super) struct Admission<'gate> {
    gate: &'gate AdmissionGate,
    path: Arc<str>,
    token: u64,
    origin: OriginKey,
    epoch: Option<u64>,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.gate
            .release(&self.path, self.token, &self.origin, self.epoch);
    }
}

impl Path {
    const fn admits(&self, exclusive: bool) -> bool {
        self.exclusive.is_none() && (!exclusive || self.shared == 0)
    }

    fn idle(&self) -> bool {
        self.exclusive.is_none() && self.shared == 0 && self.waiting.is_empty()
    }
}

impl GateState {
    const fn next_epoch(&mut self) -> u64 {
        self.epochs += 1;
        self.epochs
    }

    fn admit(
        &mut self,
        path: &Arc<str>,
        token: u64,
        exclusive: bool,
        origin: &OriginKey,
    ) -> Option<u64> {
        if exclusive {
            self.paths.entry(Arc::clone(path)).or_default().exclusive = Some(token);
            return None;
        }
        if !self.origins.contains_key(origin) {
            let epoch = self.next_epoch();
            self.origins.insert(
                origin.clone(),
                Origin {
                    epoch,
                    ..Origin::default()
                },
            );
        }
        let entry = self.origins.get_mut(origin)?;
        if entry.yielded {
            return None;
        }
        entry.count += 1;
        let held = self.paths.entry(Arc::clone(path)).or_default();
        held.shared += 1;
        *held.holders.entry(origin.clone()).or_default() += 1;
        Some(entry.epoch)
    }

    fn release_shared(&mut self, path: &Arc<str>, origin: &OriginKey, epoch: u64) {
        let Some(entry) = self.origins.get_mut(origin) else {
            return;
        };
        if entry.epoch != epoch || entry.count == 0 {
            return;
        }
        let Some(held) = self.paths.get_mut(path) else {
            return;
        };
        let Some(count) = held.holders.get_mut(origin) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            held.holders.remove(origin);
        }
        held.shared -= 1;
        entry.count -= 1;
        if entry.count == 0 && !entry.yielded {
            self.origins.remove(origin);
        }
    }

    fn advance(&mut self, path: &Arc<str>) -> Vec<Arc<Signal>> {
        let mut admitted = Vec::new();
        while let Some(next) = self
            .paths
            .get_mut(path)
            .filter(|held| {
                held.waiting
                    .front()
                    .is_some_and(|next| held.admits(next.exclusive))
            })
            .and_then(|held| held.waiting.pop_front())
        {
            if let Some(epoch) = self.admit(path, next.token, next.exclusive, &next.origin) {
                self.granted.insert(next.token, epoch);
            }
            admitted.push(next.admitted);
        }
        if self.paths.get(path).is_some_and(Path::idle) {
            self.paths.remove(path);
        }
        admitted
    }
}

impl AdmissionGate {
    pub(super) async fn admit(&self, access: FileAccess, origin: Option<&str>) -> Admission<'_> {
        let (path, exclusive) = match access {
            FileAccess::Read(path) => (path, false),
            FileAccess::Write(path) => (path, true),
        };
        let path: Arc<str> = Arc::from(path);
        let origin: OriginKey = (!exclusive).then(|| origin.map(Arc::from)).flatten();
        let token = self.tokens.fetch_add(1, Ordering::Relaxed);
        let mut admission = Admission {
            gate: self,
            path: Arc::clone(&path),
            token,
            origin: origin.clone(),
            epoch: None,
        };
        let admitted = Arc::new(Signal::new());
        let entry = self.state.update(|state| {
            let free = state
                .paths
                .get(&path)
                .is_none_or(|held| held.waiting.is_empty() && held.admits(exclusive));
            if free {
                Entry::Admitted(state.admit(&path, token, exclusive, &origin))
            } else {
                state
                    .paths
                    .entry(Arc::clone(&path))
                    .or_default()
                    .waiting
                    .push_back(Waiter {
                        token,
                        exclusive,
                        origin: origin.clone(),
                        admitted: Arc::clone(&admitted),
                    });
                Entry::Wait
            }
        });
        match entry {
            Entry::Admitted(epoch) => admission.epoch = epoch,
            Entry::Wait => admitted.wait().await,
        }
        admission
    }

    pub(super) fn observed(&self, origin: &str, observed: bool) {
        let key: OriginKey = Some(Arc::from(origin));
        let admitted = self.state.update(|state| {
            if observed {
                if let Some(entry) = state.origins.get_mut(&key) {
                    entry.yielded = false;
                    if entry.count == 0 {
                        state.origins.remove(&key);
                    }
                }
                return Vec::new();
            }
            let epoch = state.next_epoch();
            state.origins.insert(
                key.clone(),
                Origin {
                    count: 0,
                    epoch,
                    yielded: true,
                },
            );
            let released = state
                .paths
                .iter_mut()
                .filter_map(|(path, held)| {
                    held.shared -= held.holders.remove(&key)?;
                    Some(Arc::clone(path))
                })
                .collect::<Vec<_>>();
            released
                .iter()
                .flat_map(|path| state.advance(path))
                .collect::<Vec<_>>()
        });
        admitted.iter().for_each(|admitted| admitted.fire());
    }

    fn release(&self, path: &Arc<str>, token: u64, origin: &OriginKey, epoch: Option<u64>) {
        let admitted = self.state.update(|state| {
            if let Some(held) = state.paths.get_mut(path) {
                held.waiting.retain(|waiter| waiter.token != token);
                if held.exclusive == Some(token) {
                    held.exclusive = None;
                }
            }
            if let Some(epoch) = epoch.or_else(|| state.granted.remove(&token)) {
                state.release_shared(path, origin, epoch);
            }
            state.advance(path)
        });
        admitted.iter().for_each(|admitted| admitted.fire());
    }
}

enum Entry {
    Admitted(Option<u64>),
    Wait,
}
