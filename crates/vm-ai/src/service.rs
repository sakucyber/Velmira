use std::{collections::BTreeMap, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use zeroize::Zeroizing;

use crate::{
    pool::{
        self, Capability, Config, Defaults, Failure, FailureKind, ModelEntry, ModelPool,
        ModelProvider, Pool, Recovery, TaskBinding,
    },
    reading::{Action, Language, ReadingError},
    route::{RouteError, Strategy, Unavailable},
    ChatCompletion, ComicPage, Error, Storyboard,
};

pub struct Connection {
    pub endpoint: String,
    pub key: String,
    pub text_model: String,
    pub image_model: String,
    pub rpm: u32,
    pub concurrency: u16,
}

pub struct ComicService {
    pool: Pool,
}

pub struct ChatRequest<'a> {
    pub system: &'a str,
    pub text: &'a str,
    pub image: Option<&'a [u8]>,
}

impl ComicService {
    pub fn new(connections: Vec<Connection>, strategy: Strategy) -> Result<Self, Error> {
        let (config, keys) = legacy_config(connections, strategy);
        Self::from_config(config, |reference| {
            keys.get(reference).map(|key| key.to_string())
        })
    }

    pub fn from_config(
        config: Config,
        resolve: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, Error> {
        Pool::new(config, resolve)
            .map(|pool| Self { pool })
            .map_err(|error| Error::Pool(error.to_string()))
    }

    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    pub fn chat(&self, task: &str, request: ChatRequest<'_>) -> Result<(String, String), Error> {
        self.chat_checked(task, request, Ok)
    }

    pub(crate) fn chat_checked<T>(
        &self,
        task: &str,
        request: ChatRequest<'_>,
        check: impl Fn(String) -> Result<T, Error>,
    ) -> Result<(String, T), Error> {
        if request.text.trim().is_empty()
            || request.text.chars().take(12_001).count() > 12_000
            || request.system.chars().take(12_001).count() > 12_000
        {
            return Err(Error::Source);
        }
        let (capability, content) = if let Some(image) = request.image {
            if image.len() > 8 * 1024 * 1024 {
                return Err(Error::Image);
            }
            let format = if crate::image_extension(image)? == "jpg" {
                "jpeg"
            } else {
                "png"
            };
            let url = format!("data:image/{format};base64,{}", STANDARD.encode(image));
            (
                Capability::Vision,
                json!([{"type": "text", "text": request.text}, {"type":"image_url", "image_url": {"url": url}}]),
            )
        } else {
            (Capability::Chat, json!(request.text))
        };
        self.pool
            .call_with_pool(task, capability, |provider, entry, timeout| {
                let response: ChatCompletion = provider
                    .post(
                        "chat/completions",
                        json!({
                            "model": entry.model_name,
                            "messages": [
                                {"role": "system", "content": request.system},
                                {"role": "user", "content": content}
                            ]
                        }),
                        256 * 1024,
                        timeout,
                    )
                    .map_err(|error| classify(error, false))?;
                let text = response
                    .choices
                    .into_iter()
                    .next()
                    .map(|choice| choice.message.content)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| classify(Error::Response, false))?;
                Ok((
                    check(text).map_err(|error| classify(error, false))?,
                    response.usage,
                ))
            })
            .map(|reply| (reply.model_name, reply.outcome))
            .map_err(routing_error)
    }

    pub fn storyboard(&self, source: &str) -> Result<Storyboard, Error> {
        self.storyboard_with_continuity(source, "")
    }

    pub fn storyboard_with_continuity(
        &self,
        source: &str,
        continuity: &str,
    ) -> Result<Storyboard, Error> {
        self.pool
            .call_with_pool("planner", Capability::Chat, |provider, model, remaining| {
                provider
                    .storyboard_with_usage(source, continuity, &model.model_name, remaining)
                    .map_err(|error| classify(error, false))
            })
            .map(|reply| reply.outcome)
            .map_err(routing_error)
    }

    pub fn read_excerpt(
        &self,
        action: Action,
        language: Language,
        excerpt: &str,
    ) -> Result<(String, String), RouteError<ReadingError>> {
        self.read_excerpt_with_prompt(action, language, excerpt, "")
    }

    pub fn read_excerpt_with_prompt(
        &self,
        action: Action,
        language: Language,
        excerpt: &str,
        prompt: &str,
    ) -> Result<(String, String), RouteError<ReadingError>> {
        let task = match action {
            Action::Summarize => "summary",
            Action::Translate if self.pool.has_task("translate") => "translate",
            _ => "reply",
        };
        self.pool
            .call_with_pool(task, Capability::Chat, |provider, model, remaining| {
                provider
                    .read_with_usage(
                        action,
                        language,
                        excerpt,
                        prompt,
                        &model.model_name,
                        remaining,
                    )
                    .map_err(|error| match error {
                        ReadingError::Provider(error) => {
                            let failure = classify(error, false);
                            Failure {
                                error: ReadingError::Provider(failure.error),
                                kind: failure.kind,
                                recovery: failure.recovery,
                            }
                        }
                        ReadingError::Output => Failure {
                            error,
                            kind: FailureKind::InvalidResponse,
                            recovery: Recovery::NextModel,
                        },
                        other => Failure {
                            error: other,
                            kind: FailureKind::InvalidRequest,
                            recovery: Recovery::Stop,
                        },
                    })
            })
            .map(|reply| (reply.model_name, reply.outcome))
    }

    pub fn render(&self, board: &Storyboard) -> Result<ComicPage, Error> {
        self.render_layout(board, crate::manga::Layout::default())
    }

    pub fn render_layout(
        &self,
        board: &Storyboard,
        layout: crate::manga::Layout,
    ) -> Result<ComicPage, Error> {
        self.render_styled(board, layout, "")
    }

    pub fn render_styled(
        &self,
        board: &Storyboard,
        layout: crate::manga::Layout,
        style: &str,
    ) -> Result<ComicPage, Error> {
        self.pool
            .call_with_pool(
                "manga_image",
                Capability::ImageGeneration,
                |provider, model, remaining| {
                    provider
                        .render_with_usage(
                            board.clone(),
                            &model.model_name,
                            remaining,
                            layout,
                            style,
                            model.workflow.as_ref(),
                        )
                        .map_err(|error| classify(error, true))
                },
            )
            .map(|reply| reply.outcome)
            .map_err(routing_error)
    }
}

pub type SessionKeys = BTreeMap<String, Zeroizing<String>>;

pub fn legacy_config(connections: Vec<Connection>, strategy: Strategy) -> (Config, SessionKeys) {
    let mut config = Config {
        schema_version: pool::SCHEMA_VERSION,
        defaults: Defaults {
            total_timeout_ms: 180_000,
            attempt_timeout_ms: 150_000,
            max_retries: 2,
            ..Defaults::default()
        },
        providers: BTreeMap::new(),
        models: BTreeMap::new(),
        pools: BTreeMap::new(),
        task_bindings: BTreeMap::new(),
    };
    let mut secrets = BTreeMap::new();
    for (index, connection) in connections.into_iter().enumerate() {
        let id = format!("connection-{}", index + 1);
        let reference = format!("session:{id}");
        let keys = if connection.key.trim().is_empty() {
            Vec::new()
        } else {
            secrets.insert(reference.clone(), Zeroizing::new(connection.key));
            vec![reference]
        };
        config.providers.insert(
            id.clone(),
            ModelProvider {
                protocol: pool::Protocol::OpenaiCompatible,
                base_url: connection.endpoint,
                keys,
                attempt_timeout_ms: None,
                max_retries: None,
                rpm: connection.rpm,
                max_concurrency: connection.concurrency,
            },
        );
        let text = connection.text_model.trim();
        let image = connection.image_model.trim();
        for (label, name, capability) in [
            ("text", text, Capability::Chat),
            ("image", image, Capability::ImageGeneration),
        ]
        .into_iter()
        .filter(|(_, name, _)| !name.is_empty())
        {
            let model_id = if text == image {
                format!("{id}-text")
            } else {
                format!("{id}-{label}")
            };
            config
                .models
                .entry(model_id.clone())
                .or_insert_with(|| ModelEntry {
                    provider: id.clone(),
                    model_name: name.into(),
                    priority: 0,
                    weight: 1,
                    capabilities: Vec::new(),
                    workflow: None,
                })
                .capabilities
                .push(capability);
            config
                .pools
                .entry(label.into())
                .or_insert_with(|| ModelPool(Vec::new()))
                .0
                .push(model_id);
        }
    }
    for (task, name, requires) in [
        ("reply", "text", Capability::Chat),
        ("summary", "text", Capability::Chat),
        ("translate", "text", Capability::Chat),
        ("planner", "text", Capability::Chat),
        ("manga_image", "image", Capability::ImageGeneration),
    ] {
        if config.pools.contains_key(name) {
            config.task_bindings.insert(
                task.into(),
                TaskBinding {
                    pool: name.into(),
                    strategy: strategy.into(),
                    requires,
                },
            );
        }
    }
    (config, secrets)
}

fn classify(error: Error, image: bool) -> Failure<Error> {
    use FailureKind as Kind;
    let next = if image {
        Recovery::Stop
    } else {
        Recovery::NextModel
    };
    let (kind, recovery) = match &error {
        Error::Auth(401) => (Kind::Authentication, Recovery::DisableKey),
        Error::Auth(_) => (Kind::Authentication, Recovery::CoolKey(Duration::ZERO)),
        Error::Balance => (Kind::Quota, Recovery::CoolKey(Duration::ZERO)),
        Error::RateLimited(delay) => (
            Kind::RateLimit,
            Recovery::CoolKey(delay.unwrap_or_default()),
        ),
        Error::Connect => (Kind::Connect, Recovery::NextModel),
        Error::Status(404) => (Kind::ModelUnavailable, Recovery::NextModel),
        Error::ImageBackend => (Kind::Server, Recovery::NextModel),
        Error::Status(code) if *code >= 500 => (Kind::Server, next),
        Error::Timeout => (Kind::Timeout, next),
        Error::Network => (Kind::Connect, next),
        Error::Response | Error::ResponseTooLarge | Error::Storyboard | Error::Image => {
            (Kind::InvalidResponse, next)
        }
        _ => (Kind::InvalidRequest, Recovery::Stop),
    };
    Failure {
        error,
        kind,
        recovery,
    }
}

fn routing_error(error: RouteError<Error>) -> Error {
    match error {
        RouteError::Aborted { error, .. } => error,
        RouteError::Exhausted { last, attempts: 1 } => last,
        RouteError::Unavailable {
            reason: Unavailable::Deadline,
        } => Error::Timeout,
        other => Error::Pool(other.to_string()),
    }
}
