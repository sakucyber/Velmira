use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    hash::{BuildHasher, RandomState},
    sync::{Condvar, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    route::{Availability, Connection, Limits, RouteError, Unavailable},
    Provider,
};

#[path = "pool_config.rs"]
mod config;
pub use config::*;
#[path = "pool_comfy.rs"]
pub(crate) mod comfy;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Ordered,
    RoundRobin,
    Random,
    LeastUsage,
}

impl From<crate::route::Strategy> for Strategy {
    fn from(strategy: crate::route::Strategy) -> Self {
        match strategy {
            crate::route::Strategy::Ordered => Self::Ordered,
            crate::route::Strategy::RoundRobin => Self::RoundRobin,
            crate::route::Strategy::Random => Self::Random,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("模型池配置无效：{0}")]
    Config(String),
    #[error("未设置任务 {0} 的模型池")]
    UnknownTask(String),
    #[error("当前任务没有可用模型，请检查密钥、并发或冷却状态")]
    Unavailable,
    #[error(transparent)]
    Transport(#[from] crate::Error),
}

impl Error {
    fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Usage {
    #[serde(default, alias = "prompt_tokens")]
    pub input_tokens: Option<u64>,
    #[serde(default, alias = "completion_tokens")]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

impl Usage {
    fn total(self) -> Option<u64> {
        self.total_tokens
            .or_else(|| Some(self.input_tokens?.saturating_add(self.output_tokens?)))
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Authentication,
    Quota,
    RateLimit,
    Connect,
    Timeout,
    Server,
    ModelUnavailable,
    InvalidRequest,
    InvalidResponse,
    Cancelled,
}

#[derive(Clone, Copy)]
pub(crate) enum Recovery {
    Stop,
    NextModel,
    DisableKey,
    CoolKey(Duration),
}

pub(crate) struct Failure<E> {
    pub error: E,
    pub kind: FailureKind,
    pub recovery: Recovery,
}

#[derive(Serialize)]
pub struct ModelStatus {
    pub model_id: String,
    pub model_name: String,
    pub provider: String,
    pub successful: u64,
    pub failed: u64,
    pub calls: u64,
    pub in_flight: u16,
    pub average_ms: Option<f64>,
    pub failure_rate: Option<f64>,
    pub total_tokens: Option<u64>,
    pub usage_reports: u64,
    pub last_error: Option<FailureKind>,
    pub degraded: bool,
    pub ready_keys: usize,
    pub cooling_keys: usize,
    pub rejected_keys: usize,
    pub missing_keys: usize,
}

#[derive(Serialize)]
pub struct PoolStatus {
    pub task_bindings: BTreeMap<String, TaskBinding>,
    pub pools: BTreeMap<String, ModelPool>,
    pub models: Vec<ModelStatus>,
    pub recent_attempts: Vec<AttemptLog>,
}

#[derive(Clone, Serialize)]
pub struct AttemptLog {
    pub unix_ms: u64,
    pub task: String,
    pub pool: String,
    pub model: String,
    pub provider: String,
    pub elapsed_ms: u64,
    pub usage: Option<Usage>,
    pub success: bool,
    pub error: Option<FailureKind>,
}

pub struct Selection {
    pub model_id: String,
    pub model: ModelEntry,
}

pub(crate) struct Reply<T> {
    pub outcome: T,
    pub model_name: String,
}

struct Endpoint {
    attempts: u8,
    timeout: Duration,
    keys: Vec<Provider>,
    missing_keys: usize,
}

struct Model {
    id: String,
    entry: ModelEntry,
    provider: usize,
}

struct Task {
    config: TaskBinding,
    models: Vec<usize>,
}

#[derive(Default)]
struct Stats {
    calls: u64,
    in_flight: u16,
    successful: u64,
    failed: u64,
    elapsed_ms: u64,
    tokens: u64,
    usage_reports: u64,
    penalty_until: Option<Instant>,
    recent_failures: u64,
    last_error: Option<FailureKind>,
    last_selected: u64,
}

impl Stats {
    fn degraded(&self, now: Instant) -> bool {
        self.penalty_until.is_some_and(|until| until > now)
    }
}

#[derive(Clone, Copy, Default)]
enum KeyHealth {
    #[default]
    Ready,
    Cooling(Instant),
    Rejected,
}

impl KeyHealth {
    fn available(self, now: Instant) -> bool {
        match self {
            Self::Ready => true,
            Self::Cooling(until) => until <= now,
            Self::Rejected => false,
        }
    }
}

struct ProviderState {
    budget: Connection,
    keys: Vec<KeyHealth>,
    cursor: usize,
}

struct State {
    stats: Vec<Stats>,
    providers: Vec<ProviderState>,
    cursor: HashMap<String, usize>,
    sequence: u64,
    recent_attempts: VecDeque<AttemptLog>,
}

pub struct Pool {
    config: Config,
    endpoints: Vec<Endpoint>,
    models: Vec<Model>,
    tasks: BTreeMap<String, Task>,
    random: RandomState,
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Candidate {
    model: usize,
    key: usize,
}

struct RequestState {
    visited: HashSet<Candidate>,
    attempts: Vec<u8>,
    pinned_model: Option<usize>,
}

impl Pool {
    pub fn new(
        config: Config,
        mut resolve: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, Error> {
        config.validate()?;
        let mut states = Vec::new();
        let mut endpoints = Vec::new();
        let mut provider_ids = BTreeMap::new();
        for (id, item) in &config.providers {
            let mut keys = Vec::<Provider>::new();
            let mut missing_keys = 0;
            if item.keys.is_empty() {
                keys.push(Provider::connect(
                    crate::parse_endpoint(&item.base_url)?,
                    String::new(),
                )?);
            }
            for reference in &item.keys {
                match resolve(reference).filter(|key| !key.trim().is_empty()) {
                    Some(key) if keys.iter().all(|existing| existing.key.as_str() != key) => {
                        keys.push(Provider::connect(
                            crate::parse_endpoint(&item.base_url)?,
                            key,
                        )?);
                    }
                    Some(_) => {} // Two references to the same secret share a single slot.
                    None => missing_keys += 1,
                }
            }
            provider_ids.insert(id, endpoints.len());
            states.push(ProviderState {
                budget: Connection::new(Limits {
                    rpm: item.rpm,
                    concurrency: item.max_concurrency,
                }),
                keys: vec![KeyHealth::Ready; keys.len()],
                cursor: 0,
            });
            endpoints.push(Endpoint {
                attempts: 1 + item.max_retries.unwrap_or(config.defaults.max_retries),
                timeout: Duration::from_millis(
                    item.attempt_timeout_ms
                        .unwrap_or(config.defaults.attempt_timeout_ms),
                ),
                keys,
                missing_keys,
            });
        }
        let models: Vec<_> = config
            .models
            .iter()
            .map(|(id, entry)| Model {
                id: id.clone(),
                entry: entry.clone(),
                provider: provider_ids[&entry.provider],
            })
            .collect();
        let ids: HashMap<_, _> = models
            .iter()
            .enumerate()
            .map(|(index, model)| (model.id.as_str(), index))
            .collect();
        let tasks = config
            .task_bindings
            .iter()
            .map(|(id, binding)| {
                let candidates = config.pools[&binding.pool]
                    .0
                    .iter()
                    .filter(|name| {
                        config.models[*name]
                            .capabilities
                            .contains(&binding.requires)
                    })
                    .map(|name| ids[name.as_str()])
                    .collect();
                (
                    id.clone(),
                    Task {
                        config: binding.clone(),
                        models: candidates,
                    },
                )
            })
            .collect();
        let stats = (0..models.len()).map(|_| Stats::default()).collect();
        Ok(Self {
            config,
            endpoints,
            models,
            tasks,
            random: RandomState::new(),
            state: Mutex::new(State {
                stats,
                providers: states,
                cursor: HashMap::new(),
                sequence: 0,
                recent_attempts: VecDeque::new(),
            }),
            changed: Condvar::new(),
        })
    }

    pub fn has_task(&self, task: &str) -> bool {
        self.tasks.contains_key(task)
    }

    pub fn get_model_for_task(&self, task: &str) -> Result<Selection, Error> {
        let binding = self
            .tasks
            .get(task)
            .ok_or_else(|| Error::UnknownTask(task.into()))?;
        let mut state = self.lock();
        let candidate = self
            .select(
                task,
                binding,
                binding.config.requires,
                &self.request_state(),
                &mut state,
                Instant::now(),
            )
            .map_err(|_| Error::Unavailable)?;
        let model = &self.models[candidate.model];
        Ok(Selection {
            model_id: model.id.clone(),
            model: model.entry.clone(),
        })
    }

    pub fn snapshot(&self) -> PoolStatus {
        let state = self.lock();
        let now = Instant::now();
        let models = self
            .models
            .iter()
            .zip(&state.stats)
            .map(|(model, stats)| {
                let completed = stats.successful + stats.failed;
                let keys = &state.providers[model.provider].keys;
                ModelStatus {
                    model_id: model.id.clone(),
                    model_name: model.entry.model_name.clone(),
                    provider: model.entry.provider.clone(),
                    successful: stats.successful,
                    failed: stats.failed,
                    calls: stats.calls,
                    in_flight: stats.in_flight,
                    average_ms: (completed > 0).then(|| stats.elapsed_ms as f64 / completed as f64),
                    failure_rate: (completed > 0).then(|| stats.failed as f64 / completed as f64),
                    total_tokens: (stats.usage_reports > 0).then_some(stats.tokens),
                    usage_reports: stats.usage_reports,
                    last_error: stats.last_error,
                    degraded: stats.degraded(now),
                    ready_keys: keys.iter().filter(|key| key.available(now)).count(),
                    cooling_keys: keys
                        .iter()
                        .filter(|key| matches!(key, KeyHealth::Cooling(until) if *until > now))
                        .count(),
                    rejected_keys: keys
                        .iter()
                        .filter(|key| matches!(key, KeyHealth::Rejected))
                        .count(),
                    missing_keys: self.endpoints[model.provider].missing_keys,
                }
            })
            .collect();
        PoolStatus {
            task_bindings: self.config.task_bindings.clone(),
            pools: self.config.pools.clone(),
            models,
            recent_attempts: state.recent_attempts.iter().cloned().collect(),
        }
    }

    pub(crate) fn call_with_pool<T, E>(
        &self,
        name: &str,
        capability: Capability,
        mut invoke: impl FnMut(
            &Provider,
            &ModelEntry,
            Duration,
        ) -> Result<(T, Option<Usage>), Failure<E>>,
    ) -> Result<Reply<T>, RouteError<E>> {
        let Some(task) = self.tasks.get(name) else {
            return Err(RouteError::Unavailable {
                reason: Unavailable::NoCandidate,
            });
        };
        let deadline =
            Instant::now() + Duration::from_millis(self.config.defaults.total_timeout_ms);
        let limit = if self.config.defaults.failover {
            1 + self.config.defaults.max_retries
        } else {
            1
        };
        let mut request = self.request_state();
        let mut last_error = None;
        let mut calls = 0;
        let mut reason = Unavailable::NoCandidate;
        while calls < limit {
            let lease = match self.acquire(name, task, capability, deadline, &request) {
                Ok(lease) => lease,
                Err(unavailable) => {
                    reason = unavailable;
                    break;
                }
            };
            let selected = lease.candidate;
            let model = &self.models[selected.model];
            let endpoint = &self.endpoints[model.provider];
            let Some(remaining) = deadline
                .checked_duration_since(Instant::now())
                .filter(|time| !time.is_zero())
            else {
                reason = Unavailable::Deadline;
                break;
            };
            request.visited.insert(selected);
            request.attempts[model.provider] += 1;
            calls += 1;
            match invoke(
                &endpoint.keys[selected.key],
                &model.entry,
                remaining.min(endpoint.timeout),
            ) {
                Ok((outcome, usage)) => {
                    lease.report_success(usage);
                    return Ok(Reply {
                        outcome,
                        model_name: model.entry.model_name.clone(),
                    });
                }
                Err(failure) => {
                    lease.report_failure(failure.kind, failure.recovery);
                    match failure.recovery {
                        Recovery::Stop => {
                            return Err(RouteError::Aborted {
                                error: failure.error,
                                attempts: usize::from(calls),
                            })
                        }
                        Recovery::DisableKey | Recovery::CoolKey(_) => {
                            request.pinned_model = Some(selected.model)
                        }
                        Recovery::NextModel => {
                            request.pinned_model = None;
                            for key in 0..endpoint.keys.len() {
                                request.visited.insert(Candidate {
                                    model: selected.model,
                                    key,
                                });
                            }
                        }
                    }
                    last_error = Some(failure.error);
                }
            }
        }
        match last_error {
            Some(last) => Err(RouteError::Exhausted {
                last,
                attempts: usize::from(calls),
            }),
            None => Err(RouteError::Unavailable { reason }),
        }
    }

    fn request_state(&self) -> RequestState {
        RequestState {
            visited: HashSet::new(),
            attempts: vec![0; self.endpoints.len()],
            pinned_model: None,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn acquire<'a>(
        &'a self,
        name: &'a str,
        task: &'a Task,
        capability: Capability,
        deadline: Instant,
        request: &RequestState,
    ) -> Result<Lease<'a>, Unavailable> {
        let mut state = self.lock();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(Unavailable::Deadline);
            }
            match self.select(name, task, capability, request, &mut state, now) {
                Ok(candidate) => {
                    let provider = &mut state.providers[self.models[candidate.model].provider];
                    provider.budget.reserve(now);
                    provider.cursor = (candidate.key + 1) % provider.keys.len();
                    state.sequence = state.sequence.wrapping_add(1);
                    let sequence = state.sequence;
                    let stats = &mut state.stats[candidate.model];
                    stats.in_flight += 1;
                    stats.calls += 1;
                    stats.last_selected = sequence;
                    if matches!(task.config.strategy, Strategy::RoundRobin) {
                        let index = task
                            .models
                            .iter()
                            .position(|id| *id == candidate.model)
                            .expect("candidate from task");
                        state
                            .cursor
                            .insert(name.into(), (index + 1) % task.models.len());
                    }
                    return Ok(Lease {
                        pool: self,
                        candidate,
                        task: name,
                        pool_name: &task.config.pool,
                        start: now,
                        settled: false,
                    });
                }
                Err(SelectionError::Busy(until)) => {
                    let wake = until.map_or(deadline, |value| value.min(deadline));
                    (state, _) = self
                        .changed
                        .wait_timeout(state, wake.saturating_duration_since(now))
                        .unwrap_or_else(|error| error.into_inner());
                }
                Err(SelectionError::NoKey) => return Err(Unavailable::Cooldown),
                Err(SelectionError::NoCandidate) => return Err(Unavailable::NoCandidate),
            }
        }
    }

    fn select(
        &self,
        name: &str,
        task: &Task,
        capability: Capability,
        request: &RequestState,
        state: &mut State,
        now: Instant,
    ) -> Result<Candidate, SelectionError> {
        let cursor = state.cursor.get(name).copied().unwrap_or_default();
        let mut ready = Vec::new();
        let mut blocked = None;
        let mut no_key = false;
        for index in 0..task.models.len() {
            let model_id = task.models[(index + cursor) % task.models.len()];
            let model = &self.models[model_id];
            let provider = model.provider;
            if !model.entry.capabilities.contains(&capability)
                || request.attempts[provider] >= self.endpoints[provider].attempts
            {
                continue;
            }
            let status = &mut state.providers[provider];
            let candidate = (0..status.keys.len()).find_map(|offset| {
                let key = (offset + status.cursor) % status.keys.len();
                let candidate = Candidate {
                    model: model_id,
                    key,
                };
                if request.visited.contains(&candidate) {
                    return None;
                }
                if status.keys[key].available(now) {
                    Some(candidate)
                } else {
                    no_key = true;
                    None
                }
            });
            let Some(candidate) = candidate else { continue };
            match status.budget.available(now) {
                Availability::Ready => ready.push(candidate),
                Availability::Cooldown => no_key = true,
                Availability::Busy(until) => {
                    blocked = Some(match (blocked.flatten(), until) {
                        (Some(old), Some(new)) => Some(Instant::min(old, new)),
                        (None, new) => new,
                        (old, None) => old,
                    });
                }
            }
        }
        if ready.is_empty() {
            return Err(match blocked {
                Some(until) => SelectionError::Busy(until),
                None if no_key => SelectionError::NoKey,
                None => SelectionError::NoCandidate,
            });
        }
        if let Some(candidate) = ready
            .iter()
            .find(|candidate| Some(candidate.model) == request.pinned_model)
        {
            return Ok(*candidate);
        }
        select_ready(
            &self.models,
            &state.stats,
            ready,
            task.config.strategy,
            now,
            self.random.hash_one(state.sequence),
        )
        .ok_or(SelectionError::NoCandidate)
    }
}

enum SelectionError {
    Busy(Option<Instant>),
    NoKey,
    NoCandidate,
}

fn select_ready(
    models: &[Model],
    stats: &[Stats],
    mut ready: Vec<Candidate>,
    strategy: Strategy,
    now: Instant,
    random: u64,
) -> Option<Candidate> {
    let rank = |candidate: &Candidate| {
        (
            stats[candidate.model].degraded(now),
            models[candidate.model].entry.priority,
        )
    };
    let best = ready.iter().map(rank).min()?;
    ready.retain(|candidate| rank(candidate) == best);
    match strategy {
        Strategy::Ordered | Strategy::RoundRobin => ready.first().copied(),
        Strategy::Random => {
            let weight = |candidate: &Candidate| u64::from(models[candidate.model].entry.weight);
            let mut needle = random % ready.iter().map(weight).sum::<u64>();
            ready.into_iter().find(|candidate| {
                let value = weight(candidate);
                if needle < value {
                    true
                } else {
                    needle -= value;
                    false
                }
            })
        }
        Strategy::LeastUsage => {
            let compare_tokens = ready.iter().all(|candidate| {
                let item = &stats[candidate.model];
                item.usage_reports == item.successful
            });
            ready.into_iter().min_by_key(|candidate| {
                let item = &stats[candidate.model];
                let failures = if item.degraded(now) {
                    item.recent_failures
                } else {
                    0
                };
                let usage = if compare_tokens {
                    item.tokens
                } else {
                    item.successful
                };
                (item.in_flight, failures, usage, item.last_selected)
            })
        }
    }
}

struct Lease<'a> {
    pool: &'a Pool,
    candidate: Candidate,
    task: &'a str,
    pool_name: &'a str,
    start: Instant,
    settled: bool,
}

impl Lease<'_> {
    fn report_success(mut self, usage: Option<Usage>) {
        self.settle(Ok(usage));
    }

    fn report_failure(mut self, kind: FailureKind, recovery: Recovery) {
        self.settle(Err((kind, recovery)));
    }

    fn settle(&mut self, outcome: Result<Option<Usage>, (FailureKind, Recovery)>) {
        self.settled = true;
        let now = Instant::now();
        let model = &self.pool.models[self.candidate.model];
        let elapsed_ms = now.duration_since(self.start).as_millis() as u64;
        let mut state = self.pool.lock();
        let provider = &mut state.providers[model.provider];
        provider.budget.release();
        let cooldown = Duration::from_millis(self.pool.config.defaults.failure_penalty_ms);
        if let Err((_, recovery)) = outcome {
            let slot = &mut provider.keys[self.candidate.key];
            match recovery {
                Recovery::DisableKey => *slot = KeyHealth::Rejected,
                Recovery::CoolKey(delay) if !matches!(slot, KeyHealth::Rejected) => {
                    let until = now + cooldown.max(delay.min(Duration::from_secs(3600)));
                    *slot = KeyHealth::Cooling(match *slot {
                        KeyHealth::Cooling(previous) => previous.max(until),
                        _ => until,
                    });
                }
                _ => {}
            }
        }
        let stats = &mut state.stats[self.candidate.model];
        stats.in_flight -= 1;
        stats.elapsed_ms = stats.elapsed_ms.saturating_add(elapsed_ms);
        match outcome {
            Ok(usage) => {
                stats.successful += 1;
                if let Some(total) = usage.and_then(Usage::total) {
                    stats.tokens = stats.tokens.saturating_add(total);
                    stats.usage_reports += 1;
                }
            }
            Err((kind, recovery)) => {
                stats.failed += 1;
                stats.last_error = Some(kind);
                if matches!(recovery, Recovery::NextModel)
                    || matches!(
                        kind,
                        FailureKind::InvalidResponse | FailureKind::Server | FailureKind::Timeout
                    )
                {
                    stats.recent_failures = if stats.degraded(now) {
                        stats.recent_failures + 1
                    } else {
                        1
                    };
                    stats.penalty_until = Some(now + cooldown);
                }
            }
        }
        let usage = outcome.ok().flatten();
        let error = outcome.err().map(|(kind, _)| kind);
        if state.recent_attempts.len() == 128 {
            state.recent_attempts.pop_front();
        }
        state.recent_attempts.push_back(AttemptLog {
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            task: self.task.into(),
            pool: self.pool_name.into(),
            model: model.id.clone(),
            provider: model.entry.provider.clone(),
            elapsed_ms,
            usage,
            success: outcome.is_ok(),
            error,
        });
        drop(state);
        self.pool.changed.notify_all();
        log::info!(
            target: "velmira::model_pool",
            "task={} pool={} model={} provider={} elapsed_ms={} input_tokens={:?} output_tokens={:?} total_tokens={:?} success={} error={:?}",
            self.task, self.pool_name, model.id, model.entry.provider, elapsed_ms,
            usage.and_then(|value| value.input_tokens), usage.and_then(|value| value.output_tokens),
            usage.and_then(Usage::total), outcome.is_ok(), error,
        );
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.settle(Err((FailureKind::Cancelled, Recovery::Stop)));
        }
    }
}
