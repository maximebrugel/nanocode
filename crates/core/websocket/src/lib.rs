#![cfg(not(target_family = "wasm"))]

mod connect;
mod error;
mod frame;
mod handshake;
mod message;
mod protocol;
mod stream;

pub use connect::{
    Connector, MaybeTlsStream, client_async_tls_with_config, connect_async_tls_with_config,
};
pub use error::{
    CapacityError, Error, ProtocolError, Result, SubProtocolError, TlsError, UrlError,
};
pub use frame::{CloseCode, CloseFrame, Frame, FrameHeader, OpCode};
pub use handshake::{IntoClientRequest, client, derive_accept_key, server};
pub use message::{Message, Utf8Bytes};
pub use protocol::{READ_BUFFER_BYTES, WebSocketConfig};
pub use stream::{
    WebSocketStream, accept_hdr_async, accept_hdr_async_with_config, accept_upgraded,
    client_async_with_config,
};
