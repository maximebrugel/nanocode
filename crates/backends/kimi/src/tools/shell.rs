use super::{Builtins, Outcome};
use serde_json::Value;
use std::path::Path;

/// Bash and the background tasks it starts.
pub struct Shell {}

impl Shell {
    pub fn new(_cwd: &Path) -> Self {
        Self {}
    }

    pub async fn bash(&self, _builtins: &Builtins, _args: &Value) -> Outcome {
        Outcome::error("Bash is not ported yet")
    }

    pub fn list(&self, _args: &Value) -> Outcome {
        Outcome::error("TaskList is not ported yet")
    }

    pub fn output(&self, _args: &Value) -> Outcome {
        Outcome::error("TaskOutput is not ported yet")
    }

    pub async fn stop(&self, _args: &Value) -> Outcome {
        Outcome::error("TaskStop is not ported yet")
    }

    pub async fn wait(&self, _args: &Value) -> Outcome {
        Outcome::error("WaitFor is not ported yet")
    }

    pub fn notifications(&self) -> Vec<String> {
        Vec::new()
    }

    pub async fn shutdown(&self) {}
}
