use std::{future::Future, pin::Pin};

use nanocode_bytes::Bytes;
use nanocode_http::{Client, Method, Url, redirect};
use nanocode_shell::{Failure, Fetch, Request, Response};

pub struct Http {
    client: Client,
}

impl Http {
    pub fn new() -> Result<Self, nanocode_http::Error> {
        let client = Client::builder()
            .redirect(redirect::Policy::none())
            .build()?;
        Ok(Self { client })
    }
}

impl Fetch for Http {
    fn fetch(&self, request: Request) -> Pin<Box<dyn Future<Output = Result<Response, Failure>>>> {
        let client = self.client.clone();
        Box::pin(async move { fetch(&client, request).await })
    }
}

async fn fetch(client: &Client, request: Request) -> Result<Response, Failure> {
    let mut method = Method::from_bytes(request.method.as_bytes()).map_err(other)?;
    let mut url = Url::parse(&request.url).map_err(other)?;
    let mut body = request.body.clone();
    let mut redirects = 0;
    loop {
        let mut builder = client.request(method.clone(), url.clone());
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        let mut response = builder.body(body.to_vec()).send().await.map_err(failure)?;
        let status = response.status();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if let (Some(follow), Some(location), true) =
            (request.follow, location, status.is_redirection())
        {
            if redirects >= follow {
                return Err(Failure::Redirects);
            }
            redirects += 1;
            url = url.join(&location).map_err(other)?;
            if status.as_u16() == 303 || (status.as_u16() < 307 && method == Method::POST) {
                method = Method::GET;
                body = Bytes::new();
            }
            continue;
        }
        let mut collected = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if (collected.len() + chunk.len()) as u64 > request.limit {
                return Err(Failure::TooLarge);
            }
            collected.extend_from_slice(&chunk);
        }
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        return Ok(Response {
            protocol: format!("{:?}", response.version()),
            status: status.as_u16(),
            reason: status.canonical_reason().unwrap_or_default().to_owned(),
            headers,
            body: Bytes::from(collected),
            url: response.url().to_string(),
            redirects,
        });
    }
}

fn failure(error: nanocode_http::Error) -> Failure {
    match (error.is_timeout(), error.is_connect()) {
        (true, _) => Failure::Timeout,
        (false, true) => Failure::Connect,
        (false, false) => Failure::Other(error.to_string()),
    }
}

fn other(error: impl std::fmt::Display) -> Failure {
    Failure::Other(error.to_string())
}
