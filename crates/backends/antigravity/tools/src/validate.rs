use serde_json::Value;

pub fn validate(declaration: &str, args: &Value) -> Result<(), String> {
    let Ok(declaration) = serde_json::from_str::<Value>(declaration) else {
        return Ok(());
    };
    let parameters = &declaration["parameters"];
    let empty = serde_json::Map::new();
    let properties = parameters["properties"].as_object().unwrap_or(&empty);
    let given = args.as_object().unwrap_or(&empty);
    let missing = parameters["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| !given.contains_key(*name))
        .collect::<Vec<_>>();
    let mut extra = given
        .keys()
        .filter(|name| !properties.contains_key(name.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    extra.sort_unstable();
    let mut lines = Vec::new();
    if !missing.is_empty() {
        lines.push(format!(
            "- missing {} {}",
            plural(missing.len(), "property", "properties"),
            quoted(&missing)
        ));
    }
    if !extra.is_empty() {
        lines.push(format!(
            "- additional properties {} not allowed",
            quoted(&extra)
        ));
    }
    for (name, value) in given {
        if let Some(kind) = properties
            .get(name)
            .and_then(|schema| schema["type"].as_str())
            && !matches_type(kind, value)
        {
            lines.push(format!(
                "- at '/{name}': got {}, want {}",
                type_name(value),
                kind.to_lowercase()
            ));
        }
    }
    match lines.is_empty() {
        true => Ok(()),
        false => Err(format!("invalid arguments:\n{}", lines.join("\n"))),
    }
}

const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    match count {
        1 => one,
        _ => many,
    }
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn matches_type(kind: &str, value: &Value) -> bool {
    match kind {
        "STRING" => value.is_string(),
        "INTEGER" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        "NUMBER" => value.is_number(),
        "BOOLEAN" => value.is_boolean(),
        "ARRAY" => value.is_array(),
        "OBJECT" => value.is_object(),
        _ => true,
    }
}

const fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
