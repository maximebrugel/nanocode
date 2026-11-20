use crate::model::{Change, Corrupt, Digest, Manifest, Refusal};
use nanocode_bytes::Bytes;
use nanocode_durability_core::StoreFuture;
use nanocode_platform::{MaybeSend, MaybeSync};
use std::{fmt, io};

#[derive(Clone, PartialEq, Eq)]
pub struct Blob {
    digest: Digest,
    bytes: Bytes,
}

impl Blob {
    #[must_use]
    pub fn new(bytes: Bytes) -> Self {
        Self {
            digest: Digest::of(&bytes),
            bytes,
        }
    }

    /// A blob whose digest the caller already computed over `bytes`.
    pub(crate) const fn hashed(digest: Digest, bytes: Bytes) -> Self {
        Self { digest, bytes }
    }

    pub fn verified(digest: Digest, bytes: Bytes) -> Result<Self, Failure> {
        let blob = Self::new(bytes);
        if blob.digest == digest {
            Ok(blob)
        } else {
            Err(Failure::Corrupt(Corrupt {
                path: digest.hex(),
                why: "blob bytes do not match their digest",
            }))
        }
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    #[must_use]
    pub const fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

impl fmt::Debug for Blob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Blob({:?}, {} bytes)",
            self.digest,
            self.bytes.len()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    change: Change,
    blobs: Vec<Blob>,
}

impl Batch {
    pub fn new(change: Change, supply: impl Fn(&Digest) -> Option<Blob>) -> Result<Self, Digest> {
        let mut blobs = Vec::with_capacity(change.added.len());
        for digest in &change.added {
            match supply(digest) {
                Some(blob) if blob.digest == *digest => blobs.push(blob),
                _ => return Err(*digest),
            }
        }
        Ok(Self { change, blobs })
    }

    #[must_use]
    pub const fn change(&self) -> &Change {
        &self.change
    }

    #[must_use]
    pub fn blobs(&self) -> &[Blob] {
        &self.blobs
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Refused(Refusal),
    Corrupt(Corrupt),
    Missing(Digest),
    Backend(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(formatter),
            Self::Corrupt(corrupt) => corrupt.fmt(formatter),
            Self::Missing(digest) => {
                write!(formatter, "workspace blob {} is missing", digest.hex())
            }
            Self::Backend(error) => write!(formatter, "workspace store: {error}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<Failure> for io::Error {
    fn from(failure: Failure) -> Self {
        let kind = match &failure {
            Failure::Refused(refusal) => refusal.kind(),
            Failure::Corrupt(_) | Failure::Missing(_) => io::ErrorKind::InvalidData,
            Failure::Backend(_) => io::ErrorKind::Other,
        };
        Self::new(kind, failure)
    }
}

impl From<Refusal> for Failure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<Corrupt> for Failure {
    fn from(corrupt: Corrupt) -> Self {
        Self::Corrupt(corrupt)
    }
}

pub trait Store: MaybeSend + MaybeSync {
    fn load<'a>(&'a self, workspace: &'a str) -> StoreFuture<'a, Result<Manifest, Failure>>;

    fn commit<'a>(
        &'a self,
        workspace: &'a str,
        batch: &'a Batch,
    ) -> StoreFuture<'a, Result<u64, Failure>>;

    fn blob<'a>(&'a self, digest: &'a Digest) -> StoreFuture<'a, Result<Blob, Failure>>;
}
