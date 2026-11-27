//! In-process model routing and per-connection request budgets.

use std::{
    collections::{hash_map::RandomState, HashMap, HashSet, VecDeque},
    hash::BuildHasher,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const MINUTE: Duration = Duration::from_secs(60);
const MAX_CANDIDATES: usize = 20;
const MAX_CURSORS: usize = 32;
const MAX_ATTEMPTS: u8 = 3;
const MAX_DELAY: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    #[default]
    Ordered,
    RoundRobin,
    Random,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of network starts in a rolling minute. Zero is unlimited.
    pub rpm: u32,
    pub concurrency: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rpm: 0,
            concurrency: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Stable shared budget identifier, such as a canonical connection URL.
    pub connection: String,
    pub model: String,
    pub limits: Limits,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub strategy: Strategy,
    pub timeout: Duration,
    pub cooldown: Duration,
    pub retry_delay: Duration,
    pub attempts_per_candidate: u8,
    pub rounds: u8,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            strategy: Strategy::Ordered,
            timeout: Duration::from_secs(120),
            cooldown: MINUTE,
            retry_delay: Duration::from_millis(250),
            attempts_per_candidate: MAX_ATTEMPTS,
            rounds: MAX_ATTEMPTS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("模型池须包含 1 至 20 个模型")]
    CandidateCount,
    #[error("连接、模型名称或连接请求上限无效")]
    Candidate,
    #[error("模型池不能重复添加同一连接中的同一模型")]
    Duplicate,
    #[error("模型池的超时、重试次数或冷却时间无效")]
    Policy,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError<E> {
    #[error("{error}")]
    Aborted { error: E, attempts: usize },
    #[error("模型池尝试 {attempts} 次仍失败：{last}")]
    Exhausted { last: E, attempts: usize },
    #[error("{reason}")]
    Unavailable { reason: Unavailable },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    #[error("当前任务没有可用的候选模型")]
    NoCandidate,
    #[error("模型连接繁忙或已达到请求上限")]
    Capacity,
    #[error("模型连接正在冷却，请稍后再试")]
    Cooldown,
    #[error("等待模型连接超时")]
    Deadline,
}

#[derive(Clone, Copy)]
enum Disposition {
    Retry,
    Cooldown,
    Failover,
    Abort,
}

pub struct Failure<E> {
    error: E,
    disposition: Disposition,
    retry_after: Option<Duration>,
}

impl<E> Failure<E> {
    /// Retry in the next round. Use only when a second request is safe.
    pub fn retry(error: E) -> Self {
        Self::new(error, Disposition::Retry)
    }

    /// Fail over and cool the connection, including other models sharing it.
    pub fn cooldown(error: E) -> Self {
        Self::new(error, Disposition::Cooldown)
    }

    /// Try another candidate without retrying this model on this call.
    pub fn failover(error: E) -> Self {
        Self::new(error, Disposition::Failover)
    }

    /// Stop immediately, for example if the request may already have been billed.
    pub fn abort(error: E) -> Self {
        Self::new(error, Disposition::Abort)
    }

    pub fn with_retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay.min(MAX_DELAY));
        self
    }

    fn new(error: E, disposition: Disposition) -> Self {
        Self {
            error,
            disposition,
            retry_after: None,
        }
    }
}

#[derive(Clone)]
pub struct Router {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    candidates: Vec<(Candidate, usize)>,
    policy: Policy,
    random: RandomState,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    connections: Vec<Connection>,
    sequence: u64,
    cursors: HashMap<u32, u64>,
}

pub(crate) struct Connection {
    limits: Limits,
    starts: VecDeque<Instant>,
    active: u16,
    cooldown_until: Option<Instant>,
}

impl Connection {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            starts: VecDeque::new(),
            active: 0,
            cooldown_until: None,
        }
    }

    pub(crate) fn reserve(&mut self, now: Instant) {
        self.active += 1;
        if self.limits.rpm != 0 {
            self.starts.push_back(now);
        }
    }

    pub(crate) fn release(&mut self) {
        self.active -= 1;
    }

    pub(crate) fn available(&mut self, now: Instant) -> Availability {
        if let Some(until) = self.cooldown_until {
            if now < until {
                return Availability::Cooldown;
            }
            self.cooldown_until = None;
        }
        while self
            .starts
            .front()
            .is_some_and(|started| now.duration_since(*started) >= MINUTE)
        {
            self.starts.pop_front();
        }
        if self.active >= self.limits.concurrency {
            return Availability::Busy(None);
        }
        if self.limits.rpm != 0 && self.starts.len() >= self.limits.rpm as usize {
            return Availability::Busy(self.starts.front().map(|start| *start + MINUTE));
        }
        Availability::Ready
    }
}

pub(crate) enum Availability {
    Ready,
    Busy(Option<Instant>),
    Cooldown,
}

#[derive(Clone, Copy)]
struct Work {
    candidate: usize,
    attempts: u8,
    earliest: Instant,
}

struct Permit<'a> {
    router: &'a RouterInner,
    connection: usize,
}

impl Permit<'_> {
    fn cool(&self, until: Instant) {
        let mut state = self.router.state();
        let connection = &mut state.connections[self.connection];
        connection.cooldown_until = Some(
            connection
                .cooldown_until
                .map_or(until, |current| current.max(until)),
        );
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.router.state().connections[self.connection].release();
        self.router.changed.notify_all();
    }
}

enum Next<'a> {
    Invoke(Work, Permit<'a>, Duration),
    Skip,
    Done(Unavailable),
}

impl RouterInner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn next<'a>(
        &'a self,
        queue: &mut VecDeque<Work>,
        connection_attempts: &mut [u8],
        deadline: Instant,
    ) -> Next<'a> {
        let mut state = self.state();
        let mut waited_for_capacity = false;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Next::Done(if waited_for_capacity {
                    Unavailable::Capacity
                } else {
                    Unavailable::Deadline
                });
            }
            let mut wake = deadline;
            let mut has_ready_later = false;
            for _ in 0..queue.len() {
                let work = queue.pop_front().expect("known nonempty work queue");
                let (_, connection) = &self.candidates[work.candidate];
                let connection = *connection;
                if connection_attempts[connection] >= MAX_ATTEMPTS {
                    return Next::Skip;
                }
                if work.earliest > now {
                    wake = wake.min(work.earliest);
                    has_ready_later = true;
                    queue.push_back(work);
                    continue;
                }
                let slot = &mut state.connections[connection];
                match slot.available(now) {
                    Availability::Ready => {
                        slot.reserve(now);
                        connection_attempts[connection] += 1;
                        return Next::Invoke(
                            work,
                            Permit {
                                router: self,
                                connection,
                            },
                            deadline - now,
                        );
                    }
                    Availability::Cooldown => return Next::Skip,
                    Availability::Busy(when) => {
                        if let Some(when) = when {
                            wake = wake.min(when);
                        }
                        waited_for_capacity = true;
                        has_ready_later = true;
                        queue.push_back(work);
                    }
                }
            }
            if !has_ready_later {
                return Next::Done(Unavailable::Cooldown);
            }
            (state, _) = self
                .changed
                .wait_timeout(state, wake - now)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

impl Router {
    pub fn new(candidates: Vec<Candidate>, policy: Policy) -> Result<Self, ConfigError> {
        if candidates.is_empty() || candidates.len() > MAX_CANDIDATES {
            return Err(ConfigError::CandidateCount);
        }
        if policy.timeout.is_zero()
            || policy.timeout > MAX_DELAY
            || policy.cooldown > MAX_DELAY
            || policy.retry_delay > MAX_DELAY
            || !(1..=MAX_ATTEMPTS).contains(&policy.rounds)
            || !(1..=MAX_ATTEMPTS).contains(&policy.attempts_per_candidate)
        {
            return Err(ConfigError::Policy);
        }
        let mut connections = Vec::<Connection>::new();
        let mut seen_models = HashSet::new();
        let mut connection_ids = HashMap::new();
        let mut routes = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if candidate.connection.trim().is_empty()
                || candidate.connection.len() > 8192
                || candidate.model.trim().is_empty()
                || candidate.model.len() > 1024
                || candidate.limits.concurrency == 0
            {
                return Err(ConfigError::Candidate);
            }
            if !seen_models.insert((candidate.connection.clone(), candidate.model.clone())) {
                return Err(ConfigError::Duplicate);
            }
            let id = *connection_ids
                .entry(candidate.connection.clone())
                .or_insert_with(|| {
                    let id = connections.len();
                    connections.push(Connection::new(candidate.limits));
                    id
                });
            let limits = &mut connections[id].limits;
            limits.concurrency = limits.concurrency.min(candidate.limits.concurrency);
            limits.rpm = match (limits.rpm, candidate.limits.rpm) {
                (0, right) => right,
                (left, 0) => left,
                (left, right) => left.min(right),
            };
            routes.push((candidate, id));
        }
        Ok(Self {
            inner: Arc::new(RouterInner {
                candidates: routes,
                policy,
                random: RandomState::new(),
                state: Mutex::new(State {
                    connections,
                    sequence: 0,
                    cursors: HashMap::new(),
                }),
                changed: Condvar::new(),
            }),
        })
    }

    /// Run one logical request. Share this router across concurrent callers.
    /// The callback must honor the supplied remaining timeout. A successful
    /// callback is returned as-is, even if it finishes after that timeout.
    pub fn run<T, E>(
        &self,
        invoke: impl FnMut(&Candidate, Duration) -> Result<T, Failure<E>>,
    ) -> Result<T, RouteError<E>> {
        self.run_filtered(|_| true, invoke)
    }

    /// Route among a subset of models while sharing connection limits and cooldowns.
    /// Each recurring subset has its own round-robin cursor.
    pub fn run_filtered<T, E>(
        &self,
        mut accepts: impl FnMut(&Candidate) -> bool,
        mut invoke: impl FnMut(&Candidate, Duration) -> Result<T, Failure<E>>,
    ) -> Result<T, RouteError<E>> {
        let inner = &self.inner;
        let policy = inner.policy;
        let start = Instant::now();
        let deadline = start + policy.timeout;
        let mut order = inner
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, (candidate, _))| accepts(candidate).then_some(index))
            .collect::<Vec<_>>();
        if order.is_empty() {
            return Err(RouteError::Unavailable {
                reason: Unavailable::NoCandidate,
            });
        }
        let (sequence, connection_count) = {
            let mut state = inner.state();
            let sequence = if policy.strategy == Strategy::RoundRobin {
                let key = order.iter().fold(0, |mask, index| mask | (1 << index));
                if state.cursors.len() == MAX_CURSORS && !state.cursors.contains_key(&key) {
                    state.cursors.clear();
                }
                let cursor = state.cursors.entry(key).or_default();
                let previous = *cursor;
                *cursor = cursor.wrapping_add(1);
                previous
            } else {
                let previous = state.sequence;
                state.sequence = state.sequence.wrapping_add(1);
                previous
            };
            (sequence, state.connections.len())
        };
        match policy.strategy {
            Strategy::Ordered => {}
            Strategy::RoundRobin => {
                let first = (sequence % order.len() as u64) as usize;
                order.rotate_left(first);
            }
            Strategy::Random => {
                for index in (1..order.len()).rev() {
                    let random = inner.random.hash_one((sequence, index));
                    order.swap(index, (random % (index + 1) as u64) as usize);
                }
            }
        }
        let mut todo = order
            .into_iter()
            .map(|candidate| Work {
                candidate,
                attempts: 0,
                earliest: start,
            })
            .collect::<VecDeque<_>>();
        let mut connection_attempts = vec![0; connection_count];
        let mut attempts = 0;
        let mut last_error = None;
        let mut reason = Unavailable::Cooldown;
        for round in 1..=policy.rounds {
            let mut later = VecDeque::new();
            while !todo.is_empty() {
                let (mut work, permit, remaining) =
                    match inner.next(&mut todo, &mut connection_attempts, deadline) {
                        Next::Invoke(work, permit, remaining) => (work, permit, remaining),
                        Next::Skip => continue,
                        Next::Done(unavailable) => {
                            reason = unavailable;
                            break;
                        }
                    };
                attempts += 1;
                work.attempts += 1;
                match invoke(&inner.candidates[work.candidate].0, remaining) {
                    Ok(result) => return Ok(result),
                    Err(failure) => {
                        let now = Instant::now();
                        let delay = failure.retry_after.unwrap_or_default();
                        match failure.disposition {
                            Disposition::Abort => {
                                return Err(RouteError::Aborted {
                                    error: failure.error,
                                    attempts,
                                });
                            }
                            Disposition::Cooldown => {
                                permit.cool(now + policy.cooldown.max(delay));
                            }
                            Disposition::Retry
                                if work.attempts < policy.attempts_per_candidate
                                    && round < policy.rounds =>
                            {
                                work.earliest = now + policy.retry_delay.max(delay);
                                later.push_back(work);
                            }
                            Disposition::Retry | Disposition::Failover => {}
                        }
                        last_error = Some(failure.error);
                    }
                }
            }
            if later.is_empty() || Instant::now() >= deadline {
                break;
            }
            todo = later;
        }
        if let Some(last) = last_error {
            Err(RouteError::Exhausted { last, attempts })
        } else {
            Err(RouteError::Unavailable { reason })
        }
    }
}
