use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{Error, Strategy};
use crate::validate_endpoint;

pub const SCHEMA_VERSION: u8 = 2;
const MAX_MODELS: usize = 64;
const MAX_KEYS: usize = 16;
const MAX_TIMEOUT_MS: u64 = 3_600_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u8,
    #[serde(default)]
    pub defaults: Defaults,
    pub providers: BTreeMap<String, ModelProvider>,
    pub models: BTreeMap<String, ModelEntry>,
    pub pools: BTreeMap<String, ModelPool>,
    pub task_bindings: BTreeMap<String, TaskBinding>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    pub total_timeout_ms: u64,
    pub attempt_timeout_ms: u64,
    pub max_retries: u8,
    pub failover: bool,
    pub failure_penalty_ms: u64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            total_timeout_ms: 120_000,
            attempt_timeout_ms: 45_000,
            max_retries: 3,
            failover: true,
            failure_penalty_ms: 30_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    OpenaiCompatible,
    Comfyui,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProvider {
    #[serde(default)]
    pub protocol: Protocol,
    pub base_url: String,
    #[serde(default)]
    pub keys: Vec<String>,
    pub attempt_timeout_ms: Option<u64>,
    pub max_retries: Option<u8>,
    #[serde(default)]
    pub rpm: u32,
    #[serde(default = "one")]
    pub max_concurrency: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Chat,
    Vision,
    ImageGeneration,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub provider: String,
    pub model_name: String,
    #[serde(default)]
    pub priority: u16,
    #[serde(default = "one")]
    pub weight: u16,
    pub capabilities: Vec<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ModelPool(pub Vec<String>);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBinding {
    pub pool: String,
    pub strategy: Strategy,
    pub requires: Capability,
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::config("不支持的配置版本"));
        }
        let limits = &self.defaults;
        if !valid_timeout(limits.attempt_timeout_ms)
            || !valid_timeout(limits.total_timeout_ms)
            || limits.failure_penalty_ms == 0
            || limits.failure_penalty_ms > MAX_TIMEOUT_MS
            || limits.max_retries > 16
        {
            return Err(Error::config("默认超时、重试或降权时间不在允许范围内"));
        }
        if [
            self.providers.len(),
            self.models.len(),
            self.pools.len(),
            self.task_bindings.len(),
        ]
        .into_iter()
        .any(|count| !(1..=MAX_MODELS).contains(&count))
        {
            return Err(Error::config("提供商、模型、池和任务各需配置 1 至 64 项"));
        }
        for (id, provider) in &self.providers {
            validate_id(id)?;
            validate_endpoint(&provider.base_url)
                .map_err(|_| Error::config(format!("提供商 {id} 的地址无效")))?;
            if provider.keys.len() > MAX_KEYS
                || provider.keys.iter().any(|key| !valid_reference(key))
                || has_duplicates(&provider.keys)
                || provider.max_concurrency == 0
                || provider.max_concurrency > 256
                || provider.rpm > 10_000
                || provider.max_retries.is_some_and(|count| count > 16)
                || provider
                    .attempt_timeout_ms
                    .is_some_and(|ms| !valid_timeout(ms))
            {
                return Err(Error::config(format!(
                    "提供商 {id} 的连接限制或密钥引用无效"
                )));
            }
        }
        // A model may be listed in several pools under the same stable ID.
        let mut models = HashSet::new();
        for (id, model) in &self.models {
            validate_id(id)?;
            let provider = self
                .providers
                .get(&model.provider)
                .ok_or_else(|| Error::config(format!("模型 {id} 的提供商不存在")))?;
            if model.model_name.trim().is_empty()
                || model.model_name.len() > 1024
                || model.model_name.chars().any(char::is_control)
                || model.weight == 0
                || model.capabilities.is_empty()
                || has_duplicates(&model.capabilities)
                || !models.insert((&model.provider, &model.model_name))
            {
                return Err(Error::config(format!("模型 {id} 重复或定义无效")));
            }
            match (provider.protocol, &model.workflow) {
                (Protocol::OpenaiCompatible, None) => {}
                (Protocol::Comfyui, Some(workflow))
                    if model.capabilities == [Capability::ImageGeneration]
                        && super::comfy::valid_workflow(workflow) => {}
                _ => {
                    return Err(Error::config(format!(
                        "模型 {id} 的协议、能力或工作流不匹配"
                    )))
                }
            }
        }
        for (id, pool) in &self.pools {
            validate_id(id)?;
            if pool.0.is_empty()
                || pool.0.len() > MAX_MODELS
                || has_duplicates(&pool.0)
                || pool.0.iter().any(|model| !self.models.contains_key(model))
            {
                return Err(Error::config(format!(
                    "模型池 {id} 为空，或引用了重复、未知的模型"
                )));
            }
        }
        for (task, binding) in &self.task_bindings {
            validate_id(task)?;
            if !self.pools.get(&binding.pool).is_some_and(|pool| {
                pool.0
                    .iter()
                    .any(|id| self.models[id].capabilities.contains(&binding.requires))
            }) {
                return Err(Error::config(format!("任务 {task} 没有满足能力要求的模型")));
            }
        }
        Ok(())
    }
}

fn one() -> u16 {
    1
}

fn valid_timeout(value: u64) -> bool {
    (1..=MAX_TIMEOUT_MS).contains(&value)
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn validate_id(value: &str) -> Result<(), Error> {
    if valid_id(value) {
        Ok(())
    } else {
        Err(Error::config(
            "ID 应为 1 至 64 个字母、数字、点、连字符或下划线",
        ))
    }
}

fn valid_reference(reference: &str) -> bool {
    reference
        .split_once(':')
        .is_some_and(|(source, id)| matches!(source, "env" | "session") && valid_id(id))
}

fn has_duplicates<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    values.iter().collect::<HashSet<_>>().len() != values.len()
}
