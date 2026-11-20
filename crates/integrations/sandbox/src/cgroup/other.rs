use super::{Controllers, Limits, Unavailable, Usage};
use std::{convert::Infallible, io, path::Path, time::Duration};

pub struct Tree(Infallible);

pub struct Group(Infallible);

pub const fn detect() -> Result<Tree, Unavailable> {
    Err(Unavailable::Unsupported)
}

pub fn join(_procs: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        Unavailable::Unsupported.to_string(),
    ))
}

impl Tree {
    #[must_use]
    pub const fn base(&self) -> &Path {
        match self.0 {}
    }

    #[must_use]
    pub const fn controllers(&self) -> Controllers {
        match self.0 {}
    }

    #[must_use]
    pub const fn stranded(&self) -> u64 {
        match self.0 {}
    }

    pub fn create(&self, _name: &str, _limits: &Limits) -> io::Result<Group> {
        match self.0 {}
    }
}

impl Group {
    #[must_use]
    pub const fn path(&self) -> &Path {
        match self.0 {}
    }

    #[must_use]
    pub const fn procs(&self) -> &Path {
        match self.0 {}
    }

    pub fn kill(&self) -> io::Result<()> {
        match self.0 {}
    }

    pub fn usage(&self) -> io::Result<Usage> {
        match self.0 {}
    }

    pub async fn remove(self, _settle: Duration) -> io::Result<()> {
        match self.0 {}
    }
}
