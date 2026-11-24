use super::{Builtins, Outcome};
use serde_json::Value;

pub async fn glob(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("Glob is not ported yet")
}

pub async fn grep(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("Grep is not ported yet")
}
