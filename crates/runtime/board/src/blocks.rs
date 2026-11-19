use nanocode_schema::{Compiler, Draft, Schemas};
use serde_json::Value;

use crate::messages::{Block, Refusal};

const LOCATION: &str = "board-schema:///";
const LEVELS: u64 = 16;
const NODES: u64 = 512;
const SUBSCHEMAS: u64 = 64;
const INDENT: usize = 3;
const BACKTICK: u8 = 0x60;

const KEYWORDS: [&str; 26] = [
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "const",
    "oneOf",
    "anyOf",
    "allOf",
    "not",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "minProperties",
    "maxProperties",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "format",
    "title",
    "description",
    "$schema",
];

const FORMATS: [&str; 9] = [
    "date-time", "date", "time", "email", "uri", "uuid", "hostname", "ipv4", "ipv6",
];

#[derive(Default)]
struct Size {
    nodes: u64,
    schemas: u64,
}

fn walk(schema: &Value, depth: u64, size: &mut Size) -> Result<(), String> {
    if depth > LEVELS {
        return Err(format!("a schema nests deeper than {LEVELS} levels"));
    }
    let Value::Object(map) = schema else {
        return Err(format!("a subschema at depth {depth} is not an object"));
    };
    size.schemas += 1;
    for (key, value) in map {
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(format!("{key} is not a structural keyword"));
        }
        size.nodes += 1;
        match key.as_str() {
            "properties" => {
                let Value::Object(properties) = value else {
                    return Err("properties is not an object".to_owned());
                };
                for property in properties.values() {
                    walk(property, depth + 1, size)?;
                }
            }
            "items" | "not" => walk(value, depth + 1, size)?,
            "additionalProperties" if !value.is_boolean() => walk(value, depth + 1, size)?,
            "oneOf" | "anyOf" | "allOf" => match value {
                Value::Array(options) if !options.is_empty() => {
                    for option in options {
                        walk(option, depth + 1, size)?;
                    }
                }
                _ => return Err(format!("{key} is not a list of schemas")),
            },
            "enum" => match value {
                Value::Array(values) => size.nodes += values.len() as u64,
                _ => return Err("enum is not a list".to_owned()),
            },
            "required" => {
                if !value.as_array().is_some_and(|names| names.iter().all(Value::is_string)) {
                    return Err("required is not a list of names".to_owned());
                }
            }
            "format" if !value.as_str().is_some_and(|format| FORMATS.contains(&format)) => {
                return Err(format!("format {value} is not one the board checks"));
            }
            _ => {}
        }
    }
    if size.nodes > NODES {
        return Err(format!("a schema holds more than {NODES} nodes"));
    }
    if size.schemas > SUBSCHEMAS {
        return Err(format!("a schema holds more than {SUBSCHEMAS} subschemas"));
    }
    Ok(())
}

pub(crate) fn definition(definition: &Value) -> Result<(), String> {
    if definition.get("type").and_then(Value::as_str) != Some("object") {
        return Err("a schema describes an object".to_owned());
    }
    walk(definition, 0, &mut Size::default())?;
    compile(definition).map(drop)
}

fn compile(definition: &Value) -> Result<(Schemas, nanocode_schema::SchemaIndex), String> {
    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler.enable_format_assertions();
    compiler
        .add_resource(LOCATION, definition.clone())
        .map_err(|error| error.to_string())?;
    let mut schemas = Schemas::new();
    let index = compiler
        .compile(LOCATION, &mut schemas)
        .map_err(|error| error.to_string())?;
    Ok((schemas, index))
}

enum Nest {
    Top,
    Quote,
    Item(usize),
}

struct Fence<'a> {
    mark: u8,
    len: usize,
    block: Option<String>,
    body: Vec<&'a str>,
    nest: Nest,
}

fn indent(line: &str) -> usize {
    line.bytes().take_while(|byte| *byte == b' ').count()
}

fn dedent(line: &str, width: usize) -> Option<&str> {
    (indent(line) >= width).then(|| &line[width..])
}

fn quote(line: &str) -> Option<&str> {
    if indent(line) > INDENT {
        return None;
    }
    let rest = line.trim_start_matches(' ').strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn item(line: &str) -> Option<(usize, &str)> {
    let lead = indent(line);
    if lead > INDENT {
        return None;
    }
    let rest = &line[lead..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let marker = match rest.as_bytes().get(digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => 1,
        Some(b'.' | b')') if (1..=9).contains(&digits) => digits + 1,
        _ => return None,
    };
    let after = &rest[marker..];
    let gap = indent(after);
    if gap == 0 && !after.is_empty() {
        return None;
    }
    let gap = if gap > 4 || after.trim().is_empty() { 1 } else { gap };
    Some((lead + marker + gap, after.get(gap..).unwrap_or("")))
}

fn opener(line: &str) -> Option<(u8, usize, &str)> {
    if indent(line) > INDENT {
        return None;
    }
    let rest = line.trim_start_matches(' ');
    let mark = *rest.as_bytes().first()?;
    if mark != BACKTICK && mark != b'~' {
        return None;
    }
    let len = rest.bytes().take_while(|byte| *byte == mark).count();
    let info = rest[len..].trim();
    (len >= 3 && !(mark == BACKTICK && info.bytes().any(|byte| byte == BACKTICK))).then_some((mark, len, info))
}

fn closes(line: &str, mark: u8, len: usize) -> bool {
    let rest = line.trim_start_matches(' ');
    let run = rest.bytes().take_while(|byte| *byte == mark).count();
    indent(line) <= INDENT && run >= len && rest[run..].trim().is_empty()
}

fn nested<'a>(nest: Nest, rest: &str) -> Option<Fence<'a>> {
    opener(rest).map(|(mark, len, _)| Fence {
        mark,
        len,
        block: None,
        body: Vec::new(),
        nest,
    })
}

pub(crate) fn fences(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut found = Vec::new();
    let mut open: Option<Fence<'_>> = None;
    let mut list: Option<usize> = None;
    for line in text.lines() {
        if let Some(fence) = &mut open {
            let inner = match fence.nest {
                Nest::Top => Some(line),
                Nest::Quote => quote(line),
                Nest::Item(_) if line.trim().is_empty() => Some(""),
                Nest::Item(width) => dedent(line, width),
            };
            if let Some(inner) = inner {
                if closes(inner, fence.mark, fence.len) {
                    if let Some(id) = fence.block.take() {
                        found.push((id, fence.body.join("\n")));
                    }
                    open = None;
                } else {
                    fence.body.push(inner);
                }
                continue;
            }
            open = None;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = quote(line) {
            list = None;
            open = nested(Nest::Quote, rest);
            continue;
        }
        if let Some((width, rest)) = item(line) {
            list = Some(width);
            open = nested(Nest::Item(width), rest);
            continue;
        }
        if let Some(width) = list
            && let Some(rest) = dedent(line, width)
        {
            open = nested(Nest::Item(width), rest);
            continue;
        }
        list = None;
        open = opener(line).map(|(mark, len, info)| Fence {
            mark,
            len,
            block: info.strip_prefix("json#").map(str::to_owned),
            body: Vec::new(),
            nest: Nest::Top,
        });
    }
    match open {
        Some(Fence { block: Some(id), .. }) => Err(format!("the fence of block {id} never closes")),
        _ => Ok(found),
    }
}

pub fn split(id: &str) -> Option<(&str, u64)> {
    let (name, version) = id.rsplit_once('@')?;
    let version = version.parse().ok().filter(|version| *version > 0)?;
    Some((name, version))
}

pub fn block(id: &str, data: &Value) -> String {
    let fence = "\x60\x60\x60";
    format!("{fence}json#{id}\n{data}\n{fence}")
}

pub(crate) fn read(text: &str, lookup: impl Fn(&str, u64) -> Option<Value>) -> Result<Vec<Block>, Refusal> {
    let mut blocks = Vec::new();
    for (id, body) in fences(text).map_err(Refusal::Block)? {
        let (name, version) = split(&id).ok_or_else(|| Refusal::UnknownSchema(id.clone()))?;
        let schema = lookup(name, version).ok_or_else(|| Refusal::UnknownSchema(id.clone()))?;
        let data: Value = serde_json::from_str(&body)
            .map_err(|error| Refusal::Block(format!("block {id} is not JSON: {error}")))?;
        if !data.is_object() {
            return Err(Refusal::Block(format!("block {id} is not an object")));
        }
        let (compiled, index) = compile(&schema).map_err(Refusal::Schema)?;
        if let Err(error) = compiled.validate(&data, index) {
            return Err(Refusal::Block(format!("block {id}: {error}")));
        }
        blocks.push(Block {
            schema: name.to_owned(),
            version,
            data,
        });
    }
    Ok(blocks)
}
