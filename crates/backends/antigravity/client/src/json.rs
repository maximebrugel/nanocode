use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    Str(String),
    Raw(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl Json {
    pub fn object<K: Into<String>>(fields: impl IntoIterator<Item = (K, Self)>) -> Self {
        Self::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    pub fn int(number: i64) -> Self {
        Self::Number(number.into())
    }

    pub fn sorted(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(flag) => Self::Bool(*flag),
            Value::Number(number) => Self::Number(number.clone()),
            Value::String(text) => Self::Str(text.clone()),
            Value::Array(items) => Self::Array(items.iter().map(Self::sorted).collect()),
            Value::Object(map) => {
                let mut fields = map
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::sorted(value)))
                    .collect::<Vec<_>>();
                fields.sort_by(|left, right| left.0.cmp(&right.0));
                Self::Object(fields)
            }
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(true) => out.push_str("true"),
            Self::Bool(false) => out.push_str("false"),
            Self::Number(number) => out.push_str(&number.to_string()),
            Self::Str(text) => write_str(text, out),
            Self::Raw(raw) => out.push_str(raw),
            Self::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write_str(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(text: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(text).unwrap_or_default());
}
