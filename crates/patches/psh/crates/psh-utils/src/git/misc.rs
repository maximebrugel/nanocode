//! check-ignore, hash-object, for-each-ref and apply: small commands over the ignore
//! rules, the object format, the references and patches.

use std::io::Write;

use super::cmd::ref_format;
use super::repo::normalize;
use super::rev;
use super::store::{Kind, join, object_oid};
use super::wild::wildmatch;
use super::worktree::{Work, ignored};
use super::write::Op;
use super::{Error, Git, Io, Res, option};

/// A path as given, relative to the worktree's root, or None outside it.
fn in_tree(git: &Git<'_>, root: &[u8], path: &str) -> Option<Vec<u8>> {
    let full = if path.starts_with('/') {
        normalize(path.as_bytes())
    } else {
        normalize(&join(&git.cwd, path.as_bytes()))
    };
    if full == root {
        return Some(Vec::new());
    }
    let rest = full.strip_prefix(root)?;
    rest.strip_prefix(b"/").map(<[u8]>::to_vec)
}

/// git check-ignore: prints the paths the ignore rules exclude; status 0 when one is.
pub fn check_ignore(git: &Git<'_>, io: &mut Io<'_>, args: &[String]) -> Res<i32> {
    let mut quiet = false;
    let mut paths = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-q" | "--quiet" => quiet = true,
            "--" => {
                paths.extend(args[i + 1..].iter().cloned());
                break;
            }
            _ if arg.starts_with('-') => return Err(option("check-ignore", arg)),
            _ => paths.push(arg.to_string()),
        }
        i += 1;
    }
    if paths.is_empty() {
        return Err(Error::Fatal("no path specified".to_string()));
    }
    let root = git.work_tree()?.to_vec();
    let work = Work::new(&git.repo, &root)?;
    let env = |key: &[u8]| -> Option<Vec<u8>> {
        git.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let mut any = false;
    for path in &paths {
        let Some(rel) = in_tree(git, &root, path) else {
            return Err(Error::Fatal(format!(
                "{path}: '{path}' is outside repository at '{}'",
                String::from_utf8_lossy(&root)
            )));
        };
        if rel.is_empty() || !ignored(&work, &rel, &env)? {
            continue;
        }
        any = true;
        if !quiet {
            writeln!(io.out, "{path}")?;
        }
    }
    Ok(if any { 0 } else { 1 })
}

/// git hash-object: the id files (or stdin) have as objects, stored with -w.
pub fn hash_object(git: &Git<'_>, io: &mut Io<'_>, args: &[String]) -> Res<i32> {
    let mut store = false;
    let mut stdin = false;
    let mut kind = Kind::Blob;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-w" => store = true,
            "--stdin" => stdin = true,
            "--no-filters" => {}
            "-t" => {
                i += 1;
                kind = match args.get(i).map(String::as_str) {
                    Some("blob") => Kind::Blob,
                    Some("commit") => Kind::Commit,
                    Some("tree") => Kind::Tree,
                    Some("tag") => Kind::Tag,
                    Some(other) => return Err(option("hash-object", &format!("-t {other}"))),
                    None => return Err(Error::Usage("error: switch `t' requires a value".to_string())),
                };
            }
            "--" => {
                files.extend(args[i + 1..].iter().cloned());
                break;
            }
            _ if arg.starts_with('-') => return Err(option("hash-object", arg)),
            _ => files.push(arg.to_string()),
        }
        i += 1;
    }
    let mut inputs = Vec::with_capacity(files.len() + 1);
    if stdin {
        inputs.push(git.stdin.clone());
    }
    for file in &files {
        let path = if file.starts_with('/') {
            file.as_bytes().to_vec()
        } else {
            join(&git.cwd, file.as_bytes())
        };
        match git.repo.fs.read_opt(&path)? {
            Some(data) => inputs.push(data),
            None => {
                return Err(Error::Fatal(format!(
                    "could not open '{file}' for reading: No such file or directory"
                )));
            }
        }
    }
    for data in inputs {
        let oid = if store {
            git.repo.odb.write(kind, data)?
        } else {
            object_oid(kind, &data)
        };
        writeln!(io.out, "{}", oid.hex())?;
    }
    Ok(0)
}

/// git for-each-ref: the references, sorted and formatted.
pub fn for_each_ref(git: &Git<'_>, io: &mut Io<'_>, args: &[String]) -> Res<i32> {
    let mut format = "%(objectname) %(objecttype)\t%(refname)".to_string();
    let mut sort: Option<String> = None;
    let mut count: Option<usize> = None;
    let mut patterns: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let value = |i: &mut usize, name: &str| -> Res<String> {
            match arg.strip_prefix(name).and_then(|v| v.strip_prefix('=')) {
                Some(v) => Ok(v.to_string()),
                None => {
                    *i += 1;
                    args.get(*i).cloned().ok_or_else(|| {
                        Error::Usage(format!("error: option `{}' requires a value", &name[2..]))
                    })
                }
            }
        };
        if arg == "--format" || arg.starts_with("--format=") {
            format = value(&mut i, "--format")?;
        } else if arg == "--sort" || arg.starts_with("--sort=") {
            sort = Some(value(&mut i, "--sort")?);
        } else if arg == "--count" || arg.starts_with("--count=") {
            let text = value(&mut i, "--count")?;
            count = Some(text.parse().map_err(|_| option("for-each-ref", arg))?);
        } else if arg.starts_with('-') {
            return Err(option("for-each-ref", arg));
        } else {
            patterns.push(arg.to_string());
        }
        i += 1;
    }
    let mut refs = git.repo.refs("refs/")?;
    refs.retain(|(name, _)| {
        patterns.is_empty()
            || patterns.iter().any(|p| {
                let p = p.trim_end_matches('/');
                name == p
                    || name.strip_prefix(p).is_some_and(|r| r.starts_with('/'))
                    || wildmatch(p.as_bytes(), name.as_bytes(), true)
            })
    });
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(key) = &sort {
        let (descending, key) = match key.strip_prefix('-') {
            Some(key) => (true, key),
            None => (false, key.as_str()),
        };
        match key {
            "refname" => {}
            "committerdate" | "creatordate" | "authordate" => {
                let mut dated = Vec::with_capacity(refs.len());
                for (name, oid) in refs {
                    let (peeled, kind) = git.repo.peel(&oid)?;
                    let time = if kind == Kind::Commit {
                        let commit = git.repo.commit(&peeled)?;
                        let line = if key == "authordate" {
                            &commit.author
                        } else {
                            &commit.committer
                        };
                        super::repo::Ident::parse(line).time
                    } else {
                        0
                    };
                    dated.push((time, name, oid));
                }
                dated.sort_by_key(|d| d.0);
                refs = dated.into_iter().map(|(_, name, oid)| (name, oid)).collect();
            }
            _ => return Err(option("for-each-ref", &format!("--sort={key}"))),
        }
        if descending {
            refs.reverse();
        }
    }
    if let Some(count) = count {
        refs.truncate(count);
    }
    let head = rev::symbolic(&git.repo, "HEAD")?;
    for (name, oid) in &refs {
        let line = ref_format(
            &git.repo,
            &format,
            name,
            Some(oid),
            head.as_deref() == Some(name.as_str()),
        )?;
        writeln!(io.out, "{line}")?;
    }
    Ok(0)
}

/// One file's change in a patch: its old and new paths (None for /dev/null) and
/// its hunks.
struct FilePatch {
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
    hunks: Vec<Hunk>,
}

/// A hunk: where it starts in the old file (1-based), and its lines, each with its
/// kind (' ', '-' or '+') and its text with the newline it has.
struct Hunk {
    old_start: usize,
    lines: Vec<(u8, Vec<u8>)>,
}

/// The path of a ---/+++ line, past its prefix of strip directories.
fn patch_path(text: &[u8], strip: usize) -> Option<Vec<u8>> {
    let text = text.split(|&b| b == b'\t').next().unwrap_or_default();
    let text = text.strip_suffix(b"\r").unwrap_or(text);
    if text == b"/dev/null" {
        return None;
    }
    let mut rest = text;
    for _ in 0..strip {
        match rest.iter().position(|&b| b == b'/') {
            Some(at) => rest = &rest[at + 1..],
            None => break,
        }
    }
    Some(rest.to_vec())
}

fn parse_patch(text: &[u8], strip: usize) -> Res<Vec<FilePatch>> {
    let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
    let mut files: Vec<FilePatch> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let (Some(old), Some(new)) = (
            line.strip_prefix(b"--- "),
            lines.get(i + 1).and_then(|l| l.strip_prefix(b"+++ ")),
        ) {
            let trim = |t: &[u8]| t.strip_suffix(b"\n").unwrap_or(t).to_vec();
            files.push(FilePatch {
                old: patch_path(&trim(old), strip),
                new: patch_path(&trim(new), strip),
                hunks: Vec::new(),
            });
            i += 2;
            continue;
        }
        if line.starts_with(b"@@ -") {
            let Some(file) = files.last_mut() else {
                return Err(Error::Says("error: patch fragment without header\n".to_string(), 128));
            };
            let header = String::from_utf8_lossy(line).into_owned();
            let ranges: Vec<&str> = header.split_whitespace().collect();
            let count = |range: &str| -> (usize, usize) {
                let mut parts = range[1..].splitn(2, ',');
                let start = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let len = parts.next().map_or(1, |s| s.parse().unwrap_or(0));
                (start, len)
            };
            let (old_start, mut old_left) = count(ranges.get(1).copied().unwrap_or("-0"));
            let (_, mut new_left) = count(ranges.get(2).copied().unwrap_or("+0"));
            let mut hunk = Hunk {
                old_start,
                lines: Vec::new(),
            };
            i += 1;
            while i < lines.len() && (old_left > 0 || new_left > 0) {
                let body = lines[i];
                let (kind, rest) = match body.split_first() {
                    Some((&b'\\', _)) => {
                        i += 1;
                        continue;
                    }
                    Some((&k, rest)) if k == b' ' || k == b'-' || k == b'+' => (k, rest),
                    _ if body == b"\n" => (b' ', &b"\n"[..]),
                    _ => break,
                };
                if kind != b'+' {
                    old_left = old_left.saturating_sub(1);
                }
                if kind != b'-' {
                    new_left = new_left.saturating_sub(1);
                }
                hunk.lines.push((kind, rest.to_vec()));
                i += 1;
            }
            // "\ No newline at end of file" takes the newline off the line before it.
            while i < lines.len() && lines[i].starts_with(b"\\") {
                if let Some(last) = hunk.lines.last_mut()
                    && last.1.last() == Some(&b'\n')
                {
                    last.1.pop();
                }
                i += 1;
            }
            file.hunks.push(hunk);
            continue;
        }
        i += 1;
    }
    Ok(files)
}

/// The file with the hunks applied, or the first hunk that does not fit. A hunk is
/// looked for where it says, then further and further from there.
fn apply_hunks(old: &[u8], hunks: &[Hunk]) -> Result<Vec<u8>, usize> {
    let lines: Vec<&[u8]> = old.split_inclusive(|&b| b == b'\n').collect();
    let mut out = Vec::with_capacity(old.len());
    let mut at = 0usize;
    for (n, hunk) in hunks.iter().enumerate() {
        let before: Vec<&[u8]> = hunk
            .lines
            .iter()
            .filter(|(k, _)| *k != b'+')
            .map(|(_, t)| t.as_slice())
            .collect();
        let want = hunk.old_start.saturating_sub(1).max(at);
        let fits = |start: usize| -> bool {
            start + before.len() <= lines.len()
                && before.iter().zip(&lines[start..]).all(|(a, b)| a == b)
        };
        let mut found = None;
        for distance in 0..=lines.len() {
            if want + distance <= lines.len() && fits(want + distance) {
                found = Some(want + distance);
                break;
            }
            if distance > 0 && distance <= want && want - distance >= at && fits(want - distance) {
                found = Some(want - distance);
                break;
            }
        }
        let Some(start) = found else {
            return Err(n);
        };
        for line in &lines[at..start] {
            out.extend_from_slice(line);
        }
        for (kind, text) in &hunk.lines {
            if *kind != b'-' {
                out.extend_from_slice(text);
            }
        }
        at = start + before.len();
    }
    for line in &lines[at..] {
        out.extend_from_slice(line);
    }
    Ok(out)
}

/// git apply: a unified diff (as git diff writes it) applied to the worktree, or with
/// --check only tried.
pub fn apply(git: &Git<'_>, _io: &mut Io<'_>, args: &[String]) -> Res<i32> {
    let (mut check, mut reverse, mut strip) = (false, false, 1usize);
    let mut files = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "-R" | "--reverse" => reverse = true,
            "-v" | "--verbose" | "--whitespace=nowarn" | "--whitespace=fix" => {}
            a if a.starts_with("-p") => {
                strip = a[2..].parse().map_err(|_| option("apply", a))?;
            }
            a if a.starts_with('-') && a != "-" => return Err(option("apply", a)),
            _ => files.push(arg.clone()),
        }
    }
    let mut text = Vec::new();
    if files.is_empty() || files.iter().any(|f| f == "-") {
        text.extend_from_slice(&git.stdin);
    }
    for file in files.iter().filter(|f| *f != "-") {
        let path = if file.starts_with('/') {
            file.as_bytes().to_vec()
        } else {
            join(&git.cwd, file.as_bytes())
        };
        match git.repo.fs.read_opt(&path)? {
            Some(data) => text.extend_from_slice(&data),
            None => {
                return Err(Error::Fatal(format!("can't open patch '{file}': No such file or directory")));
            }
        }
    }
    let mut patches = parse_patch(&text, strip)?;
    if patches.is_empty() {
        return Err(Error::Says("error: No valid patches in input (allow with \"--allow-empty\")\n".to_string(), 128));
    }
    if reverse {
        for patch in &mut patches {
            std::mem::swap(&mut patch.old, &mut patch.new);
            for hunk in &mut patch.hunks {
                for line in &mut hunk.lines {
                    line.0 = match line.0 {
                        b'-' => b'+',
                        b'+' => b'-',
                        k => k,
                    };
                }
            }
        }
        // A reversed hunk starts where the new file had it; the offset search finds it.
    }
    let root = git.work_tree().map(<[u8]>::to_vec).unwrap_or_else(|_| git.cwd.clone());
    let mut results: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
    for patch in &patches {
        let name = patch.new.clone().or_else(|| patch.old.clone()).unwrap_or_default();
        let shown = String::from_utf8_lossy(&name).into_owned();
        let current = match &patch.old {
            Some(old) => match git.repo.fs.read_opt(&join(&root, old))? {
                Some(data) => data,
                None => {
                    return Err(Error::Says(format!("error: {shown}: No such file or directory\n"), 1));
                }
            },
            None => {
                if git.repo.fs.read_opt(&join(&root, &name))?.is_some() {
                    return Err(Error::Says(format!("error: {shown}: already exists in working directory\n"), 1));
                }
                Vec::new()
            }
        };
        match apply_hunks(&current, &patch.hunks) {
            Ok(data) => results.push((
                name.clone(),
                patch.new.as_ref().map(|_| data),
            )),
            Err(n) => {
                let line = patch.hunks[n].old_start;
                return Err(Error::Says(
                    format!("error: patch failed: {shown}:{line}\nerror: {shown}: patch does not apply\n"),
                    1,
                ));
            }
        }
    }
    if check {
        return Ok(0);
    }
    for (path, data) in results {
        match data {
            Some(data) => git.write(join(&root, &path), data),
            None => git.op(Op::Remove(join(&root, &path))),
        }
    }
    Ok(0)
}
