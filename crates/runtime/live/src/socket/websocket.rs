use std::pin::Pin;
use std::task::{Context, Poll};

use nanocode_io::{AsyncRead, AsyncWrite};
use nanocode_sync::{Sink, Stream};
use nanocode_websocket::{Error, Message, WebSocketStream};

use super::{Inbound, Mutations, SocketPolicy, serve_frames};
use crate::hub::{Hub, SealPolicy, ViewSet};

struct Frames<S>(WebSocketStream<S>);

impl<S: AsyncRead + AsyncWrite + Unpin> Stream for Frames<S> {
    type Item = Result<Inbound, Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0)
            .poll_next(context)
            .map(|message| match message? {
                Ok(Message::Text(text)) => Some(Ok(Inbound::Text(text.into()))),
                Ok(Message::Ping(_) | Message::Pong(_)) => Some(Ok(Inbound::Control)),
                Ok(Message::Binary(_) | Message::Frame(_)) => {
                    Some(Ok(Inbound::Invalid("live frames are JSON text")))
                }
                Ok(Message::Close(_)) => None,
                Err(error) => Some(Err(error)),
            })
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Sink<String> for Frames<S> {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.0).poll_ready(context)
    }

    fn start_send(mut self: Pin<&mut Self>, text: String) -> Result<(), Error> {
        Pin::new(&mut self.0).start_send(Message::text(text))
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), Error>> {
        Pin::new(&mut self.0).poll_close(context)
    }
}

pub async fn serve<IO, V, S, M>(
    socket: WebSocketStream<IO>,
    reader: S::Reader,
    hub: &Hub<V, S>,
    intake: &M,
    policy: &SocketPolicy,
) -> Result<(), Error>
where
    IO: AsyncRead + AsyncWrite + Unpin,
    V: ViewSet,
    S: SealPolicy,
    M: Mutations<S::Reader>,
{
    serve_frames(Frames(socket), reader, hub, intake, policy).await
}
