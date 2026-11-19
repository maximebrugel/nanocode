//! Slack events and interactivity payloads to board events: a message becomes a board message in its thread.

use std::collections::BTreeMap;

use nanocode_board::EventId;
use serde_json::{Value, json};

use super::github::{Parts, keyed, marker, text};
use super::{Author, Control, Event, Inbound, Target};

pub const SYNC: &str = "slack";

const PLAIN: [&str; 4] = ["bot_message", "thread_broadcast", "file_share", "me_message"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub bot: Option<String>,
}

fn thread(channel: &str, root: &str) -> Target {
    Target::new(SYNC, format!("{channel}:{root}"))
}

fn root(message: &Value) -> Option<&str> {
    text(message, "/thread_ts").or_else(|| text(message, "/ts"))
}

fn micros(ts: &str) -> Option<u64> {
    let (secs, frac) = ts.split_once('.').unwrap_or((ts, ""));
    let frac = frac.chars().chain(std::iter::repeat('0')).take(6).collect::<String>();
    secs.parse::<u64>().ok()?.checked_mul(1_000_000)?.checked_add(frac.parse::<u64>().ok()?)
}

fn named(message: &Value, user: &str) -> String {
    ["/user_profile/display_name", "/user_profile/real_name", "/username"]
        .iter()
        .filter_map(|at| text(message, at))
        .find(|name| !name.is_empty())
        .unwrap_or(user)
        .to_owned()
}

fn author(config: &Config, message: &Value) -> Option<Author> {
    let id = text(message, "/user").filter(|id| !id.is_empty());
    let bot_id = text(message, "/bot_id").filter(|id| !id.is_empty());
    let user = id.or(bot_id)?;
    let bot = bot_id.is_some() || text(message, "/subtype") == Some("bot_message") || (id.is_some() && config.bot.as_deref() == id);
    Some(Author { user: user.to_owned(), name: named(message, user), bot })
}

fn unescape(raw: &str) -> String {
    raw.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn body(message: &Value) -> (String, Option<EventId>) {
    let raw = unescape(text(message, "/text").unwrap_or_default());
    match text(message, "/metadata/event_payload/board").filter(|id| !id.is_empty()) {
        Some(id) => (marker(&raw).0, Some(EventId(id.to_owned()))),
        None => marker(&raw),
    }
}

fn posted(config: &Config, channel: &str, event: &Value) -> Option<Parts> {
    let ts = text(event, "/ts")?;
    let (body, marker) = body(event);
    if body.is_empty() {
        return None;
    }
    let author = author(config, event)?;
    let message = Event::Message { thread: thread(channel, root(event)?), message: ts.to_owned(), author, body, context: None, sub: None, marker };
    Some(vec![("message", message)])
}

fn changed(channel: &str, event: &Value) -> Option<Parts> {
    let inner = event.get("message")?;
    let version = micros(text(inner, "/edited/ts")?)?;
    let edited = Event::Edited { thread: thread(channel, root(inner)?), message: text(inner, "/ts")?.to_owned(), body: body(inner).0, version };
    Some(vec![("edit", edited)])
}

fn deleted(channel: &str, event: &Value) -> Option<Parts> {
    let ts = text(event, "/deleted_ts")?;
    let top = event.get("previous_message").and_then(root).unwrap_or(ts);
    Some(vec![("delete", Event::Deleted { thread: thread(channel, top), message: ts.to_owned() })])
}

fn message(config: &Config, event: &Value) -> Option<Parts> {
    let channel = text(event, "/channel")?;
    match text(event, "/subtype") {
        None => posted(config, channel, event),
        Some(kind) if PLAIN.contains(&kind) => posted(config, channel, event),
        Some("message_changed") => changed(channel, event),
        Some("message_deleted") => deleted(channel, event),
        Some(_) => None,
    }
}

fn reaction(config: &Config, event: &Value) -> Option<Parts> {
    if text(event, "/item/type") != Some("message") {
        return None;
    }
    let user = text(event, "/user")?;
    let author = Author { user: user.to_owned(), name: user.to_owned(), bot: config.bot.as_deref() == Some(user) };
    let ts = text(event, "/item/ts")?;
    let thread = thread(text(event, "/item/channel")?, ts);
    let emoji = text(event, "/reaction")?.to_owned();
    Some(vec![("reaction", Event::Reaction { thread, message: ts.to_owned(), author, emoji })])
}

fn form(control: &str, payload: &Value) -> BTreeMap<String, String> {
    let prefix = format!("{control}/");
    let mut values = BTreeMap::new();
    let blocks = payload.pointer("/state/values").and_then(Value::as_object).into_iter().flatten();
    for (_, fields) in blocks.filter(|(block, _)| block.starts_with(&prefix)) {
        for (field, input) in fields.as_object().into_iter().flatten() {
            let value = text(input, "/value").or_else(|| text(input, "/selected_option/value")).or_else(|| text(input, "/selected_date"));
            if let Some(value) = value {
                values.insert(field.clone(), value.to_owned());
            }
        }
    }
    values
}

fn clicked(config: &Config, payload: &Value) -> Option<Parts> {
    let control = text(payload.pointer("/actions/0")?, "/action_id")?;
    let ts = text(payload, "/container/message_ts").or_else(|| text(payload, "/message/ts"))?;
    let channel = text(payload, "/container/channel_id").or_else(|| text(payload, "/channel/id"))?;
    let top = text(payload, "/message/thread_ts").or_else(|| text(payload, "/container/thread_ts")).unwrap_or(ts);
    let user = text(payload, "/user/id")?;
    let name = text(payload, "/user/username").or_else(|| text(payload, "/user/name")).unwrap_or(user);
    let author = Author { user: user.to_owned(), name: name.to_owned(), bot: config.bot.as_deref() == Some(user) };
    let values = form(control, payload);
    let event = Event::Clicked { thread: thread(channel, top), message: ts.to_owned(), author, control: control.to_owned(), values };
    Some(vec![("click", event)])
}

pub fn parse(delivery: &str, payload: &Value) -> Vec<Inbound> {
    parse_with(&Config::default(), delivery, payload)
}

pub fn parse_with(config: &Config, delivery: &str, payload: &Value) -> Vec<Inbound> {
    let parts = match text(payload, "/type") {
        Some("event_callback") => {
            let event = payload.get("event");
            match event.and_then(|event| text(event, "/type")) {
                Some("message") => event.and_then(|event| message(config, event)),
                Some("reaction_added") => event.and_then(|event| reaction(config, event)),
                _ => None,
            }
        }
        Some("block_actions") => clicked(config, payload),
        _ => None,
    };
    keyed(delivery, parts.unwrap_or_default())
}

pub fn blocks(text: &str, controls: &[Control]) -> Value {
    let mut blocks = vec![json!({"type": "section", "text": {"type": "mrkdwn", "text": text}})];
    let buttons = controls
        .iter()
        .filter_map(|control| match control {
            Control::Button { id, label } => Some(json!({"type": "button", "action_id": id, "text": {"type": "plain_text", "text": label}})),
            Control::Form { .. } => None,
        })
        .collect::<Vec<_>>();
    if !buttons.is_empty() {
        blocks.push(json!({"type": "actions", "elements": buttons}));
    }
    for control in controls {
        if let Control::Form { id, label, fields } = control {
            for field in fields {
                blocks.push(json!({
                    "type": "input",
                    "block_id": format!("{id}/{field}"),
                    "label": {"type": "plain_text", "text": format!("{label}: {field}")},
                    "element": {"type": "plain_text_input", "action_id": field},
                }));
            }
            let submit = json!({"type": "button", "action_id": id, "text": {"type": "plain_text", "text": label}});
            blocks.push(json!({"type": "actions", "elements": [submit]}));
        }
    }
    Value::Array(blocks)
}
