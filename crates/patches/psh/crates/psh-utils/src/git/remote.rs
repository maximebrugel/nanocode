use std::collections::HashSet;
use std::io::{self, BufWriter};
use std::sync::mpsc;

use psh_engine::util::{BlockingOptions, LocalBoxFuture, SyncCtx, UtilCtx};

use super::index::{Index, IndexEntry};
use super::pack::{self, Indexed};
use super::pkt::{self, Pkt, Reader};
use super::repo::{self, Repo, parse_tree, tag_target};
use super::store::{Data, Fs, Kind, Oid, corrupt, join};
use super::write::{self, IndexOut, Mode, Op, Plan, valid_ref};
use super::{Error, Io, Res, VERSION};

pub struct Call {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub trait Remote {
    fn call(&self, call: Call) -> LocalBoxFuture<'static, Result<Answer, String>>;
}

const ZERO: Oid = Oid([0; 20]);
const ABBREV: usize = 7;
const SUMMARY_WIDTH: usize = 2 * ABBREV + 3;
const GIT_PACK_WINDOW_DEFAULT: usize = 10;
const GIT_PACK_DEPTH_DEFAULT: usize = 50;
const UPLOAD: &str = "git-upload-pack";
const RECEIVE: &str = "git-receive-pack";
const COMMANDS: [&str; 5] = ["clone", "fetch", "pull", "push", "ls-remote"];
const HTTP_OK: u16 = 200;
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_FORBIDDEN: u16 = 403;
const HTTP_NOT_FOUND: u16 = 404;
const MODE_TREE: u32 = 0o40000;
const MODE_EXEC: u32 = 0o100755;
const MODE_LINK: u32 = 0o120000;
const MODE_GITLINK: u32 = 0o160000;
const LACKS: &str = "git that reaches repositories other than over HTTP";
const MERGES: &str = "git that merges histories: it only moves a branch forward";
const INFINITE_DEPTH: u32 = 0x7fff_ffff;

struct Invocation {
    cwd: Vec<u8>,
    overrides: Vec<(String, String)>,
    globals: Vec<Vec<u8>>,
    command: String,
    args: Vec<String>,
}

fn invocation(ctx: &UtilCtx<'_>, argv: &[Vec<u8>]) -> Option<Invocation> {
    let args: Vec<String> = argv
        .get(1..)?
        .iter()
        .map(|arg| String::from_utf8(arg.clone()).ok())
        .collect::<Option<_>>()?;
    let mut cwd = ctx.cwd().to_vec();
    let mut overrides = Vec::new();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "-C" => {
                let dir = args.get(i + 1)?;
                cwd = if dir.starts_with('/') {
                    dir.clone().into_bytes()
                } else {
                    join(&cwd, dir.as_bytes())
                };
                i += 2;
            }
            "-c" => {
                let pair = args.get(i + 1)?;
                let (key, value) = pair.split_once('=').unwrap_or((pair, "true"));
                overrides.push((super::config_key(key), value.to_string()));
                i += 2;
            }
            "--no-pager" | "-P" => i += 1,
            _ => break,
        }
    }
    let command = args.get(i)?;
    if !COMMANDS.contains(&command.as_str()) || args[i + 1..].iter().any(|a| a == "--help") {
        return None;
    }
    Some(Invocation {
        cwd: repo::normalize(&cwd),
        overrides,
        globals: argv[1..=i].to_vec(),
        command: command.clone(),
        args: args[i + 1..].to_vec(),
    })
}

pub async fn run_with(ctx: &mut UtilCtx<'_>, argv: &[Vec<u8>], remote: Option<&dyn Remote>) -> i32 {
    let (Some(remote), Some(inv)) = (remote, invocation(ctx, argv)) else {
        return super::run(ctx, argv).await;
    };
    let result = match inv.command.as_str() {
        "clone" => clone(ctx, remote, &inv).await,
        "fetch" => fetch_command(ctx, remote, &inv).await,
        "pull" => pull(ctx, remote, &inv).await,
        "push" => push(ctx, remote, &inv).await,
        _ => ls_remote(ctx, remote, &inv).await,
    };
    match result {
        Ok(status) => status,
        Err(e) => report(ctx, e).await,
    }
}

async fn report(ctx: &mut UtilCtx<'_>, e: Error) -> i32 {
    let (text, status) = match e {
        Error::Io(e) => (
            format!("fatal: {}\n", psh_engine::host::error_text(&e)),
            128,
        ),
        Error::Fatal(msg) => (format!("fatal: {msg}\n"), 128),
        Error::Usage(msg) => (format!("{msg}\n"), 129),
        Error::Lacks(what, lacks) => (
            format!("git: {what} (this simulated shell has no {lacks})\n"),
            127,
        ),
        Error::Exit(status) => (String::new(), status),
        Error::Says(text, status) => (text, status),
    };
    ctx.error_raw(text.as_bytes()).await;
    status
}

async fn say(ctx: &mut UtilCtx<'_>, text: &str) {
    ctx.error_raw(text.as_bytes()).await;
}

pub fn clean(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

fn base(url: &str) -> &str {
    url.trim_end_matches('/')
}

fn http(command: &str, url: &str) -> Res<()> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok(());
    }
    Err(Error::Lacks(
        format!("'{command}' of '{}'", clean(url)),
        LACKS,
    ))
}

fn agent() -> String {
    format!("git/{VERSION}")
}

fn header<'a>(answer: &'a Answer, name: &str) -> Option<&'a str> {
    answer
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

async fn exchange(remote: &dyn Remote, url: &str, call: Call) -> Res<Answer> {
    let shown = clean(url);
    let shown = base(&shown);
    let answer = remote
        .call(call)
        .await
        .map_err(|why| Error::Fatal(format!("unable to access '{shown}/': {why}")))?;
    match answer.status {
        HTTP_OK => Ok(answer),
        HTTP_UNAUTHORIZED | HTTP_FORBIDDEN => Err(Error::Fatal(format!(
            "Authentication failed for '{shown}/'"
        ))),
        HTTP_NOT_FOUND => Err(Error::Fatal(format!("repository '{shown}/' not found"))),
        code => Err(Error::Fatal(format!(
            "unable to access '{shown}/': The requested URL returned error: {code}"
        ))),
    }
}

struct Advert {
    refs: Vec<(String, Oid)>,
    caps: Vec<String>,
    head: Option<String>,
}

impl Advert {
    fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }

    fn get(&self, name: &str) -> Option<Oid> {
        self.refs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, oid)| *oid)
    }
}

fn parse_advert(body: &[u8], service: &str) -> io::Result<Advert> {
    let bad = || corrupt("the remote sent a malformed ref advertisement");
    let mut reader = Reader::new(body);
    if reader.text()? != Some(format!("# service={service}").as_bytes()) {
        return Err(bad());
    }
    if !matches!(reader.next()?, Pkt::Flush) {
        return Err(bad());
    }
    let mut refs = Vec::new();
    let mut caps = Vec::new();
    let mut first = true;
    while let Some(line) = reader.text()? {
        let line = if first {
            first = false;
            match line.iter().position(|&b| b == 0) {
                Some(nul) => {
                    caps = String::from_utf8_lossy(&line[nul + 1..])
                        .split(' ')
                        .filter(|c| !c.is_empty())
                        .map(str::to_string)
                        .collect();
                    &line[..nul]
                }
                None => line,
            }
        } else {
            line
        };
        let space = line.iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let oid = Oid::from_hex(&line[..space]).ok_or_else(bad)?;
        let name = String::from_utf8(line[space + 1..].to_vec()).map_err(|_| bad())?;
        if name != "capabilities^{}" {
            refs.push((name, oid));
        }
    }
    let head = caps
        .iter()
        .find_map(|c| c.strip_prefix("symref=HEAD:"))
        .map(str::to_string);
    Ok(Advert { refs, caps, head })
}

async fn advertised(remote: &dyn Remote, url: &str, service: &str) -> Res<Advert> {
    let call = Call {
        method: "GET",
        url: format!("{}/info/refs?service={service}", base(url)),
        headers: vec![("User-Agent".to_string(), agent())],
        body: Vec::new(),
    };
    let answer = exchange(remote, url, call).await?;
    let expected = format!("application/x-{service}-advertisement");
    if header(&answer, "content-type") != Some(expected.as_str()) {
        return Err(Error::Fatal(format!(
            "{}/info/refs not valid: is this a git repository?",
            base(&clean(url))
        )));
    }
    parse_advert(&answer.body, service).map_err(Error::Io)
}

struct Want<'a> {
    wants: &'a [Oid],
    haves: &'a [Oid],
    shallow: &'a [Oid],
    depth: Option<u32>,
}

struct Uploaded {
    pack: Vec<u8>,
    shallow: Vec<Oid>,
    unshallow: Vec<Oid>,
}

async fn upload(remote: &dyn Remote, url: &str, advert: &Advert, want: &Want<'_>) -> Res<Uploaded> {
    if (want.depth.is_some() || !want.shallow.is_empty()) && !advert.has("shallow") {
        return Err(Error::Fatal(
            "Server does not support shallow clients".to_string(),
        ));
    }
    let mut caps = Vec::new();
    for cap in ["ofs-delta", "include-tag"] {
        if advert.has(cap) {
            caps.push(cap.to_string());
        }
    }
    if advert.caps.iter().any(|c| c.starts_with("agent=")) {
        caps.push(format!("agent={}", agent()));
    }
    let mut body = Vec::new();
    for (i, oid) in want.wants.iter().enumerate() {
        let line = if i == 0 && !caps.is_empty() {
            format!("want {} {}\n", oid.hex(), caps.join(" "))
        } else {
            format!("want {}\n", oid.hex())
        };
        pkt::line(&mut body, line.as_bytes());
    }
    for oid in want.shallow {
        pkt::line(&mut body, format!("shallow {}\n", oid.hex()).as_bytes());
    }
    if let Some(depth) = want.depth {
        pkt::line(&mut body, format!("deepen {depth}\n").as_bytes());
    }
    pkt::flush(&mut body);
    for have in want.haves {
        pkt::line(&mut body, format!("have {}\n", have.hex()).as_bytes());
    }
    pkt::line(&mut body, b"done\n");
    let call = Call {
        method: "POST",
        url: format!("{}/{UPLOAD}", base(url)),
        headers: vec![
            ("User-Agent".to_string(), agent()),
            (
                "Content-Type".to_string(),
                format!("application/x-{UPLOAD}-request"),
            ),
            (
                "Accept".to_string(),
                format!("application/x-{UPLOAD}-result"),
            ),
        ],
        body,
    };
    let answer = exchange(remote, url, call).await?;
    let mut reader = Reader::new(&answer.body);
    let mut shallow = Vec::new();
    let mut unshallow = Vec::new();
    let bad = || Error::Io(corrupt("the remote sent a malformed shallow line"));
    loop {
        match reader.next()? {
            Pkt::Data(line) => {
                let line = line.strip_suffix(b"\n").unwrap_or(line);
                if let Some(hex) = line.strip_prefix(b"shallow ") {
                    shallow.push(Oid::from_hex(hex).ok_or_else(bad)?);
                    continue;
                }
                if let Some(hex) = line.strip_prefix(b"unshallow ") {
                    unshallow.push(Oid::from_hex(hex).ok_or_else(bad)?);
                    continue;
                }
                if let Some(why) = line.strip_prefix(b"ERR ") {
                    return Err(Error::Fatal(format!(
                        "remote error: {}",
                        String::from_utf8_lossy(why)
                    )));
                }
                let more = [&b" continue"[..], b" common", b" ready"]
                    .iter()
                    .any(|tail| line.ends_with(tail));
                if line == b"NAK" || (line.starts_with(b"ACK ") && !more) {
                    break;
                }
            }
            Pkt::Flush | Pkt::Delim => {}
            Pkt::End => return Err(Error::Io(corrupt("the remote sent no pack"))),
        }
    }
    Ok(Uploaded {
        pack: reader.rest().to_vec(),
        shallow,
        unshallow,
    })
}

fn parents_of(body: &[u8]) -> Vec<Oid> {
    body.split(|&b| b == b'\n')
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix(b"parent "))
        .filter_map(Oid::from_hex)
        .collect()
}

fn missing(pack: &[u8], indexed: &Indexed, tips: &[Oid], shallow: &[Oid]) -> Res<Vec<Oid>> {
    let mut cut = HashSet::new();
    for oid in shallow {
        if let Some((Kind::Commit, body)) = indexed.read(pack, oid)? {
            cut.extend(parents_of(&body));
        }
    }
    Ok(indexed
        .outside(tips)
        .into_iter()
        .filter(|oid| !cut.contains(oid))
        .collect())
}

fn pack_count(pack: &[u8]) -> usize {
    pack.get(8..12)
        .map_or(0, |n| u32::from_be_bytes([n[0], n[1], n[2], n[3]]) as usize)
}

fn shallow_op(common: &[u8], shallow: &[Oid]) -> Op {
    let path = join(common, b"shallow");
    if shallow.is_empty() {
        return Op::Remove(path);
    }
    let text: String = shallow
        .iter()
        .map(|oid| format!("{}\n", oid.hex()))
        .collect();
    Op::Write {
        path,
        data: text.into_bytes(),
        mode: Mode::Plain,
    }
}

fn without_packed(text: &[u8], names: &[String]) -> Option<Vec<u8>> {
    let mut kept = Vec::with_capacity(text.len());
    let (mut dropped, mut found) = (false, false);
    for line in text.split_inclusive(|&b| b == b'\n') {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        if body.first() == Some(&b'^') {
            if !dropped {
                kept.extend_from_slice(line);
            }
            continue;
        }
        dropped = body.len() > 41 && names.iter().any(|n| &body[41..] == n.as_bytes());
        found |= dropped;
        if !dropped {
            kept.extend_from_slice(line);
        }
    }
    found.then_some(kept)
}

fn delete_ops(local: &Local, names: &[String]) -> Vec<Op> {
    let mut ops = Vec::new();
    for name in names {
        ops.push(ref_op(&local.common, name, None));
        ops.push(Op::Remove(join(
            &local.common,
            format!("logs/{name}").as_bytes(),
        )));
        let mut dir = name.as_str();
        while let Some((up, _)) = dir.rsplit_once('/') {
            if up.matches('/').count() < 3 {
                break;
            }
            ops.push(Op::Rmdir(join(&local.common, up.as_bytes())));
            ops.push(Op::Rmdir(join(
                &local.common,
                format!("logs/{up}").as_bytes(),
            )));
            dir = up;
        }
    }
    if let Some(kept) = local
        .packed
        .as_deref()
        .and_then(|text| without_packed(text, names))
    {
        ops.push(Op::Write {
            path: join(&local.common, b"packed-refs"),
            data: kept,
            mode: Mode::Plain,
        });
    }
    ops
}

fn incomplete() -> Error {
    Error::Fatal("remote did not send all necessary objects".to_string())
}

/// Indexes a received pack; `keep` keeps its objects for a caller that reads them all.
fn received(pack: &[u8], wants: &[Oid], keep: bool) -> Res<Indexed> {
    let index = if keep { pack::index_keeping } else { pack::index };
    let indexed = index(pack).map_err(|e| {
        Error::Fatal(format!(
            "index-pack failed: {}",
            psh_engine::host::error_text(&e)
        ))
    })?;
    if wants.iter().any(|want| indexed.kind(want).is_none()) {
        return Err(incomplete());
    }
    Ok(indexed)
}

fn pack_ops(objects: &[u8], pack: Vec<u8>, indexed: &Indexed) -> Vec<Op> {
    let dir = join(objects, b"pack");
    let name = format!("pack-{}", indexed.checksum.hex());
    let path = |prefix: &str, ext: &str| join(&dir, format!("{prefix}{name}.{ext}").as_bytes());
    vec![
        Op::Write {
            path: path("tmp_", "pack"),
            data: pack,
            mode: Mode::Plain,
        },
        Op::Write {
            path: path("tmp_", "idx"),
            data: indexed.idx(),
            mode: Mode::Plain,
        },
        Op::Rename {
            from: path("tmp_", "pack"),
            to: path("", "pack"),
        },
        Op::Rename {
            from: path("tmp_", "idx"),
            to: path("", "idx"),
        },
    ]
}

fn safe_ref(name: &str) -> bool {
    name.starts_with("refs/") && valid_ref(name)
}

fn ref_op(common: &[u8], name: &str, oid: Option<Oid>) -> Op {
    let path = join(common, name.as_bytes());
    match oid {
        Some(oid) => Op::Write {
            path,
            data: format!("{}\n", oid.hex()).into_bytes(),
            mode: Mode::Plain,
        },
        None => Op::Remove(path),
    }
}

struct Local {
    common: Vec<u8>,
    git_dir: Vec<u8>,
    config: Vec<(String, String)>,
    refs: Vec<(String, Oid)>,
    head: Option<String>,
    head_oid: Option<Oid>,
    present: Vec<bool>,
    shallow: Vec<Oid>,
    packed: Option<Vec<u8>>,
    symbolic: Vec<String>,
    work_tree: Option<Vec<u8>>,
}

impl Local {
    fn get(&self, key: &str) -> Option<&str> {
        self.config
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, key: &str) -> Vec<&str> {
        self.config
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn get_ref(&self, name: &str) -> Option<Oid> {
        self.refs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, oid)| *oid)
    }

    fn branch(&self) -> Option<&str> {
        self.head.as_deref()?.strip_prefix("refs/heads/")
    }
}

enum Job {
    Clone {
        cwd: Vec<u8>,
        dir: String,
    },
    Local {
        cwd: Vec<u8>,
        overrides: Vec<(String, String)>,
        ask: Vec<Oid>,
    },
    Objects {
        cwd: Vec<u8>,
        overrides: Vec<(String, String)>,
        tips: Vec<Oid>,
        known: Vec<Oid>,
        checks: Vec<(Oid, Oid)>,
        ofs_delta: bool,
    },
}

enum Found {
    Clone {
        plan: Plan,
        top: Vec<u8>,
        git_dir: Vec<u8>,
    },
    Local(Local),
    Objects {
        pack: Vec<u8>,
        checks: Vec<(bool, bool)>,
    },
}

type Sent = mpsc::Sender<Res<Found>>;

async fn ask(ctx: &mut UtilCtx<'_>, job: Job) -> Res<Found> {
    let (sent, got) = mpsc::channel();
    let status = ctx
        .run_blocking(BlockingOptions { stdin: false }, (job, sent), probe)
        .await;
    got.try_iter().last().unwrap_or(Err(Error::Exit(status)))
}

async fn local(ctx: &mut UtilCtx<'_>, inv: &Invocation, ask_for: Vec<Oid>) -> Res<Local> {
    let job = Job::Local {
        cwd: inv.cwd.clone(),
        overrides: inv.overrides.clone(),
        ask: ask_for,
    };
    match ask(ctx, job).await? {
        Found::Local(local) => Ok(local),
        Found::Clone { .. } | Found::Objects { .. } => Err(Error::Exit(128)),
    }
}

fn probe(ctx: &mut SyncCtx, (job, sent): (Job, Sent)) -> i32 {
    let mut out = BufWriter::new(ctx.writer(1));
    let err = ctx.writer(2);
    let fs = Fs::new(ctx);
    let env = |name: &[u8]| ctx.var(name).map(<[u8]>::to_vec);
    let found = match job {
        Job::Clone { cwd, dir } => {
            let mut io = Io { out: &mut out, err };
            clone_probe(&fs, &cwd, &dir, &env, &mut io)
        }
        Job::Local {
            cwd,
            overrides,
            ask,
        } => local_probe(&fs, &cwd, &env, &overrides, &ask),
        Job::Objects {
            cwd,
            overrides,
            tips,
            known,
            checks,
            ofs_delta,
        } => Repo::discover(&fs, &cwd, &env, &overrides)
            .and_then(|repo| objects(&repo, &tips, &known, &checks, ofs_delta)),
    };
    if !matches!(&found, Err(Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock) {
        let _ = sent.send(found);
    }
    0
}

fn clone_probe(
    fs: &Fs<'_>,
    cwd: &[u8],
    dir: &str,
    env: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
    io: &mut Io<'_>,
) -> Res<Found> {
    let top = repo::normalize(&if dir.starts_with('/') {
        dir.as_bytes().to_vec()
    } else {
        join(cwd, dir.as_bytes())
    });
    if fs.stat_opt(&top, true)?.is_some() && !fs.list(&top).is_ok_and(|names| names.is_empty()) {
        return Err(Error::Fatal(format!(
            "destination path '{dir}' already exists and is not an empty directory."
        )));
    }
    let shown = String::from_utf8_lossy(&top).into_owned();
    let mut plan = Plan::default();
    super::setup::init(fs, cwd, env, &["-q".to_string(), shown], io, &mut plan)?;
    let git_dir = join(&top, b".git");
    Ok(Found::Clone { plan, top, git_dir })
}

fn local_probe(
    fs: &Fs<'_>,
    cwd: &[u8],
    env: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
    overrides: &[(String, String)],
    ask: &[Oid],
) -> Res<Found> {
    let repo = Repo::discover(fs, cwd, env, overrides)?;
    let present = ask
        .iter()
        .map(|oid| repo.odb.has(oid))
        .collect::<io::Result<_>>()?;
    let mut shallow: Vec<Oid> = fs
        .read_opt(&join(&repo.common_dir, b"shallow"))?
        .unwrap_or_default()
        .split(|&b| b == b'\n')
        .filter_map(Oid::from_hex)
        .collect();
    crate::order::sort(&mut shallow);
    let refs = repo.refs("refs/")?;
    let mut symbolic = Vec::new();
    for (name, _) in &refs {
        if matches!(repo.read_ref(name)?, Some(repo::RefValue::Symbolic(_))) {
            symbolic.push(name.clone());
        }
    }
    Ok(Found::Local(Local {
        common: repo.common_dir.clone(),
        git_dir: repo.git_dir.clone(),
        config: repo.config.entries.clone(),
        refs,
        head: repo.head_ref()?,
        head_oid: repo.head()?,
        present,
        shallow,
        packed: fs.read_opt(&join(&repo.common_dir, b"packed-refs"))?,
        symbolic,
        work_tree: repo.work_tree.clone(),
    }))
}

fn reach(
    repo: &Repo<'_>,
    root: Oid,
    skip: &HashSet<Oid>,
    seen: &mut HashSet<Oid>,
    out: Option<&mut Vec<(Kind, Data, u32)>>,
) -> Res<()> {
    let mut out = out;
    let mut stack = vec![(root, 0)];
    while let Some((tree, name)) = stack.pop() {
        if skip.contains(&tree) || !seen.insert(tree) {
            continue;
        }
        let (kind, data) = repo.object(&tree)?;
        for entry in parse_tree(&data)? {
            let hash = pack::name_hash(&entry.name);
            match entry.mode {
                MODE_TREE => stack.push((entry.oid, hash)),
                MODE_GITLINK => {}
                _ if skip.contains(&entry.oid) || !seen.insert(entry.oid) => {}
                _ => {
                    if let Some(out) = out.as_deref_mut() {
                        let (kind, data) = repo.object(&entry.oid)?;
                        out.push((kind, data, hash));
                    }
                }
            }
        }
        if let Some(out) = out.as_deref_mut() {
            out.push((kind, data, name));
        }
    }
    Ok(())
}

fn deltas(config: &repo::Config, ofs_delta: bool) -> Res<pack::Deltas> {
    let number = |key: &str, default: usize| match config.get(key) {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| Error::Fatal(format!("bad numeric config value '{text}' for '{key}'"))),
    };
    if !ofs_delta {
        return Ok(pack::Deltas {
            window: 0,
            depth: 0,
        });
    }
    Ok(pack::Deltas {
        window: number("pack.window", GIT_PACK_WINDOW_DEFAULT)?,
        depth: number("pack.depth", GIT_PACK_DEPTH_DEFAULT)?,
    })
}

fn objects(
    repo: &Repo<'_>,
    tips: &[Oid],
    known: &[Oid],
    checks: &[(Oid, Oid)],
    ofs_delta: bool,
) -> Res<Found> {
    let deltas = deltas(&repo.config, ofs_delta)?;
    let mut results = Vec::new();
    for (old, new) in checks {
        let has = repo.odb.has(old)?;
        let forward = has && {
            let (old, old_kind) = repo.peel(old)?;
            let (new, new_kind) = repo.peel(new)?;
            old_kind == Kind::Commit
                && new_kind == Kind::Commit
                && super::rev::is_ancestor(repo, old, new)?
        };
        results.push((has, forward));
    }
    let mut excluded: HashSet<Oid> = HashSet::new();
    let mut done: HashSet<Oid> = HashSet::new();
    let mut stack = Vec::new();
    for oid in known {
        if *oid == ZERO || !repo.odb.has(oid)? {
            continue;
        }
        excluded.insert(*oid);
        let (peeled, kind) = repo.peel(oid)?;
        if kind == Kind::Commit {
            stack.push(peeled);
        }
    }
    while let Some(commit) = stack.pop() {
        if done.insert(commit) {
            stack.extend(repo.commit(&commit)?.parents.iter().copied());
        }
    }
    let mut out: Vec<(Kind, Data, u32)> = Vec::new();
    let mut trees = Vec::new();
    let mut boundary = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<Oid> = tips.to_vec();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        if done.contains(&oid) {
            boundary.push(oid);
            continue;
        }
        if excluded.contains(&oid) {
            continue;
        }
        let (kind, data) = repo.object(&oid)?;
        match kind {
            Kind::Tag => {
                let target =
                    tag_target(&data).ok_or_else(|| Error::Io(corrupt("a tag names no object")))?;
                stack.push(target);
            }
            Kind::Commit => {
                let commit = repo.commit(&oid)?;
                trees.push(commit.tree);
                stack.extend(commit.parents.iter().copied());
            }
            Kind::Tree => trees.push(oid),
            Kind::Blob => {}
        }
        if kind != Kind::Tree {
            out.push((kind, data, 0));
        }
    }
    for commit in boundary {
        let tree = repo.commit(&commit)?.tree;
        reach(repo, tree, &HashSet::new(), &mut excluded, None)?;
    }
    let mut kept = HashSet::new();
    for tree in trees {
        reach(repo, tree, &excluded, &mut kept, Some(&mut out))?;
    }
    let borrowed: Vec<pack::Object<'_>> = out
        .iter()
        .map(|(kind, data, name)| pack::Object {
            kind: *kind,
            data,
            name: *name,
        })
        .collect();
    Ok(Found::Objects {
        pack: pack::write(&borrowed, deltas),
        checks: results,
    })
}

struct Spec {
    force: bool,
    src: String,
    dst: String,
}

fn parse_spec(text: &str) -> Spec {
    let (force, rest) = match text.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (src, dst) = rest.split_once(':').unwrap_or((rest, ""));
    Spec {
        force,
        src: src.to_string(),
        dst: dst.to_string(),
    }
}

fn map_spec(spec: &Spec, name: &str) -> Option<String> {
    match spec.src.split_once('*') {
        Some((head, tail)) => {
            let middle = name.strip_prefix(head)?.strip_suffix(tail)?;
            let (dst_head, dst_tail) = spec.dst.split_once('*')?;
            Some(format!("{dst_head}{middle}{dst_tail}"))
        }
        None if spec.src == name && !spec.dst.is_empty() => Some(spec.dst.clone()),
        None => None,
    }
}

fn mapped(specs: &[Spec], name: &str) -> Option<String> {
    specs.iter().find_map(|spec| map_spec(spec, name))
}

fn default_specs(name: &str) -> Vec<Spec> {
    vec![parse_spec(&format!("+refs/heads/*:refs/remotes/{name}/*"))]
}

fn short(name: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(name)
}

fn abbrev(oid: &Oid) -> String {
    oid.hex()[..ABBREV].to_string()
}

fn summary_line(
    flag: char,
    summary: &str,
    from: &str,
    to: &str,
    width: usize,
    note: &str,
) -> String {
    format!(" {flag} {summary:<SUMMARY_WIDTH$} {from:<width$} -> {to}{note}\n")
}

struct Target {
    name: Option<String>,
    url: String,
}

fn target(local: &Local, arg: Option<&str>, command: &str) -> Res<Target> {
    let name = match arg {
        Some(arg) => arg.to_string(),
        None => local
            .branch()
            .and_then(|b| local.get(&format!("branch.{b}.remote")))
            .unwrap_or("origin")
            .to_string(),
    };
    if name.contains("://") {
        http(command, &name)?;
        return Ok(Target {
            name: None,
            url: name,
        });
    }
    match local.get(&format!("remote.{name}.url")) {
        Some(url) => {
            http(command, url)?;
            Ok(Target {
                name: Some(name),
                url: url.to_string(),
            })
        }
        None => Err(Error::Fatal(format!(
            "'{name}' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists."
        ))),
    }
}

fn specs_of(local: &Local, target: &Target) -> Vec<Spec> {
    match &target.name {
        Some(name) => {
            let configured: Vec<Spec> = local
                .all(&format!("remote.{name}.fetch"))
                .into_iter()
                .map(parse_spec)
                .collect();
            if configured.is_empty() {
                default_specs(name)
            } else {
                configured
            }
        }
        None => Vec::new(),
    }
}

fn unmap_spec(spec: &Spec, name: &str) -> Option<String> {
    match spec.dst.split_once('*') {
        Some((head, tail)) => {
            let middle = name.strip_prefix(head)?.strip_suffix(tail)?;
            let (src_head, src_tail) = spec.src.split_once('*')?;
            Some(format!("{src_head}{middle}{src_tail}"))
        }
        None if spec.dst == name => Some(spec.src.clone()),
        None => None,
    }
}

fn full_dst(dst: &str, src: &str) -> String {
    if dst.starts_with("refs/") {
        return dst.to_string();
    }
    match src.strip_prefix("refs/tags/") {
        Some(_) => format!("refs/tags/{dst}"),
        None => format!("refs/heads/{dst}"),
    }
}

struct Update {
    from: String,
    to: Option<String>,
    force: bool,
    head: bool,
    old: Option<Oid>,
    new: Oid,
}

struct Fetched {
    heads: Vec<(String, Oid)>,
    rejected: bool,
}

#[derive(Default)]
struct FetchOptions {
    quiet: bool,
    depth: Option<u32>,
    prune: Option<bool>,
    update_head_ok: bool,
}

fn chosen_refs(
    advert: &Advert,
    specs: &[Spec],
    explicit: &[String],
) -> Res<(Vec<Update>, Vec<Spec>)> {
    let mut updates = Vec::new();
    let mut wild = Vec::new();
    let update = |from: &str, oid: Oid, to: Option<String>, force: bool| Update {
        from: from.to_string(),
        to: to.filter(|to| safe_ref(to)),
        force,
        head: true,
        old: None,
        new: oid,
    };
    let forced = |name: &str| {
        specs
            .iter()
            .find(|spec| map_spec(spec, name).is_some())
            .is_some_and(|spec| spec.force)
    };
    if explicit.is_empty() {
        for (name, oid) in &advert.refs {
            if let Some(to) = mapped(specs, name) {
                updates.push(update(name, *oid, Some(to), forced(name)));
            }
        }
        return Ok((updates, Vec::new()));
    }
    for arg in explicit {
        let spec = parse_spec(arg);
        if spec.src.contains('*') {
            if !spec.dst.contains('*') {
                return Err(Error::Fatal(format!("invalid refspec '{arg}'")));
            }
            for (name, oid) in &advert.refs {
                if let Some(to) = map_spec(&spec, name) {
                    updates.push(update(name, *oid, Some(to), spec.force));
                }
            }
            wild.push(spec);
            continue;
        }
        let found = [
            spec.src.clone(),
            format!("refs/{}", spec.src),
            format!("refs/tags/{}", spec.src),
            format!("refs/heads/{}", spec.src),
        ]
        .into_iter()
        .find_map(|name| advert.get(&name).map(|oid| (name, oid)));
        let Some((name, oid)) = found else {
            return Err(Error::Fatal(format!(
                "couldn't find remote ref {}",
                spec.src
            )));
        };
        if spec.dst.is_empty() {
            updates.push(update(&name, oid, mapped(specs, &name), forced(&name)));
            continue;
        }
        let to = full_dst(&spec.dst, &name);
        if !safe_ref(&to) {
            return Err(Error::Fatal(format!("invalid refspec '{arg}'")));
        }
        updates.push(update(&name, oid, Some(to), spec.force));
    }
    let mut extra: Vec<Update> = Vec::new();
    for u in &updates {
        let Some(to) = mapped(specs, &u.from).filter(|to| safe_ref(to)) else {
            continue;
        };
        let taken = |list: &[Update]| list.iter().any(|o| o.to.as_ref() == Some(&to));
        if !taken(&updates) && !taken(&extra) {
            extra.push(Update {
                head: false,
                ..update(&u.from, u.new, Some(to.clone()), forced(&u.from))
            });
        }
    }
    updates.append(&mut extra);
    Ok((updates, wild))
}

fn stale(local: &Local, advert: &Advert, specs: &[Spec]) -> Vec<String> {
    local
        .refs
        .iter()
        .map(|(name, _)| name)
        .filter(|name| !local.symbolic.contains(name))
        .filter(|name| {
            specs
                .iter()
                .find_map(|spec| unmap_spec(spec, name))
                .is_some_and(|src| advert.get(&src).is_none())
        })
        .cloned()
        .collect()
}

async fn fetch(
    ctx: &mut UtilCtx<'_>,
    remote: &dyn Remote,
    inv: &Invocation,
    first: &Local,
    target: &Target,
    explicit: &[String],
    options: &FetchOptions,
) -> Res<Fetched> {
    let shown = clean(&target.url);
    let advert = advertised(remote, &target.url, UPLOAD).await?;
    let specs = specs_of(first, target);
    let (mut updates, wild) = chosen_refs(&advert, &specs, explicit)?;
    let shallow_mode = options.depth.is_some() || !first.shallow.is_empty();
    let mut follow: Vec<Update> = Vec::new();
    if explicit.is_empty() {
        for (name, oid) in &advert.refs {
            if name.starts_with("refs/tags/") && !name.ends_with("^{}") && safe_ref(name) {
                follow.push(Update {
                    from: name.clone(),
                    to: Some(name.clone()),
                    force: false,
                    head: true,
                    old: None,
                    new: *oid,
                });
            }
        }
    }
    if !shallow_mode {
        updates.append(&mut follow);
    }
    if !options.update_head_ok
        && let Some(head) = &first.head
        && let Some(work) = &first.work_tree
        && updates.iter().any(|u| u.to.as_ref() == Some(head))
    {
        return Err(Error::Fatal(format!(
            "refusing to fetch into branch '{head}' checked out at '{}'",
            String::from_utf8_lossy(work)
        )));
    }
    let mut asked: Vec<Oid> = Vec::new();
    for update in updates.iter().chain(&follow) {
        if !asked.contains(&update.new) {
            asked.push(update.new);
        }
    }
    let second = local(ctx, inv, asked.clone()).await?;
    for update in updates.iter_mut().chain(follow.iter_mut()) {
        update.old = update.to.as_deref().and_then(|to| second.get_ref(to));
    }
    updates.retain(|u| !(u.from.starts_with("refs/tags/") && u.old.is_some()));
    follow.retain(|u| u.old.is_none());
    let present = |oid: &Oid| {
        asked
            .iter()
            .zip(&second.present)
            .any(|(asked, present)| asked == oid && *present)
    };
    let mut wants: Vec<Oid> = Vec::new();
    for update in &updates {
        let needed = options.depth.is_some() || !present(&update.new);
        if needed && !wants.contains(&update.new) {
            wants.push(update.new);
        }
    }
    let must: Vec<Oid> = wants.iter().filter(|oid| !present(oid)).copied().collect();
    let mut plan = Plan::default();
    let mut in_pack: Vec<Oid> = Vec::new();
    if !wants.is_empty() {
        let mut haves: Vec<Oid> = Vec::new();
        for (_, oid) in &second.refs {
            if !haves.contains(oid) {
                haves.push(*oid);
            }
        }
        let want = Want {
            wants: &wants,
            haves: &haves,
            shallow: &second.shallow,
            depth: options.depth,
        };
        let got = upload(remote, &target.url, &advert, &want).await?;
        let data = got.pack;
        let indexed = received(&data, &must, false)?;
        let mut next = second.shallow.clone();
        for oid in got.shallow {
            if !next.contains(&oid) {
                next.push(oid);
            }
        }
        next.retain(|oid| !got.unshallow.contains(oid));
        crate::order::sort(&mut next);
        let outside = missing(&data, &indexed, &wants, &next)?;
        if !outside.is_empty() {
            let third = local(ctx, inv, outside).await?;
            if third.present.iter().any(|present| !present) {
                return Err(incomplete());
            }
        }
        in_pack.extend(
            follow
                .iter()
                .filter(|u| indexed.kind(&u.new).is_some())
                .map(|u| u.new),
        );
        if next != second.shallow {
            plan.ops.push(shallow_op(&second.common, &next));
        }
        if pack_count(&data) > 0 {
            plan.ops
                .extend(pack_ops(&join(&second.common, b"objects"), data, &indexed));
        }
    }
    follow.retain(|u| present(&u.new) || in_pack.contains(&u.new));
    updates.append(&mut follow);
    let checks: Vec<(Oid, Oid)> = updates
        .iter()
        .filter(|u| u.to.is_some())
        .filter_map(|u| u.old.filter(|old| *old != u.new).map(|old| (old, u.new)))
        .collect();
    let mut forward: Vec<(Oid, Oid)> = Vec::new();
    if !checks.is_empty() {
        write::apply(ctx, std::mem::take(&mut plan)).await?;
        let job = Job::Objects {
            cwd: inv.cwd.clone(),
            overrides: inv.overrides.clone(),
            tips: Vec::new(),
            known: Vec::new(),
            checks: checks.clone(),
            ofs_delta: false,
        };
        let Found::Objects {
            checks: results, ..
        } = ask(ctx, job).await?
        else {
            return Err(Error::Exit(128));
        };
        forward.extend(
            checks
                .iter()
                .zip(results)
                .filter(|(_, (_, ff))| *ff)
                .map(|(check, _)| *check),
        );
    }
    let prune = options.prune.unwrap_or_else(|| {
        let named = target
            .name
            .as_ref()
            .and_then(|name| first.get(&format!("remote.{name}.prune")));
        named
            .or_else(|| first.get("fetch.prune"))
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    });
    let pruned = if !prune {
        Vec::new()
    } else if explicit.is_empty() {
        stale(&second, &advert, &specs)
    } else {
        stale(&second, &advert, &wild)
    };
    let mut lines = String::new();
    let width = updates
        .iter()
        .filter(|u| u.old != Some(u.new))
        .map(|u| short(&u.from).len())
        .chain((!pruned.is_empty()).then_some("(none)".len()))
        .max()
        .unwrap_or(0);
    for name in &pruned {
        lines.push_str(&summary_line(
            '-',
            "[deleted]",
            "(none)",
            short(name),
            width,
            "",
        ));
    }
    plan.ops.extend(delete_ops(&second, &pruned));
    let mut rejected = false;
    for update in &updates {
        let Some(to) = &update.to else {
            continue;
        };
        if update.old == Some(update.new) {
            continue;
        }
        let (flag, summary, note) = match update.old {
            None if to.starts_with("refs/tags/") => ('*', "[new tag]".to_string(), ""),
            None if update.from.starts_with("refs/heads/") => ('*', "[new branch]".to_string(), ""),
            None => ('*', "[new ref]".to_string(), ""),
            Some(old) if forward.contains(&(old, update.new)) => (
                ' ',
                format!("{}..{}", abbrev(&old), abbrev(&update.new)),
                "",
            ),
            Some(old) if update.force => (
                '+',
                format!("{}...{}", abbrev(&old), abbrev(&update.new)),
                "  (forced update)",
            ),
            Some(_) => ('!', "[rejected]".to_string(), "  (non-fast-forward)"),
        };
        if flag == '!' {
            rejected = true;
        } else {
            plan.ops.push(ref_op(&second.common, to, Some(update.new)));
        }
        lines.push_str(&summary_line(
            flag,
            &summary,
            short(&update.from),
            short(to),
            width,
            note,
        ));
    }
    let upstream = first.branch().and_then(|b| local_merge(first, b, target));
    let mut fetch_head = String::new();
    for (i, update) in updates.iter().enumerate() {
        if !update.head || (update.from.starts_with("refs/tags/") && explicit.is_empty()) {
            continue;
        }
        let merge = if explicit.is_empty() {
            upstream.as_deref() == Some(update.from.as_str())
        } else {
            i == 0
        };
        let what = match update.from.strip_prefix("refs/heads/") {
            Some(branch) => format!("branch '{branch}' of "),
            None => match update.from.strip_prefix("refs/tags/") {
                Some(tag) => format!("tag '{tag}' of "),
                None => format!("'{}' of ", update.from),
            },
        };
        let mark = if merge { "" } else { "not-for-merge" };
        fetch_head.push_str(&format!("{}\t{mark}\t{what}{shown}\n", update.new.hex()));
    }
    plan.ops.push(Op::Write {
        path: join(&second.git_dir, b"FETCH_HEAD"),
        data: fetch_head.into_bytes(),
        mode: Mode::Plain,
    });
    write::apply(ctx, plan).await?;
    if !lines.is_empty() && (!options.quiet || rejected) {
        let text = format!("From {}\n{lines}", base(&shown));
        if rejected {
            ctx.error_raw(text.as_bytes()).await;
        } else {
            say(ctx, &text).await;
        }
    }
    Ok(Fetched {
        heads: updates
            .iter()
            .filter(|u| u.head)
            .map(|u| (u.from.clone(), u.new))
            .collect(),
        rejected,
    })
}

fn local_merge(local: &Local, branch: &str, target: &Target) -> Option<String> {
    let remote = local.get(&format!("branch.{branch}.remote"))?;
    if Some(remote) != target.name.as_deref() {
        return None;
    }
    local
        .get(&format!("branch.{branch}.merge"))
        .map(str::to_string)
}

fn depth_arg(text: &str) -> Res<u32> {
    match text.parse::<u32>() {
        Ok(depth) if depth > 0 && depth <= INFINITE_DEPTH => Ok(depth),
        _ => Err(Error::Fatal(format!(
            "depth {text} is not a positive number"
        ))),
    }
}

async fn fetch_command(ctx: &mut UtilCtx<'_>, remote: &dyn Remote, inv: &Invocation) -> Res<i32> {
    let mut options = FetchOptions::default();
    let mut unshallow = false;
    let mut positional = Vec::new();
    let mut i = 0;
    while let Some(arg) = inv.args.get(i) {
        match arg.as_str() {
            "-q" | "--quiet" => options.quiet = true,
            "--prune" | "-p" => options.prune = Some(true),
            "--no-prune" => options.prune = Some(false),
            "--update-head-ok" | "-u" => options.update_head_ok = true,
            "--unshallow" => unshallow = true,
            "--depth" => {
                i += 1;
                let value = inv.args.get(i).ok_or_else(|| super::option("fetch", arg))?;
                options.depth = Some(depth_arg(value)?);
            }
            a if a.starts_with("--depth=") => options.depth = Some(depth_arg(&a[8..])?),
            "--no-tags" | "--tags" | "-t" | "-v" | "--verbose" | "--progress" | "--no-progress" => {
            }
            a if a.starts_with('-') => return Err(super::option("fetch", a)),
            _ => positional.push(arg.clone()),
        }
        i += 1;
    }
    let first = local(ctx, inv, Vec::new()).await?;
    if unshallow {
        if options.depth.is_some() {
            return Err(Error::Fatal(
                "options '--depth' and '--unshallow' cannot be used together".to_string(),
            ));
        }
        if first.shallow.is_empty() {
            return Err(Error::Fatal(
                "--unshallow on a complete repository does not make sense".to_string(),
            ));
        }
        options.depth = Some(INFINITE_DEPTH);
    }
    let target = target(&first, positional.first().map(String::as_str), "fetch")?;
    let explicit = positional.get(1..).unwrap_or_default();
    let fetched = fetch(ctx, remote, inv, &first, &target, explicit, &options).await?;
    Ok(if fetched.rejected { 1 } else { 0 })
}

async fn pull(ctx: &mut UtilCtx<'_>, remote: &dyn Remote, inv: &Invocation) -> Res<i32> {
    let (mut only, mut quiet) = (false, false);
    let mut positional = Vec::new();
    for arg in &inv.args {
        match arg.as_str() {
            "--ff-only" => only = true,
            "-q" | "--quiet" => quiet = true,
            "--ff" | "--no-rebase" | "--no-edit" | "--stat" | "-n" | "--no-stat" | "-v"
            | "--verbose" => {}
            "--rebase" | "-r" | "--no-ff" | "--squash" => {
                return Err(Error::Lacks(format!("'pull' with '{arg}'"), MERGES));
            }
            a if a.starts_with('-') => return Err(super::option("pull", a)),
            _ => positional.push(arg.clone()),
        }
    }
    let first = local(ctx, inv, Vec::new()).await?;
    let Some(branch) = first.branch().map(str::to_string) else {
        return Err(Error::Fatal(
            "You are not currently on a branch.".to_string(),
        ));
    };
    let target = target(&first, positional.first().map(String::as_str), "pull")?;
    let explicit = positional.get(1..).unwrap_or_default();
    let merge = if explicit.is_empty() {
        match local_merge(&first, &branch, &target) {
            Some(merge) => Some(merge),
            None => {
                return Err(Error::Says(
                    format!(
                        "There is no tracking information for the current branch.\nPlease specify which branch you want to merge with.\n\n    git pull <remote> <branch>\n\nIf you wish to set tracking information for this branch you can do so with:\n\n    git branch --set-upstream-to=<remote>/<branch> {branch}\n\n"
                    ),
                    1,
                ));
            }
        }
    } else {
        None
    };
    let options = FetchOptions {
        quiet,
        ..FetchOptions::default()
    };
    let fetched = fetch(ctx, remote, inv, &first, &target, explicit, &options).await?;
    if fetched.rejected {
        return Ok(1);
    }
    let theirs = match &merge {
        Some(merge) => fetched
            .heads
            .iter()
            .find(|(name, _)| name == merge)
            .map(|(_, oid)| *oid)
            .ok_or_else(|| {
                Error::Fatal(format!(
                    "couldn't find remote ref {}",
                    merge.trim_start_matches("refs/heads/")
                ))
            })?,
        None => fetched.heads[0].1,
    };
    let mut argv: Vec<Vec<u8>> = vec![b"git".to_vec()];
    argv.extend(inv.globals.iter().cloned());
    argv.push(b"-C".to_vec());
    argv.push(inv.cwd.clone());
    argv.push(b"merge".to_vec());
    if only {
        argv.push(b"--ff-only".to_vec());
    }
    if quiet {
        argv.push(b"-q".to_vec());
    }
    argv.push(theirs.hex().into_bytes());
    Ok(super::run(ctx, &argv).await)
}

async fn ls_remote(ctx: &mut UtilCtx<'_>, remote: &dyn Remote, inv: &Invocation) -> Res<i32> {
    let (mut heads, mut tags) = (false, false);
    let mut positional = Vec::new();
    for arg in &inv.args {
        match arg.as_str() {
            "--heads" | "-h" | "--branches" | "-b" => heads = true,
            "--tags" | "-t" => tags = true,
            "-q" | "--quiet" => {}
            a if a.starts_with('-') => return Err(super::option("ls-remote", a)),
            _ => positional.push(arg.clone()),
        }
    }
    let url = match positional.first() {
        Some(arg) if arg.contains("://") => {
            http("ls-remote", arg)?;
            arg.clone()
        }
        arg => {
            let first = local(ctx, inv, Vec::new()).await?;
            target(&first, arg.map(String::as_str), "ls-remote")?.url
        }
    };
    let advert = advertised(remote, &url, UPLOAD).await?;
    let mut out = String::new();
    for (name, oid) in &advert.refs {
        let wanted = (!heads && !tags)
            || (heads && name.starts_with("refs/heads/"))
            || (tags && name.starts_with("refs/tags/"));
        let pattern = positional.get(1..).unwrap_or_default();
        let matches = pattern.is_empty()
            || pattern
                .iter()
                .any(|p| name == p || name.ends_with(&format!("/{p}")));
        if wanted && matches {
            out.push_str(&format!("{}\t{name}\n", oid.hex()));
        }
    }
    ctx.write(out.as_bytes()).await?;
    Ok(0)
}

fn humanish(url: &str) -> String {
    let shown = clean(url);
    let path = shown
        .split_once("://")
        .map_or(shown.as_str(), |(_, rest)| rest);
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn parse_commit_tree(data: &[u8]) -> Option<Oid> {
    Oid::from_hex(data.strip_prefix(b"tree ")?.get(..40)?)
}

fn worktree(
    pack: &[u8],
    indexed: &Indexed,
    tree: Oid,
    top: &[u8],
    ops: &mut Vec<Op>,
) -> Res<Vec<IndexEntry>> {
    let missing = || Error::Fatal("remote did not send all necessary objects".to_string());
    let mut entries = Vec::new();
    let mut stack = vec![(Vec::new(), tree)];
    while let Some((prefix, tree)) = stack.pop() {
        let (_, data) = indexed.read(pack, &tree)?.ok_or_else(missing)?;
        for entry in parse_tree(&data)? {
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&entry.name);
            if entry.name.is_empty()
                || entry.name == b"."
                || entry.name == b".."
                || entry.name.eq_ignore_ascii_case(b".git")
                || entry.name.contains(&b'/')
            {
                return Err(Error::Fatal(format!(
                    "the remote's tree has an entry that is not a file name: '{}'",
                    String::from_utf8_lossy(&path)
                )));
            }
            let at = join(top, &path);
            match entry.mode {
                MODE_TREE => {
                    stack.push((path, entry.oid));
                    continue;
                }
                MODE_GITLINK => {
                    ops.push(Op::Mkdir(at));
                    entries.push(IndexEntry::new(path, MODE_GITLINK, entry.oid));
                    continue;
                }
                _ => {}
            }
            let (_, blob) = indexed.read(pack, &entry.oid)?.ok_or_else(missing)?;
            let mode = match entry.mode {
                MODE_LINK => {
                    ops.push(Op::Link {
                        path: at,
                        target: blob,
                    });
                    MODE_LINK
                }
                MODE_EXEC => {
                    ops.push(Op::Write {
                        path: at,
                        data: blob,
                        mode: Mode::Exec,
                    });
                    MODE_EXEC
                }
                _ => {
                    ops.push(Op::Write {
                        path: at,
                        data: blob,
                        mode: Mode::Plain,
                    });
                    0o100644
                }
            };
            let mut index_entry = IndexEntry::new(path, mode, entry.oid);
            index_entry.fresh = true;
            entries.push(index_entry);
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

async fn clone(ctx: &mut UtilCtx<'_>, remote: &dyn Remote, inv: &Invocation) -> Res<i32> {
    let mut quiet = false;
    let mut depth = None;
    let mut single = None;
    let mut no_tags = false;
    let mut branch: Option<String> = None;
    let mut positional = Vec::new();
    let mut i = 0;
    while let Some(arg) = inv.args.get(i) {
        match arg.as_str() {
            "-q" | "--quiet" => quiet = true,
            "-b" | "--branch" => {
                i += 1;
                branch = inv.args.get(i).cloned();
            }
            "--depth" => {
                i += 1;
                let value = inv.args.get(i).ok_or_else(|| super::option("clone", arg))?;
                depth = Some(depth_arg(value)?);
            }
            a if a.starts_with("--depth=") => depth = Some(depth_arg(&a[8..])?),
            "--single-branch" => single = Some(true),
            "--no-single-branch" => single = Some(false),
            "--no-tags" => no_tags = true,
            "--" | "-v" | "--verbose" | "--progress" | "--no-progress" => {}
            a if a.starts_with("--branch=") => branch = Some(a[9..].to_string()),
            a if a.starts_with('-') => return Err(super::option("clone", a)),
            _ => positional.push(arg.clone()),
        }
        i += 1;
    }
    let Some(url) = positional.first().cloned() else {
        return Err(Error::Says(
            "fatal: You must specify a repository to clone.\n".to_string(),
            129,
        ));
    };
    http("clone", &url)?;
    let dir = match positional.get(1) {
        Some(dir) => dir.clone(),
        None => humanish(&url),
    };
    if dir.is_empty() {
        return Err(Error::Fatal(format!(
            "could not name a directory for '{}'",
            clean(&url)
        )));
    }
    let job = Job::Clone {
        cwd: inv.cwd.clone(),
        dir: dir.clone(),
    };
    let Found::Clone {
        mut plan,
        top,
        git_dir,
    } = ask(ctx, job).await?
    else {
        return Err(Error::Exit(128));
    };
    if !quiet {
        say(ctx, &format!("Cloning into '{dir}'...\n")).await;
    }
    let shown = clean(&url);
    let advert = advertised(remote, &url, UPLOAD).await?;
    let single = single.unwrap_or(depth.is_some());
    let mut heads: Vec<(String, Oid)> = advert
        .refs
        .iter()
        .filter(|(name, _)| name.starts_with("refs/heads/") && safe_ref(name))
        .cloned()
        .collect();
    let mut tags: Vec<(String, Oid)> = advert
        .refs
        .iter()
        .filter(|(name, _)| {
            !no_tags && name.starts_with("refs/tags/") && !name.ends_with("^{}") && safe_ref(name)
        })
        .cloned()
        .collect();
    let head_oid = advert.get("HEAD");
    let mut config = format!(
        "[remote \"origin\"]\n\turl = {shown}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"
    );
    if no_tags {
        config.push_str("\ttagOpt = --no-tags\n");
    }
    if heads.is_empty() && tags.is_empty() && head_oid.is_none() {
        plan.ops.push(Op::Append {
            path: join(&git_dir, b"config"),
            data: config.into_bytes(),
        });
        write::apply(ctx, plan).await?;
        say(
            ctx,
            "warning: You appear to have cloned an empty repository.\n",
        )
        .await;
        return Ok(0);
    }
    let checkout: Option<(Option<String>, Oid)> = match &branch {
        Some(name) => {
            let full = format!("refs/heads/{name}");
            match advert.get(&full) {
                Some(oid) => Some((Some(full), oid)),
                None => match advert.get(&format!("refs/tags/{name}")) {
                    Some(oid) => Some((None, oid)),
                    None => {
                        return Err(Error::Fatal(format!(
                            "Remote branch {name} not found in upstream origin"
                        )));
                    }
                },
            }
        }
        None => match (&advert.head, head_oid) {
            (Some(head), _) if advert.get(head).is_some() && safe_ref(head) => {
                advert.get(head).map(|oid| (Some(head.clone()), oid))
            }
            (_, Some(oid)) => Some((
                heads
                    .iter()
                    .find(|(_, o)| *o == oid)
                    .map(|(name, _)| name.clone()),
                oid,
            )),
            (_, None) => None,
        },
    };
    if single {
        let only = checkout.as_ref().and_then(|(name, _)| name.clone());
        heads.retain(|(name, _)| Some(name) == only.as_ref());
        if let Some(name) = &only {
            let short = &name["refs/heads/".len()..];
            config = config.replace(
                "+refs/heads/*:refs/remotes/origin/*",
                &format!("+{name}:refs/remotes/origin/{short}"),
            );
        }
    }
    let mut wants: Vec<Oid> = Vec::new();
    for oid in heads
        .iter()
        .chain(tags.iter().filter(|_| !single))
        .map(|(_, oid)| *oid)
        .chain(checkout.as_ref().map(|(_, oid)| *oid))
    {
        if !wants.contains(&oid) {
            wants.push(oid);
        }
    }
    let want = Want {
        wants: &wants,
        haves: &[],
        shallow: &[],
        depth,
    };
    let got = upload(remote, &url, &advert, &want).await?;
    let data = got.pack;
    let indexed = received(&data, &wants, true)?;
    let mut shallow = got.shallow;
    crate::order::sort(&mut shallow);
    shallow.dedup();
    if !missing(&data, &indexed, &wants, &shallow)?.is_empty() {
        return Err(incomplete());
    }
    if single {
        tags.retain(|(_, oid)| indexed.kind(oid).is_some());
    }
    if !shallow.is_empty() {
        plan.ops.push(shallow_op(&git_dir, &shallow));
    }
    let mut checkout_ops = Vec::new();
    let mut index = None;
    if let Some((_, oid)) = &checkout {
        let mut commit = *oid;
        let missing = || Error::Fatal("remote did not send all necessary objects".to_string());
        let tree = loop {
            let (kind, body) = indexed.read(&data, &commit)?.ok_or_else(missing)?;
            match kind {
                Kind::Tag => commit = tag_target(&body).ok_or_else(missing)?,
                Kind::Commit => break parse_commit_tree(&body).ok_or_else(missing)?,
                Kind::Tree | Kind::Blob => {
                    return Err(Error::Fatal(format!(
                        "the remote's HEAD names a {}",
                        kind.name()
                    )));
                }
            }
        };
        let entries = worktree(&data, &indexed, tree, &top, &mut checkout_ops)?;
        index = Some(IndexOut {
            path: join(&git_dir, b"index"),
            root: top.clone(),
            index: Index {
                entries,
                ..Index::default()
            },
        });
    }
    plan.ops
        .extend(pack_ops(&join(&git_dir, b"objects"), data, &indexed));
    for (name, oid) in &heads {
        let tracking = format!("refs/remotes/origin/{}", &name["refs/heads/".len()..]);
        plan.ops.push(ref_op(&git_dir, &tracking, Some(*oid)));
    }
    for (name, oid) in &tags {
        plan.ops.push(ref_op(&git_dir, name, Some(*oid)));
    }
    match &checkout {
        Some((Some(full), oid)) => {
            let name = &full["refs/heads/".len()..];
            config.push_str(&format!(
                "[branch \"{name}\"]\n\tremote = origin\n\tmerge = {full}\n"
            ));
            plan.ops.push(ref_op(&git_dir, full, Some(*oid)));
            plan.ops.push(Op::Write {
                path: join(&git_dir, b"refs/remotes/origin/HEAD"),
                data: format!("ref: refs/remotes/origin/{name}\n").into_bytes(),
                mode: Mode::Plain,
            });
            plan.ops.push(Op::Write {
                path: join(&git_dir, b"HEAD"),
                data: format!("ref: {full}\n").into_bytes(),
                mode: Mode::Plain,
            });
        }
        Some((None, oid)) => plan.ops.push(Op::Write {
            path: join(&git_dir, b"HEAD"),
            data: format!("{}\n", oid.hex()).into_bytes(),
            mode: Mode::Plain,
        }),
        None => {}
    }
    plan.ops.push(Op::Append {
        path: join(&git_dir, b"config"),
        data: config.into_bytes(),
    });
    plan.ops.extend(checkout_ops);
    plan.index = index;
    write::apply(ctx, plan).await?;
    Ok(0)
}

struct Push {
    src: Option<Oid>,
    dst: String,
    force: bool,
    old: Oid,
    rejected: Option<&'static str>,
}

fn resolve_src(local: &Local, src: &str) -> Option<(String, Oid)> {
    if src == "HEAD" {
        let oid = local.head_oid?;
        return Some((
            local.head.clone().unwrap_or_else(|| "HEAD".to_string()),
            oid,
        ));
    }
    [
        src.to_string(),
        format!("refs/{src}"),
        format!("refs/tags/{src}"),
        format!("refs/heads/{src}"),
    ]
    .into_iter()
    .find_map(|name| local.get_ref(&name).map(|oid| (name, oid)))
}

/// A ref to push: its source (full name and id; none to delete), destination and
/// whether to force.
type PushWant = (Option<(String, Oid)>, String, bool);

async fn push(ctx: &mut UtilCtx<'_>, remote: &dyn Remote, inv: &Invocation) -> Res<i32> {
    let (mut force, mut upstream, mut quiet, mut delete) = (false, false, false, false);
    let mut positional = Vec::new();
    for arg in &inv.args {
        match arg.as_str() {
            "-f" | "--force" => force = true,
            "-u" | "--set-upstream" => upstream = true,
            "-q" | "--quiet" => quiet = true,
            "-d" | "--delete" => delete = true,
            "-v" | "--verbose" | "--progress" | "--no-progress" | "--no-verify" => {}
            a if a.starts_with('-') => return Err(super::option("push", a)),
            _ => positional.push(arg.clone()),
        }
    }
    let first = local(ctx, inv, Vec::new()).await?;
    let branch = first.branch().map(str::to_string);
    let arg = positional.first().map(String::as_str).or_else(|| {
        let b = branch.as_deref()?;
        first.get(&format!("branch.{b}.pushremote"))
    });
    let target = target(&first, arg, "push")?;
    let shown = base(&clean(&target.url)).to_string();
    let mut wanted: Vec<PushWant> = Vec::new();
    let specs = positional.get(1..).unwrap_or_default();
    if specs.is_empty() {
        let Some(branch) = &branch else {
            return Err(Error::Fatal("You are not currently on a branch.\nTo push the history leading to the current (detached HEAD)\nstate now, use\n\n    git push origin HEAD:<name-of-remote-branch>\n".to_string()));
        };
        let full = format!("refs/heads/{branch}");
        let dst = match local_merge(&first, branch, &target) {
            Some(merge) => merge,
            None if upstream => full.clone(),
            None => {
                let name = target.name.clone().unwrap_or_else(|| shown.clone());
                return Err(Error::Fatal(format!(
                    "The current branch {branch} has no upstream branch.\nTo push the current branch and set the remote as upstream, use\n\n    git push --set-upstream {name} {branch}\n"
                )));
            }
        };
        let oid = first
            .get_ref(&full)
            .ok_or_else(|| Error::Fatal(format!("src refspec {branch} does not match any")))?;
        wanted.push((Some((full, oid)), dst, force));
    }
    for text in specs {
        let spec = parse_spec(text);
        if delete {
            let dst = if spec.src.starts_with("refs/") {
                spec.src.clone()
            } else {
                format!("refs/heads/{}", spec.src)
            };
            wanted.push((None, dst, true));
            continue;
        }
        let source = if spec.src.is_empty() {
            None
        } else {
            Some(resolve_src(&first, &spec.src).ok_or_else(|| {
                Error::Fatal(format!("src refspec {} does not match any", spec.src))
            })?)
        };
        let dst = match (spec.dst.as_str(), &source) {
            ("", Some((name, _))) if name.starts_with("refs/") => name.clone(),
            ("", _) => format!("refs/heads/{}", spec.src),
            (dst, _) if dst.starts_with("refs/") => dst.to_string(),
            (dst, Some((name, _))) if name.starts_with("refs/tags/") => format!("refs/tags/{dst}"),
            (dst, _) => format!("refs/heads/{dst}"),
        };
        wanted.push((source, dst, force || spec.force));
    }
    for (_, dst, _) in &wanted {
        if !safe_ref(dst) {
            return Err(Error::Fatal(format!("invalid refspec destination '{dst}'")));
        }
    }
    let advert = advertised(remote, &target.url, RECEIVE).await?;
    let mut pushes: Vec<Push> = Vec::new();
    for (source, dst, force) in &wanted {
        let old = advert.get(dst).unwrap_or(ZERO);
        let src = source.as_ref().map(|(_, oid)| *oid);
        if src.is_none() && old == ZERO {
            return Err(Error::Says(
                format!(
                    "error: unable to delete '{}': remote ref does not exist\nerror: failed to push some refs to '{shown}'\n",
                    short(dst)
                ),
                1,
            ));
        }
        if src == Some(old) {
            continue;
        }
        pushes.push(Push {
            src,
            dst: dst.clone(),
            force: *force,
            old,
            rejected: None,
        });
    }
    let tracking = specs_of(&first, &target);
    let mut plan = Plan::default();
    if upstream
        && let Some((Some((full, _)), dst, _)) = wanted.first()
        && let (Some(name), Some(branch)) = (&target.name, full.strip_prefix("refs/heads/"))
    {
        plan.ops.push(Op::Append {
            path: join(&first.common, b"config"),
            data: format!("[branch \"{branch}\"]\n\tremote = {name}\n\tmerge = {dst}\n")
                .into_bytes(),
        });
    }
    if pushes.is_empty() {
        write::apply(ctx, plan).await?;
        if !quiet {
            say(ctx, "Everything up-to-date\n").await;
        }
        return Ok(0);
    }
    let checks: Vec<(Oid, Oid)> = pushes
        .iter()
        .filter_map(|p| match p.src {
            Some(new) if !p.force && p.old != ZERO => Some((p.old, new)),
            _ => None,
        })
        .collect();
    let mut known: Vec<Oid> = advert.refs.iter().map(|(_, oid)| *oid).collect();
    known.dedup();
    let job = Job::Objects {
        cwd: inv.cwd.clone(),
        overrides: inv.overrides.clone(),
        tips: pushes.iter().filter_map(|p| p.src).collect(),
        known,
        checks: checks.clone(),
        ofs_delta: advert.has("ofs-delta"),
    };
    let Found::Objects {
        pack: data,
        checks: verdicts,
    } = ask(ctx, job).await?
    else {
        return Err(Error::Exit(128));
    };
    let mut verdicts = verdicts.into_iter();
    for p in &mut pushes {
        if p.src.is_some() && !p.force && p.old != ZERO {
            match verdicts.next() {
                Some((true, true)) => {}
                Some((false, _)) => p.rejected = Some("fetch first"),
                _ => p.rejected = Some("non-fast-forward"),
            }
        }
    }
    let mut report: Vec<(String, Result<(), String>)> = Vec::new();
    let accepted: Vec<&Push> = pushes.iter().filter(|p| p.rejected.is_none()).collect();
    if !accepted.is_empty() {
        let mut caps = Vec::new();
        let reports = advert.has("report-status");
        if reports {
            caps.push("report-status".to_string());
        }
        if accepted.iter().any(|p| p.src.is_none()) {
            if !advert.has("delete-refs") {
                return Err(Error::Fatal(
                    "the receiving end does not support deleting refs".to_string(),
                ));
            }
            caps.push("delete-refs".to_string());
        }
        if advert.caps.iter().any(|c| c.starts_with("agent=")) {
            caps.push(format!("agent={}", agent()));
        }
        let mut body = Vec::new();
        for (i, p) in accepted.iter().enumerate() {
            let new = p.src.unwrap_or(ZERO);
            let line = if i == 0 {
                format!(
                    "{} {} {}\0{}\n",
                    p.old.hex(),
                    new.hex(),
                    p.dst,
                    caps.join(" ")
                )
            } else {
                format!("{} {} {}\n", p.old.hex(), new.hex(), p.dst)
            };
            pkt::line(&mut body, line.as_bytes());
        }
        pkt::flush(&mut body);
        if accepted.iter().any(|p| p.src.is_some()) {
            body.extend_from_slice(&data);
        }
        let call = Call {
            method: "POST",
            url: format!("{}/{RECEIVE}", base(&target.url)),
            headers: vec![
                ("User-Agent".to_string(), agent()),
                (
                    "Content-Type".to_string(),
                    format!("application/x-{RECEIVE}-request"),
                ),
                (
                    "Accept".to_string(),
                    format!("application/x-{RECEIVE}-result"),
                ),
            ],
            body,
        };
        let answer = exchange(remote, &target.url, call).await?;
        if reports {
            report = parse_report(&answer.body)?;
        } else {
            report = accepted.iter().map(|p| (p.dst.clone(), Ok(()))).collect();
        }
    }
    let mut lines = String::new();
    let mut failed = false;
    for p in &pushes {
        let from = match p.src {
            Some(_) => wanted
                .iter()
                .find(|(_, dst, _)| *dst == p.dst)
                .and_then(|(s, _, _)| s.as_ref().map(|(name, _)| short(name).to_string()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let to = short(&p.dst);
        if let Some(why) = p.rejected {
            failed = true;
            lines.push_str(&format!(" ! [rejected]        {from} -> {to} ({why})\n"));
            continue;
        }
        match report
            .iter()
            .find(|(name, _)| *name == p.dst)
            .map(|(_, r)| r)
        {
            Some(Ok(())) => {}
            Some(Err(why)) => {
                failed = true;
                lines.push_str(&format!(" ! [remote rejected] {from} -> {to} ({why})\n"));
                continue;
            }
            None => {
                failed = true;
                lines.push_str(&format!(
                    " ! [remote failure]  {from} -> {to} (remote failed to report status)\n"
                ));
                continue;
            }
        }
        let line = match (p.src, p.old == ZERO) {
            (None, _) => format!(" - [deleted]         {to}\n"),
            (Some(_), true) if p.dst.starts_with("refs/tags/") => {
                format!(" * [new tag]         {from} -> {to}\n")
            }
            (Some(_), true) => format!(" * [new branch]      {from} -> {to}\n"),
            (Some(new), false) if p.force => format!(
                " + {}...{} {from} -> {to} (forced update)\n",
                abbrev(&p.old),
                abbrev(&new)
            ),
            (Some(new), false) => {
                format!("   {}..{}  {from} -> {to}\n", abbrev(&p.old), abbrev(&new))
            }
        };
        lines.push_str(&line);
        if let Some(local_name) = mapped(&tracking, &p.dst).filter(|n| safe_ref(n)) {
            plan.ops.push(ref_op(&first.common, &local_name, p.src));
        }
    }
    write::apply(ctx, plan).await?;
    if !quiet || failed {
        let mut text = format!("To {shown}\n{lines}");
        if failed {
            text.push_str(&format!("error: failed to push some refs to '{shown}'\n"));
            if pushes.iter().any(|p| p.rejected == Some("fetch first")) {
                text.push_str("hint: Updates were rejected because the remote contains work that you do not\nhint: have locally. Integrate the remote changes (e.g. 'git pull ...') before pushing again.\n");
            } else if pushes.iter().any(|p| p.rejected.is_some()) {
                text.push_str("hint: Updates were rejected because the tip of your current branch is behind\nhint: its remote counterpart. Integrate the remote changes (e.g. 'git pull ...') before pushing again.\n");
            }
        }
        say(ctx, &text).await;
    }
    Ok(i32::from(failed))
}

fn parse_report(body: &[u8]) -> Res<Vec<(String, Result<(), String>)>> {
    let mut reader = Reader::new(body);
    let unpack = reader
        .text()?
        .ok_or_else(|| Error::Fatal("the remote sent no status report".to_string()))?;
    let unpack = String::from_utf8_lossy(unpack).into_owned();
    let unpacked = match unpack.strip_prefix("unpack ") {
        Some("ok") => None,
        Some(why) => Some(why.to_string()),
        None => {
            return Err(Error::Fatal(
                "the remote sent a malformed status report".to_string(),
            ));
        }
    };
    let mut report = Vec::new();
    while let Some(line) = reader.text()? {
        let line = String::from_utf8_lossy(line).into_owned();
        if let Some(name) = line.strip_prefix("ok ") {
            let result = match &unpacked {
                Some(why) => Err(format!("unpacker error: {why}")),
                None => Ok(()),
            };
            report.push((name.to_string(), result));
        } else if let Some(rest) = line.strip_prefix("ng ") {
            let (name, why) = rest.split_once(' ').unwrap_or((rest, "failed"));
            report.push((name.to_string(), Err(why.to_string())));
        }
    }
    Ok(report)
}
