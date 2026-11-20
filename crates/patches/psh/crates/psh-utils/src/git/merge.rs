//! Three-way merges: git merge of histories that diverged, cherry-pick and revert.
//! Trees merge path by path; a file both sides changed merges line by line, and what
//! does not merge is left with conflict markers in the worktree and its three versions
//! in the index.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::ops::Range;

use super::checkout::two_way;
use super::cmd::shorten;
use super::commit::{cleanup, commit_object, write_tree};
use super::diff::{DiffOpts, Printer, detect_renames, flatten, trees};
use super::index::IndexEntry;
use super::line_diff::{changed_lines, lines};
use super::patch::is_binary;
use super::repo::{Ident, Repo};
use super::rev;
use super::stage::edit;
use super::store::{Kind, Oid, join};
use super::wild::Pathspec;
use super::worktree::Work;
use super::write::Who;
use super::{Error, Git, Io, Res, option};

const MODE_FILE_MASK: u32 = 0o170000;
const MODE_FILE: u32 = 0o100000;

/// A path whose versions did not merge: its base, ours and theirs (stages 1 to 3),
/// and the worktree file with conflict markers, when there is one to write.
pub struct Conflict {
    pub path: Vec<u8>,
    pub stages: [Option<(u32, Oid)>; 3],
    pub text: Option<Vec<u8>>,
}

/// What merging three trees gives: the files (a conflicted path holds the version
/// left in the worktree), the conflicts, and the lines git prints on the way.
pub struct Merged {
    pub files: Vec<(Vec<u8>, u32, Oid)>,
    pub conflicts: Vec<Conflict>,
    pub notes: Vec<String>,
}

type Files = BTreeMap<Vec<u8>, (u32, Oid)>;

fn files_of(repo: &Repo<'_>, tree: Option<&Oid>) -> Res<Files> {
    let mut list = Vec::new();
    if let Some(tree) = tree {
        flatten(repo, tree, b"", &Pathspec::default(), &mut list)?;
    }
    Ok(list
        .into_iter()
        .map(|(path, mode, oid)| (path, (mode, oid)))
        .collect())
}

fn blob(repo: &Repo<'_>, oid: &Oid) -> Res<Vec<u8>> {
    let (kind, data) = repo.object(oid)?;
    if kind != Kind::Blob {
        return Err(Error::Fatal(format!("{} is not a blob", oid.hex())));
    }
    Ok(data.to_vec())
}

/// Merges theirs into ours, both changed from base, as git's ort strategy does for
/// what agents meet: a rename on one side is followed, directory and file conflicts are
/// left alone.
pub fn merge_trees(
    repo: &Repo<'_>,
    base: Option<&Oid>,
    ours: Option<&Oid>,
    theirs: Option<&Oid>,
    labels: (&str, &str),
) -> Res<Merged> {
    merge_trees_at(repo, base, ours, theirs, labels, 0)
}

/// [merge_trees] at a depth of git's recursion over merge bases: below the top a
/// conflict leaves no stages and no messages, only its file as the virtual base
/// takes it (conflict markers two longer per level, or the base's version).
fn merge_trees_at(
    repo: &Repo<'_>,
    base: Option<&Oid>,
    ours: Option<&Oid>,
    theirs: Option<&Oid>,
    labels: (&str, &str),
    depth: usize,
) -> Res<Merged> {
    let mut merged = merge_level(repo, base, ours, theirs, labels, depth)?;
    if depth > 0 {
        let conflicts = std::mem::take(&mut merged.conflicts);
        merged.notes.clear();
        merged
            .files
            .retain(|(path, _, _)| !conflicts.iter().any(|c| &c.path == path));
        for conflict in conflicts {
            let entry = match (conflict.text, conflict.stages) {
                (Some(text), [_, Some(o), _]) => Some((o.0, repo.odb.write(Kind::Blob, text)?)),
                (_, [base, _, _]) => base,
            };
            if let Some((mode, oid)) = entry {
                merged.files.push((conflict.path, mode, oid));
            }
        }
        merged.files.sort_by(|a, b| a.0.cmp(&b.0));
    }
    Ok(merged)
}

/// The renames on each side (base to side, as git's merge finds them) that the other
/// side did not rename: the file's base and other-side versions move to the new
/// name, so the path by path merge merges its contents there. The paths each side
/// has the moved files under, by new name.
#[allow(clippy::type_complexity)]
fn follow_renames(
    repo: &Repo<'_>,
    (base, ours, theirs): (Option<&Oid>, Option<&Oid>, Option<&Oid>),
    (b_files, o_files, t_files): (&mut Files, &mut Files, &mut Files),
) -> Res<BTreeMap<Vec<u8>, (Vec<u8>, Vec<u8>)>> {
    let mut names = BTreeMap::new();
    let enabled = repo
        .config
        .bool("merge.renames")
        .or_else(|| repo.config.bool("diff.renames"))
        != Some(false);
    let (Some(base), true) = (base, enabled) else {
        return Ok(names);
    };
    let renames = |side: Option<&Oid>| -> Res<Vec<(Vec<u8>, Vec<u8>)>> {
        let Some(side) = side else {
            return Ok(Vec::new());
        };
        let mut changes = Vec::new();
        trees(
            repo,
            Some(base),
            Some(side),
            b"",
            &Pathspec::default(),
            &mut changes,
        )?;
        Ok(detect_renames(repo, changes, 30000)?
            .into_iter()
            .filter_map(|c| Some((c.from?, c.path)))
            .collect())
    };
    let (by_ours, by_theirs) = (renames(ours)?, renames(theirs)?);
    let lacks =
        |what: &str| Error::Lacks(format!("a merge where {what}"), "git's rename conflicts");
    for (old, new) in &by_theirs {
        if let Some((_, ours_new)) = by_ours.iter().find(|(o, _)| o == old) {
            if ours_new != new {
                return Err(lacks("both sides renamed a file, to different names"));
            }
            if let Some(entry) = b_files.remove(old) {
                b_files.insert(new.clone(), entry);
            }
            continue;
        }
        if !o_files.contains_key(old) || o_files.contains_key(new) {
            return Err(lacks("one side renamed a file the other deleted or added"));
        }
        for files in [&mut *b_files, &mut *o_files] {
            if let Some(entry) = files.remove(old) {
                files.insert(new.clone(), entry);
            }
        }
        names.insert(new.clone(), (old.clone(), new.clone()));
    }
    for (old, new) in &by_ours {
        if by_theirs.iter().any(|(o, _)| o == old) {
            continue;
        }
        if !t_files.contains_key(old) || t_files.contains_key(new) {
            return Err(lacks("one side renamed a file the other deleted or added"));
        }
        for files in [&mut *b_files, &mut *t_files] {
            if let Some(entry) = files.remove(old) {
                files.insert(new.clone(), entry);
            }
        }
        names.insert(new.clone(), (new.clone(), old.clone()));
    }
    Ok(names)
}

/// The tree of a merge's files.
fn tree_of(repo: &Repo<'_>, files: &[(Vec<u8>, u32, Oid)]) -> Res<Oid> {
    let entries: Vec<IndexEntry> = files
        .iter()
        .map(|(path, mode, oid)| IndexEntry::new(path.clone(), *mode, *oid))
        .collect();
    let refs: Vec<&IndexEntry> = entries.iter().collect();
    Ok(write_tree(repo, &refs, 0, b"", None)?.oid)
}

/// The tree git's ort strategy merges against when there are several merge bases
/// (newest first): the bases merged into one, oldest first, each pair against its own
/// merge bases, recursively.
fn virtual_base(repo: &Repo<'_>, bases: &[Oid], depth: usize) -> Res<Oid> {
    let mut order = bases.to_vec();
    order.reverse();
    let mut tree = repo.commit(&order[0])?.tree;
    let mut merged_from = vec![order[0]];
    for next in &order[1..] {
        let inner = rev::merge_bases(repo, *next, &merged_from)?;
        let base = match inner.len() {
            0 => None,
            1 => Some(repo.commit(&inner[0])?.tree),
            _ => Some(virtual_base(repo, &inner, depth + 1)?),
        };
        let next_tree = repo.commit(next)?.tree;
        let merged = merge_trees_at(
            repo,
            base.as_ref(),
            Some(&tree),
            Some(&next_tree),
            ("Temporary merge branch 1", "Temporary merge branch 2"),
            depth,
        )?;
        tree = tree_of(repo, &merged.files)?;
        merged_from.push(*next);
    }
    Ok(tree)
}

fn merge_level(
    repo: &Repo<'_>,
    base: Option<&Oid>,
    ours: Option<&Oid>,
    theirs: Option<&Oid>,
    labels: (&str, &str),
    depth: usize,
) -> Res<Merged> {
    let (mut b_files, mut o_files, mut t_files) = (
        files_of(repo, base)?,
        files_of(repo, ours)?,
        files_of(repo, theirs)?,
    );
    let names = follow_renames(
        repo,
        (base, ours, theirs),
        (&mut b_files, &mut o_files, &mut t_files),
    )?;
    let paths: BTreeSet<&Vec<u8>> = b_files
        .keys()
        .chain(o_files.keys())
        .chain(t_files.keys())
        .collect();
    let mut merged = Merged {
        files: Vec::new(),
        conflicts: Vec::new(),
        notes: Vec::new(),
    };
    for path in paths {
        let (b, o, t) = (
            b_files.get(path).copied(),
            o_files.get(path).copied(),
            t_files.get(path).copied(),
        );
        let shown = String::from_utf8_lossy(path).into_owned();
        // A renamed file's conflict markers name each side's path, as git's do.
        let renamed = names.get(path).map(|(o, t)| {
            let side = |label: &str, p: &[u8]| format!("{label}:{}", String::from_utf8_lossy(p));
            (side(labels.0, o), side(labels.1, t))
        });
        let take = if o == t {
            o
        } else if b == o {
            t
        } else if b == t {
            o
        } else {
            None
        };
        if o == t || b == o || b == t {
            if let Some((mode, oid)) = take {
                merged.files.push((path.clone(), mode, oid));
            }
            continue;
        }
        match (o, t) {
            (Some(o), Some(t))
                if o.0 & MODE_FILE_MASK == MODE_FILE && t.0 & MODE_FILE_MASK == MODE_FILE =>
            {
                merged.notes.push(format!("Auto-merging {shown}"));
                let mode = if o.0 == t.0 || b.is_some_and(|b| b.0 == t.0) {
                    o.0
                } else {
                    t.0
                };
                let base_text = match b {
                    Some(b) if b.0 & MODE_FILE_MASK == MODE_FILE => blob(repo, &b.1)?,
                    _ => Vec::new(),
                };
                let (ours_text, theirs_text) = (blob(repo, &o.1)?, blob(repo, &t.1)?);
                let kind = if b.is_some() { "content" } else { "add/add" };
                if [&base_text, &ours_text, &theirs_text]
                    .iter()
                    .any(|t| is_binary(t))
                {
                    merged.notes.push(format!(
                        "warning: Cannot merge binary files: {shown} ({} vs. {})",
                        labels.0, labels.1
                    ));
                    merged
                        .notes
                        .push(format!("CONFLICT ({kind}): Merge conflict in {shown}"));
                    merged.files.push((path.clone(), o.0, o.1));
                    merged.conflicts.push(Conflict {
                        path: path.clone(),
                        stages: [b, Some(o), Some(t)],
                        text: None,
                    });
                    continue;
                }
                let (text, clean) = merge_text(
                    &base_text,
                    &ours_text,
                    &theirs_text,
                    renamed
                        .as_ref()
                        .map_or(labels, |(o, t)| (o.as_str(), t.as_str())),
                    7 + 2 * depth,
                );
                if clean {
                    let oid = repo.odb.write(Kind::Blob, text)?;
                    merged.files.push((path.clone(), mode, oid));
                } else {
                    merged
                        .notes
                        .push(format!("CONFLICT ({kind}): Merge conflict in {shown}"));
                    merged.files.push((path.clone(), o.0, o.1));
                    merged.conflicts.push(Conflict {
                        path: path.clone(),
                        stages: [b, Some(o), Some(t)],
                        text: Some(text),
                    });
                }
            }
            (Some(o), None) => {
                merged.notes.push(format!(
                    "CONFLICT (modify/delete): {shown} deleted in {} and modified in {}.  Version {} of {shown} left in tree.",
                    labels.1, labels.0, labels.0
                ));
                merged.files.push((path.clone(), o.0, o.1));
                merged.conflicts.push(Conflict {
                    path: path.clone(),
                    stages: [b, Some(o), None],
                    text: None,
                });
            }
            (None, Some(t)) => {
                merged.notes.push(format!(
                    "CONFLICT (modify/delete): {shown} deleted in {} and modified in {}.  Version {} of {shown} left in tree.",
                    labels.0, labels.1, labels.1
                ));
                merged.files.push((path.clone(), t.0, t.1));
                merged.conflicts.push(Conflict {
                    path: path.clone(),
                    stages: [b, None, Some(t)],
                    text: None,
                });
            }
            (o, t) => {
                merged
                    .notes
                    .push(format!("CONFLICT (content): Merge conflict in {shown}"));
                if let Some(o) = o {
                    merged.files.push((path.clone(), o.0, o.1));
                }
                merged.conflicts.push(Conflict {
                    path: path.clone(),
                    stages: [b, o, t],
                    text: None,
                });
            }
        }
    }
    Ok(merged)
}

/// Where a side's hunks put a stretch of the base: the side's lines for base lines
/// from..to, given what its earlier hunks added (delta).
fn side_range(hunks: &[(Range<usize>, Range<usize>)], from: usize, to: usize, delta: isize) -> Range<usize> {
    let grown: isize = hunks
        .iter()
        .map(|(b, s)| s.len() as isize - b.len() as isize)
        .sum();
    let start = (from as isize + delta) as usize;
    let end = (to as isize + delta + grown) as usize;
    start..end
}

fn push_lines(out: &mut Vec<u8>, lines: &[&[u8]]) {
    for line in lines {
        out.extend_from_slice(line);
    }
}

/// Lines with a newline after the last, as conflict markers need.
fn push_closed(out: &mut Vec<u8>, lines: &[&[u8]]) {
    push_lines(out, lines);
    if out.last().is_some_and(|&b| b != b'\n') {
        out.push(b'\n');
    }
}

/// A line by line three-way merge (git's merge style, zealous): the text, and whether
/// it merged without conflicts.
/// `marker` is the conflict markers' length: 7, longer in a virtual merge base.
fn merge_text(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    labels: (&str, &str),
    marker: usize,
) -> (Vec<u8>, bool) {
    let b: Vec<&[u8]> = lines(base).collect();
    let o: Vec<&[u8]> = lines(ours).collect();
    let t: Vec<&[u8]> = lines(theirs).collect();
    let ho = changed_lines(base, ours);
    let ht = changed_lines(base, theirs);
    let mut out = Vec::with_capacity(ours.len().max(theirs.len()));
    let mut clean = true;
    let (mut i, mut j) = (0usize, 0usize);
    let (mut delta_o, mut delta_t) = (0isize, 0isize);
    let mut copied = 0usize;
    while i < ho.len() || j < ht.len() {
        // The group of hunks that overlap or touch, from either side.
        let first_o = ho.get(i).map(|h| h.0.start);
        let first_t = ht.get(j).map(|h| h.0.start);
        let from = match (first_o, first_t) {
            (Some(a), Some(c)) => a.min(c),
            (Some(a), None) => a,
            (None, Some(c)) => c,
            (None, None) => break,
        };
        let mut to = from;
        let (start_i, start_j) = (i, j);
        loop {
            let mut grew = false;
            if let Some(h) = ho.get(i)
                && h.0.start <= to
            {
                to = to.max(h.0.end);
                i += 1;
                grew = true;
            }
            if let Some(h) = ht.get(j)
                && h.0.start <= to
            {
                to = to.max(h.0.end);
                j += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        push_lines(&mut out, &b[copied..from]);
        copied = to;
        let (group_o, group_t) = (&ho[start_i..i], &ht[start_j..j]);
        let ro = side_range(group_o, from, to, delta_o);
        let rt = side_range(group_t, from, to, delta_t);
        delta_o += group_o
            .iter()
            .map(|(b, s)| s.len() as isize - b.len() as isize)
            .sum::<isize>();
        delta_t += group_t
            .iter()
            .map(|(b, s)| s.len() as isize - b.len() as isize)
            .sum::<isize>();
        let (so, st) = (&o[ro], &t[rt]);
        if group_t.is_empty() || so == st {
            push_lines(&mut out, so);
            continue;
        }
        if group_o.is_empty() {
            push_lines(&mut out, st);
            continue;
        }
        // Lines both sides start or end with are not part of the conflict.
        let mut head = 0;
        while head < so.len() && head < st.len() && so[head] == st[head] {
            head += 1;
        }
        let mut tail = 0;
        while tail < so.len() - head
            && tail < st.len() - head
            && so[so.len() - 1 - tail] == st[st.len() - 1 - tail]
        {
            tail += 1;
        }
        clean = false;
        push_lines(&mut out, &so[..head]);
        out.extend_from_slice(format!("{} {}\n", "<".repeat(marker), labels.0).as_bytes());
        push_closed(&mut out, &so[head..so.len() - tail]);
        out.extend_from_slice(format!("{}\n", "=".repeat(marker)).as_bytes());
        push_closed(&mut out, &st[head..st.len() - tail]);
        out.extend_from_slice(format!("{} {}\n", ">".repeat(marker), labels.1).as_bytes());
        push_lines(&mut out, &so[so.len() - tail..]);
    }
    push_lines(&mut out, &b[copied.min(b.len())..]);
    (out, clean)
}

/// Puts a merge's result in the index and the worktree, from ours: the merged tree's
/// files where nothing local is in the way, then each conflict's stages and markers.
/// The merged tree, or git's message when local changes would be lost.
fn apply(
    git: &Git<'_>,
    ours: &Oid,
    merged: &Merged,
) -> Res<Result<Oid, String>> {
    let repo = &git.repo;
    let root = git.work_tree()?.to_vec();
    let tree = tree_of(repo, &merged.files)?;
    let work = Work::new(repo, &root)?;
    let mut index = edit(&work)?;
    if let Some(refusal) = two_way(git, &work, &mut index, Some(ours), Some(&tree), true)? {
        return Ok(Err(refusal));
    }
    for conflict in &merged.conflicts {
        index.set_unmerged(&conflict.path, &conflict.stages);
        if let Some(text) = &conflict.text {
            git.write(join(&root, &conflict.path), text.clone());
        }
    }
    git.set_index(index);
    Ok(Ok(tree))
}

/// A name for what is merged, as git's message gives it.
fn merge_name(repo: &Repo<'_>, spec: &str) -> Res<String> {
    for (prefix, what) in [
        ("refs/heads/", "branch"),
        ("refs/tags/", "tag"),
        ("refs/remotes/", "remote-tracking branch"),
    ] {
        if repo.read_ref(&format!("{prefix}{spec}"))?.is_some() {
            return Ok(format!("{what} '{spec}'"));
        }
    }
    Ok(format!("commit '{spec}'"))
}

/// git merge of histories that diverged (or --no-ff): a merge commit, or the
/// conflicts left to resolve.
pub struct Options<'o> {
    pub spec: &'o str,
    pub message: Option<String>,
    pub quiet: bool,
    pub stat: bool,
    pub commit: bool,
}

pub fn three_way(git: &Git<'_>, io: &mut Io<'_>, head: Oid, theirs: Oid, opts: Options<'_>) -> Res<i32> {
    let repo = &git.repo;
    let bases = rev::merge_bases(repo, head, &[theirs])?;
    let base_tree = match bases.as_slice() {
        [] => {
            return Err(Error::Fatal("refusing to merge unrelated histories".to_string()));
        }
        [base] => Some(repo.commit(base)?.tree),
        _ => Some(virtual_base(repo, &bases, 1)?),
    };
    let ours = repo.commit(&head)?.tree;
    let their_tree = repo.commit(&theirs)?.tree;
    git.lock_index()?;
    git.hooks(&["post-merge"])?;
    let merged = merge_trees(repo, base_tree.as_ref(), Some(&ours), Some(&their_tree), ("HEAD", opts.spec))?;
    let tree = match apply(git, &ours, &merged)? {
        Ok(tree) => tree,
        Err(refusal) => return Err(Error::Says(format!("{refusal}Merge with strategy ort failed.\n"), 2)),
    };
    git.write(join(&repo.git_dir, b"ORIG_HEAD"), format!("{}\n", head.hex()).into_bytes());
    let mut message = match &opts.message {
        Some(message) => message.clone(),
        None => {
            let mut text = format!("Merge {}", merge_name(repo, opts.spec)?);
            if let Some(branch) = repo.head_ref()? {
                let short = shorten(&branch);
                let default = repo.config.get("init.defaultbranch").unwrap_or("master");
                if short != "master" && short != "main" && short != default {
                    text.push_str(&format!(" into {short}"));
                }
            }
            text
        }
    };
    if !message.ends_with('\n') {
        message.push('\n');
    }
    // git's -q leaves the merge's own messages, as Auto-merging.
    for note in &merged.notes {
        writeln!(io.out, "{note}")?;
    }
    if !merged.conflicts.is_empty() || !opts.commit {
        git.write(join(&repo.git_dir, b"MERGE_HEAD"), format!("{}\n", theirs.hex()).into_bytes());
        git.write(join(&repo.git_dir, b"MERGE_MODE"), Vec::new());
        let mut text = message.clone();
        if !merged.conflicts.is_empty() {
            text.push_str("\n# Conflicts:\n");
            for conflict in &merged.conflicts {
                text.push_str(&format!("#\t{}\n", String::from_utf8_lossy(&conflict.path)));
            }
        }
        git.write(join(&repo.git_dir, b"MERGE_MSG"), text.into_bytes());
        if merged.conflicts.is_empty() {
            if !opts.quiet {
                writeln!(io.out, "Automatic merge went well; stopped before committing as requested")?;
            }
            return Ok(0);
        }
        writeln!(io.out, "Automatic merge failed; fix conflicts and then commit the result.")?;
        return Ok(1);
    }
    let author = git.ident(Who::Author, true)?;
    let committer = git.ident(Who::Committer, true)?;
    let oid = commit_object(repo, &tree, &[head, theirs], &author, &committer, &cleanup(&message))?;
    git.move_head(oid, &format!("merge {}: Merge made by the 'ort' strategy.", opts.spec))?;
    if opts.quiet {
        return Ok(0);
    }
    writeln!(io.out, "Merge made by the 'ort' strategy.")?;
    if opts.stat {
        print_stat(git, io, &ours, &tree)?;
    }
    Ok(0)
}

fn print_stat(git: &Git<'_>, io: &mut Io<'_>, old: &Oid, new: &Oid) -> Res<()> {
    let repo = &git.repo;
    let mut changes = Vec::new();
    trees(repo, Some(old), Some(new), b"", &Pathspec::default(), &mut changes)?;
    let mut opts = DiffOpts::new(repo, git.columns)?;
    if let Some(minimum) = opts.renames {
        changes = detect_renames(repo, changes, minimum)?;
    }
    opts.stat = true;
    opts.summary = true;
    Printer { repo, opts: &opts }.print(io.out, &changes)?;
    Ok(())
}

/// git cherry-pick and git revert of one commit each: its change (or the change
/// undone) merged onto HEAD and committed.
pub fn pick(git: &Git<'_>, io: &mut Io<'_>, args: &[String], revert: bool) -> Res<i32> {
    let name = if revert { "revert" } else { "cherry-pick" };
    let mut specs = Vec::new();
    let mut record = false;
    let mut commit = true;
    for arg in args {
        match arg.as_str() {
            "--no-edit" | "--ff" | "--allow-empty" => {}
            "-x" => record = true,
            "-n" | "--no-commit" => commit = false,
            "-e" | "--edit" => {
                return Err(Error::Lacks(
                    format!("'{name} --edit' opens an editor"),
                    "editor: leave out --edit",
                ));
            }
            a if a.starts_with('-') => return Err(option(name, a)),
            _ => specs.push(arg.clone()),
        }
    }
    let [spec] = specs.as_slice() else {
        return Err(option(name, "several commits, or none"));
    };
    let repo = &git.repo;
    super::status::in_progress(git)?;
    let target = match rev::resolve(repo, spec, &git.prefix)? {
        Some(oid) => rev::commit_of(repo, oid)?,
        None => return Err(Error::Fatal(format!("bad revision '{spec}'"))),
    };
    let Some(head) = repo.head()? else {
        return Err(Error::Lacks(
            format!("'{name}' onto a branch with no commits"),
            "git that picks onto an unborn branch",
        ));
    };
    let head = rev::commit_of(repo, head)?;
    let picked = repo.commit(&target)?;
    if picked.parents.len() > 1 {
        return Err(Error::Says(
            format!(
                "error: commit {} is a merge but no -m option was given.\nfatal: {name} failed\n",
                target.hex()
            ),
            128,
        ));
    }
    let parent_tree = match picked.parents.first() {
        Some(parent) => Some(repo.commit(parent)?.tree),
        None => None,
    };
    let short = repo.short(&target)?;
    let subject = String::from_utf8_lossy(&super::pretty::subject(picked.message())).into_owned();
    let label = if revert {
        format!("parent of {short} ({subject})")
    } else {
        format!("{short} ({subject})")
    };
    let (base, theirs) = if revert {
        (Some(picked.tree), parent_tree)
    } else {
        (parent_tree, Some(picked.tree))
    };
    let ours = repo.commit(&head)?.tree;
    git.lock_index()?;
    let merged = merge_trees(repo, base.as_ref(), Some(&ours), theirs.as_ref(), ("HEAD", &label))?;
    let tree = match apply(git, &ours, &merged)? {
        Ok(tree) => tree,
        Err(refusal) => {
            return Err(Error::Says(format!("{refusal}fatal: {name} failed\n"), 128));
        }
    };
    let mut message = if revert {
        format!(
            "Revert \"{subject}\"\n\nThis reverts commit {}.\n",
            target.hex()
        )
    } else {
        String::from_utf8_lossy(picked.message()).into_owned()
    };
    if record && !revert {
        message = format!("{}\n\n(cherry picked from commit {})\n", message.trim_end(), target.hex());
    }
    for note in &merged.notes {
        writeln!(io.out, "{note}")?;
    }
    if !merged.conflicts.is_empty() {
        let head_file: &[u8] = if revert { b"REVERT_HEAD" } else { b"CHERRY_PICK_HEAD" };
        git.write(join(&repo.git_dir, head_file), format!("{}\n", target.hex()).into_bytes());
        let mut text = message.clone();
        text.push_str("\n# Conflicts:\n");
        for conflict in &merged.conflicts {
            text.push_str(&format!("#\t{}\n", String::from_utf8_lossy(&conflict.path)));
        }
        git.write(join(&repo.git_dir, b"MERGE_MSG"), text.into_bytes());
        let what = if revert { "revert" } else { "apply" };
        let hint = if revert {
            "hint: After resolving the conflicts, mark them with\nhint: \"git add/rm <pathspec>\", then run\nhint: \"git revert --continue\".\nhint: You can instead skip this commit with \"git revert --skip\".\nhint: To abort and get back to the state before \"git revert\",\nhint: run \"git revert --abort\".\n"
        } else {
            "hint: After resolving the conflicts, mark them with\nhint: \"git add/rm <pathspec>\", then run\nhint: \"git cherry-pick --continue\".\nhint: You can instead skip this commit with \"git cherry-pick --skip\".\nhint: To abort and get back to the state before \"git cherry-pick\",\nhint: run \"git cherry-pick --abort\".\n"
        };
        // Not an error return: what the pick wrote to the worktree and index stays.
        io.warn(&format!("error: could not {what} {short}... {subject}\n{hint}"));
        return Ok(1);
    }
    if !commit {
        return Ok(0);
    }
    let author = if revert {
        git.ident(Who::Author, true)?
    } else {
        String::from_utf8_lossy(&picked.author).into_owned()
    };
    let committer = git.ident(Who::Committer, true)?;
    let message = cleanup(&message);
    let oid = commit_object(repo, &tree, &[head], &author, &committer, &message)?;
    let first = message.lines().next().unwrap_or_default();
    git.move_head(oid, &format!("{name}: {first}"))?;
    let branch = match repo.head_ref()? {
        Some(name) => shorten(&name).to_string(),
        None => "detached HEAD".to_string(),
    };
    writeln!(io.out, "[{branch} {}] {first}", repo.short(&oid)?)?;
    let (a, c) = (Ident::parse(author.as_bytes()), Ident::parse(committer.as_bytes()));
    if (a.name, a.email) != (c.name, c.email) {
        io.out.write_all(b" Author: ")?;
        io.out.write_all(a.name)?;
        io.out.write_all(b" <")?;
        io.out.write_all(a.email)?;
        io.out.write_all(b">\n")?;
    }
    writeln!(io.out, " Date: {}", super::pretty::date_default(a.time, a.zone))?;
    let mut changes = Vec::new();
    trees(repo, Some(&ours), Some(&tree), b"", &Pathspec::default(), &mut changes)?;
    super::diff::commit_summary(repo, io.out, changes)?;
    Ok(0)
}
