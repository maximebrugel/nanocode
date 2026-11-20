use crate::{Chunks, PutError, Sha256Digest, Stored};

pub async fn receive(
    chunks: &mut dyn Chunks,
    sha256: Sha256Digest,
    bytes_max: u64,
) -> Result<(Vec<u8>, Stored), PutError> {
    let mut bytes = Vec::new();
    let mut hasher = nanocode_digest::Sha256::new();
    loop {
        match chunks.next().await {
            Ok(Some(chunk)) => {
                if (bytes.len() + chunk.len()) as u64 > bytes_max {
                    return Err(PutError::TooLarge { bytes_max });
                }
                hasher.update(&chunk);
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => return Err(PutError::Source(error)),
        }
    }
    let actual = Sha256Digest(hasher.finalize());
    if actual != sha256 {
        return Err(PutError::ShaMismatch {
            expected: sha256,
            actual,
        });
    }
    let stored = Stored {
        size_bytes: bytes.len() as u64,
        sha256: actual,
    };
    Ok((bytes, stored))
}
