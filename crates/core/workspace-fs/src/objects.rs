use crate::store::Failure;
use nanocode_bytes::Bytes;
use nanocode_durability_core::StoreFuture;
use nanocode_platform::{MaybeSend, MaybeSync};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub key: String,
    pub modified_ms: u64,
}

pub trait Objects: MaybeSend + MaybeSync {
    fn put<'a>(&'a self, key: &'a str, bytes: &'a Bytes) -> StoreFuture<'a, Result<(), Failure>>;

    fn get<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<Option<Bytes>, Failure>>;

    fn delete<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<(), Failure>>;

    fn list<'a>(
        &'a self,
        prefix: &'a str,
        after: &'a str,
        limit: usize,
    ) -> StoreFuture<'a, Result<Vec<Listed>, Failure>>;
}

#[must_use]
pub fn plain_key(key: &str) -> bool {
    !key.is_empty()
        && key.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
}

#[cfg(not(target_family = "wasm"))]
pub use dir::DirObjects;

#[cfg(not(target_family = "wasm"))]
mod dir {
    use super::{Listed, Objects, plain_key};
    use crate::store::Failure;
    use nanocode_bytes::Bytes;
    use nanocode_durability_core::StoreFuture;
    use std::{
        fs, io,
        io::Write,
        path::{Path, PathBuf},
        time::UNIX_EPOCH,
    };

    pub struct DirObjects {
        root: PathBuf,
    }

    impl DirObjects {
        #[must_use]
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into() }
        }

        fn path(&self, key: &str) -> Result<PathBuf, Failure> {
            if plain_key(key) {
                Ok(self.root.join(key))
            } else {
                Err(Failure::Backend(format!(
                    "object key {key:?} is not a path of plain names"
                )))
            }
        }
    }

    fn backend(error: io::Error) -> Failure {
        Failure::Backend(format!("object directory: {error}"))
    }

    async fn blocking<T: Send + 'static>(
        work: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> Result<T, Failure> {
        nanocode_platform::spawn_blocking(work)
            .await
            .map_err(|_| Failure::Backend("an object directory worker stopped".to_owned()))?
            .map_err(backend)
    }

    fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("a key has no parent"))?;
        fs::create_dir_all(parent)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("a key has no name"))?;
        let scratch = parent.join(format!(".{name}.{}", nanocode_platform::uuid_v4().simple()));
        let mut file = fs::File::create(&scratch)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&scratch, path)
    }

    fn walk(
        root: &Path,
        at: &Path,
        prefix: &str,
        after: &str,
        found: &mut Vec<Listed>,
    ) -> io::Result<()> {
        let entries = match fs::read_dir(at) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let Some(key) = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .map(|key| key.replace(std::path::MAIN_SEPARATOR, "/"))
            else {
                continue;
            };
            if !plain_key(&key) {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if key.starts_with(prefix) || prefix.starts_with(&format!("{key}/")) {
                    walk(root, &path, prefix, after, found)?;
                }
            } else if kind.is_file() && key.starts_with(prefix) && key.as_str() > after {
                let modified_ms = entry
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_millis();
                found.push(Listed {
                    key,
                    modified_ms: u64::try_from(modified_ms).map_err(io::Error::other)?,
                });
            }
        }
        Ok(())
    }

    impl Objects for DirObjects {
        fn put<'a>(
            &'a self,
            key: &'a str,
            bytes: &'a Bytes,
        ) -> StoreFuture<'a, Result<(), Failure>> {
            Box::pin(async move {
                let path = self.path(key)?;
                let bytes = bytes.clone();
                blocking(move || write(&path, &bytes)).await
            })
        }

        fn get<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<Option<Bytes>, Failure>> {
            Box::pin(async move {
                let path = self.path(key)?;
                blocking(move || match fs::read(&path) {
                    Ok(bytes) => Ok(Some(Bytes::from(bytes))),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                })
                .await
            })
        }

        fn delete<'a>(&'a self, key: &'a str) -> StoreFuture<'a, Result<(), Failure>> {
            Box::pin(async move {
                let path = self.path(key)?;
                blocking(move || match fs::remove_file(&path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                    _ => Ok(()),
                })
                .await
            })
        }

        fn list<'a>(
            &'a self,
            prefix: &'a str,
            after: &'a str,
            limit: usize,
        ) -> StoreFuture<'a, Result<Vec<Listed>, Failure>> {
            Box::pin(async move {
                let (root, prefix, after) =
                    (self.root.clone(), prefix.to_owned(), after.to_owned());
                blocking(move || {
                    let mut found = Vec::new();
                    walk(&root, &root, &prefix, &after, &mut found)?;
                    found.sort_unstable_by(|a, b| a.key.cmp(&b.key));
                    found.truncate(limit);
                    Ok(found)
                })
                .await
            })
        }
    }
}
