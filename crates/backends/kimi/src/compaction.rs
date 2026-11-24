use crate::prompts;
use nanocode_backend_kimi_client::{Message, Origin, estimate_message, estimate_text};

const TRIGGER_RATIO: f64 = 0.85;
const RESERVED_TOKENS: u64 = 50_000;
const USER_MESSAGE_TOKENS: u64 = 20_000;
const USER_HEAD_TOKENS: u64 = 2_000;
pub const MAX_OUTPUT_TOKENS: u64 = 128 * 1024;
pub const MARGIN_RATIO: f64 = 0.02;
pub const ATTEMPTS: u32 = 5;

pub fn should_compact(used: u64, window: u64) -> bool {
    window > 0
        && (used as f64 >= window as f64 * TRIGGER_RATIO
            || (RESERVED_TOKENS < window && used + RESERVED_TOKENS >= window))
}

pub fn used(overhead: u64, messages: &[Message]) -> u64 {
    let counted = overhead + messages.iter().map(estimate_message).sum::<u64>();
    let anchored = messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| {
            let Message::Assistant {
                usage: Some(usage), ..
            } = message
            else {
                return None;
            };
            (usage.total() > 0).then(|| {
                usage.total()
                    + messages[index + 1..]
                        .iter()
                        .map(estimate_message)
                        .sum::<u64>()
            })
        });
    anchored.map_or(counted, |anchored| anchored.max(counted))
}

pub fn without_oldest(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .skip(1)
        .skip_while(|message| matches!(message, Message::Tool { .. }))
        .cloned()
        .collect()
}

pub fn compacted(history: &[Message], summary: &str) -> Vec<Message> {
    let users = history
        .iter()
        .filter_map(|message| match message {
            Message::User {
                content,
                origin: Origin::Prompt,
            } => Some(content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut kept = select(&users)
        .into_iter()
        .map(Message::from)
        .collect::<Vec<_>>();
    kept.push(Message::user(prompts::summary(summary), Origin::Summary));
    kept.push(Message::user(prompts::continuation(), Origin::Reminder));
    kept
}

enum Kept {
    Text(String),
    Elision(u64),
}

fn tokens(text: &str) -> u64 {
    estimate_text("user") + estimate_text(text)
}

fn select(users: &[String]) -> Vec<Kept> {
    let total = users.iter().map(|text| tokens(text)).sum::<u64>();
    if total <= USER_MESSAGE_TOKENS {
        return users.iter().cloned().map(Kept::Text).collect();
    }
    let mut tail = Vec::new();
    let mut tail_left = USER_MESSAGE_TOKENS - USER_HEAD_TOKENS;
    let mut head_end = users.len();
    let mut dropped_prefix = None;
    for (index, text) in users.iter().enumerate().rev() {
        if tail_left == 0 {
            break;
        }
        let size = tokens(text);
        head_end = index;
        if size <= tail_left {
            tail.push(text.clone());
            tail_left -= size;
            continue;
        }
        let suffix = suffix_within(text, tail_left);
        let prefix = &text[..text.len() - suffix.len()];
        if !prefix.is_empty() {
            dropped_prefix = Some(prefix.to_owned());
        }
        tail.push(suffix.to_owned());
        break;
    }
    tail.reverse();
    let mut candidates = users[..head_end].to_vec();
    candidates.extend(dropped_prefix);
    let mut head = Vec::new();
    let mut head_left = USER_HEAD_TOKENS;
    for text in candidates {
        if head_left == 0 {
            break;
        }
        let size = tokens(&text);
        if size <= head_left {
            head.push(text);
            head_left -= size;
            continue;
        }
        head.push(prefix_within(&text, head_left).to_owned());
        break;
    }
    let kept = head
        .iter()
        .chain(&tail)
        .map(|text| tokens(text))
        .sum::<u64>();
    let mut selection = head.into_iter().map(Kept::Text).collect::<Vec<_>>();
    selection.push(Kept::Elision(total.saturating_sub(kept)));
    selection.extend(tail.into_iter().map(Kept::Text));
    selection
}

const fn fits(ascii: u64, other: u64, budget: u64) -> bool {
    ascii.div_ceil(4) + other <= budget
}

fn prefix_within(text: &str, budget: u64) -> &str {
    let (mut ascii, mut other, mut end) = (0, 0, 0);
    for char in text.chars() {
        if char.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
        if !fits(ascii, other, budget) {
            break;
        }
        end += char.len_utf8();
    }
    &text[..end]
}

fn suffix_within(text: &str, budget: u64) -> &str {
    let (mut ascii, mut other, mut start) = (0, 0, text.len());
    for (index, char) in text.char_indices().rev() {
        if char.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
        if !fits(ascii, other, budget) {
            break;
        }
        start = index;
    }
    &text[start..]
}

impl From<Kept> for Message {
    fn from(kept: Kept) -> Self {
        match kept {
            Kept::Elision(omitted) => Self::user(prompts::elision(omitted), Origin::Reminder),
            Kept::Text(text) => Self::user(text, Origin::Prompt),
        }
    }
}
