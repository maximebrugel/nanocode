use std::collections::BTreeSet;

use crate::graph::Graph;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Everything,
    Crates(BTreeSet<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    pub everything_files: Vec<String>,
    pub everything_prefixes: Vec<String>,
}

impl Rules {
    pub fn cargo() -> Self {
        Self {
            everything_files: [
                "Cargo.toml",
                "Cargo.lock",
                "rust-toolchain",
                "rust-toolchain.toml",
            ]
            .map(str::to_owned)
            .to_vec(),
            everything_prefixes: [".cargo/", "scripts/check"].map(str::to_owned).to_vec(),
        }
    }
}

pub fn affected(graph: &Graph, rules: &Rules, changed: &[String]) -> Scope {
    if changed.iter().any(|path| {
        rules.everything_files.iter().any(|file| file == path)
            || rules
                .everything_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
    }) {
        return Scope::Everything;
    }
    let touched: BTreeSet<&str> = changed
        .iter()
        .filter_map(|path| graph.owner(path))
        .map(|package| package.name.as_str())
        .collect();
    Scope::Crates(
        graph
            .with_dependents(&touched)
            .into_iter()
            .map(str::to_owned)
            .collect(),
    )
}
