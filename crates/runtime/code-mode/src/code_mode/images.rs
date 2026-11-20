use serde_json::Value;

use crate::code_mode_description::image_reader;

const READER_FIELD: &str = "image_reader";

fn reader(tool: &Value) -> Option<(&str, &'static str)> {
    let name = tool
        .get("tool_name")
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)?;
    image_reader(name).map(|field| (name, field))
}

pub(super) fn with_reader(metadata: &[Value]) -> Vec<Value> {
    let mut tools = metadata.to_vec();
    if let Some((index, field)) = tools
        .iter()
        .enumerate()
        .find_map(|(index, tool)| reader(tool).map(|(_, field)| (index, field)))
        && let Some(Value::Object(fields)) = tools.get_mut(index)
    {
        fields.insert(READER_FIELD.to_owned(), Value::from(field));
    }
    tools
}
