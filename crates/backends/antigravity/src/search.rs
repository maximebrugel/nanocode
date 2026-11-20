use crate::prompts::SEARCH;
use nanocode_backend_antigravity_client::{Client, Json};
use nanocode_backend_antigravity_tools::{WebSearch, WebSearchFuture};

pub struct GoogleSearch {
    pub client: Client,
    pub model: String,
}

impl WebSearch for GoogleSearch {
    fn search<'a>(&'a self, query: &'a str, domain: Option<&'a str>) -> WebSearchFuture<'a> {
        Box::pin(async move {
            let text = match domain {
                Some(domain) => format!("{query} site:{domain}"),
                None => query.to_owned(),
            };
            let body = Json::object(
                [
                    (
                        "contents",
                        Json::Array(vec![Json::object([
                            ("role", Json::str("user")),
                            (
                                "parts",
                                Json::Array(vec![Json::object([("text", Json::str(text))])]),
                            ),
                        ])]),
                    ),
                    (
                        "systemInstruction",
                        Json::object([
                            ("role", Json::str("user")),
                            (
                                "parts",
                                Json::Array(vec![Json::object([("text", Json::str(SEARCH))])]),
                            ),
                        ]),
                    ),
                    (
                        "tools",
                        Json::Array(vec![Json::object([(
                            "googleSearch",
                            Json::object([(
                                "enhancedContent",
                                Json::object([(
                                    "imageSearch",
                                    Json::object([("maxResultCount", Json::int(5))]),
                                )]),
                            )]),
                        )])]),
                    ),
                    (
                        "labels",
                        Json::object([
                            ("client", Json::str("antigravity-cli")),
                            (
                                "goog-originating-logical-product-id",
                                Json::str("48c6cd67-05ce-4112-a170-ad8e0c7c136c"),
                            ),
                            ("goog-originating-service-id", Json::str("a153-a454-6272")),
                        ]),
                    ),
                    (
                        "generationConfig",
                        Json::object([("candidateCount", Json::int(1))]),
                    ),
                ]
                .into_iter()
                .filter(|(key, _)| *key != "labels" || !self.client.endpoint().route.is_vertex()),
            )
            .encode();
            let reply = self
                .client
                .generate(&self.model, &body)
                .await
                .map_err(|error| error.to_string())?;
            let summary = reply
                .parts
                .iter()
                .filter(|part| !part.is_thought())
                .filter_map(|part| part.text.as_deref())
                .collect::<String>();
            if summary.trim().is_empty() {
                return Err("no summary returned from GenerateContent".to_owned());
            }
            let sources = reply
                .grounding
                .as_ref()
                .and_then(|grounding| grounding.get("groundingChunks"))
                .and_then(serde_json::Value::as_array)
                .map(|chunks| {
                    chunks
                        .iter()
                        .filter_map(|chunk| chunk.get("web"))
                        .enumerate()
                        .map(|(index, web)| {
                            let field = |key: &str| {
                                web.get(key)
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned()
                            };
                            format!("[{}] [{}]({})", index + 1, field("title"), field("uri"))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut out =
                format!("The search for {query:?} returned the following summary:\n{summary}");
            if !sources.is_empty() {
                out.push_str("\n\nSources:\n");
                out.push_str(&sources.join("\n"));
            }
            out.push_str("\n\nNote: Source URLs are HTTP redirects; check where they resolve if you need the direct link.");
            Ok(out)
        })
    }
}
