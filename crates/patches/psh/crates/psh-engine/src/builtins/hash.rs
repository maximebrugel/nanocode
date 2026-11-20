//! The command hash table and `hash [-lr] [-p path] [-dt] [name ...]`.
//!
//! Commands found through `PATH` are remembered with how often they ran; a remembered
//! path is used as it is, without searching again. The table belongs to the value of
//! `PATH` it was filled under: when `PATH` changes, it starts empty (bash empties it on
//! every assignment to `PATH`).

use std::rc::Rc;

use crate::host::FileKind;
use crate::state::{R, Vp};

use super::{out, s, shell_quote};

#[derive(Clone, Default)]
pub(crate) struct HashTable {
    /// The `PATH` the entries were found with.
    path: Vec<u8>,
    /// Name, path, hits; in insertion order.
    entries: Vec<(Vec<u8>, Vec<u8>, u32)>,
    dot: Vec<Vec<u8>>,
    utility: Vec<Vec<u8>>,
}

impl HashTable {
    fn sync(&mut self, path: &[u8]) {
        if self.path != path {
            self.path = path.to_vec();
            self.entries.clear();
            self.dot.clear();
            self.utility.clear();
        }
    }

    fn get(&self, name: &[u8]) -> Option<&Vec<u8>> {
        self.entries.iter().find(|e| e.0 == name).map(|e| &e.1)
    }

    fn remove(&mut self, name: &[u8]) -> bool {
        self.dot.retain(|n| n != name);
        self.utility.retain(|n| n != name);
        let before = self.entries.len();
        self.entries.retain(|e| e.0 != name);
        self.entries.len() != before
    }

    fn insert_dot(&mut self, name: &[u8], path: Vec<u8>, hits: u32, dot: bool) {
        self.dot.retain(|n| n != name);
        if dot {
            self.dot.push(name.to_vec());
        }
        self.insert(name, path, hits);
    }

    fn insert(&mut self, name: &[u8], path: Vec<u8>, hits: u32) {
        self.utility.retain(|n| n != name);
        match self.entries.iter_mut().find(|e| e.0 == name) {
            Some(e) => {
                e.1 = path;
                e.2 = hits;
            }
            None => self.entries.push((name.to_vec(), path, hits)),
        }
    }
}

fn current_path(vp: &Vp) -> Vec<u8> {
    vp.var("PATH")
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| crate::exec::DEFAULT_PATH.to_vec())
}

/// The table, emptied first if `PATH` changed since it was filled.
fn table(vp: &mut Vp) -> &mut HashTable {
    let path = current_path(vp);
    let t = Rc::make_mut(&mut vp.hash);
    t.sync(&path);
    t
}

/// The remembered path of a command, if any (`type` says "hashed").
pub(crate) async fn hashed(vp: &Vp, name: &[u8]) -> Option<Vec<u8>> {
    if vp.hash.path != current_path(vp) {
        return None;
    }
    let path = vp.hash.get(name)?;
    check(vp, name, path, vp.hash.dot.iter().any(|n| n == name)).await
}

/// Builtins a real system also has as programs in /usr/bin (coreutils, procps).
const PROGRAM_BUILTINS: &[&[u8]] = &[
    b"[", b"echo", b"false", b"kill", b"printf", b"pwd", b"test", b"true",
];

/// Where the program a sandbox's psh utility (or a builtin a real system also has as a
/// program) stands in for would be, when `path` (else `PATH`) has /usr/bin: a sandbox has
/// no program files, but scripts that ask `type -a`, `type -P`, `hash` or `which` expect
/// one there, as on a real system. Running that path runs the utility, as the host
/// resolves its programs to /usr/bin.
pub(crate) async fn stand_in(vp: &Vp, name: &[u8], path: Option<&[u8]>) -> Option<Vec<u8>> {
    if !vp.core.utilities.sandbox || name.is_empty() || name.contains(&b'/') {
        return None;
    }
    let path = path.map_or_else(|| current_path(vp), <[u8]>::to_vec);
    if !vp
        .host()
        .path_entries(&path)
        .iter()
        .any(|d| d.as_slice() == b"/usr/bin")
    {
        return None;
    }
    if !PROGRAM_BUILTINS.contains(&name) && crate::util::lookup(vp, name).await.is_none() {
        return None;
    }
    Some([b"/usr/bin/".as_slice(), name].concat())
}

pub(crate) async fn names_program(vp: &Vp, name: &[u8]) -> bool {
    !vp.hash.utility.iter().any(|n| n == name) && hashed(vp, name).await.is_some()
}

pub(crate) async fn remember_utility(vp: &mut Vp, name: &[u8]) {
    if vp.core.utilities.sandbox || (vp.var("PATH").is_none() && !cfg!(windows)) {
        return;
    }
    let synced = vp.var("PATH") == Some(vp.hash.path.as_slice());
    let searched = take_searched(vp, name);
    if synced && vp.hash.get(name).is_some() {
        if let Some(e) = table(vp).entries.iter_mut().find(|e| e.0 == name) {
            e.2 += 1;
        }
        return;
    }
    let found = match searched {
        Some(found) => found,
        None => search_dot(vp, name, None).await,
    };
    if let Some((found, dot)) = found
        && executable(vp, &found).await
    {
        let t = table(vp);
        t.insert_dot(name, found, 1, dot);
        t.utility.push(name.to_vec());
    }
}

async fn executable(vp: &Vp, path: &[u8]) -> bool {
    let abs = vp.abs_path(path);
    matches!(vp.host().stat(&abs, true).await, Ok(m) if m.kind != FileKind::Dir)
        && matches!(
            vp.host().access(&abs, crate::host::Access::Execute).await,
            Ok(true)
        )
}

async fn check(vp: &Vp, name: &[u8], path: &[u8], dot: bool) -> Option<Vec<u8>> {
    let relative = !path.starts_with(b"/") && vp.abs_path(path) != path;
    if dot || relative {
        let tail = if relative { path } else { name };
        let here = if tail.starts_with(b"./") {
            tail.to_vec()
        } else {
            [b"./".as_slice(), tail].concat()
        };
        if executable(vp, &here).await {
            return Some(here);
        }
        if path.starts_with(b".")
            && let Some(end) = path.iter().rposition(|&b| b == b'/')
            && same_dir(vp, &path[..end]).await
        {
            return None;
        }
    }
    Some(path.to_vec())
}

async fn same_dir(vp: &Vp, dir: &[u8]) -> bool {
    let host = vp.host();
    match (
        host.canonicalize(&vp.abs_path(dir)).await,
        host.canonicalize(&vp.cwd).await,
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Searches `PATH` (or `path`) for a command name without a slash, ignoring the table:
/// the first executable, else the first other file (which then fails to run with
/// "Permission denied", as in bash). A relative `PATH` entry gives a relative path
/// (`dir/name`), as bash shows it.
pub(crate) async fn search(vp: &Vp, name: &[u8], path: Option<&[u8]>) -> Option<Vec<u8>> {
    search_dot(vp, name, path).await.map(|f| f.0)
}

type Searched = Option<(Vec<u8>, bool)>;

/// A PATH search: name, PATH, cwd and result.
type LastSearch = (Vec<u8>, Vec<u8>, Vec<u8>, Searched);

thread_local! {
    /// The last `PATH` search a utility lookup made: (name, `PATH`, cwd, result). The
    /// run that follows the lookup takes it to fill the table instead of searching
    /// again (each miss is a `stat` per `PATH` entry, slow on some file systems).
    static SEARCHED: std::cell::RefCell<Option<LastSearch>> =
        const { std::cell::RefCell::new(None) };
}

/// `search` for a utility lookup, kept for the `remember_utility` of the run that
/// follows it.
pub(crate) async fn search_for_utility(vp: &Vp, name: &[u8]) -> Option<Vec<u8>> {
    let found = search_dot(vp, name, None).await;
    let shown = found.as_ref().map(|f| f.0.clone());
    if let Some(path) = vp.var("PATH") {
        let key = (name.to_vec(), path.to_vec(), vp.cwd.clone(), found);
        SEARCHED.with(|s| *s.borrow_mut() = Some(key));
    }
    shown
}

/// The search the lookup of `name` just made, if it was under the same `PATH` and cwd.
fn take_searched(vp: &Vp, name: &[u8]) -> Option<Searched> {
    let (n, path, cwd, found) = SEARCHED.with(|s| s.borrow_mut().take())?;
    (n == name && vp.var("PATH") == Some(path.as_slice()) && vp.cwd == cwd).then_some(found)
}

async fn search_dot(vp: &Vp, name: &[u8], path: Option<&[u8]>) -> Option<(Vec<u8>, bool)> {
    let path = match path {
        Some(p) => p.to_vec(),
        None if vp.var("PATH").is_none() && !cfg!(windows) => {
            return executable(vp, name).await.then(|| (name.to_vec(), false));
        }
        None => current_path(vp),
    };
    let mut fallback = None;
    let mut dot = false;
    // On a host whose resolve looks at the file system itself, a candidate that is not
    // there is not found either: one `stat` per missing entry, not two.
    let plain = !cfg!(windows) && vp.host().native_paths().is_some();
    for dir in vp.host().path_entries(&path) {
        let dir: &[u8] = if dir.is_empty() { b"." } else { &dir[..] };
        let relative = vp.abs_path(dir) != dir;
        let joined = |base: &[u8]| {
            let mut shown = dir.to_vec();
            if !shown.ends_with(b"/") {
                shown.push(b'/');
            }
            shown.extend_from_slice(base);
            shown
        };
        let cand = joined(name);
        let abs = vp.abs_path(&cand);
        let meta = if plain || fallback.is_none() {
            Some(vp.host().stat(&abs, true).await)
        } else {
            None
        };
        let missing = plain && matches!(meta, Some(Err(_)));
        if !missing && let Some(found) = vp.host().resolve(name, dir, &vp.cwd).await {
            if !relative {
                return Some((found, dot));
            }
            // The host may have added an extension (PATHEXT): keep its file name.
            let base = found.rsplit(|&b| b == b'/').next().unwrap_or(name);
            return Some((joined(base), dot));
        }
        if relative && !dot && same_dir(vp, dir).await {
            dot = true;
        }
        if fallback.is_none() && matches!(meta, Some(Ok(m)) if m.kind != FileKind::Dir) {
            fallback = Some((if relative { cand } else { abs }, dot));
        }
    }
    fallback
}

/// Finds the program for a command name without a slash, as running it does: the
/// table, else a search of `PATH` (or `path`, for `command -p`), which is remembered.
/// `run` counts a hit.
pub(crate) async fn find(
    vp: &mut Vp,
    name: &[u8],
    path: Option<&[u8]>,
    run: bool,
) -> Option<Vec<u8>> {
    let hit = u32::from(run);
    if path.is_none()
        && let Some(e) = table(vp).entries.iter().find(|e| e.0 == name).cloned()
    {
        let dot = vp.hash.dot.iter().any(|n| n == name);
        if let Some(p) = check(vp, name, &e.1, dot).await {
            if let Some(e) = table(vp).entries.iter_mut().find(|e| e.0 == name) {
                e.2 += hit;
            }
            return Some(p);
        }
    }
    if path.is_none() && vp.var("PATH").is_none() && !cfg!(windows) {
        return search(vp, name, None).await;
    }
    let (found, dot) = search_dot(vp, name, path).await?;
    table(vp).insert_dot(name, found.clone(), hit, dot && path.is_none());
    Some(found)
}

const USAGE: &[u8] = b"hash: usage: hash [-lr] [-p pathname] [-dt] [name ...]\n";

pub(super) async fn hash(vp: &mut Vp, args: &[Vec<u8>]) -> R {
    let (mut list, mut reset, mut delete, mut show) = (false, false, false, false);
    let mut set: Option<Vec<u8>> = None;
    let mut i = 1;
    'opts: while let Some(a) = args.get(i) {
        if a == b"--" {
            i += 1;
            break;
        }
        if a.len() < 2 || a[0] != b'-' {
            break;
        }
        for (j, &c) in a.iter().enumerate().skip(1) {
            match c {
                b'l' => list = true,
                b'r' => reset = true,
                b'd' => delete = true,
                b't' => show = true,
                b'p' => {
                    let value = if j + 1 < a.len() {
                        a[j + 1..].to_vec()
                    } else {
                        i += 1;
                        match args.get(i) {
                            Some(v) => v.clone(),
                            None => {
                                vp.error("hash: -p: option requires an argument").await;
                                vp.error_raw(USAGE).await;
                                return Ok(2);
                            }
                        }
                    };
                    set = Some(value);
                    i += 1;
                    continue 'opts;
                }
                _ => {
                    vp.error(&format!("hash: -{}: invalid option", c as char))
                        .await;
                    vp.error_raw(USAGE).await;
                    return Ok(2);
                }
            }
        }
        i += 1;
    }
    let names = &args[i..];
    if reset {
        let t = table(vp);
        t.entries.clear();
        t.dot.clear();
    }
    if (show || delete) && names.is_empty() {
        let o = if show { 't' } else { 'd' };
        vp.error(&format!("hash: -{o}: option requires an argument"))
            .await;
        return Ok(1);
    }
    if names.is_empty() {
        if reset || set.is_some() {
            return Ok(0);
        }
        let t = table(vp).clone();
        if t.entries.is_empty() {
            return out(vp, "hash", b"hash: hash table empty\n").await;
        }
        let mut buf = Vec::new();
        if list {
            for (n, p, _) in &t.entries {
                buf.extend_from_slice(b"builtin hash -p ");
                buf.extend_from_slice(&super::declare::set_quote(p, vp.utf8()));
                buf.push(b' ');
                buf.extend_from_slice(&shell_quote(n, vp.utf8()));
                buf.push(b'\n');
            }
        } else {
            buf.extend_from_slice(b"hits\tcommand\n");
            for (_, p, hits) in &t.entries {
                buf.extend_from_slice(format!("{hits:4}\t").as_bytes());
                buf.extend_from_slice(p);
                buf.push(b'\n');
            }
        }
        return out(vp, "hash", &buf).await;
    }
    let mut status = 0;
    if let Some(p) = set {
        if vp.opts.restricted && p.contains(&b'/') {
            vp.error(&format!("hash: {}: restricted", s(&p))).await;
            return Ok(1);
        }
        let is_dir = matches!(
            vp.host().stat(&vp.abs_path(&p), true).await,
            Ok(m) if m.kind == FileKind::Dir
        );
        for n in names {
            if is_dir {
                vp.error(&format!("hash: {}: Is a directory", s(&p))).await;
                status = 1;
            } else {
                table(vp).insert(n, p.clone(), 0);
            }
        }
        return Ok(status);
    }
    if delete {
        for n in names {
            if !table(vp).remove(n) {
                vp.error(&format!("hash: {}: not found", s(n))).await;
                status = 1;
            }
        }
        return Ok(status);
    }
    if show {
        let mut buf = Vec::new();
        for n in names {
            let entry = table(vp).get(n).cloned();
            let dot = vp.hash.dot.iter().any(|d| d == n);
            let entry = match entry {
                Some(p) => check(vp, n, &p, dot).await,
                None => None,
            };
            match entry {
                Some(p) => {
                    if list {
                        buf.extend_from_slice(b"builtin hash -p ");
                        buf.extend_from_slice(&p);
                        buf.push(b' ');
                        buf.extend_from_slice(n);
                    } else {
                        if names.len() > 1 {
                            buf.extend_from_slice(n);
                            buf.push(b'\t');
                        }
                        buf.extend_from_slice(&p);
                    }
                    buf.push(b'\n');
                }
                None => {
                    if !buf.is_empty() {
                        super::write_fd(vp, 1, "hash", &std::mem::take(&mut buf)).await?;
                    }
                    vp.error(&format!("hash: {}: not found", s(n))).await;
                    status = 1;
                }
            }
        }
        if !buf.is_empty() && !super::write_fd(vp, 1, "hash", &buf).await? {
            status = 1;
        }
        return Ok(status);
    }
    for n in names {
        // Paths, builtins and functions are not remembered.
        if n.contains(&b'/')
            || super::is_builtin(vp, n)
            || std::str::from_utf8(n).is_ok_and(|f| vp.funcs.contains_key(f))
        {
            continue;
        }
        let found = match search_dot(vp, n, None).await {
            Some((p, dot)) if executable(vp, &p).await => Some((p, dot)),
            _ => None,
        };
        // A sandbox's utility is remembered at the path it stands in for, and still
        // runs as the utility.
        let standing = match found {
            Some(_) => None,
            None => stand_in(vp, n, None).await,
        };
        match (found, standing) {
            (Some((p, dot)), _) => table(vp).insert_dot(n, p, 0, dot),
            (None, Some(p)) => {
                let t = table(vp);
                t.insert(n, p, 0);
                t.utility.push(n.to_vec());
            }
            (None, None) => {
                table(vp).remove(n);
                vp.error(&format!("hash: {}: not found", s(n))).await;
                status = 1;
            }
        }
    }
    Ok(status)
}
