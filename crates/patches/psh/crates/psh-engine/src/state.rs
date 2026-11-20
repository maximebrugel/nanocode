//! The state of a virtual process: variables, positional parameters, cwd, descriptors,
//! `$?`. A subshell (pipeline stage, `( )`, `$(…)`) starts as a copy of its parent.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use psh_syntax::{Ast, LineIndex};

use crate::fd::{Fd, FdTable};
use crate::host::Host;
use crate::sched::Sched;

/// What the whole engine shares: the host, the scheduler, settings.
pub(crate) struct Core {
    pub host: Rc<dyn Host>,
    pub sched: Rc<Sched>,
    /// The nesting limit (D1050).
    pub max_depth: u32,
    /// Ids handed out for `$!` until real jobs exist.
    pub jobs: Cell<u32>,
    /// The script ended normally: externals of tasks dropped now are released, not
    /// abandoned (background jobs outlive the shell, as in bash).
    pub release_on_drop: Cell<bool>,
    /// The signal whose default action ended the run (D1903): a standalone shell then
    /// dies by it.
    pub died_of: Cell<Option<i32>>,
    /// The signal and pid of the external that just ended by a signal, for the report
    /// after its command (D1303).
    pub fg_signal: Cell<Option<(i32, u32)>>,
    /// The in-process utilities and their off switches (D1080).
    pub utilities: crate::util::UtilPolicy,
    /// `bash` and `sh` run in psh (D1410).
    pub nested_shells: bool,
    /// The program psh runs in, if any: never the real `bash` a nested one falls
    /// through to (D1413).
    pub own_program: Option<Vec<u8>>,
    /// `$RANDOM`'s state (shared by subshells, which bash reseeds; close enough).
    pub rng: Cell<u64>,
    /// `$RANDOM`'s seed for bash's own generator, once seeded or first read.
    pub rseed: Cell<Option<u32>>,
    /// The last `$RANDOM`: bash never returns the same value twice in a row.
    pub last_random: Cell<u32>,
    /// `$SECONDS` counts from this time: nanoseconds since the epoch; may be before it (`SECONDS=100` on a host whose
    /// clock reads the epoch).
    pub seconds_base: Cell<i128>,
    /// This process is a forked copy of the shell that runs one background job
    /// (`Host::prepare_fork`): `None` in the shell itself, `Some(None)` while the job
    /// runs, `Some(Some(status))` once it ended, which ends the run.
    pub forked: Cell<Option<Option<i32>>>,
    /// Told the signals the shell needs caught (`ShellConfig::catch`).
    pub catch: Option<Rc<dyn Fn(u64)>>,
    /// The signals the shell's traps need caught, a bit each (all of them for an EXIT
    /// trap, an interactive shell or a forked copy).
    pub trap_wants: Cell<u64>,
    /// Waits for a foreground command in progress: while one runs, every signal is
    /// caught, so the shell abandons the command before it ends (D1903).
    pub fg_waits: Cell<u32>,
    /// What `catch` was last told; all bits until it was told anything.
    pub caught: Cell<u64>,
    /// Some text the shell parsed could make a value whose subscript arithmetic
    /// expands (`arith::gate`): until then `arith::expanding` and `arith::probe` only
    /// evaluate.
    pub arith_subst_gate: Cell<bool>,
}

/// Where a vproc's pending signals are (D1411): the shell's mask, which the embedder's
/// `ControlHandle` and the standalone shell's handlers set, or the mask of a shell psh
/// runs for `bash`/`sh` (a virtual process: `kill`, `timeout` and jobs signal it, its
/// traps run). A subshell shares its shell's; only the shell's own vproc (`top`)
/// takes them.
#[derive(Clone, Default)]
pub(crate) enum Sigs {
    #[default]
    Shell,
    Nested(Rc<NestedSigs>),
}

/// The pending signals of a shell psh runs for `bash`/`sh` (D1411).
#[derive(Default)]
pub(crate) struct NestedSigs {
    pending: Cell<u64>,
    children: Cell<u32>,
    chld_trap: Cell<bool>,
    pub cont: Cell<bool>,
    stop: Cell<Option<i32>>,
    stop_waker: RefCell<Option<std::task::Waker>>,
    waiters: RefCell<Vec<std::task::Waker>>,
    /// The signal whose default action ended the shell.
    pub died_of: Cell<Option<i32>>,
    /// The pid of the real program it ran instead, once a signal ended that (D1413):
    /// what the caller's report names.
    pub real_pid: Cell<Option<u32>>,
    /// It was abandoned: the TERM went to its process group, so the program it waits
    /// for got it too and its waits give way to a trapped signal as well.
    pub group: Cell<bool>,
}

impl NestedSigs {
    pub(crate) fn stopped(&self, sig: i32) {
        self.stop.set(Some(sig));
        if let Some(w) = self.stop_waker.take() {
            w.wake();
        }
    }

    pub(crate) fn poll_stop(&self, cx: &mut std::task::Context<'_>) -> std::task::Poll<i32> {
        match self.stop.take() {
            Some(sig) => std::task::Poll::Ready(sig),
            None => {
                *self.stop_waker.borrow_mut() = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        }
    }

    pub(crate) fn take_cont(&self) -> bool {
        self.cont.replace(false)
    }

    pub(crate) async fn stop_until_cont(&self, sig: i32) {
        self.cont.set(false);
        self.stopped(sig);
        std::future::poll_fn(|cx| {
            if self.cont.replace(false) {
                return std::task::Poll::Ready(());
            }
            let mut ws = self.waiters.borrow_mut();
            if !ws.iter().any(|x| x.will_wake(cx.waker())) {
                ws.push(cx.waker().clone());
            }
            std::task::Poll::Pending
        })
        .await;
    }

    pub(crate) fn take_for(&self, w: &std::task::Waker) -> u64 {
        let mut ws = self.waiters.borrow_mut();
        if !ws.iter().any(|x| x.will_wake(w)) {
            ws.push(w.clone());
        }
        self.pending.replace(0)
    }

    /// Sends `sig` (Linux numbering): its waits that give way to signals wake.
    pub(crate) fn raise(&self, sig: i32) {
        if (1..64).contains(&sig) {
            self.pending.set(self.pending.get() | 1u64 << sig);
            for w in self.waiters.borrow_mut().drain(..) {
                w.wake();
            }
        }
    }
}

pub(crate) fn bash_path(core: &Core) -> Vec<u8> {
    core.own_program
        .clone()
        .unwrap_or_else(|| b"/bin/bash".to_vec())
}

impl Sigs {
    pub(crate) fn peek(&self, core: &Core) -> u64 {
        match self {
            Sigs::Shell => core.sched.peek_signals(),
            Sigs::Nested(n) => n.pending.get(),
        }
    }

    pub(crate) fn take(&self, core: &Core) -> u64 {
        match self {
            Sigs::Shell => core.sched.take_signals(),
            Sigs::Nested(n) => n.pending.replace(0),
        }
    }

    pub(crate) fn child_ended(&self, core: &Core) {
        match self {
            Sigs::Shell => core.sched.child_ended(),
            Sigs::Nested(n) if n.chld_trap.get() => {
                n.children.set(n.children.get() + 1);
                n.raise(crate::host::SIGCHLD);
            }
            Sigs::Nested(_) => {}
        }
    }

    pub(crate) fn set_chld_trap(&self, core: &Core, on: bool) {
        let flag = match self {
            Sigs::Shell => &core.sched.chld_trap,
            Sigs::Nested(n) => &n.chld_trap,
        };
        if on && !flag.get() {
            self.take_children(core);
        }
        flag.set(on);
    }

    pub(crate) fn take_children(&self, core: &Core) -> u32 {
        match self {
            Sigs::Shell => core.sched.take_children(),
            Sigs::Nested(n) => n.children.replace(0),
        }
    }

    /// Sends `sig` to the shell itself (`kill $$`, D1284).
    pub(crate) fn raise(&self, core: &Core, sig: i32) {
        match self {
            Sigs::Shell => {
                core.sched.raise(sig);
                core.sched.wake_signal_waiters();
            }
            Sigs::Nested(n) => n.raise(sig),
        }
    }

    /// Wakes `w` when a signal is sent to the shell (D1903).
    /// Waits give way to trapped signals too (an abandoned shell, D1411).
    pub(crate) fn group(&self) -> bool {
        matches!(self, Sigs::Nested(n) if n.group.get())
    }

    pub(crate) fn on_signal(&self, core: &Core, w: &std::task::Waker) {
        match self {
            Sigs::Shell => core.sched.on_signal(w),
            Sigs::Nested(n) => {
                let mut ws = n.waiters.borrow_mut();
                if !ws.iter().any(|x| x.will_wake(w)) {
                    ws.push(w.clone());
                }
            }
        }
    }

    /// The signal's default action ends the shell (D1903).
    pub(crate) fn died_of(&self, core: &Core, sig: i32) {
        match self {
            Sigs::Shell => core.died_of.set(Some(sig)),
            Sigs::Nested(n) => n.died_of.set(Some(sig)),
        }
    }
}

impl Core {
    /// Tells `catch` the signals the shell needs caught now, if that changed: those
    /// its traps need, and all of them while it waits for a foreground command. The
    /// others keep their default action, as in bash: a process that reads its own
    /// caught-signal mask sees only those.
    pub(crate) fn update_catch(&self) {
        let Some(catch) = &self.catch else {
            return;
        };
        let wants = if self.fg_waits.get() > 0 {
            u64::MAX
        } else {
            self.trap_wants.get()
        };
        if self.caught.replace(wants) != wants {
            catch(wants);
        }
    }

    pub(crate) fn next_job_id(&self) -> u32 {
        self.jobs.set(self.jobs.get() + 1);
        // Looks like a pid, cannot be one of ours.
        100_000 + self.jobs.get()
    }
}

/// A cache by a word's span.
pub(crate) type SpanMap<T> = RefCell<crate::vars::NameMap<(u32, u32), T>>;

/// A parsed chunk plus the line table of its source, for `line N:` in messages.
pub(crate) struct Code {
    pub ast: Arc<Ast>,
    pub lines: Rc<LineIndex>,
    /// Added to line numbers: a backquote body parsed on its own counts from the
    /// line of its command.
    pub line_base: u32,
    /// The arithmetic words of this chunk that are literal text, parsed
    /// (`arith::literal`), by the word's span.
    pub arith: SpanMap<Rc<crate::arith::Literal>>,
    /// The loops of this chunk compiled to flat code (`flat::compiled`), by span.
    pub flat: SpanMap<Option<Rc<crate::flat::Prog>>>,
}

impl Code {
    pub(crate) fn line(&self, offset: u32) -> u32 {
        // Text that came from an alias counts as the line of the alias word.
        let offset = if offset >= self.ast.script_len() {
            self.ast
                .original_span(psh_syntax::Span::new(offset, offset))
                .start
        } else {
            offset
        };
        self.lines.line(offset) + self.line_base
    }
}

/// How control leaves a command other than by finishing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unwind {
    /// The (sub)shell ends with this status: `exit`, a fatal error, a signal.
    Exit(i32),
    /// The rest of the top-level command line is abandoned, `$?` is this status, and
    /// the script goes on with the next line (bash's behavior for expansion errors,
    /// assignments to readonly variables and the like). Ends a subshell.
    Abort(i32),
    // Functions and loops (later language work) unwind with these.
    #[allow(dead_code)]
    Return(i32),
    #[allow(dead_code)]
    Break(u32),
    #[allow(dead_code)]
    Continue(u32),
}

impl Unwind {
    /// The status when this reaches the top of a (sub)shell.
    pub(crate) fn status(self, last: i32) -> i32 {
        match self {
            Unwind::Exit(s) | Unwind::Abort(s) | Unwind::Return(s) => s,
            Unwind::Break(_) | Unwind::Continue(_) => last,
        }
    }
}

pub(crate) type R = Result<i32, Unwind>;

pub(crate) use crate::vars::{Case, Value, Var, Vars};

/// `declare -l` / `-u` conversion.
pub(crate) fn apply_case(case: Case, value: Vec<u8>) -> Vec<u8> {
    match case {
        Case::None => value,
        Case::Capitalize => {
            let lower = apply_case(Case::Lower, value);
            match String::from_utf8(lower) {
                Ok(s) => {
                    let mut c = s.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().chain(c).collect::<String>().into_bytes(),
                        None => Vec::new(),
                    }
                }
                Err(e) => e.into_bytes(),
            }
        }
        Case::Lower | Case::Upper => match String::from_utf8(value) {
            Ok(s) if case == Case::Lower => s.to_lowercase().into_bytes(),
            Ok(s) => s.to_uppercase().into_bytes(),
            Err(e) => {
                let mut v = e.into_bytes();
                if case == Case::Lower {
                    v.make_ascii_lowercase();
                } else {
                    v.make_ascii_uppercase();
                }
                v
            }
        },
    }
}

/// Whether `s` is a valid variable name.
pub(crate) fn is_name(s: &[u8]) -> bool {
    !s.is_empty()
        && (s[0].is_ascii_alphabetic() || s[0] == b'_')
        && s.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// A virtual process: the shell or one of its subshells.
#[derive(Clone)]
pub(crate) struct Vp {
    pub core: Rc<Core>,
    pub vars: Vars,
    /// Environment entries inherited with names that are not shell variables; passed
    /// on to externals unchanged.
    pub foreign_env: Rc<Vec<(Vec<u8>, Vec<u8>)>>,
    pub arg0: Vec<u8>,
    pub args: Vec<Vec<u8>>,
    /// The logical working directory, absolute.
    pub cwd: Vec<u8>,
    pub fds: FdTable,
    /// `$?`
    pub status: i32,
    /// The line of the command running, for messages.
    pub line: u32,
    /// Nesting of subshells, substitutions and (later) function calls.
    pub depth: u32,
    /// `$!`: the newest background job.
    pub last_bg: Option<Rc<crate::jobs::Job>>,
    /// The background jobs this vproc started (`jobs`, `wait`); a subshell starts
    /// with none.
    pub jobs: Rc<RefCell<crate::jobs::JobTable>>,
    /// The job this vproc runs in, if it is part of one (`cmd &`).
    pub job: Option<Rc<crate::jobs::Job>>,
    /// This vproc's last act, a pipeline stage that is one simple command (the
    /// command's address): an external it runs takes over the vproc's descriptors, as
    /// bash execs it, so the vproc holds no pipe end the external closes (D1902). While
    /// that command's external starts, `exec_now` says so.
    pub exec_tail: Option<usize>,
    pub exec_now: bool,
    pub fg_text: Option<String>,
    /// A command substitution's vproc: its programs' signal deaths are not reported.
    pub in_subst: bool,
    pub ignores: u64,
    pub program: bool,
    /// Function definitions; shared with subshells until one defines or unsets.
    pub funcs: Rc<HashMap<String, Rc<Func>>>,
    /// One frame per running function call: the variables its `local`s shadow.
    pub frames: Vec<Frame>,
    /// Loops running in this function body (or at top level): `break`/`continue`.
    pub loops: u32,
    /// `getopts`' place inside a cluster of options (`-abc`): the `OPTIND` it was
    /// at, and the char offset in that argument.
    pub getopts: (i64, usize),
    /// Descriptors of the running command's process substitutions (`<(…)`), closed
    /// when it completes.
    pub subst_fds: Vec<u32>,
    pub last_procsub: Option<crate::sched::Join<i32>>,
    /// `{var}>` redirections of the running command: they outlast it (D1326), so the
    /// restore of its descriptors applies them again (`None`: closed).
    pub var_fds: Vec<(u32, Option<crate::fd::Fd>)>,
    /// This vproc is a pipeline stage that is a `( … )`: the subshell is the stage
    /// itself (bash counts it once in `BASH_SUBSHELL`).
    pub stage_subshell: bool,
    /// A pipeline stage's place at the stages' start gate, until its command runs
    /// (`exec::pass_gate`); a subshell of the stage has none.
    pub stage_gate: Option<Rc<crate::exec::GateTicket>>,
    pub time_subshell: Option<bool>,
    /// The last words of the simple commands running, innermost last: `$_` once each
    /// completes.
    pub last_args: Vec<Vec<u8>>,
    /// `$BASH_SUBSHELL`
    pub subshell_level: u32,
    /// The last arithmetic evaluation failed on an unset variable under `set -u`: the
    /// error ends the shell, as an unbound variable in a word does.
    pub arith_unbound: bool,
    /// The last arithmetic error was in the expression itself (syntax, division by 0),
    /// not from a variable it read.
    pub arith_syntax: bool,
    /// An array subscript failed to evaluate as arithmetic: bash abandons the whole
    /// script (or subshell) with status 1, not only the command line.
    pub sub_fatal: bool,
    /// The names of the functions `export -f` marked: a `bash` psh runs itself starts
    /// with them, and a program gets them as bash passes them (`BASH_FUNC_name%%`).
    pub exported_funcs: Rc<Vec<String>>,
    pub traced_funcs: Rc<Vec<String>>,
    pub readonly_funcs: Rc<Vec<String>>,
    /// `$BASHPID`: the shell's pid; `None` in a subshell or a nested `bash`, which are
    /// no processes: reading it there is an error (D1413).
    pub bashpid: Option<u32>,
    /// `$$`: the host's pid; `None` in a nested `bash`/`sh`, which has none (D1413).
    pub shell_pid: Option<u32>,
    /// The script's own arguments, the bottom of `BASH_ARGV`.
    pub script_args: Vec<Vec<u8>>,
    /// `alias` definitions; a subshell's own (bash keeps them per process).
    pub aliases: psh_syntax::Aliases,
    /// `shopt expand_aliases`: off, as in non-interactive bash.
    pub expand_aliases: bool,
    /// Running function calls and `source`d files, innermost last (`caller`, and
    /// later `FUNCNAME`, `BASH_SOURCE`, `BASH_LINENO`).
    pub calls: Vec<Call>,
    /// Remembered command paths (`hash`); shared with subshells until one changes it.
    pub hash: Rc<crate::builtins::hash::HashTable>,
    /// Limits set with `ulimit` (soft, hard), for what this vproc starts; the shell
    /// itself is not limited (D1293).
    pub rlimits: Vec<(crate::host::Rlimit, crate::host::Limit, crate::host::Limit)>,
    /// `pushd`'s directory stack below the current directory, top first.
    pub dirstack: Vec<Vec<u8>>,
    /// The file creation mask set with `umask`; `None`: the process's.
    pub umask: Option<u32>,
    /// Builtins turned off with `enable -n`.
    pub disabled: Rc<Vec<Vec<u8>>>,
    /// `set` and `shopt` options (D1280).
    pub opts: crate::options::Options,
    /// errexit and the ERR trap are suspended: a condition (`if`, `while`, `&&`/`||`
    /// except the last, `!`) is running, or a function or compound command called from
    /// one (D1281).
    pub no_errexit: bool,
    pub bad_assign: bool,
    pub err_trap_new: bool,
    /// Traps (D1283); shared with subshells until one changes them.
    pub traps: Rc<crate::builtins::trap::Traps>,
    /// A trap action is running, entered with this `$?`: a bare `exit` uses this
    /// status.
    pub in_trap: Option<i32>,
    pub signal_trap: bool,
    pub in_debug_trap: bool,
    pub in_err_trap: bool,
    pub own_sigs: Cell<u64>,
    /// This is the shell itself, not a subshell: it takes the shell's signals.
    pub top: bool,
    /// This vproc is a background job of one simple command that has not run it yet:
    /// if the command turns out to be no program, the job forks first (D1900).
    pub fork_job: bool,
    /// This vproc is a background job of one simple command that has not run it yet:
    /// a `%job` command word there is `bg`, not `fg`.
    pub bg_simple: bool,
    /// This vproc is a pipeline stage of one simple command that has not run it yet:
    /// bash looks up a `%job` command word only in a shell that has not forked for
    /// it, so there it is a plain command name.
    pub stage_simple: bool,
    /// A forked simple command's own address (a pipeline stage or `cmd &`): bash's child
    /// holds the EXIT trap it inherited until it runs the command, so an expansion error
    /// in the command's own words runs that trap there.
    pub own_simple: Option<usize>,
    /// The vproc this subshell was copied from ran an EXIT trap of its own (a trap a
    /// subshell only inherited is gone in bash's grandchildren).
    pub parent_exit: bool,
    /// Where the shell's pending signals are (D1411).
    pub sigs: Sigs,
    /// Levels of command substitution and `eval`: `set -x` repeats PS4's first char.
    pub xtrace_level: u32,
    pub cmd_name: Option<CmdName>,
    pub arith_notes: Vec<String>,
    /// Expanding subscripts reached during arithmetic (`arith::expanding`).
    pub arith_subst: crate::arith::Subst,
}

#[derive(Clone, Copy)]
pub(crate) enum CmdName {
    Builtin(&'static str),
    Function,
}

/// A function: its body command in the chunk that defined it.
pub(crate) struct Func {
    pub code: Rc<Code>,
    pub body: psh_syntax::CmdId,
    /// The definition itself (`f() …`), for `declare -f`.
    pub def: psh_syntax::CmdId,
    /// The file it was defined in (the sourced file, else `$0`).
    pub source: String,
}

/// A function call or a `source`d file, as `caller` reports it.
#[derive(Clone)]
pub(crate) struct Call {
    /// The function's name, or `source`.
    pub func: String,
    /// The file of the code running in it: where the function was defined, or the
    /// sourced file.
    pub source: String,
    /// The line it was called from.
    pub line: u32,
    /// A sourced file (`return` ends it; messages carry its name).
    pub sourced: bool,
    /// Its arguments (`BASH_ARGV`, `BASH_ARGC` under extdebug).
    pub args: Vec<Vec<u8>>,
}

/// A function call's locals: each shadowed variable with its value before the call
/// (`None`: it did not exist), restored when the call returns (dynamic scope).
#[derive(Clone, Default)]
pub(crate) struct Frame {
    pub options: Option<u32>,
    pub saved: Vec<(String, Option<Var>)>,
    /// The names of `saved` that are assignments in front of the call, not locals.
    pub temps: Vec<String>,
    pub builtin: bool,
}

impl Vp {
    pub(crate) fn host(&self) -> &dyn Host {
        &*self.core.host
    }

    /// A copy for a subshell, one level deeper. `break` and `continue` end it when it is
    /// in loops of the parent, except where the caller resets `loops` (`( )`, `&`).
    pub(crate) fn subshell(&self) -> Vp {
        let mut sub = self.clone();
        sub.depth += 1;
        sub.top = false;
        sub.own_sigs = Cell::new(0);
        sub.fork_job = false;
        sub.bg_simple = false;
        sub.stage_simple = false;
        sub.own_simple = None;
        sub.parent_exit = self
            .traps
            .map
            .get(&crate::builtins::trap::TrapKey::Exit)
            .is_some_and(|t| t.active && !t.action.is_empty());
        sub.traps = self.traps.for_subshell(
            self.opts.get(crate::options::SetOpt::Errtrace),
            self.opts.get(crate::options::SetOpt::Functrace)
                || self.opts.shopt(crate::options::Shopt::Extdebug),
        );
        sub.jobs = Default::default();
        sub.subshell_level += 1;
        sub.stage_subshell = false;
        sub.stage_gate = None;
        sub.time_subshell = None;
        sub.bashpid = None;
        sub
    }

    /// Refreshes a dynamic variable (`$RANDOM`, `$SECONDS`, …) before it is read.
    pub(crate) fn refresh_dynamic(&mut self, name: &str) {
        use crate::vars::Dynamic;
        let Some(d) = self.vars.get(name).and_then(|v| v.dynamic) else {
            return;
        };
        let now = self.core.host.now();
        let v = match d {
            Dynamic::Random => self.bash_random().to_string(),
            Dynamic::Srandom => ((self.next_random() >> 16) as u32).to_string(),
            Dynamic::Seconds => ((now.as_nanos() as i128 - self.core.seconds_base.get()).max(0)
                / 1_000_000_000)
                .to_string(),
            Dynamic::EpochSeconds => now.as_secs().to_string(),
            Dynamic::EpochRealtime => format!("{}.{:06}", now.as_secs(), now.subsec_micros()),
            Dynamic::FuncName
            | Dynamic::BashSource
            | Dynamic::BashLineno
            | Dynamic::BashArgv
            | Dynamic::BashArgc => {
                let items = self.call_stack(d);
                if let Some(var) = self.vars.get_mut(name) {
                    // bash shows an empty FUNCNAME as declared only, the others as `()`.
                    var.declared_only = items.is_empty() && d == Dynamic::FuncName;
                    var.value = Value::Indexed(
                        items
                            .into_iter()
                            .enumerate()
                            .map(|(i, v)| (i as i64, v))
                            .collect(),
                    );
                }
                return;
            }
            Dynamic::Groups => {
                let groups = self.core.host.info().groups;
                if let Some(var) = self.vars.get_mut(name) {
                    var.value = Value::Indexed(
                        groups
                            .iter()
                            .enumerate()
                            .map(|(i, g)| (i as i64, g.to_string().into_bytes()))
                            .collect(),
                    );
                }
                return;
            }
            Dynamic::BashArgv0 => {
                let a0 = self.arg0.clone();
                if let Some(var) = self.vars.get_mut(name) {
                    var.value = Value::Str(a0);
                }
                return;
            }
            Dynamic::BashAliases => {
                let mut a = crate::vars::Assoc::default();
                for (k, v) in self.aliases.iter() {
                    a.insert(k.to_vec(), v.to_vec());
                }
                if let Some(var) = self.vars.get_mut(name) {
                    var.value = Value::Assoc(a);
                }
                return;
            }
            Dynamic::BashMonoseconds => now.as_secs().to_string(),
            Dynamic::Lineno => self.line.to_string(),
            Dynamic::BashPid => self.bashpid.map(|p| p.to_string()).unwrap_or_default(),
            Dynamic::BashSubshell => self.subshell_level.to_string(),
            Dynamic::Dirstack => {
                let mut m = std::collections::BTreeMap::new();
                m.insert(0, self.cwd.clone());
                for (k, d) in self.dirstack.iter().enumerate() {
                    m.insert(k as i64 + 1, d.clone());
                }
                if let Some(var) = self.vars.get_mut(name) {
                    var.value = Value::Indexed(m);
                }
                return;
            }
        };
        if let Some(var) = self.vars.get_mut(name) {
            var.value = Value::Str(v.into_bytes());
        }
    }

    /// `FUNCNAME`, `BASH_SOURCE`, `BASH_LINENO`, `BASH_ARGV`, `BASH_ARGC`, innermost
    /// first. A script run from a file has a bottom frame: `main` (in `FUNCNAME` only
    /// below a call), its name, line 0.
    fn call_stack(&self, d: crate::vars::Dynamic) -> Vec<Vec<u8>> {
        use crate::vars::Dynamic;
        let file = !self.opts.dash_c && !self.opts.stdin;
        let calls = self.calls.iter().rev();
        let mut out: Vec<Vec<u8>> = match d {
            Dynamic::FuncName if self.calls.iter().all(|c| c.sourced) => Vec::new(),
            Dynamic::FuncName => calls.map(|c| c.func.clone().into_bytes()).collect(),
            Dynamic::BashSource => calls.map(|c| c.source.clone().into_bytes()).collect(),
            Dynamic::BashLineno => calls.map(|c| c.line.to_string().into_bytes()).collect(),
            Dynamic::BashArgc | Dynamic::BashArgv => {
                // Function frames under extdebug only; `source` always records its
                // arguments; the script's own frame is at the bottom (bash).
                let extdebug = self.opts.shopt(crate::options::Shopt::Extdebug);
                let mut frames: Vec<&[Vec<u8>]> = self
                    .calls
                    .iter()
                    .rev()
                    .filter(|c| c.sourced || extdebug)
                    .map(|c| &c.args[..])
                    .collect();
                if extdebug || !frames.is_empty() {
                    frames.push(&self.script_args);
                }
                return if d == Dynamic::BashArgc {
                    frames
                        .iter()
                        .map(|f| f.len().to_string().into_bytes())
                        .collect()
                } else {
                    frames
                        .iter()
                        .flat_map(|f| f.iter().rev().cloned())
                        .collect()
                };
            }
            _ => Vec::new(),
        };
        if file {
            match d {
                Dynamic::FuncName if !out.is_empty() => out.push(b"main".to_vec()),
                Dynamic::BashSource => out.push(self.arg0.clone()),
                Dynamic::BashLineno => out.push(b"0".to_vec()),
                _ => {}
            }
        }
        out
    }

    /// `$RANDOM`: bash's generator (5.1+), so a seeded sequence matches bash's.
    fn bash_random(&self) -> u32 {
        let mut seed = self.core.rseed.get().unwrap_or_else(|| {
            let now = self.core.host.now();
            (now.as_secs() as u32) ^ now.subsec_micros() ^ self.core.host.pid()
        });
        loop {
            seed = park_miller(seed);
            let rv = ((seed >> 16) ^ (seed & 0xffff)) & 0x7fff;
            if rv != self.core.last_random.get() {
                self.core.rseed.set(Some(seed));
                self.core.last_random.set(rv);
                return rv;
            }
        }
    }

    fn next_random(&self) -> u64 {
        // xorshift64*: any well-mixed sequence will do; bash's own is not reproduced.
        let mut x = self.core.rng.get();
        if x == 0 {
            x = 0x9e37_79b9_7f4a_7c15
                ^ u64::from(self.core.host.pid())
                ^ self.core.host.now().as_nanos() as u64;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.core.rng.set(x);
        x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 16
    }

    /// An assignment to a dynamic variable: `RANDOM=n` seeds, `SECONDS=n` restarts the
    /// count at n.
    pub(crate) fn assigned_dynamic(&mut self, name: &str) {
        use crate::vars::Dynamic;
        let Some(var) = self.vars.get(name) else {
            return;
        };
        let n: i64 = var
            .get()
            .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse().ok())
            .unwrap_or(0);
        match var.dynamic {
            Some(Dynamic::Random) => {
                self.core
                    .rng
                    .set((n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
                self.core.rseed.set(Some(n as u32));
                self.core.last_random.set(0);
            }
            Some(Dynamic::BashArgv0) => {
                if let Some(v) = var.get() {
                    self.arg0 = v.to_vec();
                }
            }
            Some(Dynamic::Seconds) => {
                let now = self.core.host.now().as_nanos() as i128;
                self.core
                    .seconds_base
                    .set(now - i128::from(n.max(0)) * 1_000_000_000);
            }
            _ => {}
        }
    }

    /// Whether a signal is ignored here (`trap '' SIG`): externals start with it ignored
    /// (D1284).
    pub(crate) fn signal_trap_ignored(&self, sig: i32) -> bool {
        self.traps
            .map
            .get(&crate::builtins::trap::TrapKey::Signal(sig))
            .is_some_and(|t| t.action.is_empty())
    }

    /// Makes `name` local to the innermost function call, or to the assignments in front
    /// of a builtin that bound it (saving its outer value once).
    pub(crate) fn make_local(&mut self, name: &str) {
        let old = self.vars.get(name).cloned();
        if let Some(k) = self.local_frame(name)
            && !self.frames[k].saved.iter().any(|(n, _)| n == name)
        {
            self.frames[k].saved.push((name.to_string(), old));
        }
    }

    pub(crate) fn local_frame(&self, name: &str) -> Option<usize> {
        self.frames
            .iter()
            .rposition(|f| !f.builtin || f.saved.iter().any(|(n, _)| n == name))
    }

    pub(crate) fn in_function(&self) -> bool {
        self.frames.iter().any(|f| !f.builtin)
    }

    pub(crate) fn fd(&self, n: u32) -> Option<Fd> {
        self.fds.get(n).cloned()
    }

    /// The variable a name refers to, following namerefs (`declare -n`); at most a few
    /// levels, as bash (a loop resolves to the last name seen).
    pub(crate) fn resolve<'n>(&self, name: &'n str) -> std::borrow::Cow<'n, str> {
        match self.resolve_checked(name) {
            Ok(n) => n,
            Err(_) => std::borrow::Cow::Borrowed(name),
        }
    }

    /// Like `resolve`, but a cycle or a chain longer than bash's 8 is an error: bash's
    /// warning text (`NAME: circular name reference`); the reference is then unset.
    pub(crate) fn resolve_checked<'n>(
        &self,
        name: &'n str,
    ) -> Result<std::borrow::Cow<'n, str>, String> {
        let mut cur = std::borrow::Cow::Borrowed(name);
        let mut seen: Vec<String> = Vec::new();
        loop {
            let next = match self.vars.get(&cur) {
                Some(v) if v.nameref => match v.get() {
                    Some(t) if !t.is_empty() => String::from_utf8_lossy(t).into_owned(),
                    _ => return Ok(cur),
                },
                _ => return Ok(cur),
            };
            // A reference to an element ends the chain; `target` splits it.
            if next.contains('[') {
                return Ok(std::borrow::Cow::Owned(next));
            }
            seen.push(cur.into_owned());
            if seen.contains(&next) {
                return Err(format!("{name}: circular name reference"));
            }
            if seen.len() > 8 {
                return Err(format!("{name}: maximum nameref depth (8) exceeded"));
            }
            cur = std::borrow::Cow::Owned(next);
        }
    }

    /// What a name refers to after namerefs: a variable, or an element of one
    /// (`declare -n r='a[2]'`): `(name, Some(subscript))`.
    pub(crate) fn target(&self, name: &str) -> Result<(String, Option<Vec<u8>>), String> {
        let r = self.resolve_checked(name)?;
        if let Some(open) = r.find('[')
            && r.ends_with(']')
        {
            return Ok((
                r[..open].to_string(),
                Some(self.simple_sub(r[open + 1..r.len() - 1].as_bytes())),
            ));
        }
        Ok((r.into_owned(), None))
    }

    /// A nameref's subscript where no async expansion is possible: quotes around it
    /// removed, `$name` and `${name}` substituted (the expander does the full job where
    /// it can, `expand::expand_sub_text`).
    pub(crate) fn simple_sub(&self, sub: &[u8]) -> Vec<u8> {
        let t = sub.trim_ascii();
        if t.len() >= 2 && (t[0] == b'"' || t[0] == b'\'') && t[t.len() - 1] == t[0] {
            return t[1..t.len() - 1].to_vec();
        }
        let name = t
            .strip_prefix(b"${")
            .and_then(|n| n.strip_suffix(b"}"))
            .or_else(|| t.strip_prefix(b"$"));
        if let Some(n) = name
            && is_name(n)
        {
            return self
                .var(&String::from_utf8_lossy(n))
                .unwrap_or_default()
                .to_vec();
        }
        sub.to_vec()
    }

    /// An element by a subscript that needs no arithmetic (a number, an associative
    /// key): for read-only lookups through namerefs.
    fn elem_literal(&self, name: &str, sub: &[u8]) -> Option<&[u8]> {
        match &self.vars.get(name)?.value {
            Value::Assoc(a) => a.get(sub),
            Value::Indexed(m) => {
                let i: i64 = std::str::from_utf8(sub).ok()?.trim().parse().ok()?;
                let i = if i < 0 {
                    m.keys().next_back()? + 1 + i
                } else {
                    i
                };
                m.get(&i).map(Vec::as_slice)
            }
            Value::Str(s) if sub == b"0" => Some(s),
            _ => None,
        }
    }

    /// The scalar value of a variable (element 0 of an array), following namerefs.
    pub(crate) fn var(&self, name: &str) -> Option<&[u8]> {
        // A name that is no reference is its own target, and `target` would copy it.
        match self.vars.get(name) {
            Some(v) if !v.nameref => return v.get(),
            None if !name.contains('[') => return None,
            _ => {}
        }
        match self.target(name) {
            Ok((n, Some(sub))) => self.elem_literal(&n, &sub),
            Ok((n, None)) => self.vars.value(&n),
            Err(_) => None,
        }
    }

    /// The variable itself, following namerefs.
    pub(crate) fn var_ref(&self, name: &str) -> Option<&Var> {
        let name = self.resolve(name);
        self.vars.get(&name)
    }

    /// Whether a variable has a value (`test -v`): a set scalar, or element 0 / any
    /// element of an array (bash: `-v a` is element 0).
    pub(crate) fn is_set(&self, name: &str) -> bool {
        // `a[sub]`: an element; numeric subscripts only for indexed arrays.
        if let Some(open) = name.find('[')
            && name.ends_with(']')
        {
            let base = self.resolve(&name[..open]).into_owned();
            let sub = &name[open + 1..name.len() - 1];
            return match self.vars.get(&base).map(|v| &v.value) {
                Some(Value::Assoc(a)) => match sub {
                    "@" | "*" => a.len() > 0,
                    k => a.get(k.as_bytes()).is_some(),
                },
                Some(Value::Indexed(m)) => match sub {
                    "@" | "*" => !m.is_empty(),
                    k => k.trim().parse::<i64>().ok().is_some_and(|i| {
                        let i = if i < 0 {
                            m.keys().next_back().map_or(-1, |mx| mx + 1 + i)
                        } else {
                            i
                        };
                        m.contains_key(&i)
                    }),
                },
                Some(Value::Str(_)) => matches!(sub, "0" | "@" | "*"),
                _ => false,
            };
        }
        match self.target(name) {
            Ok((n, Some(sub))) => {
                let mut full = n;
                full.push('[');
                full.push_str(&String::from_utf8_lossy(&sub));
                full.push(']');
                self.is_set(&full)
            }
            Ok((n, None)) => self.vars.value(&n).is_some(),
            Err(_) => false,
        }
    }

    /// Sets the scalar value (or element 0) without any checks: for the shell's own
    /// variables (`OPTIND`, `PWD`, …). Assignments a script makes go through `assign`.
    pub(crate) fn set_var(&mut self, name: &str, value: Vec<u8>) {
        let name = self.resolve(name).into_owned();
        let allexport = self.opts.get(crate::options::SetOpt::Allexport);
        let var = self.vars.entry(&name);
        var.set(value);
        // `set -a`: every variable assigned is exported.
        var.exported |= allexport;
    }

    /// An assignment `name=value`: readonly check, `-i` arithmetic, `-l`/`-u` case,
    /// namerefs. The error is bash's message without the `line N:` prefix.
    pub(crate) fn assign(&mut self, name: &str, value: Vec<u8>) -> Result<(), String> {
        self.assign_with(name, value, false)
    }

    /// Whether `name+=value` appends to the variable's own string in place: a set,
    /// plain scalar that no attribute, reference or computed value changes.
    pub(crate) fn appends_in_place(&self, name: &str) -> bool {
        self.vars.get(name).is_some_and(|v| {
            !v.nameref
                && !v.readonly
                && !v.integer
                && v.case == Case::None
                && !v.declared_only
                && v.dynamic.is_none()
                && matches!(v.value, crate::vars::Value::Str(_))
        })
    }

    /// `name=value` or `name+=value`.
    pub(crate) fn assign_with(
        &mut self,
        name: &str,
        value: Vec<u8>,
        append: bool,
    ) -> Result<(), String> {
        // A name that is no reference is its own target, and `target` would copy it;
        // without an attribute that changes what is assigned, the value goes in as it is.
        let (own, plain) = match self.vars.get(name) {
            Some(v) => (
                !v.nameref,
                !v.readonly && !v.integer && v.case == Case::None,
            ),
            None => (!name.contains('['), true),
        };
        // `name+=value` on a plain string: append in place rather than copying the old
        // value, so building a string in a loop stays linear.
        if append && self.appends_in_place(name) {
            let allexport = self.opts.get(crate::options::SetOpt::Allexport);
            if let Some(var) = self.vars.get_mut(name)
                && let crate::vars::Value::Str(s) = &mut var.value
            {
                s.extend_from_slice(&value);
                var.declared_only = false;
                var.exported |= allexport;
                return Ok(());
            }
        }
        if own {
            return self.assign_target(name, value, append, plain && !append);
        }
        let (name, sub) = match self.target(name) {
            Ok(t) => t,
            // An assignment through a circular reference fails (bash warns).
            Err(msg) => return Err(format!("warning: {msg}")),
        };
        if let Some(sub) = sub {
            return self.assign_elem(&name, &sub, value, append);
        }
        self.assign_target(&name, value, append, false)
    }

    /// `assign_with` once namerefs are followed: `name` is the variable itself.
    /// `as_is`: the caller saw that `prepare_value` would return the value unchanged.
    fn assign_target(
        &mut self,
        name: &str,
        value: Vec<u8>,
        append: bool,
        as_is: bool,
    ) -> Result<(), String> {
        let value = match as_is {
            true => value,
            false => self.prepare_value(name, value, append)?,
        };
        let allexport = self.opts.get(crate::options::SetOpt::Allexport);
        let var = self.vars.entry(name);
        var.set(value);
        var.exported |= allexport;
        if var.dynamic.is_some() {
            self.assigned_dynamic(name);
        }
        Ok(())
    }

    /// Applies the target's attributes to a value about to be assigned (`append`:
    /// `+=`, which for `-i` adds).
    pub(crate) fn prepare_value(
        &mut self,
        name: &str,
        value: Vec<u8>,
        append: bool,
    ) -> Result<Vec<u8>, String> {
        if self.vars.get(name).is_some_and(|v| v.readonly) {
            return Err(format!("{name}: readonly variable"));
        }
        self.prepare_value_unchecked(name, value, append)
    }

    /// `prepare_value` without the readonly check (for assignments in front of a
    /// command, which bash checks when it applies them).
    pub(crate) fn prepare_value_unchecked(
        &mut self,
        name: &str,
        value: Vec<u8>,
        append: bool,
    ) -> Result<Vec<u8>, String> {
        if append {
            self.refresh_dynamic(name);
        }
        let (integer, case) = match self.vars.get(name) {
            Some(v) => (v.integer, v.case),
            None => (false, Case::None),
        };
        let mut value = value;
        if integer {
            let mut n = crate::arith::eval(self, &value)?;
            if append {
                let old = self
                    .vars
                    .value(name)
                    .map(<[u8]>::to_vec)
                    .unwrap_or_default();
                n = n.wrapping_add(crate::arith::eval(self, &old)?);
            }
            value = n.to_string().into_bytes();
        } else if append {
            let mut old = self
                .vars
                .value(name)
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            old.extend_from_slice(&value);
            value = old;
        }
        Ok(apply_case(case, value))
    }

    /// How patterns match here: the locale, `extglob`, and case folding (`nocaseglob`
    /// or `nocasematch`, as the caller says) (D1321).
    pub(crate) fn match_opts(&self, nocase: bool) -> crate::pattern::MatchOpts {
        crate::pattern::MatchOpts {
            utf8: self.utf8(),
            extglob: self.opts.shopt(crate::options::Shopt::Extglob),
            nocase,
        }
    }

    /// Whether the locale is UTF-8 (`LC_ALL`, `LC_CTYPE`, `LANG`, first set wins).
    pub(crate) fn utf8(&self) -> bool {
        for name in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Some(v) = self.var(name)
                && !v.is_empty()
            {
                let v = v.to_ascii_lowercase();
                return v.windows(5).any(|w| w == b"utf-8") || v.windows(4).any(|w| w == b"utf8");
            }
        }
        // No locale at all: bash runs in the C locale.
        false
    }

    pub(crate) fn collates(&self) -> bool {
        for name in ["LC_ALL", "LC_COLLATE", "LANG"] {
            if let Some(v) = self.var(name)
                && !v.is_empty()
            {
                return !(v == b"C" || v == b"POSIX" || v.starts_with(b"C."));
            }
        }
        false
    }

    /// The environment for an external: exported variables with values, plus
    /// `extra` (assignments in front of the command), which win.
    pub(crate) fn env(&self, extra: &[(String, Vec<u8>)]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut env: Vec<(Vec<u8>, Vec<u8>)> = self.foreign_env.as_ref().clone();
        for (name, var) in self.vars.iter() {
            // Arrays are not exported (bash).
            if var.exported
                && let Value::Str(v) = &var.value
                && !extra.iter().any(|(n, _)| n == name)
            {
                env.push((name.as_bytes().to_vec(), v.clone()));
            }
        }
        for (n, v) in extra {
            env.push((n.as_bytes().to_vec(), v.clone()));
        }
        // Exported functions, as bash passes them: `BASH_FUNC_name%%=() { body }`.
        for name in self.exported_funcs.iter() {
            if let Some(f) = self.funcs.get(name) {
                let text = psh_syntax::print_function_bash(&f.code.ast, f.def);
                if let Some((_, body)) = text.split_once("() ") {
                    let body = body.trim_start_matches('\n');
                    env.push((
                        format!("BASH_FUNC_{name}%%").into_bytes(),
                        format!("() {body}").into_bytes(),
                    ));
                }
            }
        }
        env.sort();
        env
    }

    /// The name in front of messages: the file of the innermost function or sourced
    /// file (bash's `BASH_SOURCE[0]`), else `$0` (a script psh runs itself has its own,
    /// D1300).
    pub(crate) fn msg_name(&self) -> String {
        match self.calls.last() {
            Some(c) => c.source.clone(),
            None => String::from_utf8_lossy(&self.arg0).into_owned(),
        }
    }

    /// Writes a message to fd 2, as `NAME: line N: MSG`. Write errors are ignored, as
    /// bash does.
    pub(crate) async fn error(&self, msg: &str) {
        let text = format!("{}: line {}: {}\n", self.msg_name(), self.line, msg);
        self.error_raw(text.as_bytes()).await;
    }

    pub(crate) async fn error_raw(&self, text: &[u8]) {
        if let Some(fd) = self.fd(2) {
            let _ = fd.write_all(text).await;
        }
    }

    /// An absolute path for `p`, relative to the cwd.
    pub(crate) fn abs_path(&self, p: &[u8]) -> Vec<u8> {
        if p.first() == Some(&b'/') || is_windows_abs(p) {
            p.to_vec()
        } else {
            let mut out = self.cwd.clone();
            if out.last() != Some(&b'/') {
                out.push(b'/');
            }
            out.extend_from_slice(p);
            out
        }
    }
}

/// One step of bash's `intrand32`: Park–Miller's minimal standard, by Schrage's method.
fn park_miller(last: u32) -> u32 {
    let last = if last == 0 { 123_459_876 } else { i64::from(last) };
    let (h, l) = (last / 127_773, last % 127_773);
    let t = 16_807 * l - 2_836 * h;
    (if t < 0 { t + 0x7fff_ffff } else { t }) as u32
}

/// `C:/x` or `C:\x`.
fn is_windows_abs(p: &[u8]) -> bool {
    cfg!(windows)
        && p.len() >= 3
        && p[0].is_ascii_alphabetic()
        && p[1] == b':'
        && (p[2] == b'/' || p[2] == b'\\')
}
