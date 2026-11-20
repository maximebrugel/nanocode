use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub dir: String,
    pub library: bool,
    pub dependencies: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub root: String,
    pub packages: BTreeMap<String, Package>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GraphError {
    Json(String),
    Field(&'static str),
}

fn field<'a>(value: &'a Value, name: &'static str) -> Result<&'a Value, GraphError> {
    value.get(name).ok_or(GraphError::Field(name))
}

fn text<'a>(value: &'a Value, name: &'static str) -> Result<&'a str, GraphError> {
    field(value, name)?.as_str().ok_or(GraphError::Field(name))
}

impl Graph {
    pub fn from_metadata(json: &str) -> Result<Self, GraphError> {
        let metadata: Value =
            serde_json::from_str(json).map_err(|error| GraphError::Json(error.to_string()))?;
        let root = format!("{}/", text(&metadata, "workspace_root")?);
        let members: BTreeSet<&str> = field(&metadata, "workspace_members")?
            .as_array()
            .ok_or(GraphError::Field("workspace_members"))?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let mut packages = BTreeMap::new();
        for package in field(&metadata, "packages")?
            .as_array()
            .ok_or(GraphError::Field("packages"))?
        {
            if !members.contains(text(package, "id")?) {
                continue;
            }
            let name = text(package, "name")?.to_owned();
            let manifest = text(package, "manifest_path")?;
            let dir = manifest
                .strip_prefix(root.as_str())
                .unwrap_or(manifest)
                .trim_end_matches("Cargo.toml")
                .to_owned();
            let library = field(package, "targets")?
                .as_array()
                .ok_or(GraphError::Field("targets"))?
                .iter()
                .filter_map(|target| target.get("kind").and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .any(|kind| kind == "lib" || kind == "rlib");
            let dependencies = field(package, "dependencies")?
                .as_array()
                .ok_or(GraphError::Field("dependencies"))?
                .iter()
                .filter(|dependency| dependency.get("path").is_some_and(Value::is_string))
                .filter_map(|dependency| dependency.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            packages.insert(
                name.clone(),
                Package {
                    name,
                    dir,
                    library,
                    dependencies,
                },
            );
        }
        Ok(Self { root, packages })
    }

    pub fn owner(&self, path: &str) -> Option<&Package> {
        self.packages
            .values()
            .filter(|package| path.starts_with(package.dir.as_str()))
            .max_by_key(|package| package.dir.len())
    }

    pub fn dependents(&self) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut dependents: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for package in self.packages.values() {
            for dependency in &package.dependencies {
                if let Some(dependency) = self.packages.get(dependency) {
                    dependents
                        .entry(dependency.name.as_str())
                        .or_default()
                        .insert(package.name.as_str());
                }
            }
        }
        dependents
    }

    pub fn with_dependents<'a>(&'a self, seeds: &BTreeSet<&'a str>) -> BTreeSet<&'a str> {
        let dependents = self.dependents();
        let mut reached = seeds.clone();
        let mut todo: Vec<&str> = seeds.iter().copied().collect();
        while let Some(name) = todo.pop() {
            for dependent in dependents.get(name).into_iter().flatten() {
                if reached.insert(dependent) {
                    todo.push(dependent);
                }
            }
        }
        reached
    }
}
