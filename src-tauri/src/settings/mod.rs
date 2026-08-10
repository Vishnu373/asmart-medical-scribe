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
    /// Internal: cached GPU detection result (§8.8). Never doctor-facing.
    pub gpu: GpuSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mic_device: None,
            vad_threshold: 0.5,
            idle_timeout: 30,
            gpu: GpuSettings::default(),
        }
    }
}

/// How far the one-time GPU detection (§8.8) has got on this machine.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GpuState {
    /// Never probed, or invalidated and awaiting re-detection.
    #[default]
    Pending,
    /// Probed; an adapter passed the memory floor.
    Done,
    /// Probed correctly, and the honest answer is "no usable GPU" — none
    /// present, no driver, or below the floor. A **success** and a supported
    /// configuration: do not re-probe this every launch.
    Unusable,
    /// The probe itself broke (driver fault, child process died). Worth
    /// retrying on update and worth seeing in a support log.
    Failed,
}

/// Which compute backend the LLM loads on (§8.8).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    Dgpu,
    Igpu,
    Cpu,
}

/// Cached detection result (§8.8). `adapter` and `memory_mb` drive no decision —
/// they exist so one settings file answers "what is this doctor running on".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GpuSettings {
    pub state: GpuState,
    pub backend: Option<GpuBackend>,
    pub adapter: Option<String>,
    pub memory_mb: Option<u64>,
    /// Probe attempts so far; bounds the retry so a faulting device cannot loop.
    pub attempts: u32,
}

impl Settings {
    /// Loads settings from `path`, or returns defaults if the file is absent or
    /// unparseable. An unknown enum value (e.g. a `gpu.state` written by a newer
    /// build, then downgraded) must not abort startup over a PHI-free config file.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let json = fs::read_to_string(path).context("read settings file")?;
        // serde_json::from_str(&json).context("parse settings JSON")
        match serde_json::from_str(&json) {
            Ok(settings) => Ok(settings),
            Err(e) => {
                log::warn!("settings.json unparseable, falling back to defaults: {e}");
                Ok(Self::default())
            }
        }
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
    fn gpu_defaults_to_pending_and_empty() {
        let g = Settings::default().gpu;
        assert_eq!(g.state, GpuState::Pending);
        assert_eq!(g.backend, None);
        assert_eq!(g.adapter, None);
        assert_eq!(g.memory_mb, None);
        assert_eq!(g.attempts, 0);
    }

    #[test]
    fn settings_json_without_gpu_key_yields_pending() {
        // An old-format settings.json predating §8.8 must still load.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"mic_device":null,"vad_threshold":0.5,"idle_timeout":30}"#,
        )
        .unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.gpu, GpuSettings::default());
    }

    #[test]
    fn each_gpu_state_round_trips_through_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        for (i, (state, backend)) in [
            (GpuState::Pending, None),
            (GpuState::Done, Some(GpuBackend::Dgpu)),
            (GpuState::Done, Some(GpuBackend::Igpu)),
            (GpuState::Unusable, Some(GpuBackend::Cpu)),
            (GpuState::Failed, Some(GpuBackend::Cpu)),
        ]
        .into_iter()
        .enumerate()
        {
            let path = dir.path().join(format!("settings{i}.json"));
            let mut s = Settings::default();
            s.gpu = GpuSettings {
                state,
                backend,
                adapter: Some("Intel Arc Graphics".to_string()),
                memory_mb: Some(8192),
                attempts: 1,
            };
            s.save(&path).unwrap();
            assert_eq!(Settings::load(&path).unwrap(), s);
        }
    }

    #[test]
    fn unknown_gpu_state_falls_back_to_defaults_instead_of_failing() {
        // A newer build's state string, then a downgrade: must not abort startup.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, r#"{"gpu":{"state":"probing"}}"#).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
    }

    #[test]
    fn gpu_enums_serialize_as_lowercase_strings() {
        // §8.8 fixes the on-disk spelling; these are read by other tooling.
        let mut s = Settings::default();
        s.gpu.state = GpuState::Unusable;
        s.gpu.backend = Some(GpuBackend::Igpu);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""state":"unusable""#), "{json}");
        assert!(json.contains(r#""backend":"igpu""#), "{json}");
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
