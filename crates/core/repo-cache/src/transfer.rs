use std::collections::BTreeSet;

use nanocode_shell::{FileSystem, Reply};

use crate::{
    error::Error,
    object::{Id, Kind, Object},
    refs::{Ref, read_ref, update_ref},
    sim::{self, Mutation},
    store::Store,
};

const HEADS: &[u8] = b"refs/heads/";

pub trait Objects {
    fn get(&self, id: &Id) -> Option<Object>;
    fn has(&self, id: &Id) -> bool;
    fn put_all(&self, objects: Vec<Object>) -> Reply<()>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Received {
    pub objects: usize,
    pub refs: usize,
}

pub fn wants(advertised: &[Ref], receiver: &dyn Objects) -> Vec<Id> {
    let mut out: Vec<Id> = advertised
        .iter()
        .map(|known| known.id)
        .filter(|id| !receiver.has(id))
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn haves(receiver: &dyn Objects, tips: &[Id], limit: usize) -> Vec<Id> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    let mut todo: Vec<Id> = tips.to_vec();
    while let Some(id) = todo.pop() {
        if order.len() >= limit {
            break;
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(object) = receiver.get(&id) else {
            continue;
        };
        if object.kind != Kind::Commit && !tips.contains(&id) {
            continue;
        }
        order.push(id);
        if object.kind == Kind::Commit {
            todo.extend(object.links().unwrap_or_default());
        }
    }
    order
}

fn closure(
    sender: &dyn Objects,
    from: &[Id],
    stop: &BTreeSet<Id>,
    strict: bool,
) -> Result<Vec<Id>, Error> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    let mut todo: Vec<Id> = from.to_vec();
    while let Some(id) = todo.pop() {
        if stop.contains(&id) || !seen.insert(id) {
            continue;
        }
        let Some(object) = sender.get(&id) else {
            if strict {
                return Err(Error::Unreachable(id));
            }
            continue;
        };
        order.push(id);
        todo.extend(object.links()?);
    }
    Ok(order)
}

pub fn missing(sender: &dyn Objects, wants: &[Id], haves: &[Id]) -> Result<Vec<Id>, Error> {
    let held: BTreeSet<Id> = if sim::mutated(Mutation::ResendHeld) {
        BTreeSet::new()
    } else {
        closure(sender, haves, &BTreeSet::new(), false)?
            .into_iter()
            .collect()
    };
    closure(sender, wants, &held, true)
}

pub fn pack(sender: &dyn Objects, wants: &[Id], haves: &[Id]) -> Result<Vec<Object>, Error> {
    missing(sender, wants, haves)?
        .iter()
        .map(|id| sender.get(id).ok_or(Error::Unreachable(*id)))
        .collect()
}

pub fn verify(receiver: &dyn Objects, pack: &[Object], tips: &[Id]) -> Result<(), Error> {
    let sent: BTreeSet<Id> = pack.iter().map(Object::id).collect();
    let present = |id: &Id| sent.contains(id) || receiver.has(id);
    for object in pack {
        if let Some(id) = object.links()?.into_iter().find(|id| !present(id)) {
            return Err(Error::Unreachable(id));
        }
    }
    match tips.iter().find(|id| !present(id)) {
        Some(id) => Err(Error::Unreachable(*id)),
        None => Ok(()),
    }
}

pub async fn receive(
    fs: &dyn FileSystem,
    store: &Store,
    receiver: &dyn Objects,
    pack: Vec<Object>,
    advertised: &[Ref],
    peer: &[u8],
) -> Result<Received, Error> {
    let moves: Vec<(Vec<u8>, Id)> = advertised
        .iter()
        .filter_map(|known| {
            let branch = known.name.strip_prefix(HEADS)?;
            let target = if sim::mutated(Mutation::FetchMovesHeads) {
                known.name.clone()
            } else {
                store.tracking(peer, branch)
            };
            Some((target, known.id))
        })
        .collect();
    let tips: Vec<Id> = moves.iter().map(|(_, id)| *id).collect();
    verify(receiver, &pack, &tips)?;
    let objects = pack.len();
    let early = sim::mutated(Mutation::RefsBeforeObjects);
    let mut refs = 0;
    if early {
        refs = move_refs(fs, store, &moves).await?;
    }
    receiver.put_all(pack).get().await?;
    if !early {
        refs = move_refs(fs, store, &moves).await?;
    }
    Ok(Received { objects, refs })
}

async fn move_refs(
    fs: &dyn FileSystem,
    store: &Store,
    moves: &[(Vec<u8>, Id)],
) -> Result<usize, Error> {
    let mut moved = 0;
    for (name, id) in moves {
        let current = read_ref(fs, store, name).await?;
        if current != Some(*id) {
            update_ref(fs, store, name, current, *id).await?;
            moved += 1;
        }
    }
    Ok(moved)
}
