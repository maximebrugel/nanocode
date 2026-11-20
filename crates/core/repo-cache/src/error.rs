use std::{fmt, io};

use crate::object::Id;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Name(Vec<u8>),
    Corrupt(&'static str),
    Unreachable(Id),
    Busy(Vec<u8>),
    Stale { name: Vec<u8>, current: Option<Id> },
    Taken(Vec<u8>),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(out, "{error}"),
            Self::Name(name) => write!(out, "invalid name {}", String::from_utf8_lossy(name)),
            Self::Corrupt(what) => write!(out, "corrupt {what}"),
            Self::Unreachable(id) => write!(out, "object {} is neither sent nor held", id.hex()),
            Self::Busy(name) => write!(out, "{} is locked", String::from_utf8_lossy(name)),
            Self::Stale { name, current } => write!(
                out,
                "{} moved to {}",
                String::from_utf8_lossy(name),
                current.map_or_else(|| "nothing".to_owned(), |id| id.hex())
            ),
            Self::Taken(name) => write!(out, "{} is already in use", String::from_utf8_lossy(name)),
        }
    }
}

impl std::error::Error for Error {}
