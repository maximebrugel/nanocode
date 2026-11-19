use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    io::{self, Write},
};

use nanocode::agent::events::{AgentEvent, AgentEventKind, TextDeltaRef};
use serde::Deserialize;
use serde_json::Value;

#[derive(Eq, PartialEq)]
enum Block {
    Reasoning(u64),
    Answer(String),
    Notice,
}

#[derive(Default, Deserialize)]
struct Payload<'a> {
    #[serde(default, borrow)]
    text: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    item_id: Option<Cow<'a, str>>,
    #[serde(default)]
    model_call_index: Option<u64>,
    #[serde(default, borrow)]
    tool: Option<Cow<'a, str>>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    observational: Option<Value>,
}

impl Payload<'_> {
    fn key(&self) -> Cow<'_, str> {
        match &self.item_id {
            Some(id) => Cow::Borrowed(id),
            None => Cow::Owned(match self.model_call_index {
                Some(index) => format!("call-{index}"),
                None => "call-null".to_owned(),
            }),
        }
    }
}

struct Pipe {
    call: Option<u64>,
    written: Vec<u8>,
    replay: Option<Replay>,
}

struct Replay {
    shown: Vec<u8>,
    matched: usize,
}

pub struct Printer<O> {
    answers: O,
    pipe: Option<Pipe>,
    notices: Vec<u8>,
    block: Option<Block>,
    open: bool,
    streamed: HashSet<String>,
    calls: HashMap<String, Option<u64>>,
}

impl<O: Write> Printer<O> {
    pub fn new(answers: O, terminal: bool) -> Self {
        Self {
            answers,
            pipe: (!terminal).then(|| Pipe {
                call: None,
                written: Vec::new(),
                replay: None,
            }),
            notices: Vec::new(),
            block: None,
            open: false,
            streamed: HashSet::new(),
            calls: HashMap::new(),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if !self.notices.is_empty() {
            let mut stderr = io::stderr().lock();
            stderr.write_all(&self.notices)?;
            stderr.flush()?;
            self.notices.clear();
        }
        self.answers.flush()
    }

    pub fn event(&mut self, event: &AgentEvent, delta: Option<TextDeltaRef<'_>>) -> io::Result<()> {
        if !matches!(
            event.kind,
            AgentEventKind::ReasoningSummaryDelta
                | AgentEventKind::AssistantDelta
                | AgentEventKind::AssistantMessage
                | AgentEventKind::ToolCall
                | AgentEventKind::RunError
                | AgentEventKind::ModelAttemptRetrying
        ) {
            return Ok(());
        }
        if event.kind == AgentEventKind::ModelAttemptRetrying {
            let payload = serde_json::from_str::<Value>(event.payload.get()).unwrap_or_default();
            return self.retrying(&payload);
        }
        let payload = match delta {
            Some(delta) => Payload {
                text: Some(delta.text),
                item_id: delta.item_id,
                model_call_index: Some(delta.model_call_index.into()),
                ..Payload::default()
            },
            None => serde_json::from_str::<Payload>(event.payload.get()).unwrap_or_default(),
        };
        let text = payload.text.as_deref().unwrap_or_default();
        match event.kind {
            AgentEventKind::ReasoningSummaryDelta => self.write(
                Block::Reasoning(payload.model_call_index.unwrap_or_default()),
                text,
            ),
            AgentEventKind::AssistantDelta => {
                let key = payload.key();
                if !self.calls.contains_key(&*key) {
                    self.calls
                        .insert(key.clone().into_owned(), payload.model_call_index);
                }
                self.answer(&key, payload.model_call_index, text)
            }
            AgentEventKind::AssistantMessage if !self.streamed.contains(&*payload.key()) => {
                self.answer(&payload.key(), payload.model_call_index, text)
            }
            AgentEventKind::ToolCall => {
                self.notice(&format!("· {}", payload.tool.as_deref().unwrap_or("tool")))
            }
            AgentEventKind::RunError
                if payload.observational.as_ref() == Some(&Value::Bool(true)) =>
            {
                self.notice(&format!(
                    "warning: {}",
                    payload
                        .error
                        .as_ref()
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ))
            }
            _ => Ok(()),
        }
    }

    pub fn finish(mut self, final_message: &str) -> io::Result<()> {
        self.close()?;
        self.settle()?;
        if self.streamed.is_empty() {
            writeln!(self.answers, "{final_message}")?;
        }
        self.flush()
    }

    fn retrying(&mut self, payload: &Value) -> io::Result<()> {
        let call = payload["model_call_index"].as_u64();
        let dead: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, owner)| **owner == call)
            .map(|(key, _)| key.clone())
            .collect();
        let mut shown = false;
        for key in dead {
            self.calls.remove(&key);
            shown |= self.streamed.remove(&key);
        }
        let notice = crate::events::retry_notice(payload);
        if let Some(pipe) = &mut self.pipe {
            if pipe.call == call {
                let shown = match pipe.replay.take() {
                    Some(replay) => replay.shown,
                    None => std::mem::take(&mut pipe.written),
                };
                pipe.written.clear();
                if !shown.is_empty() {
                    pipe.replay = Some(Replay { shown, matched: 0 });
                    if matches!(self.block, Some(Block::Answer(_))) {
                        self.block = None;
                        self.open = false;
                    }
                }
            }
            return self.notice(&format!("nanocode: {notice}"));
        }
        if shown {
            self.notice(&format!(
                "nanocode: {notice}; the partial reply above is discarded"
            ))
        } else {
            self.notice(&format!("nanocode: {notice}"))
        }
    }

    fn answer(&mut self, key: &str, call: Option<u64>, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if matches!(&self.block, Some(Block::Answer(current)) if current == key) {
            self.emit(text.as_bytes())?;
            self.open = !text.ends_with('\n');
            return Ok(());
        }
        self.streamed.insert(key.to_owned());
        self.close()?;
        if self.pipe.as_ref().is_some_and(|pipe| pipe.call != call) {
            self.settle()?;
            if let Some(pipe) = &mut self.pipe {
                pipe.call = call;
                pipe.written.clear();
            }
        }
        self.write(Block::Answer(key.to_owned()), text)
    }

    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        let Some(pipe) = &mut self.pipe else {
            return self.answers.write_all(bytes);
        };
        pipe.written.extend_from_slice(bytes);
        let Some(replay) = &mut pipe.replay else {
            return self.answers.write_all(bytes);
        };
        let rest = &replay.shown[replay.matched..];
        let same = rest.iter().zip(bytes).take_while(|(a, b)| a == b).count();
        if same == bytes.len() {
            replay.matched += same;
            Ok(())
        } else if same == rest.len() {
            pipe.replay = None;
            self.answers.write_all(&bytes[same..])
        } else {
            self.diverge()
        }
    }

    fn settle(&mut self) -> io::Result<()> {
        match self.pipe.as_ref().and_then(|pipe| pipe.replay.as_ref()) {
            Some(replay) if replay.matched < replay.shown.len() => self.diverge(),
            _ => {
                if let Some(pipe) = &mut self.pipe {
                    pipe.replay = None;
                }
                Ok(())
            }
        }
    }

    fn diverge(&mut self) -> io::Result<()> {
        let Some(pipe) = &mut self.pipe else {
            return Ok(());
        };
        if pipe
            .replay
            .take()
            .is_some_and(|replay| !replay.shown.ends_with(b"\n"))
        {
            self.answers.write_all(b"\n")?;
        }
        self.answers.write_all(
            b"--- nanocode: retried reply; the partial reply above is discarded ---\n",
        )?;
        self.answers.write_all(&pipe.written)?;
        writeln!(
            self.notices,
            "nanocode: the retried reply differs from the partial reply above, so it follows a separator line in full"
        )
    }

    fn write(&mut self, block: Block, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if self.block.as_ref() != Some(&block) {
            self.close()?;
        }
        if matches!(block, Block::Reasoning(_)) {
            self.notices.extend_from_slice(text.as_bytes());
        } else {
            self.emit(text.as_bytes())?;
        }
        self.block = Some(block);
        self.open = !text.ends_with('\n');
        Ok(())
    }

    fn notice(&mut self, line: &str) -> io::Result<()> {
        self.close()?;
        self.block = Some(Block::Notice);
        writeln!(self.notices, "{line}")
    }

    fn close(&mut self) -> io::Result<()> {
        match self.block {
            Some(Block::Reasoning(_)) => {
                if self.open {
                    self.notices.push(b'\n');
                }
                self.notices.push(b'\n');
            }
            Some(Block::Answer(_)) if self.open => {
                self.emit(b"\n")?;
            }
            _ => {}
        }
        self.block = None;
        self.open = false;
        self.flush()
    }
}
