use super::{Builtins, Outcome};
use serde_json::Value;

pub async fn fetch(_builtins: &Builtins, _args: &Value) -> Outcome {
    Outcome::error("FetchURL is not ported yet")
}
