use std::fmt;

use psh_classify::classify;
use serde_json::Value;

use crate::{
    HandRef, Target, Workspace,
    sim::{self, Mutation},
};

pub const AUTO: &str = nanocode_code_mode::embedded::AUTO;

const DEFINED_FUNCTION: &str = "calls a function defined in the script";
const DYNAMIC_NAME: &str = "the command name is dynamic";
const BY_PATH: &str = "runs a program given by its path";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Shell {
    pub tool: &'static str,
    pub script: &'static str,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Auto {
    pub shells: &'static [Shell],
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Origin {
    Explicit,
    Auto,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Reason {
    Explicit,
    Tool,
    Script,
    NoTool,
    NoScript,
    Unparsed,
    Dynamic,
    ByPath(Box<str>),
    NoCommand(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    pub target: Target,
    pub reason: Reason,
}

impl Reason {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Tool => "tool",
            Self::Script => "script",
            Self::NoTool => "no_tool",
            Self::NoScript => "no_script",
            Self::Unparsed => "unparsed",
            Self::Dynamic => "dynamic",
            Self::ByPath(_) => "by_path",
            Self::NoCommand(_) => "no_command",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit => formatter.write_str("the call named its target"),
            Self::Tool => formatter.write_str("the workspace runs this tool"),
            Self::Script => formatter.write_str("the workspace has every command in the script"),
            Self::NoTool => formatter.write_str("the workspace cannot run this tool"),
            Self::NoScript => formatter.write_str("the call carries no script to read"),
            Self::Unparsed => formatter.write_str("the script did not parse"),
            Self::Dynamic => formatter.write_str("the script names a command at run time"),
            Self::ByPath(name) => write!(formatter, "the script runs `{name}` by its path"),
            Self::NoCommand(name) => write!(formatter, "the workspace has no `{name}`"),
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.target, self.reason)
    }
}

#[must_use]
pub fn decide(
    explicit: Option<Target>,
    tool: &str,
    input: &Value,
    auto: &Auto,
    workspace: &dyn Workspace,
) -> Decision {
    if let Some(target) = explicit {
        return Decision {
            target,
            reason: Reason::Explicit,
        };
    }
    let reason = fit(tool, input, auto, workspace);
    let here =
        matches!(reason, Reason::Tool | Reason::Script) && !sim::mutated(Mutation::AutoPrefersHand);
    Decision {
        target: if here {
            Target::Workspace
        } else {
            Target::Hand(HandRef::Hosted)
        },
        reason,
    }
}

fn fit(tool: &str, input: &Value, auto: &Auto, workspace: &dyn Workspace) -> Reason {
    if !workspace.runs(tool) {
        return Reason::NoTool;
    }
    let Some(shell) = auto.shells.iter().find(|shell| shell.tool == tool) else {
        return Reason::Tool;
    };
    let Some(script) = input.get(shell.script).and_then(Value::as_str) else {
        return Reason::NoScript;
    };
    commands(script, workspace)
}

fn commands(script: &str, workspace: &dyn Workspace) -> Reason {
    let found = classify(script);
    if found.error.is_some() {
        return Reason::Unparsed;
    }
    for command in &found.commands {
        let Some(first) = command.words.first() else {
            continue;
        };
        match (command.name.as_deref(), command.reason) {
            (_, Some(DEFINED_FUNCTION)) => {}
            (None, _) | (_, Some(DYNAMIC_NAME)) => return Reason::Dynamic,
            (Some(_), Some(BY_PATH)) => return Reason::ByPath(first.text.as_str().into()),
            (Some(name), _) if !workspace.has(name) => return Reason::NoCommand(name.into()),
            (Some(_), _) => {}
        }
    }
    Reason::Script
}
