//! The one interpreter per process that owns the compiled-in C extensions.
//!
//! An extension's C code keeps its state in process-wide statics (numpy refuses a second
//! initialization outright; others cache type objects and interned names of the first
//! interpreter), while the bridge keeps its objects per thread. So only one interpreter in a
//! process may import them: the one on the thread this module starts. A run whose program
//! names such a package goes to it; its file operations come back over a channel to the
//! calling thread, which answers them from its own files and blocks as a local run does.

use std::{
    io,
    rc::Rc,
    sync::{OnceLock, mpsc},
};

use crate::{Child, Exited, Files, Python, Ran, Remove, Run, Stat, Stdio, Write};

/// Packages that import the compiled-in extensions without one of their own.
const DEPENDENTS: [&str; 3] = ["pylab", "mpl_toolkits", "sqlite3"];

enum Op {
    Stat(Vec<u8>),
    Read(Vec<u8>, u64),
    ReadAt(Vec<u8>, u64, usize, u64),
    Write(Vec<u8>, Vec<u8>, Write),
    List(Vec<u8>),
    Mkdir(Vec<u8>, u32),
    Remove(Vec<u8>, Remove),
    Rename(Vec<u8>, Vec<u8>),
    Spawn(Child),
}

enum Answer {
    Stat(io::Result<Stat>),
    Bytes(io::Result<Vec<u8>>),
    Done(io::Result<()>),
    Names(io::Result<Vec<Vec<u8>>>),
    Exited(io::Result<Exited>),
}

enum Said {
    Asks(Op),
    Ran(Ran),
    Failed(io::Error),
}

struct Job {
    run: Run,
    ranged: bool,
    spawns: bool,
    said: mpsc::Sender<Said>,
    answers: mpsc::Receiver<Answer>,
}

/// Whether the program this run starts names a package with compiled-in extensions.
pub fn wanted(run: &Run, files: &dyn Files) -> bool {
    let Some(text) = program(run, files) else {
        return false;
    };
    let mut names = nanocode_python_capi::extension_names()
        .map(|name| name.split('.').next().unwrap_or(name))
        .chain(DEPENDENTS);
    names.any(|name| names_word(&text, name.as_bytes()))
}

fn names_word(text: &[u8], name: &[u8]) -> bool {
    let word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut from = 0;
    while let Some(found) = find(&text[from..], name) {
        let start = from + found;
        let end = start + name.len();
        let before = start.checked_sub(1).map(|at| text[at]);
        let after = text.get(end).copied();
        if !before.is_some_and(word) && !after.is_some_and(word) {
            return true;
        }
        from = start + 1;
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The program's text: the -c command, the -m module's name, the script, or stdin.
fn program(run: &Run, files: &dyn Files) -> Option<Vec<u8>> {
    const VALUED: [&str; 3] = ["-W", "-X", "--check-hash-based-pycs"];
    let mut args = run.argv.iter();
    while let Some(arg) = args.next() {
        if arg == "-c" || arg == "-m" {
            return args.next().map(|text| text.clone().into_bytes());
        }
        if let Some(rest) = arg.strip_prefix("-c").or_else(|| arg.strip_prefix("-m"))
            && !arg.starts_with("--")
        {
            return Some(rest.as_bytes().to_vec());
        }
        if VALUED.contains(&arg.as_str()) {
            args.next();
            continue;
        }
        if arg == "-" {
            break;
        }
        if arg.starts_with('-') {
            continue;
        }
        let path = if arg.starts_with('/') {
            arg.clone().into_bytes()
        } else {
            let mut path = run.cwd.clone();
            path.push(b'/');
            path.extend_from_slice(arg.as_bytes());
            path
        };
        return files.read(&path, 1 << 24).ok();
    }
    match &run.stdio {
        Stdio::Held(stdin) => Some(stdin.clone()),
        Stdio::Process => None,
    }
}

/// Runs `run` on the interpreter that owns the extensions, answering its file operations
/// from `files` on this thread until it ends.
pub fn run(run: Run, files: &dyn Files) -> io::Result<Ran> {
    static JOBS: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();
    let ended = || io::Error::other("the thread that keeps the native interpreter ended");
    let jobs = JOBS.get_or_init(start).as_ref().ok_or_else(ended)?;
    let (said, heard) = mpsc::channel();
    let (answer, answers) = mpsc::channel();
    let job = Job {
        run,
        ranged: files.ranged(),
        spawns: files.spawns(),
        said,
        answers,
    };
    jobs.send(job).map_err(|_| ended())?;
    for message in heard {
        match message {
            Said::Asks(op) => answer.send(answered(files, op)).map_err(|_| ended())?,
            Said::Ran(ran) => return Ok(ran),
            Said::Failed(error) => return Err(error),
        }
    }
    Err(ended())
}

fn answered(files: &dyn Files, op: Op) -> Answer {
    match op {
        Op::Stat(path) => Answer::Stat(files.stat(&path)),
        Op::Read(path, limit) => Answer::Bytes(files.read(&path, limit)),
        Op::ReadAt(path, offset, len, limit) => {
            Answer::Bytes(files.read_at(&path, offset, len, limit))
        }
        Op::Write(path, data, how) => Answer::Done(files.write(&path, &data, how)),
        Op::List(path) => Answer::Names(files.list(&path)),
        Op::Mkdir(path, mode) => Answer::Done(files.mkdir(&path, mode)),
        Op::Remove(path, what) => Answer::Done(files.remove(&path, what)),
        Op::Rename(from, to) => Answer::Done(files.rename(&from, &to)),
        Op::Spawn(child) => Answer::Exited(files.spawn(child)),
    }
}

fn start() -> Option<mpsc::Sender<Job>> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let owner = move || {
        let mut python: Option<Python> = None;
        for job in queue {
            let kept = match python.take() {
                Some(kept) => Ok(kept),
                None => Python::new(),
            };
            let kept = match kept {
                Ok(kept) => kept,
                Err(error) => {
                    let _ = job.said.send(Said::Failed(error));
                    continue;
                }
            };
            let remote = Remote {
                said: job.said.clone(),
                answers: job.answers,
                ranged: job.ranged,
                spawns: job.spawns,
            };
            let ran = kept.run(job.run, Rc::new(remote));
            let _ = job.said.send(Said::Ran(ran));
            python = Some(kept);
        }
    };
    std::thread::Builder::new()
        .name("python3-native".to_owned())
        .spawn(owner)
        .ok()?;
    Some(jobs)
}

/// The calling thread's files, seen from the owning thread.
struct Remote {
    said: mpsc::Sender<Said>,
    answers: mpsc::Receiver<Answer>,
    ranged: bool,
    spawns: bool,
}

impl Remote {
    fn ask(&self, op: Op) -> Option<Answer> {
        self.said.send(Said::Asks(op)).ok()?;
        self.answers.recv().ok()
    }
}

fn gone() -> io::Error {
    io::Error::other("the shell that ran python3 stopped answering")
}

impl Files for Remote {
    fn stat(&self, path: &[u8]) -> io::Result<Stat> {
        match self.ask(Op::Stat(path.to_vec())) {
            Some(Answer::Stat(stat)) => stat,
            _ => Err(gone()),
        }
    }

    fn read(&self, path: &[u8], limit: u64) -> io::Result<Vec<u8>> {
        match self.ask(Op::Read(path.to_vec(), limit)) {
            Some(Answer::Bytes(data)) => data,
            _ => Err(gone()),
        }
    }

    fn read_at(&self, path: &[u8], offset: u64, len: usize, limit: u64) -> io::Result<Vec<u8>> {
        match self.ask(Op::ReadAt(path.to_vec(), offset, len, limit)) {
            Some(Answer::Bytes(data)) => data,
            _ => Err(gone()),
        }
    }

    fn write(&self, path: &[u8], data: &[u8], how: Write) -> io::Result<()> {
        match self.ask(Op::Write(path.to_vec(), data.to_vec(), how)) {
            Some(Answer::Done(done)) => done,
            _ => Err(gone()),
        }
    }

    fn list(&self, path: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        match self.ask(Op::List(path.to_vec())) {
            Some(Answer::Names(names)) => names,
            _ => Err(gone()),
        }
    }

    fn mkdir(&self, path: &[u8], mode: u32) -> io::Result<()> {
        match self.ask(Op::Mkdir(path.to_vec(), mode)) {
            Some(Answer::Done(done)) => done,
            _ => Err(gone()),
        }
    }

    fn remove(&self, path: &[u8], what: Remove) -> io::Result<()> {
        match self.ask(Op::Remove(path.to_vec(), what)) {
            Some(Answer::Done(done)) => done,
            _ => Err(gone()),
        }
    }

    fn rename(&self, from: &[u8], to: &[u8]) -> io::Result<()> {
        match self.ask(Op::Rename(from.to_vec(), to.to_vec())) {
            Some(Answer::Done(done)) => done,
            _ => Err(gone()),
        }
    }

    fn ranged(&self) -> bool {
        self.ranged
    }

    fn spawns(&self) -> bool {
        self.spawns
    }

    fn spawn(&self, child: Child) -> io::Result<Exited> {
        match self.ask(Op::Spawn(child)) {
            Some(Answer::Exited(exited)) => exited,
            _ => Err(gone()),
        }
    }
}
