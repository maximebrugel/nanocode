use crate::error::{Error, Operation};
use crate::request::{ByteRange, Config, Prepared, Presigned};
use crate::response::{
    Head, Put, delete_status, error_code, get_expect, head_response, put_status, wants_error_code,
};
use crate::sign::SHA256_BYTES;

const MILLIS_PER_SECOND: u64 = 1_000;

fn now_unix_seconds() -> u64 {
    nanocode_platform::unix_millis() / MILLIS_PER_SECOND
}

fn header<'a>(response: &'a nanocode_http::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

pub struct Client {
    config: Config,
    http: nanocode_http::Client,
}

impl Client {
    pub const fn new(config: Config, http: nanocode_http::Client) -> Self {
        Self { config, http }
    }

    pub const fn config(&self) -> &Config {
        &self.config
    }

    async fn send(
        &self,
        operation: Operation,
        prepared: Prepared,
        body: Option<Vec<u8>>,
    ) -> Result<nanocode_http::Response, Error> {
        let mut request = self.http.request(prepared.method, prepared.url.as_str());
        for (name, value) in prepared.headers {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        request.send().await.map_err(|error| Error::Transport {
            operation,
            detail: nanocode_http::error_detail(&error),
            retryable: nanocode_http::retryable(&error),
        })
    }

    async fn read_body(
        response: &mut nanocode_http::Response,
        operation: Operation,
        bytes_max: u64,
        bytes_expected: Option<u64>,
    ) -> Result<Vec<u8>, Error> {
        let too_large = Error::ResponseTooLarge {
            operation,
            bytes_max,
        };
        let capacity = bytes_expected.unwrap_or(0).min(bytes_max);
        let mut bytes =
            Vec::with_capacity(usize::try_from(capacity).map_err(|_| too_large.clone())?);
        while let Some(chunk) = response.chunk().await.map_err(|error| Error::Transport {
            operation,
            detail: nanocode_http::error_detail(&error),
            retryable: nanocode_http::retryable(&error),
        })? {
            if (bytes.len() + chunk.len()) as u64 > bytes_max {
                return Err(too_large);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn code(
        &self,
        response: &mut nanocode_http::Response,
        operation: Operation,
    ) -> Result<Option<String>, Error> {
        if !wants_error_code(response.status().as_u16()) {
            return Ok(None);
        }
        let body =
            Self::read_body(response, operation, self.config.response_bytes_max(), None).await?;
        Ok(error_code(&body))
    }

    pub async fn put_if_absent(
        &self,
        key: &str,
        bytes: Vec<u8>,
        sha256: &[u8; SHA256_BYTES],
    ) -> Result<Put, Error> {
        let prepared =
            self.config
                .put_if_absent(key, bytes.len() as u64, sha256, now_unix_seconds())?;
        let mut response = self.send(Operation::Put, prepared, Some(bytes)).await?;
        let code = self.code(&mut response, Operation::Put).await?;
        put_status(response.status().as_u16(), code.as_deref())
    }

    pub async fn get(&self, key: &str, range: Option<ByteRange>) -> Result<Option<Vec<u8>>, Error> {
        let prepared = self.config.get(key, range, now_unix_seconds())?;
        let mut response = self.send(Operation::Get, prepared, None).await?;
        let code = self.code(&mut response, Operation::Get).await?;
        let expected = get_expect(
            response.status().as_u16(),
            code.as_deref(),
            range,
            header(&response, "content-range"),
            header(&response, "content-length"),
            self.config.response_bytes_max(),
        )?;
        let Some(expected) = expected else {
            return Ok(None);
        };
        let bytes =
            Self::read_body(&mut response, Operation::Get, expected, Some(expected)).await?;
        if bytes.len() as u64 != expected {
            return Err(Error::Malformed {
                operation: Operation::Get,
                problem: "body shorter than content-length",
            });
        }
        Ok(Some(bytes))
    }

    pub async fn head(&self, key: &str) -> Result<Option<Head>, Error> {
        let prepared = self.config.head(key, now_unix_seconds())?;
        let response = self.send(Operation::Head, prepared, None).await?;
        head_response(
            response.status().as_u16(),
            header(&response, "content-length"),
            header(&response, "etag"),
            header(&response, "x-amz-checksum-sha256"),
        )
    }

    pub async fn delete(&self, key: &str) -> Result<(), Error> {
        let prepared = self.config.delete(key, now_unix_seconds())?;
        let mut response = self.send(Operation::Delete, prepared, None).await?;
        let code = self.code(&mut response, Operation::Delete).await?;
        delete_status(response.status().as_u16(), code.as_deref())
    }

    pub fn presign_get(&self, key: &str, expires_seconds: u64) -> Result<Presigned, Error> {
        self.config
            .presign_get(key, expires_seconds, now_unix_seconds())
    }

    pub fn presign_put(
        &self,
        key: &str,
        bytes: u64,
        sha256: &[u8; SHA256_BYTES],
        expires_seconds: u64,
    ) -> Result<Presigned, Error> {
        self.config
            .presign_put(key, bytes, sha256, expires_seconds, now_unix_seconds())
    }
}
