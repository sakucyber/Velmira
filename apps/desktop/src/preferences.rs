use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub night: bool,
    pub contents: bool,
    pub compact: bool,
    pub font_size: f32,
    pub paper: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            night: false,
            contents: true,
            compact: false,
            font_size: 19.0,
            paper: None,
        }
    }
}

pub struct SavedPreferences {
    path: PathBuf,
    latest: Preferences,
    pending: Option<(Preferences, Instant)>,
}

impl SavedPreferences {
    pub fn load(data_dir: &Path) -> (Self, Preferences) {
        let path = data_dir.join("reading-preferences.json");
        let prefs = (|| {
            let mut bytes = Vec::new();
            std::fs::File::open(&path)
                .ok()?
                .take(16_385)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > 16_384 {
                return None;
            }
            serde_json::from_slice::<Preferences>(&bytes).ok()
        })()
        .unwrap_or_default();
        (
            Self {
                path,
                latest: prefs.clone(),
                pending: None,
            },
            prefs,
        )
    }

    pub fn update(&mut self, ctx: &eframe::egui::Context, preferences: Preferences, closing: bool) {
        if preferences == self.latest {
            self.pending = None;
            return;
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|(previous, _)| previous != &preferences)
        {
            self.pending = Some((
                preferences,
                Instant::now() + std::time::Duration::from_millis(300),
            ));
        }
        let Some((value, due)) = self.pending.as_ref() else {
            return;
        };
        if !closing {
            if let Some(delay) = due.checked_duration_since(Instant::now()) {
                ctx.request_repaint_after(delay);
                return;
            }
        }
        let Some(parent) = self.path.parent() else {
            return;
        };
        let save = (|| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::create_dir_all(parent)?;
            let file = tempfile::NamedTempFile::new_in(parent)?;
            serde_json::to_writer(file.as_file(), value)?;
            file.persist(&self.path)?;
            Ok(())
        })();
        if save.is_ok() {
            self.latest = value.clone();
            self.pending = None;
        }
    }
}
