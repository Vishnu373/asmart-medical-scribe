//! Plain JSON settings store (no PHI). Design §9.3. B2.
//!
//! Settings are configuration only — model/mic/hotkey choices and a few internal
//! computed values. No transcripts, notes, or patient labels ever live here, so
//! this file is intentionally *not* encrypted.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
// Fields are added per phase; missing keys in an older settings.json must fall
// back to defaults (config-only, no PHI) rather than fail to parse.
#[serde(default)]
pub struct Settings {
    /// Doctor-facing: selected input device, `None` = system default.
    pub mic_device: Option<String>,
    /// Internal: VAD speech threshold (§6.2).
    pub vad_threshold: f32,
    /// Internal: auto-stop-on-silence seconds.
    pub idle_timeout: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mic_device: None,
            vad_threshold: 0.5,
            idle_timeout: 30,
        }
    }
}

impl Settings {
    /// Loads settings from `path`, or returns defaults if the file is absent.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = fs::read_to_string(path).context("read settings file")?;
        serde_json::from_str(&json).context("parse settings JSON")
    }

    /// Writes settings to `path` as pretty JSON, creating parent dirs.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).context("create settings directory")?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize settings")?;
        fs::write(path, json).context("write settings file")?;
        Ok(())
    }
}

/// Thread-safe settings handle managed in Tauri state (mirrors `SharedStore`).
/// Holds the live `Settings` plus the on-disk path so the `get_settings` /
/// `update_settings` commands (§9.4) can read and persist without re-reading the
/// file. Cloneable: every clone shares the same inner lock.
#[derive(Clone)]
pub struct SharedSettings {
    path: PathBuf,
    inner: Arc<Mutex<Settings>>,
}

impl SharedSettings {
    pub fn new(settings: Settings, path: PathBuf) -> Self {
        Self {
            path,
            inner: Arc::new(Mutex::new(settings)),
        }
    }

    /// Snapshot of the current settings.
    pub fn get(&self) -> Settings {
        self.inner.lock().unwrap().clone()
    }

    /// Persist the new settings to disk, then update the in-memory copy. Writing
    /// first means a failed save leaves the cached settings untouched.
    pub fn update(&self, settings: Settings) -> Result<()> {
        settings.save(&self.path)?;
        *self.inner.lock().unwrap() = settings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_settings_update_persists_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let shared = SharedSettings::new(Settings::default(), path.clone());

        let mut next = Settings::default();
        next.mic_device = Some("USB Mic".to_string());
        shared.update(next.clone()).unwrap();

        assert_eq!(shared.get(), next); // cached copy updated
        assert_eq!(Settings::load(&path).unwrap(), next); // and persisted
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::load(&dir.path().join("settings.json")).unwrap();
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn save_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.mic_device = Some("USB Mic".to_string());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
    }

    #[test]
    fn partial_json_fills_missing_fields_with_defaults() {
        // An older build's settings.json lacking newer keys must still load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"mic_device":"USB Mic"}"#).unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.mic_device, Some("USB Mic".to_string()));
        assert_eq!(s.idle_timeout, Settings::default().idle_timeout);
    }

    #[test]
    fn contains_no_phi_fields() {
        // Guard against PHI ever leaking into the unencrypted settings file.
        let json = serde_json::to_string(&Settings::default()).unwrap();
        for phi in ["transcript", "soap", "note", "label", "record"] {
            assert!(!json.contains(phi), "settings must not carry PHI field: {phi}");
        }
    }
}
