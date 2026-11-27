use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{network_error, response_bytes, response_json, Error, Provider, MAX_IMAGE_RESPONSE};

const HISTORY_LIMIT: usize = 2 * 1024 * 1024;
const WORKFLOW_LIMIT: usize = 512 * 1024;

pub(super) fn valid_workflow(graph: &Value) -> bool {
    let Some(nodes) = graph.as_object() else {
        return false;
    };
    !nodes.is_empty()
        && nodes.len() <= 256
        && graph.to_string().len() <= WORKFLOW_LIMIT
        && nodes.values().all(|node| {
            node.get("class_type").and_then(Value::as_str).is_some()
                && node.get("inputs").and_then(Value::as_object).is_some()
        })
        && nodes.values().any(|node| contains_prompt(&node["inputs"]))
}

fn contains_prompt(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains("{{prompt}}"),
        Value::Object(fields) => fields.values().any(contains_prompt),
        Value::Array(items) => items.iter().any(contains_prompt),
        _ => false,
    }
}

fn insert_prompt(value: &mut Value, prompt: &str) {
    match value {
        Value::String(text) if text.contains("{{prompt}}") => {
            *text = text.replace("{{prompt}}", prompt)
        }
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|value| insert_prompt(value, prompt)),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|value| insert_prompt(value, prompt)),
        _ => {}
    }
}

#[derive(Deserialize)]
struct Job {
    prompt_id: String,
}

#[derive(Deserialize)]
struct ImageFile {
    filename: String,
    #[serde(default)]
    subfolder: String,
    #[serde(rename = "type")]
    kind: String,
}

pub(crate) fn render(
    provider: &Provider,
    workflow: &Value,
    prompt: &str,
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    let deadline = Instant::now() + timeout;
    let mut graph = workflow.clone();
    if let Some(nodes) = graph.as_object_mut() {
        for node in nodes.values_mut() {
            if let Some(inputs) = node.get_mut("inputs") {
                insert_prompt(inputs, prompt);
            }
        }
    }
    let job: Job = provider.post(
        "prompt",
        json!({"prompt": graph}),
        64 * 1024,
        remaining(deadline)?,
    )?;
    if job.prompt_id.is_empty()
        || job.prompt_id.len() > 64
        || !job
            .prompt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::Response);
    }
    let history_url = provider
        .endpoint
        .join(&format!("history/{}", job.prompt_id))
        .map_err(|_| Error::Endpoint)?;
    loop {
        let response = request(provider, history_url.clone(), deadline)?;
        let history: Value = response_json(response, HISTORY_LIMIT)?;
        if let Some(entry) = history.get(&job.prompt_id) {
            if entry.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
                return Err(Error::ImageBackend);
            }
            let file: ImageFile = entry
                .get("outputs")
                .and_then(Value::as_object)
                .and_then(|nodes| {
                    nodes
                        .values()
                        .filter_map(|node| node.get("images")?.as_array())
                        .flatten()
                        .next()
                })
                .and_then(|image| serde_json::from_value(image.clone()).ok())
                .ok_or(Error::Image)?;
            if !matches!(file.kind.as_str(), "output" | "temp")
                || file.filename.is_empty()
                || file.filename.contains(['/', '\\', '\0'])
                || file
                    .subfolder
                    .split(['/', '\\'])
                    .any(|part| part == ".." || part.contains([':', '\0']))
                || file.subfolder.starts_with(['/', '\\'])
            {
                return Err(Error::Image);
            }
            let mut url = provider
                .endpoint
                .join("view")
                .map_err(|_| Error::Endpoint)?;
            url.query_pairs_mut()
                .append_pair("filename", &file.filename)
                .append_pair("subfolder", &file.subfolder)
                .append_pair("type", &file.kind);
            return response_bytes(request(provider, url, deadline)?, MAX_IMAGE_RESPONSE);
        }
        std::thread::sleep(Duration::from_millis(350).min(remaining(deadline)?));
    }
}

fn remaining(deadline: Instant) -> Result<Duration, Error> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|timeout| !timeout.is_zero())
        .ok_or(Error::Timeout)
}

fn request(
    provider: &Provider,
    url: reqwest::Url,
    deadline: Instant,
) -> Result<reqwest::blocking::Response, Error> {
    let mut request = provider.client.get(url).timeout(remaining(deadline)?);
    if !provider.key.is_empty() {
        request = request.bearer_auth(provider.key.as_str());
    }
    let response = request.send().map_err(|error| {
        // A submitted job can still finish if its history or result becomes unreachable.
        match network_error(error) {
            Error::Timeout => Error::Timeout,
            _ => Error::Network,
        }
    })?;
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(Error::Network)
    }
}
