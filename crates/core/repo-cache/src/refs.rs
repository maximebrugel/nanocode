use std::io;

use nanocode_shell::{FileSystem, Kind as Entry, Remove, Write, join};

use crate::{
    error::Error,
    object::{ID_BYTES, Id},
    sim::{self, Mutation},
    store::Store,
};

const PACKED: &[u8] = b"packed-refs";
const LOCK: &[u8] = b".lock";
const SHARED_LOCK: &[u8] = b"refs/update.lock";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    pub name: Vec<u8>,
    pub id: Id,
}

pub fn valid_name(name: &[u8]) -> bool {
    name.starts_with(b"refs/")
        && !name.ends_with(b"/")
        && !name.ends_with(LOCK)
        && name.split(|&byte| byte == b'/').all(|part| {
            !part.is_empty() && !part.starts_with(b".") && part.iter().all(|&byte| allowed(byte))
        })
        && !name.windows(2).any(|pair| pair == b"..")
}

pub const fn allowed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+')
}

fn line(id: Id) -> Vec<u8> {
    let mut data = id.hex().into_bytes();
    data.push(b'\n');
    data
}

fn parse_line(data: &[u8]) -> Result<Id, Error> {
    let text = data.strip_suffix(b"\n").unwrap_or(data);
    Id::parse(text).ok_or(Error::Corrupt("ref"))
}

async fn packed(fs: &dyn FileSystem, store: &Store) -> Result<Vec<Ref>, Error> {
    let data = match fs
        .read(&join(store.root(), PACKED), store.read_limit())
        .get()
        .await
    {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for row in data.split(|&byte| byte == b'\n') {
        if row.is_empty() || row.starts_with(b"#") || row.starts_with(b"^") {
            continue;
        }
        let hex = row
            .get(..ID_BYTES * 2)
            .ok_or(Error::Corrupt("packed-refs"))?;
        let name = row
            .get(ID_BYTES * 2 + 1..)
            .ok_or(Error::Corrupt("packed-refs"))?;
        out.push(Ref {
            name: name.to_vec(),
            id: Id::parse(hex).ok_or(Error::Corrupt("packed-refs"))?,
        });
    }
    Ok(out)
}

pub async fn read_ref(
    fs: &dyn FileSystem,
    store: &Store,
    name: &[u8],
) -> Result<Option<Id>, Error> {
    if !valid_name(name) {
        return Err(Error::Name(name.to_vec()));
    }
    match fs
        .read(&join(store.root(), name), store.read_limit())
        .get()
        .await
    {
        Ok(data) => return parse_line(&data).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(packed(fs, store)
        .await?
        .into_iter()
        .find(|known| known.name == name)
        .map(|known| known.id))
}

pub async fn refs(fs: &dyn FileSystem, store: &Store, prefix: &[u8]) -> Result<Vec<Ref>, Error> {
    let mut out: Vec<Ref> = Vec::new();
    let mut dirs = vec![prefix.strip_suffix(b"/").unwrap_or(prefix).to_vec()];
    while let Some(dir) = dirs.pop() {
        let names = match fs.list(&join(store.root(), &dir)).get().await {
            Ok(names) => names,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for name in names {
            let full = join(&dir, &name);
            if name.ends_with(LOCK) {
                continue;
            }
            let stat = fs.stat(&join(store.root(), &full), false).get().await?;
            if stat.kind == Entry::Dir {
                dirs.push(full);
            } else if valid_name(&full) {
                let data = fs
                    .read(&join(store.root(), &full), store.read_limit())
                    .get()
                    .await?;
                out.push(Ref {
                    id: parse_line(&data)?,
                    name: full,
                });
            }
        }
    }
    for known in packed(fs, store).await? {
        if known.name.starts_with(prefix) && !out.iter().any(|loose| loose.name == known.name) {
            out.push(known);
        }
    }
    out.sort();
    Ok(out)
}

pub async fn ensure_dirs(fs: &dyn FileSystem, root: &[u8], path: &[u8]) -> Result<(), Error> {
    let mut at = root.to_vec();
    for part in path
        .split(|&byte| byte == b'/')
        .filter(|part| !part.is_empty())
    {
        at = join(&at, part);
        match fs.mkdir(&at, crate::store::DIR_MODE).get().await {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub async fn update_ref(
    fs: &dyn FileSystem,
    store: &Store,
    name: &[u8],
    expected: Option<Id>,
    new: Id,
) -> Result<(), Error> {
    if !valid_name(name) {
        return Err(Error::Name(name.to_vec()));
    }
    if let Some(cut) = name.iter().rposition(|&byte| byte == b'/') {
        ensure_dirs(fs, store.root(), &name[..cut]).await?;
    }
    let path = join(store.root(), name);
    let shared = sim::mutated(Mutation::SharedLock);
    let lock = if shared {
        join(store.root(), SHARED_LOCK)
    } else {
        [path.as_slice(), LOCK].concat()
    };
    let how = if shared { Write::Replace } else { Write::New };
    match fs.write(&lock, &line(new), how).get().await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(Error::Busy(name.to_vec()));
        }
        Err(error) => return Err(error.into()),
    }
    let checked = match read_ref(fs, store, name).await {
        Ok(current) if current == expected => Ok(()),
        Ok(current) => Err(Error::Stale {
            name: name.to_vec(),
            current,
        }),
        Err(error) => Err(error),
    };
    let moved = match checked {
        Ok(()) => fs.rename(&lock, &path).get().await.map_err(Error::from),
        Err(error) => Err(error),
    };
    if moved.is_err() {
        let _ = fs.remove(&lock, Remove::File).get().await;
    }
    moved
}
