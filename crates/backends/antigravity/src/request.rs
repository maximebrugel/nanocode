use crate::profile::Profile;
use nanocode_backend_antigravity_client::{Content, Json};

pub struct Ids {
    pub cascade: String,
    pub trajectory: String,
}

pub struct Prefix {
    pub system: Json,
    pub tools: Json,
}

impl Prefix {
    pub fn new(system: &str, declarations: &[String]) -> Self {
        Self {
            system: Json::object([
                ("role", Json::str("user")),
                (
                    "parts",
                    Json::Array(vec![Json::object([("text", Json::str(system))])]),
                ),
            ]),
            tools: Json::Array(
                declarations
                    .iter()
                    .map(|declaration| {
                        Json::object([(
                            "functionDeclarations",
                            Json::Array(vec![Json::Raw(declaration.clone())]),
                        )])
                    })
                    .collect(),
            ),
        }
    }
}

pub struct Request<'a> {
    pub contents: &'a [Content],
    pub extra: Option<&'a Content>,
    pub prefix: &'a Prefix,
    pub cached: Option<&'a str>,
    pub profile: &'a Profile,
    pub ids: &'a Ids,
    pub step: u64,
    pub call: u64,
    pub max_output_tokens: u32,
    pub execution: Option<&'a str>,
    pub labels: bool,
}

impl Request<'_> {
    pub fn encode(&self) -> String {
        let mut contents = self
            .contents
            .iter()
            .map(Content::to_json)
            .collect::<Vec<_>>();
        if let Some(extra) = self.extra {
            contents.push(extra.to_json());
        }
        let mut fields = vec![("contents".to_owned(), Json::Array(contents))];
        match self.cached {
            Some(name) => fields.push(("cachedContent".to_owned(), Json::str(name))),
            None => {
                fields.push(("systemInstruction".to_owned(), self.prefix.system.clone()));
                fields.push(("tools".to_owned(), self.prefix.tools.clone()));
            }
        }
        let flag = |value: bool| Json::str(if value { "true" } else { "false" });
        fields.extend(self.labels.then(|| {
            (
                "labels".to_owned(),
                Json::Object(
                    [
                        ("cascade_id", Json::str(self.ids.cascade.clone())),
                        ("client", Json::str("antigravity-cli")),
                        (
                            "goog-originating-logical-product-id",
                            Json::str("48c6cd67-05ce-4112-a170-ad8e0c7c136c"),
                        ),
                        ("goog-originating-service-id", Json::str("a153-a454-6272")),
                    ]
                    .into_iter()
                    .chain(
                        self.execution
                            .map(|execution| ("last_execution_id", Json::str(execution))),
                    )
                    .chain([
                        ("last_step_index", Json::str(self.step.to_string())),
                        ("model_enum", Json::str(self.profile.model_enum)),
                        (
                            "request_id",
                            Json::str(format!("{}-{}", self.ids.trajectory, self.call)),
                        ),
                        ("root_cascade_id", Json::str(self.ids.cascade.clone())),
                        ("trajectory_id", Json::str(self.ids.trajectory.clone())),
                        ("used_claude", flag(false)),
                        ("used_claude_conservative", flag(false)),
                        ("used_non_gemini_model", flag(!self.profile.gemini)),
                    ])
                    .map(|(key, value)| (key.to_owned(), value))
                    .collect(),
                ),
            )
        }));
        fields.push((
            "generationConfig".to_owned(),
            self.profile.generation(self.max_output_tokens),
        ));
        Json::Object(fields).encode()
    }
}
