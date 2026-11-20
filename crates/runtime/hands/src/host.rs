use std::num::NonZeroUsize;

use nanocode_tool_contracts::{ToolContext, ToolOutput};
use serde_json::Value;

use crate::{
    Chunks, Fetch, GetError, HandError, HandRef, HandStatus, HostFuture, PutError, Sha256Digest,
    StepOutcome, Stored,
};

pub trait HandHost: Send + Sync + 'static {
    fn status<'a>(&'a self, session: &'a str)
    -> HostFuture<'a, Result<Vec<HandStatus>, HandError>>;

    fn run<'a>(
        &'a self,
        session: &'a str,
        step: &'a str,
        hand: &'a HandRef,
        tool: &'a str,
        input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, StepOutcome>;

    fn put<'a>(
        &'a self,
        session: &'a str,
        hand: &'a HandRef,
        path: &'a str,
        chunks: Box<dyn Chunks + 'a>,
        sha256: Sha256Digest,
    ) -> HostFuture<'a, Result<Stored, PutError>>;

    fn get<'a>(
        &'a self,
        session: &'a str,
        hand: &'a HandRef,
        path: &'a str,
        chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>>;

    fn release<'a>(
        &'a self,
        session: &'a str,
        hand: &'a HandRef,
    ) -> HostFuture<'a, Result<(), HandError>>;
}

pub trait Workspace: Send + Sync + 'static {
    fn runs(&self, tool: &str) -> bool;

    fn has(&self, _command: &str) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        session: &'a str,
        tool: &'a str,
        input: Value,
        context: ToolContext<'a>,
    ) -> HostFuture<'a, ToolOutput>;

    fn put<'a>(
        &'a self,
        session: &'a str,
        path: &'a str,
        chunks: Box<dyn Chunks + 'a>,
        sha256: Sha256Digest,
    ) -> HostFuture<'a, Result<Stored, PutError>>;

    fn get<'a>(
        &'a self,
        session: &'a str,
        path: &'a str,
        chunk_bytes: NonZeroUsize,
    ) -> HostFuture<'a, Result<Fetch<'a>, GetError>>;
}
