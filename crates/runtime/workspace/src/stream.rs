use nanocode_shell::{FileDownload, SHA256_BYTES};
use nanocode_sync::{oneshot, unbounded};

use crate::{
    Workspace, stopped,
    worker::{Job, Part, Reply},
};

pub struct Uploading {
    parts: unbounded::Sender<Part>,
}

impl Workspace {
    pub async fn upload(
        &self,
        path: &[u8],
        sha256: [u8; SHA256_BYTES],
    ) -> Result<Uploading, String> {
        let (parts, received) = unbounded::channel();
        self.ask(|reply| Job::Upload(path.to_vec(), sha256, received, reply))
            .await?;
        Ok(Uploading { parts })
    }

    pub async fn download(&self, path: &[u8]) -> Result<Option<FileDownload>, String> {
        self.ask(|reply| Job::Download(path.to_vec(), reply)).await
    }
}

impl Uploading {
    pub async fn push(&self, chunk: Vec<u8>) -> Result<(), String> {
        self.send(|reply| Part::Chunk(chunk, reply)).await
    }

    pub async fn sealed(&self) -> Result<[u8; SHA256_BYTES], String> {
        self.send(Part::Seal).await
    }

    pub async fn commit(self) -> Result<(), String> {
        self.send(Part::Commit).await
    }

    async fn send<T>(&self, part: impl FnOnce(Reply<T>) -> Part) -> Result<T, String> {
        let (reply, answer) = oneshot::channel();
        if self.parts.send(part(reply)).is_err() {
            return Err(stopped());
        }
        answer.await.unwrap_or_else(|_| Err(stopped()))
    }
}
