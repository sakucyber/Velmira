use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use eframe::egui;
use serde::{Deserialize, Serialize};
use vm_ai::reading::Action;

use crate::{theme, ui};

const MAX_PLUGIN_BYTES: u64 = 64 * 1024;
const MAX_PROMPT_CHARS: usize = 2_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u8,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub prompts: Prompts,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prompts {
    pub translate: Option<String>,
    pub explain: Option<String>,
    pub summarize: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredPlugin {
    #[serde(flatten)]
    manifest: Manifest,
    #[serde(default)]
    enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Plugin {
    pub manifest: Manifest,
    pub enabled: bool,
}

#[derive(Default)]
pub struct Registry {
    plugins: Vec<Plugin>,
}

pub enum PluginAction {
    #[cfg(not(target_os = "android"))]
    Import,
    Toggle(String, bool),
    Remove(String),
}

impl Registry {
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let folder = data_dir.join("plugins");
        let mut plugins = Vec::new();
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(stored) = read_stored(&entry.path()) else {
                continue;
            };
            if stored.manifest.validate().is_ok() {
                plugins.push(Plugin {
                    manifest: stored.manifest,
                    enabled: stored.enabled,
                });
            }
        }
        plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        Ok(Self { plugins })
    }

    #[cfg(not(target_os = "android"))]
    pub fn import(&mut self, data_dir: &Path, path: &Path) -> Result<(), String> {
        let manifest: Manifest = read_json(path)?;
        manifest.validate()?;
        if self
            .plugins
            .iter()
            .any(|plugin| plugin.manifest.id == manifest.id)
        {
            return Err("已存在同 ID 插件，请先删除旧插件。".into());
        }
        let stored = StoredPlugin {
            manifest: manifest.clone(),
            enabled: true,
        };
        write_stored(&data_dir.join("plugins"), &stored)?;
        self.plugins.push(Plugin {
            manifest,
            enabled: true,
        });
        self.plugins
            .sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        Ok(())
    }

    pub fn set_enabled(&mut self, data_dir: &Path, id: &str, enabled: bool) -> Result<(), String> {
        let plugin = self
            .plugins
            .iter_mut()
            .find(|plugin| plugin.manifest.id == id)
            .ok_or("插件不存在。")?;
        plugin.enabled = enabled;
        write_stored(
            &data_dir.join("plugins"),
            &StoredPlugin {
                manifest: plugin.manifest.clone(),
                enabled,
            },
        )
    }

    pub fn remove(&mut self, data_dir: &Path, id: &str) -> Result<(), String> {
        let index = self
            .plugins
            .iter()
            .position(|plugin| plugin.manifest.id == id)
            .ok_or("插件不存在。")?;
        fs::remove_file(data_dir.join("plugins").join(format!("{id}.json")))
            .map_err(|error| error.to_string())?;
        self.plugins.remove(index);
        Ok(())
    }

    pub fn prompt_for(&self, action: Action) -> String {
        let mut prompt = String::new();
        for plugin in self.plugins.iter().filter(|plugin| plugin.enabled) {
            let value = match action {
                Action::Translate => plugin.manifest.prompts.translate.as_deref(),
                Action::Explain => plugin.manifest.prompts.explain.as_deref(),
                Action::Summarize => plugin.manifest.prompts.summarize.as_deref(),
            };
            if let Some(value) = value {
                if !prompt.is_empty() {
                    prompt.push('\n');
                }
                prompt.push_str(value.trim());
            }
        }
        prompt.chars().take(MAX_PROMPT_CHARS).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

impl Manifest {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("插件 schema_version 必须为 1。".into());
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || self.id.starts_with('-')
            || self.id.ends_with('-')
        {
            return Err("插件 ID 须为 1–64 位小写字母、数字或连字符。".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 80 {
            return Err("插件名称须为 1–80 字。".into());
        }
        if self.description.chars().count() > 280 {
            return Err("插件说明不能超过 280 字。".into());
        }
        if self.version.trim().is_empty() || self.version.chars().count() > 32 {
            return Err("插件版本号不能为空或超过 32 字。".into());
        }
        if self
            .author
            .as_ref()
            .is_some_and(|author| author.chars().count() > 80)
        {
            return Err("作者信息不能超过 80 字。".into());
        }
        let mut has_prompt = false;
        for value in [
            &self.prompts.translate,
            &self.prompts.explain,
            &self.prompts.summarize,
        ]
        .into_iter()
        .flatten()
        {
            if value.chars().count() > MAX_PROMPT_CHARS {
                return Err("单个提示词不能超过 2000 字。".into());
            }
            has_prompt |= !value.trim().is_empty();
        }
        if !has_prompt {
            return Err("插件至少需要一个非空提示词。".into());
        }
        Ok(())
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PLUGIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PLUGIN_BYTES {
        return Err("插件文件不能超过 64 KiB。".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn read_stored(path: &Path) -> Result<StoredPlugin, String> {
    read_json(path)
}

fn write_stored(folder: &Path, stored: &StoredPlugin) -> Result<(), String> {
    fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let file = tempfile::NamedTempFile::new_in(folder).map_err(|error| error.to_string())?;
    serde_json::to_writer(file.as_file(), stored).map_err(|error| error.to_string())?;
    file.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    file.persist(folder.join(format!("{}.json", stored.manifest.id)))
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub fn show(
    ui: &mut egui::Ui,
    registry: &mut Registry,
    _data_dir: &Path,
    night: bool,
) -> Option<PluginAction> {
    let palette = theme::palette(night);
    let tokens = theme::Tokens::desktop();
    let mut action = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui::section_header(
            ui,
            palette,
            "插件",
            Some("本地提示词插件，不执行代码，不访问网络。"),
        );
        #[cfg(not(target_os = "android"))]
        if ui::primary_button(ui, palette, tokens, "导入插件 JSON").clicked() {
            action = Some(PluginAction::Import);
        }
        if registry.is_empty() {
            ui::empty_state(
                ui,
                palette,
                "还没有插件",
                "导入一个包含翻译、解释或摘要提示词的 JSON 插件。",
            );
        }
        for plugin in &registry.plugins {
            ui::card(ui, palette, tokens, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&plugin.manifest.name)
                                .strong()
                                .color(palette.ink),
                        );
                        ui.small(format!(
                            "v{} · {}",
                            plugin.manifest.version,
                            plugin.manifest.author.as_deref().unwrap_or("本地作者")
                        ));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("删除").clicked() {
                            action = Some(PluginAction::Remove(plugin.manifest.id.clone()));
                        }
                        let mut enabled = plugin.enabled;
                        if ui.checkbox(&mut enabled, "启用").changed() {
                            action =
                                Some(PluginAction::Toggle(plugin.manifest.id.clone(), enabled));
                        }
                    });
                });
                ui.label(egui::RichText::new(&plugin.manifest.description).color(palette.muted));
                let mut kinds = Vec::new();
                if plugin
                    .manifest
                    .prompts
                    .translate
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    kinds.push("翻译");
                }
                if plugin
                    .manifest
                    .prompts
                    .explain
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    kinds.push("解释");
                }
                if plugin
                    .manifest
                    .prompts
                    .summarize
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
                {
                    kinds.push("摘要");
                }
                ui.small(format!("适用：{}", kinds.join(" / ")));
            });
            ui.add_space(tokens.space_sm);
        }
    });
    action
}
