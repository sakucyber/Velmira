use std::{collections::HashMap, time::Duration};

use crate::route::{Candidate, Failure, Limits, Policy, RouteError, Router, Strategy, Unavailable};
use crate::{ComicPage, Error, Provider, Storyboard};

pub struct Connection {
    pub endpoint: String,
    pub key: String,
    pub text_model: String,
    pub image_model: String,
    pub rpm: u32,
    pub concurrency: u16,
}

pub struct ComicService {
    connections: HashMap<String, Provider>,
    router: Router,
}

impl ComicService {
    pub fn new(connections: Vec<Connection>, strategy: Strategy) -> Result<Self, Error> {
        let mut providers = HashMap::with_capacity(connections.len());
        let mut candidates = Vec::new();
        for (index, connection) in connections.into_iter().enumerate() {
            let name = (index + 1).to_string();
            let provider = Provider::new(
                &connection.endpoint,
                connection.key,
                &connection.text_model,
                &connection.image_model,
            )?;
            let limits = Limits {
                rpm: connection.rpm,
                concurrency: connection.concurrency,
            };
            for model in [&provider.text_model, &provider.image_model] {
                if model.is_empty() {
                    continue;
                }
                let candidate = Candidate {
                    connection: name.clone(),
                    model: model.clone(),
                    limits,
                };
                if !candidates.contains(&candidate) {
                    candidates.push(candidate);
                }
            }
            providers.insert(name, provider);
        }
        let router = Router::new(
            candidates,
            Policy {
                strategy,
                timeout: Duration::from_secs(180),
                ..Policy::default()
            },
        )
        .map_err(|error| Error::Pool(error.to_string()))?;
        Ok(Self {
            connections: providers,
            router,
        })
    }

    pub fn storyboard(&self, source: &str) -> Result<Storyboard, Error> {
        self.router
            .run_filtered(
                |candidate| self.connections[&candidate.connection].text_model == candidate.model,
                |candidate, remaining| {
                    self.connections[&candidate.connection]
                        .storyboard(
                            source,
                            &candidate.model,
                            remaining.min(Duration::from_secs(60)),
                        )
                        .map_err(classify)
                },
            )
            .map_err(routing_error)
    }

    pub fn render(&self, board: &Storyboard) -> Result<ComicPage, Error> {
        self.render_layout(board, crate::manga::Layout::default())
    }

    pub fn render_layout(
        &self,
        board: &Storyboard,
        layout: crate::manga::Layout,
    ) -> Result<ComicPage, Error> {
        self.router
            .run_filtered(
                |candidate| self.connections[&candidate.connection].image_model == candidate.model,
                |candidate, remaining| {
                    self.connections[&candidate.connection]
                        .render_layout(board.clone(), &candidate.model, remaining, layout)
                        .map_err(classify)
                },
            )
            .map_err(routing_error)
    }
}

fn classify(error: Error) -> Failure<Error> {
    match error {
        Error::RateLimited(Some(delay)) => Failure::cooldown(error).with_retry_after(delay),
        Error::RateLimited(None) => Failure::cooldown(error),
        Error::Connect => Failure::retry(error),
        Error::Auth(_) | Error::Balance | Error::Status(404) => Failure::failover(error),
        // A timeout or failure after submission can still result in a charge.
        _ => Failure::abort(error),
    }
}

fn routing_error(error: RouteError<Error>) -> Error {
    match error {
        RouteError::Aborted { error, .. } | RouteError::Exhausted { last: error, .. } => error,
        RouteError::Unavailable {
            reason: Unavailable::Deadline,
        } => Error::Timeout,
        RouteError::Unavailable {
            reason: Unavailable::Cooldown,
        } => Error::RateLimited(None),
        RouteError::Unavailable {
            reason: Unavailable::Capacity,
        } => Error::Pool("连接达到请求频率或并发上限".into()),
        RouteError::Unavailable {
            reason: Unavailable::NoCandidate,
        } => Error::Pool("没有匹配的模型".into()),
    }
}
