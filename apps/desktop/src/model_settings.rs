use std::{fs::File, io::Read, path::Path};

use serde::{Deserialize, Serialize};
use vm_ai::route::Strategy;

pub const MAX_CONNECTIONS: usize = 8;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

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

pub fn read_settings(path: &Path) -> Result<Settings, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Settings::default());
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
    let settings: Settings = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !(1..=MAX_CONNECTIONS).contains(&settings.connections.len()) {
        return Err("模型池连接数量超出范围。".into());
    }
    Ok(settings)
}

pub fn write_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path.parent().ok_or("配置目录无效。")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(output.as_file_mut(), settings)
        .map_err(|error| error.to_string())?;
    output
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    output.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}
