use crate::{Error, Token};
use nanocode_http::Method;
use nanocode_http::header::{HeaderMap, HeaderName, HeaderValue};
use nanocode_http::layer::{Request, Sender};
use nanocode_sync::StreamExt;
use nanocode_sys::base64::STANDARD;
use serde_json::Value;
use std::time::Duration;

pub struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self, url: &str) -> Result<Value, Error> {
        if !(200..300).contains(&self.status) {
            return Err(Error::new(format!(
                "{url} returned {}: {}",
                self.status,
                self.text().trim()
            )));
        }
        serde_json::from_slice(&self.body)
            .map_err(|error| Error::new(format!("{url} returned invalid JSON: {error}")))
    }
}

pub async fn call(
    sender: &dyn Sender,
    method: Method,
    url: &str,
    headers: &[(HeaderName, String)],
    body: Vec<u8>,
) -> Result<Reply, Error> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let mut value = HeaderValue::from_str(value)
            .map_err(|error| Error::new(format!("invalid {name} header for {url}: {error}")))?;
        value.set_sensitive(true);
        map.insert(name.clone(), value);
    }
    let response = sender
        .send(Request {
            method,
            url: url.to_owned(),
            headers: map,
            body: body.into(),
        })
        .await
        .map_err(|error| Error::new(format!("{url}: {}", error.detail())))?;
    let status = response.status.as_u16();
    let headers = response.headers;
    let mut stream = response.body;
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(
            &chunk.map_err(|error| Error::new(format!("{url}: {}", error.detail())))?,
        );
    }
    Ok(Reply {
        status,
        headers,
        body,
    })
}

pub const fn named(name: &'static str) -> HeaderName {
    HeaderName::from_static(name)
}

pub async fn form(
    sender: &dyn Sender,
    url: &str,
    pairs: &[(&str, &str)],
    basic: Option<(&str, &str)>,
) -> Result<Value, Error> {
    let mut serializer = nanocode_url::form::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    let mut headers = vec![(
        named("content-type"),
        "application/x-www-form-urlencoded".to_owned(),
    )];
    if let Some((id, secret)) = basic {
        headers.push((
            named("authorization"),
            format!("Basic {}", STANDARD.encode(format!("{id}:{secret}"))),
        ));
    }
    call(
        sender,
        Method::POST,
        url,
        &headers,
        serializer.finish().into_bytes(),
    )
    .await?
    .json(url)
}

pub async fn post_json(
    sender: &dyn Sender,
    url: &str,
    bearer: &str,
    body: &Value,
) -> Result<Value, Error> {
    let headers = [
        (named("content-type"), "application/json".to_owned()),
        (named("authorization"), format!("Bearer {bearer}")),
    ];
    call(
        sender,
        Method::POST,
        url,
        &headers,
        body.to_string().into_bytes(),
    )
    .await?
    .json(url)
}

pub fn field<'a>(value: &'a Value, name: &str, url: &str) -> Result<&'a str, Error> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| Error::new(format!("{url} returned no {name}")))
}

pub fn oauth_token(value: &Value, url: &str) -> Result<Token, Error> {
    let token = field(value, "access_token", url)?.to_owned();
    let lifetime = value
        .get("expires_in")
        .and_then(|seconds| {
            seconds
                .as_u64()
                .or_else(|| seconds.as_str().and_then(|text| text.parse().ok()))
        })
        .map(Duration::from_secs);
    Ok(Token::lasting(token, lifetime))
}

pub fn iam_token(value: &Value, url: &str) -> Result<Token, Error> {
    let token = field(value, "accessToken", url)?.to_owned();
    let expires = field(value, "expireTime", url)?;
    let expires = crate::time::parse_rfc3339(expires).ok_or_else(|| {
        Error::new(format!(
            "{url} returned an unreadable expireTime {expires:?}"
        ))
    })?;
    Ok(Token::until(token, expires))
}
