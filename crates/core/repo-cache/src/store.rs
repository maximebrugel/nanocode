use std::io;

use nanocode_shell::{FileSystem, Remove, Write, join};

use crate::{
    error::Error,
    object::Id,
    refs::{allowed, ensure_dirs, update_ref, valid_name},
    sim::{self, Mutation},
};

pub const DIR_MODE: u32 = 0o755;
const DIRS: [&[u8]; 8] = [
    b"objects",
    b"objects/info",
    b"objects/pack",
    b"refs",
    b"refs/heads",
    b"refs/tags",
    b"refs/remotes",
    b"worktrees",
];
const HEADS: &[u8] = b"refs/heads/";
const REMOTES: &[u8] = b"refs/remotes/";
const SYMREF: &[u8] = b"ref: ";
const GITDIR: &[u8] = b"gitdir: ";

pub struct Config<'a> {
    pub head: &'a [u8],
    pub remote: &'a [u8],
    pub url: &'a [u8],
    pub alternates: &'a [&'a [u8]],
    pub read_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Store {
    root: Vec<u8>,
    remote: Vec<u8>,
    read_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub name: Vec<u8>,
    pub dir: Vec<u8>,
    pub branch: Vec<u8>,
    pub git_dir: Vec<u8>,
}

fn component(name: &[u8]) -> bool {
    !name.is_empty() && !name.starts_with(b".") && name.iter().all(|&byte| allowed(byte))
}

fn text(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = parts.concat();
    out.push(b'\n');
    out
}

async fn create(fs: &dyn FileSystem, path: &[u8], data: &[u8], limit: u64) -> Result<(), Error> {
    match fs.write(path, data, Write::New).get().await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fs.read(path, limit).get().await?.as_ref() == data {
                Ok(())
            } else {
                Err(Error::Taken(path.to_vec()))
            }
        }
        Err(error) => Err(error.into()),
    }
}

async fn read_opt(fs: &dyn FileSystem, path: &[u8], limit: u64) -> Result<Option<Vec<u8>>, Error> {
    match fs.read(path, limit).get().await {
        Ok(data) => Ok(Some(data.as_ref().to_vec())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn fetches_outside_remotes(config: &[u8]) -> bool {
    config.split(|&byte| byte == b'\n').any(|row| {
        let row = row.trim_ascii();
        row.starts_with(b"mirror")
            || (row.starts_with(b"fetch")
                && row
                    .iter()
                    .rposition(|&byte| byte == b':')
                    .is_none_or(|cut| !row[cut + 1..].trim_ascii().starts_with(REMOTES)))
    })
}

pub async fn open(fs: &dyn FileSystem, root: &[u8], config: &Config<'_>) -> Result<Store, Error> {
    if !component(config.remote) || !valid_name(&[HEADS, config.head].concat()) {
        return Err(Error::Name(config.remote.to_vec()));
    }
    let store = Store {
        root: root.to_vec(),
        remote: config.remote.to_vec(),
        read_limit: config.read_limit,
    };
    ensure_dirs(fs, b"/", root).await?;
    for dir in DIRS {
        ensure_dirs(fs, root, dir).await?;
    }
    let head = text(&[SYMREF, HEADS, config.head]);
    match fs
        .write(&join(root, b"HEAD"), &head, Write::New)
        .get()
        .await
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let remote = config.remote;
    let settings = [
        b"[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n[remote \""
            .as_slice(),
        remote,
        b"\"]\n\turl = ",
        config.url,
        b"\n\tfetch = +refs/heads/*:refs/remotes/",
        remote,
        b"/*\n",
    ]
    .concat();
    let path = join(root, b"config");
    match fs.write(&path, &settings, Write::New).get().await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if fetches_outside_remotes(&fs.read(&path, config.read_limit).get().await?) {
                return Err(Error::Taken(path));
            }
        }
        Err(error) => return Err(error.into()),
    }
    if !config.alternates.is_empty() {
        let list: Vec<u8> = config
            .alternates
            .iter()
            .flat_map(|dir| text(&[dir]))
            .collect();
        create(
            fs,
            &join(root, b"objects/info/alternates"),
            &list,
            config.read_limit,
        )
        .await?;
    }
    Ok(store)
}

impl Store {
    pub fn root(&self) -> &[u8] {
        &self.root
    }

    pub fn remote(&self) -> &[u8] {
        &self.remote
    }

    pub const fn read_limit(&self) -> u64 {
        self.read_limit
    }

    pub fn tracking(&self, peer: &[u8], branch: &[u8]) -> Vec<u8> {
        [REMOTES, peer, b"/", branch].concat()
    }

    pub fn head(branch: &[u8]) -> Vec<u8> {
        [HEADS, branch].concat()
    }

    fn meta(&self, name: &[u8]) -> Vec<u8> {
        join(&join(&self.root, b"worktrees"), name)
    }

    pub async fn worktree(
        &self,
        fs: &dyn FileSystem,
        name: &[u8],
        dir: &[u8],
        branch: &[u8],
        start: Id,
    ) -> Result<Worktree, Error> {
        let head_ref = Self::head(branch);
        if !component(name) || !valid_name(&head_ref) {
            return Err(Error::Name(name.to_vec()));
        }
        let shared = sim::mutated(Mutation::SharedHead);
        let meta = if shared {
            self.root.clone()
        } else {
            self.meta(name)
        };
        let git_file = join(dir, b".git");
        let link = text(&[GITDIR, &meta]);
        let limit = self.read_limit;
        let found = Worktree {
            name: name.to_vec(),
            dir: dir.to_vec(),
            branch: branch.to_vec(),
            git_dir: meta.clone(),
        };
        match read_opt(fs, &git_file, limit).await? {
            Some(data) if data == link => return Ok(found),
            Some(_) => return Err(Error::Taken(git_file)),
            None => {}
        }
        let head = text(&[SYMREF, &head_ref]);
        if shared {
            fs.write(&join(&meta, b"HEAD"), &head, Write::Replace)
                .get()
                .await?;
        } else {
            ensure_dirs(fs, &self.root, b"worktrees").await?;
            ensure_dirs(fs, &join(&self.root, b"worktrees"), name).await?;
            create(fs, &join(&meta, b"commondir"), b"../..\n", limit).await?;
            create(fs, &join(&meta, b"gitdir"), &text(&[&git_file]), limit).await?;
            create(fs, &join(&meta, b"HEAD"), &head, limit).await?;
        }
        match update_ref(fs, self, &head_ref, None, start).await {
            Ok(()) => {}
            Err(Error::Stale {
                current: Some(current),
                ..
            }) if current == start && !self.checked_out_elsewhere(fs, name, branch).await? => {}
            Err(error) => {
                if !shared {
                    for file in [b"HEAD".as_slice(), b"gitdir", b"commondir"] {
                        let _ = fs.remove(&join(&meta, file), Remove::File).get().await;
                    }
                    let _ = fs.remove(&meta, Remove::Dir).get().await;
                }
                return Err(match error {
                    Error::Stale { .. } => Error::Taken(head_ref),
                    other => other,
                });
            }
        }
        ensure_dirs(fs, b"/", dir).await?;
        create(fs, &git_file, &link, limit).await?;
        Ok(found)
    }

    async fn checked_out_elsewhere(
        &self,
        fs: &dyn FileSystem,
        name: &[u8],
        branch: &[u8],
    ) -> Result<bool, Error> {
        Ok(self
            .worktrees(fs)
            .await?
            .iter()
            .any(|other| other.name != name && other.branch == branch))
    }

    pub async fn worktrees(&self, fs: &dyn FileSystem) -> Result<Vec<Worktree>, Error> {
        let names = match fs.list(&join(&self.root, b"worktrees")).get().await {
            Ok(names) => names,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut out = Vec::new();
        for name in names {
            let meta = self.meta(&name);
            let (Some(head), Some(gitdir)) = (
                read_opt(fs, &join(&meta, b"HEAD"), self.read_limit).await?,
                read_opt(fs, &join(&meta, b"gitdir"), self.read_limit).await?,
            ) else {
                continue;
            };
            let head = head.strip_suffix(b"\n").unwrap_or(&head);
            let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(&gitdir);
            let (Some(branch), Some(dir)) = (
                head.strip_prefix(SYMREF)
                    .and_then(|full| full.strip_prefix(HEADS)),
                gitdir.strip_suffix(b"/.git"),
            ) else {
                continue;
            };
            out.push(Worktree {
                name: name.clone(),
                dir: dir.to_vec(),
                branch: branch.to_vec(),
                git_dir: meta,
            });
        }
        out.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(out)
    }
}
