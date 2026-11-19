use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use nanocode_board::EventId;

use super::{Call, Platform, PlatformError, PlatformFuture, Posted, Rendered, Target};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    Refuse,
    Lose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copy {
    pub to: Target,
    pub external: String,
    pub thread: Option<String>,
    pub sub: Option<String>,
    pub marker: EventId,
    pub rendered: Rendered,
    pub edits: u64,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Echo {
    Posted { to: Target, external: String, marker: EventId, text: String },
    Edited { to: Target, external: String, text: String },
    Deleted { to: Target, external: String },
}

type Faults = Box<dyn FnMut(&str, &Target) -> Fault + Send>;

pub struct State {
    pub copies: Vec<Copy>,
    pub echoes: Vec<Echo>,
    pub calls: u64,
    minted: u64,
    faults: Option<Faults>,
    poisoned: BTreeSet<EventId>,
}

pub struct Fake {
    pub sync: String,
    state: Mutex<State>,
}

impl Fake {
    pub fn new(sync: &str) -> Self {
        Self {
            sync: sync.to_owned(),
            state: Mutex::new(State { copies: Vec::new(), echoes: Vec::new(), calls: 0, minted: 0, faults: None, poisoned: BTreeSet::new() }),
        }
    }

    pub fn with_faults(sync: &str, faults: impl FnMut(&str, &Target) -> Fault + Send + 'static) -> Self {
        let fake = Self::new(sync);
        fake.lock().faults = Some(Box::new(faults));
        fake
    }

    pub fn poison(&self, marker: EventId) {
        self.lock().poisoned.insert(marker);
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn copies(&self) -> Vec<Copy> {
        self.lock().copies.clone()
    }

    pub fn echoes(&self) -> Vec<Echo> {
        std::mem::take(&mut self.lock().echoes)
    }

    fn fault(state: &mut State, call: &str, to: &Target) -> Fault {
        state.calls += 1;
        state.faults.as_mut().map_or(Fault::None, |faults| faults(call, to))
    }
}

fn refused(call: &str, to: &Target) -> PlatformError {
    PlatformError(format!("{call} to {to} refused"))
}

fn lost(call: &str, to: &Target) -> PlatformError {
    PlatformError(format!("{call} to {to} timed out"))
}

impl Platform for Fake {
    fn post<'a>(&'a self, call: Call<'a>) -> PlatformFuture<'a, Result<Posted, PlatformError>> {
        Box::pin(async move {
            let mut state = self.lock();
            let fault = Self::fault(&mut state, "post", call.to);
            if fault == Fault::Refuse || state.poisoned.contains(call.marker) {
                return Err(refused("post", call.to));
            }
            state.minted += 1;
            let external = format!("{}.{}", self.sync, state.minted);
            let thread = (!call.to.id.contains(':')).then(|| format!("{}:{external}", call.to.id));
            state.copies.push(Copy {
                to: call.to.clone(),
                external: external.clone(),
                thread: thread.clone(),
                sub: call.sub.map(str::to_owned),
                marker: call.marker.clone(),
                rendered: call.rendered.clone(),
                edits: 0,
                deleted: false,
            });
            state.echoes.push(Echo::Posted {
                to: call.to.clone(),
                external: external.clone(),
                marker: call.marker.clone(),
                text: call.rendered.text.clone(),
            });
            if fault == Fault::Lose {
                return Err(lost("post", call.to));
            }
            Ok(Posted { external, thread })
        })
    }

    fn edit<'a>(&'a self, call: Call<'a>, external: &'a str) -> PlatformFuture<'a, Result<(), PlatformError>> {
        Box::pin(async move {
            let mut state = self.lock();
            let fault = Self::fault(&mut state, "edit", call.to);
            if fault == Fault::Refuse {
                return Err(refused("edit", call.to));
            }
            let Some(copy) = state.copies.iter_mut().find(|copy| copy.external == external) else {
                return Err(PlatformError(format!("no message {external}")));
            };
            if !copy.deleted && copy.rendered != *call.rendered {
                copy.rendered = call.rendered.clone();
                copy.edits += 1;
                let edited = Echo::Edited { to: call.to.clone(), external: external.to_owned(), text: call.rendered.text.clone() };
                state.echoes.push(edited);
            }
            if fault == Fault::Lose {
                return Err(lost("edit", call.to));
            }
            Ok(())
        })
    }

    fn delete<'a>(&'a self, to: &'a Target, external: &'a str) -> PlatformFuture<'a, Result<(), PlatformError>> {
        Box::pin(async move {
            let mut state = self.lock();
            let fault = Self::fault(&mut state, "delete", to);
            if fault == Fault::Refuse {
                return Err(refused("delete", to));
            }
            if let Some(copy) = state.copies.iter_mut().find(|copy| copy.external == external)
                && !copy.deleted
            {
                copy.deleted = true;
                state.echoes.push(Echo::Deleted { to: to.clone(), external: external.to_owned() });
            }
            if fault == Fault::Lose {
                return Err(lost("delete", to));
            }
            Ok(())
        })
    }

    fn find<'a>(&'a self, to: &'a Target, marker: &'a EventId) -> PlatformFuture<'a, Result<Option<Posted>, PlatformError>> {
        Box::pin(async move {
            let mut state = self.lock();
            if Self::fault(&mut state, "find", to) == Fault::Refuse {
                return Err(refused("find", to));
            }
            let found = state
                .copies
                .iter()
                .find(|copy| copy.to == *to && copy.marker == *marker)
                .map(|copy| Posted { external: copy.external.clone(), thread: copy.thread.clone() });
            Ok(found)
        })
    }
}

pub fn by_thread(copies: &[Copy]) -> BTreeMap<String, Vec<&Copy>> {
    let mut threads: BTreeMap<String, Vec<&Copy>> = BTreeMap::new();
    for copy in copies {
        threads.entry(copy.to.id.clone()).or_default().push(copy);
    }
    threads
}
