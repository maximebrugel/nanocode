use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::future::{Future, pending};
use std::num::NonZeroUsize;
use std::pin::pin;
use std::sync::Arc;

use nanocode_live_fold::{
    ClientFrame, ErrCode, Intent, ServerFrame, ViewKey, decode_client, encode,
};
use nanocode_sync::future::{Either, select};
use nanocode_sync::mpsc::{self, error::TrySendError};
use nanocode_sync::stream::FuturesUnordered;
use nanocode_sync::{Inbox, Sink, SinkExt as _, Stream, StreamExt as _};
use serde_json::Value;

use crate::hub::{ConnId, Frame, Hub, Kind, Outbound, Overloaded, SealPolicy, ViewSet};
use crate::intent::{Applier, Intents, Views};

#[cfg(not(target_family = "wasm"))]
mod websocket;

#[cfg(not(target_family = "wasm"))]
pub use websocket::serve;

pub struct SocketPolicy {
    pub queued_frames: NonZeroUsize,
}

pub enum Inbound {
    Text(String),
    Control,
    Invalid(&'static str),
}

pub trait Mutations<R> {
    fn submit(&self, conn: ConnId, intent: Intent, reader: R) -> impl Future<Output = ()>;

    fn closed(&self, conn: ConnId);
}

impl<R, A, V> Mutations<R> for Intents<A, V>
where
    A: Applier,
    V: Views<Conn = ConnId, Record = A::Record>,
{
    fn submit(&self, conn: ConnId, intent: Intent, _reader: R) -> impl Future<Output = ()> {
        self.receive(conn, intent)
    }

    fn closed(&self, conn: ConnId) {
        self.close(conn);
    }
}

#[derive(Clone, Copy)]
enum Overflow {
    Sub(u32, u64),
    Socket,
}

struct Queue {
    frames: mpsc::Sender<Frame>,
    overflowed: Inbox<Overflow>,
}

impl Outbound for Queue {
    fn push(&self, frame: Frame) -> Result<(), Overloaded> {
        let overflow = match (frame.kind, frame.id) {
            (Kind::Eph, _) => None,
            (Kind::Ack, _) | (_, None) => Some(Overflow::Socket),
            (_, Some(id)) => Some(Overflow::Sub(id, frame.generation)),
        };
        match self.frames.try_send(frame) {
            Ok(()) | Err(TrySendError::Closed(_)) => Ok(()),
            Err(TrySendError::Full(_)) => match overflow {
                Some(overflow) => {
                    self.overflowed.push(overflow);
                    Err(Overloaded)
                }
                None => Ok(()),
            },
        }
    }
}

pub fn publish_ephemeral<V: ViewSet, S: SealPolicy>(hub: &Hub<V, S>, view: ViewKey, body: Value) {
    hub.eph(view, body);
}

fn error_text(id: Option<u32>, code: ErrCode, message: impl Into<String>) -> String {
    encode(&ServerFrame::Err {
        id,
        code,
        message: message.into(),
    })
}

struct Socket<'a, V: ViewSet, S: SealPolicy> {
    conn: ConnId,
    queue: Arc<Queue>,
    reader: S::Reader,
    hub: &'a Hub<V, S>,
    active: HashMap<u32, u64>,
    generation: u64,
    intents: Vec<Intent>,
    writes: Vec<String>,
}

impl<V: ViewSet, S: SealPolicy> Socket<'_, V, S> {
    fn receive(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Text(text) => match decode_client(&text) {
                Ok(frame) => self.frame(frame),
                Err(error) => {
                    self.writes
                        .push(error_text(None, ErrCode::BadFrame, error.to_string()))
                }
            },
            Inbound::Control => {}
            Inbound::Invalid(reason) => {
                self.writes
                    .push(error_text(None, ErrCode::BadFrame, reason))
            }
        }
    }

    fn frame(&mut self, frame: ClientFrame) {
        match frame {
            ClientFrame::Sub { id, view, cursor } => {
                if let Entry::Vacant(entry) = self.active.entry(id) {
                    self.generation += 1;
                    entry.insert(self.generation);
                    self.hub.subscribe(
                        self.conn,
                        id,
                        self.generation,
                        view,
                        cursor,
                        self.reader.clone(),
                    );
                } else {
                    self.writes.push(error_text(
                        Some(id),
                        ErrCode::BadFrame,
                        "this subscription id is in use",
                    ));
                }
            }
            ClientFrame::Unsub { id } => {
                if self.active.remove(&id).is_some() {
                    self.hub.unsubscribe(self.conn, id);
                }
            }
            ClientFrame::Mut(intent) => self.intents.push(intent),
        }
    }

    fn deliver(&mut self, batch: &mut Vec<Frame>) {
        for frame in batch.drain(..) {
            match (frame.kind, frame.id) {
                (Kind::Err, Some(id)) => {
                    if !self.retire(id, frame.generation) {
                        continue;
                    }
                }
                (Kind::Ack | Kind::Err, _) | (_, None) => {}
                (_, Some(id)) => {
                    if self.active.get(&id) != Some(&frame.generation) {
                        continue;
                    }
                }
            }
            let mut text = String::with_capacity(frame.tail.len() + 24);
            frame.write(&mut text);
            self.writes.push(text);
        }
    }

    fn retire(&mut self, id: u32, generation: u64) -> bool {
        if self.active.get(&id) != Some(&generation) {
            return false;
        }
        self.active.remove(&id);
        true
    }

    fn overflowed(&mut self) -> bool {
        let mut socket = false;
        for overflow in self.queue.overflowed.drain() {
            match overflow {
                Overflow::Sub(id, generation) => {
                    if self.retire(id, generation) {
                        self.writes.push(error_text(
                            Some(id),
                            ErrCode::Overloaded,
                            "the outbound queue overflowed: resubscribe from your cursor",
                        ));
                    }
                }
                Overflow::Socket => socket = true,
            }
        }
        if socket {
            self.writes.push(error_text(
                None,
                ErrCode::Overloaded,
                "the outbound queue overflowed: reconnect and resubscribe from your cursors",
            ));
        }
        socket
    }
}

enum Event<I> {
    Inbound(Option<I>),
    Outbound,
    Settled,
}

pub async fn serve_frames<T, E, V, S, M>(
    mut transport: T,
    reader: S::Reader,
    hub: &Hub<V, S>,
    intake: &M,
    policy: &SocketPolicy,
) -> Result<(), E>
where
    T: Stream<Item = Result<Inbound, E>> + Sink<String, Error = E> + Unpin,
    V: ViewSet,
    S: SealPolicy,
    M: Mutations<S::Reader>,
{
    let bound = policy.queued_frames.get();
    let (frames, mut outbound) = mpsc::channel(bound);
    let queue = Arc::new(Queue {
        frames,
        overflowed: Inbox::default(),
    });
    let conn = hub.connect(Arc::clone(&queue) as Arc<dyn Outbound>);
    let mut socket = Socket {
        conn,
        queue,
        reader,
        hub,
        active: HashMap::new(),
        generation: 0,
        intents: Vec::new(),
        writes: Vec::new(),
    };
    let mut submitting = FuturesUnordered::new();
    let mut unsettled = 0_usize;
    let mut batch = Vec::with_capacity(bound);
    let served = loop {
        let event = match select(
            select(
                pin!(transport.next()),
                pin!(outbound.recv_many(&mut batch, bound)),
            ),
            pin!(async {
                if unsettled == 0 {
                    pending::<()>().await;
                } else {
                    submitting.next().await;
                }
            }),
        )
        .await
        {
            Either::Left((Either::Left((incoming, _)), _)) => Event::Inbound(incoming),
            Either::Left((Either::Right(_), _)) => Event::Outbound,
            Either::Right(_) => Event::Settled,
        };
        match event {
            Event::Inbound(Some(Ok(inbound))) => socket.receive(inbound),
            Event::Inbound(Some(Err(error))) => break Err(error),
            Event::Inbound(None) => break Ok(()),
            Event::Settled => unsettled -= 1,
            Event::Outbound => {}
        }
        for intent in socket.intents.drain(..) {
            submitting.push(intake.submit(conn, intent, socket.reader.clone()));
            unsettled += 1;
        }
        while let Ok(frame) = outbound.try_recv() {
            batch.push(frame);
        }
        socket.deliver(&mut batch);
        let overloaded = socket.overflowed();
        if let Err(error) = write(&mut transport, &mut socket.writes).await {
            break Err(error);
        }
        if overloaded {
            break Ok(());
        }
    };
    hub.disconnect(conn);
    intake.closed(conn);
    outbound.close();
    let closed = transport.close().await;
    served.and(closed)
}

async fn write<T, E>(transport: &mut T, writes: &mut Vec<String>) -> Result<(), E>
where
    T: Sink<String, Error = E> + Unpin,
{
    if writes.is_empty() {
        return Ok(());
    }
    for text in writes.drain(..) {
        transport.feed(text).await?;
    }
    transport.flush().await
}
