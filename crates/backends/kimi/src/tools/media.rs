use super::{Builtins, Outcome};
use serde_json::Value;

pub async fn read(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("ReadMediaFile is not ported yet")
}
