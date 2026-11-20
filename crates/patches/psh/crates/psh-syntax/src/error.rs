//! Parse errors.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::span::Span;

/// A syntax error: where, and bash's message for it (without bash's `line N:` prefix).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The offending token, an empty span at the opening quote of an unterminated quote,
    /// or an empty span at the end of the input.
    pub span: Span,
    pub message: String,
    /// The input ended inside a construct: more input could complete it.
    pub incomplete: bool,
    pub resume: Option<usize>,
    /// The input ended inside a quote or an expansion (`'a`, `${a`, `$((1`): bash's
    /// lexer reports it, and a script that ends there keeps a failing status rather
    /// than 2. Not for a command list left open (`$(echo`, `if true; then`).
    pub keeps_status: bool,
    pub warnings: alloc::vec::Vec<crate::Warning>,
    pub alias_text: Option<Vec<u8>>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
