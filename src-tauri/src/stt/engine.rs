use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use log::{error, info, warn};
use transcribe_rs::{
    onnx::{
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        Quantization,
    },
    whisper_cpp::{WhisperEngine, WhisperInferenceParams},
    SpeechModel,
};

use super::text::filter_transcription_output;
use super::Transcriber;

/// Which engine backs a model. Parakeet TDT (multilingual EN+FR) is the default;
/// Whisper is the fallback (design §6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    Parakeet,
    Whisper,
}

/// The two engines we ship, trimmed from the reference's eight. Dropping the
/// engine value frees the model (used by `unload` and the idle watcher).
enum LoadedEngine {
    Whisper(WhisperEngine),
    Parakeet(ParakeetModel),
}

/// Owns the loaded STT model and transcribes audio. A background watcher unloads
/// the model after it has been idle longer than `idle_timeout` (0 = never).
pub struct SttEngine {
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    current: Arc<Mutex<Option<ModelKind>>>,
    last_activity: Arc<AtomicU64>,
    /// True while a consult is recording; the watcher never unloads then, so a
    /// long silence gap (patient steps out, pause/resume) can't pull the model
    /// out from under the next utterance.
    recording: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
    /// Transcription language: "en", "fr", or "auto".
    language: Mutex<String>,
}

impl SttEngine {
    /// Create the engine and start the idle-unload watcher. `idle_timeout` of
    /// zero disables automatic unloading.
    pub fn new(idle_timeout: Duration) -> Self {
        let engine: Arc<Mutex<Option<LoadedEngine>>> = Arc::new(Mutex::new(None));
        let current: Arc<Mutex<Option<ModelKind>>> = Arc::new(Mutex::new(None));
        let last_activity = Arc::new(AtomicU64::new(now_ms()));
        let recording = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));

        let watcher = {
            let engine = engine.clone();
            let current = current.clone();
            let last_activity = last_activity.clone();
            let recording = recording.clone();
            let shutdown = shutdown.clone();
            thread::spawn(move || {
                idle_watch(engine, current, last_activity, recording, shutdown, idle_timeout)
            })
        };

        Self {
            engine,
            current,
            last_activity,
            recording,
            shutdown,
            watcher: Some(watcher),
            // Default to auto-detect (design FR-2/FR-5: the app detects EN/FR).
            // Forcing "en" would garble a French consult under the Whisper
            // fallback before the orchestrator ever calls set_language.
            language: Mutex::new("auto".to_string()),
        }
    }

    /// Set the transcription language ("en", "fr", or "auto").
    pub fn set_language(&self, lang: impl Into<String>) {
        *self.language.lock().unwrap() = lang.into();
    }

    /// Reset the idle timer (call while recording so the model isn't unloaded
    /// mid-consult).
    pub fn touch_activity(&self) {
        self.last_activity.store(now_ms(), Ordering::Relaxed);
    }

    /// Mark recording on/off. While on, the idle watcher never unloads the
    /// model, so a long silence gap can't unload it mid-consult. The
    /// orchestrator sets this around a recording session.
    pub fn set_recording(&self, recording: bool) {
        self.recording.store(recording, Ordering::Relaxed);
        if recording {
            self.touch_activity();
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.lock_engine().is_some()
    }

    pub fn current_model(&self) -> Option<ModelKind> {
        *self.current.lock().unwrap()
    }

    /// Load a model from `model_path`, replacing any currently loaded one.
    pub fn load(&self, kind: ModelKind, model_path: &Path) -> Result<()> {
        let loaded = match kind {
            ModelKind::Whisper => LoadedEngine::Whisper(
                WhisperEngine::load(model_path)
                    .map_err(|e| anyhow!("failed to load Whisper model: {e}"))?,
            ),
            ModelKind::Parakeet => LoadedEngine::Parakeet(
                ParakeetModel::load(model_path, &Quantization::Int8)
                    .map_err(|e| anyhow!("failed to load Parakeet model: {e}"))?,
            ),
        };

        *self.lock_engine() = Some(loaded);
        *self.current.lock().unwrap() = Some(kind);
        self.touch_activity(); // don't let the watcher unload a just-loaded model
        info!("Loaded STT model: {kind:?}");
        Ok(())
    }

    pub fn unload(&self) {
        *self.lock_engine() = None;
        *self.current.lock().unwrap() = None;
    }

    /// Lock the engine mutex, recovering from poison if a previous transcription
    /// panicked (we never put a panicked engine back, so the slot is consistent).
    fn lock_engine(&self) -> MutexGuard<'_, Option<LoadedEngine>> {
        self.engine.lock().unwrap_or_else(|poisoned| {
            warn!("STT engine mutex was poisoned by a previous panic, recovering");
            poisoned.into_inner()
        })
    }
}

impl Transcriber for SttEngine {
    fn transcribe(&self, audio: &[f32]) -> Result<String> {
        self.touch_activity();

        if audio.is_empty() {
            return Ok(String::new());
        }

        let language = self.language.lock().unwrap().clone();
        let whisper_language = validated_whisper_language(&language);

        // Take the engine out so we own it during the (panic-prone) native call.
        // We catch_unwind so an engine panic unloads the model instead of
        // poisoning the mutex and hanging every later call. The take() itself
        // handles the not-loaded case, so no separate pre-check is needed.
        let mut engine = match self.lock_engine().take() {
            Some(e) => e,
            None => return Err(anyhow!("STT model is not loaded")),
        };

        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<String> {
            match &mut engine {
                LoadedEngine::Whisper(w) => {
                    let params = WhisperInferenceParams {
                        language: whisper_language.clone(),
                        ..Default::default()
                    };
                    w.transcribe_with(audio, &params)
                        .map(|r| r.text)
                        .map_err(|e| anyhow!("Whisper transcription failed: {e}"))
                }
                // Parakeet TDT v3 is multilingual and auto-detects; it takes no
                // language param here (matches the reference engine call).
                LoadedEngine::Parakeet(p) => {
                    let params = ParakeetParams {
                        timestamp_granularity: Some(TimestampGranularity::Segment),
                        ..Default::default()
                    };
                    p.transcribe_with(audio, &params)
                        .map(|r| r.text)
                        .map_err(|e| anyhow!("Parakeet transcription failed: {e}"))
                }
            }
        }));

        let text = match outcome {
            Ok(inner) => {
                // Normal path (success or engine error): put the engine back.
                *self.lock_engine() = Some(engine);
                inner?
            }
            Err(payload) => {
                // Panic: drop the engine (unload) and clear the model id so the
                // next attempt reloads from scratch.
                *self.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let msg = panic_message(payload);
                error!("STT engine panicked: {msg}. Model unloaded.");
                return Err(anyhow!(
                    "STT engine panicked: {msg}. The model has been unloaded and will reload on the next attempt."
                ));
            }
        };

        // Strip filler words / stutter artifacts (design's transcript cleanup).
        Ok(filter_transcription_output(&text, base_lang(&language), &None))
    }
}

impl Drop for SttEngine {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.watcher.take() {
            let _ = handle.join();
        }
    }
}

/// Background loop: unload the model once it has been idle past `idle_timeout`.
fn idle_watch(
    engine: Arc<Mutex<Option<LoadedEngine>>>,
    current: Arc<Mutex<Option<ModelKind>>>,
    last_activity: Arc<AtomicU64>,
    recording: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    idle_timeout: Duration,
) {
    while !shutdown.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(5));
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if idle_timeout.is_zero() {
            continue; // automatic unload disabled
        }
        if recording.load(Ordering::Relaxed) {
            continue; // never unload mid-consult, even through long silence gaps
        }

        let idle_ms = now_ms().saturating_sub(last_activity.load(Ordering::Relaxed));
        if idle_ms <= idle_timeout.as_millis() as u64 {
            continue;
        }

        let mut guard = engine.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            *guard = None;
            *current.lock().unwrap_or_else(|e| e.into_inner()) = None;
            info!("STT model unloaded after {}s idle", idle_ms / 1000);
        }
    }
}

/// Monotonic milliseconds since the first call. Using `Instant` (not
/// `SystemTime`) keeps the idle timer immune to wall-clock changes: a backward
/// step won't pin the model loaded forever and a forward jump won't unload it
/// the instant after use.
fn now_ms() -> u64 {
    static EPOCH: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);
    EPOCH.elapsed().as_millis() as u64
}

/// Base language code without region, e.g. "en-US" -> "en".
fn base_lang(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

/// Map the configured language to what Whisper expects: `Some(code)` for a
/// supported language (EN/FR), `None` to auto-detect (also for "auto"/unknown).
fn validated_whisper_language(lang: &str) -> Option<String> {
    match base_lang(lang) {
        "en" => Some("en".to_string()),
        "fr" => Some("fr".to_string()),
        _ => None,
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_validation_maps_supported_and_falls_back() {
        assert_eq!(validated_whisper_language("en"), Some("en".to_string()));
        assert_eq!(validated_whisper_language("fr-FR"), Some("fr".to_string()));
        assert_eq!(validated_whisper_language("auto"), None);
        assert_eq!(validated_whisper_language("de"), None);
    }

    #[test]
    fn transcribe_without_loaded_model_errors_but_empty_audio_is_ok() {
        // No model loaded and no native deps touched on these paths.
        let engine = SttEngine::new(Duration::ZERO);
        assert_eq!(engine.transcribe(&[]).unwrap(), "");
        assert!(engine.transcribe(&[0.1, 0.2]).is_err());
        assert!(!engine.is_loaded());
        assert_eq!(engine.current_model(), None);
    }
}
