use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
};

use crate::{theme, ui};
use serde::{Deserialize, Serialize};
use vm_ai::{
    pool::{Config, PoolStatus},
    route::Strategy,
    service::{legacy_config, ComicService, Connection, SessionKeys},
};

pub const MAX_CONNECTIONS: usize = 8;
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSettings {
    pub endpoint: String,
    pub text_model: String,
    pub image_model: String,
    pub rpm: u32,
    #[serde(default)]
    pub concurrency: Option<u16>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub strategy: Strategy,
    pub connections: Vec<ConnectionSettings>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            strategy: Strategy::Ordered,
            connections: vec![ConnectionSettings::default()],
        }
    }
}

impl Settings {
    pub fn connections(&self, mut key: impl FnMut(usize) -> String) -> Vec<Connection> {
        self.connections
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if item.endpoint.trim().is_empty()
                    && item.text_model.trim().is_empty()
                    && item.image_model.trim().is_empty()
                {
                    return None;
                }
                Some(Connection {
                    endpoint: item.endpoint.clone(),
                    key: key(index),
                    text_model: item.text_model.clone(),
                    image_model: item.image_model.clone(),
                    rpm: item.rpm,
                    concurrency: item.concurrency.unwrap_or(1),
                })
            })
            .collect()
    }
}

pub enum Profile {
    Legacy(Settings),
    Pools(Config),
}

pub fn read_settings(path: &Path) -> Result<Profile, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Profile::Legacy(Settings::default()));
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("模型池配置文件过大。".into());
    }
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if document.get("schema_version").is_some() {
        return parse_config(&bytes).map(Profile::Pools);
    }
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !(1..=MAX_CONNECTIONS).contains(&settings.connections.len()) {
        return Err("模型池连接数量超出范围。".into());
    }
    Ok(Profile::Legacy(settings))
}

pub fn parse_config(bytes: &[u8]) -> Result<Config, String> {
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err("模型池配置文件过大。".into());
    }
    let config: Config = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

pub fn write_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    if matches!(read_settings(path)?, Profile::Pools(_)) {
        return Err("当前使用新版模型池，请编辑 JSON 配置。".into());
    }
    write_document(path, settings)
}

#[cfg(target_os = "android")]
pub fn write_config(path: &Path, config: &Config) -> Result<(), String> {
    write_document(path, config)
}

fn write_document(path: &Path, document: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("配置目录无效。")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(output.as_file_mut(), document)
        .map_err(|error| error.to_string())?;
    output
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    output.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

struct ActivePool {
    config: Config,
    keys: SessionKeys,
    service: Arc<ComicService>,
}

#[derive(Default)]
pub struct SharedPool {
    active: Mutex<Option<ActivePool>>,
}

impl SharedPool {
    pub fn configured(
        &self,
        config: &Config,
        keys: &SessionKeys,
    ) -> Result<Arc<ComicService>, String> {
        self.activate(config.clone(), keys.clone(), false)
    }

    #[cfg(not(target_os = "android"))]
    pub fn configured_from_file(
        &self,
        path: &Path,
        config: &Config,
    ) -> Result<Arc<ComicService>, String> {
        #[cfg(not(windows))]
        let _ = path;
        let keys = SessionKeys::new();
        #[cfg(windows)]
        let keys = {
            use std::collections::BTreeSet;
            use zeroize::Zeroizing;

            let mut keys = keys;
            let references: BTreeSet<_> = config
                .providers
                .values()
                .flat_map(|provider| &provider.keys)
                .filter_map(|reference| reference.strip_prefix("env:"))
                .collect();
            if let Some(root) = path.parent() {
                for name in references {
                    if std::env::var_os(name).is_some() {
                        continue;
                    }
                    let path = root.join("credentials").join(format!("{name}.dpapi"));
                    let file = match File::open(&path) {
                        Ok(file) => file,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(format!("无法读取模型凭据 {name}：{error}")),
                    };
                    let mut sealed = Vec::new();
                    file.take(4097)
                        .read_to_end(&mut sealed)
                        .map_err(|error| error.to_string())?;
                    let secret = crate::pairing_cli::dpapi::unprotect(&mut sealed)
                        .map_err(|error| format!("无法打开 Windows 模型凭据 {name}：{error}"))?;
                    let secret = std::str::from_utf8(&secret)
                        .map_err(|_| format!("模型凭据 {name} 的格式无效"))?;
                    keys.insert(format!("env:{name}"), Zeroizing::new(secret.to_owned()));
                }
            }
            keys
        };
        self.configured(config, &keys)
    }

    pub fn legacy(
        &self,
        settings: &Settings,
        input: impl FnMut(usize) -> String,
        inherit: bool,
    ) -> Result<Arc<ComicService>, String> {
        let (config, keys) = legacy_config(settings.connections(input), settings.strategy);
        self.activate(config, keys, inherit)
    }

    #[cfg(not(target_os = "android"))]
    pub fn reading(
        &self,
        path: &Path,
        endpoint: &str,
        model: &str,
        key: &str,
    ) -> Result<Arc<ComicService>, String> {
        match read_settings(path)? {
            Profile::Pools(config) => self.configured_from_file(path, &config),
            Profile::Legacy(settings) => {
                let selected = settings
                    .connections
                    .iter()
                    .position(|item| item.endpoint == endpoint && item.text_model == model);
                let has_legacy = settings.connections.iter().any(|item| {
                    !item.endpoint.trim().is_empty() && !item.text_model.trim().is_empty()
                });
                let (config, keys) = if has_legacy
                    && (selected.is_some() || (endpoint.is_empty() && model.is_empty()))
                {
                    legacy_config(
                        settings.connections(|i| {
                            if selected == Some(i) {
                                key.into()
                            } else {
                                String::new()
                            }
                        }),
                        settings.strategy,
                    )
                } else {
                    legacy_config(
                        vec![Connection {
                            endpoint: endpoint.into(),
                            text_model: model.into(),
                            key: key.into(),
                            image_model: String::new(),
                            rpm: 0,
                            concurrency: 1,
                        }],
                        Strategy::Ordered,
                    )
                };
                self.activate(config, keys, true)
            }
        }
    }

    fn activate(
        &self,
        mut config: Config,
        mut keys: SessionKeys,
        inherit_legacy: bool,
    ) -> Result<Arc<ComicService>, String> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = &*active {
            if inherit_legacy {
                for (id, provider) in &mut config.providers {
                    let reference = format!("session:{id}");
                    if provider.keys.is_empty()
                        && existing
                            .config
                            .providers
                            .get(id)
                            .is_some_and(|old| old.base_url == provider.base_url)
                    {
                        if let Some(key) = existing.keys.get(&reference) {
                            keys.insert(reference.clone(), key.clone());
                            provider.keys.push(reference);
                        }
                    }
                }
            }
            if existing.config == config && existing.keys == keys {
                return Ok(Arc::clone(&existing.service));
            }
        }
        let service = Arc::new(
            ComicService::from_config(config.clone(), |reference| {
                reference
                    .strip_prefix("env:")
                    .and_then(|name| std::env::var(name).ok())
                    .or_else(|| keys.get(reference).map(|key| key.to_string()))
            })
            .map_err(|error| error.to_string())?,
        );
        *active = Some(ActivePool {
            config,
            keys,
            service: Arc::clone(&service),
        });
        Ok(service)
    }

    pub fn status(&self) -> Option<PoolStatus> {
        let service = Arc::clone(
            &self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()?
                .service,
        );
        Some(service.pool().snapshot())
    }
}

pub fn show_status(ui: &mut eframe::egui::Ui, pool: &SharedPool) {
    ui::disclosure(ui, "模型池状态", false, |ui| {
        let Some(status) = pool.status() else {
            ui::empty_state(
                ui,
                theme::palette(ui.visuals().dark_mode),
                "尚未配置模型池",
                "添加提供商、模型和任务绑定后，这里会显示健康状态。",
            );
            return;
        };
        if status.models.iter().any(|model| model.in_flight > 0) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        let palette = theme::palette(ui.visuals().dark_mode);
        #[cfg(target_os = "android")]
        let tokens = theme::Tokens::android(1.0);
        #[cfg(not(target_os = "android"))]
        let tokens = theme::Tokens::desktop();
        ui::section_header(ui, palette, "任务路由", Some("任务 → 模型池"));
        for (task, binding) in &status.task_bindings {
            ui.horizontal(|ui| {
                ui.label(
                    eframe::egui::RichText::new(task)
                        .strong()
                        .color(palette.ink),
                );
                ui.label(
                    eframe::egui::RichText::new(format!("→ {}", binding.pool)).color(palette.muted),
                );
            });
        }
        ui.add_space(tokens.space_md);
        ui::section_header(ui, palette, "模型健康", Some("调用、失败率、延迟与并发"));
        for model in status.models {
            let latency = model
                .average_ms
                .map_or("—".into(), |value| format!("{value:.0} ms"));
            let failure = model
                .failure_rate
                .map_or("—".into(), |value| format!("{:.1}%", value * 100.0));
            let token_text = model
                .total_tokens
                .map_or("缺失".into(), |value| value.to_string());
            let healthy = model.ready_keys > 0 && !model.degraded;
            ui::metric_card(
                ui,
                palette,
                tokens,
                &model.model_name,
                &failure,
                &format!(
                    "{} · 平均 {} · 并发 {} · token {}",
                    model.calls, latency, model.in_flight, token_text,
                ),
                healthy,
            );
            ui.horizontal_wrapped(|ui| {
                ui.small(format!(
                    "密钥 可用 {} · 冷却 {} · 失效 {} · 未填写 {}",
                    model.ready_keys, model.cooling_keys, model.rejected_keys, model.missing_keys
                ));
                if let Some(error) = model.last_error {
                    ui.small(
                        eframe::egui::RichText::new(format!("最近错误 {error:?}"))
                            .color(palette.danger),
                    );
                }
            });
            ui.add_space(tokens.space_sm);
        }
        ui::disclosure(ui, "最近调用", false, |ui| {
            for attempt in status.recent_attempts.iter().rev().take(12) {
                ui.small(format!(
                    "{} · {} · {} ms · {}",
                    attempt.task,
                    attempt.model,
                    attempt.elapsed_ms,
                    attempt
                        .error
                        .map_or("成功".into(), |error| format!("{error:?}"))
                ));
            }
        });
    });
}
