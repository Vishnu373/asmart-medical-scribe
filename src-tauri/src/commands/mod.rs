//! Tauri command handlers (frontend → backend). See design §9.4.
//! Commands are *requests*: the backend coordinator owns the state and its
//! guards reject illegal transitions, returning an `Err(String)` the frontend surfaces.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tauri::{AppHandle, Emitter, State};

use crate::llm::LlmEngine;
use crate::orchestrator::Coordinator;
use crate::settings::{Settings, SharedSettings};
use crate::store::{Note, Record, RecordSummary, SharedStore};
use crate::stt::{ModelKind, SttEngine};

/// Echoes a message back to the frontend with a prefix, proving the bridge works.
#[tauri::command]
pub fn ping(message: String) -> String {
    format!("pong: {message}")
}

/// IDLE → RECORDING. Rejected unless currently IDLE.
#[tauri::command]
pub fn start_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    // crate::trial::ensure_active()?;
    coordinator.start_recording()?;
    crate::telemetry::track_event("session_started", serde_json::json!({}));
    Ok(())
}

/// RECORDING → PROCESSING → IDLE: stop capture, drain the queue, return to IDLE.
#[tauri::command]
pub fn stop_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<Option<String>, String> {
    let saved = coordinator.stop_recording()?;
    crate::telemetry::track_event(
        "session_completed",
        serde_json::json!({ "saved": saved.is_some() }),
    );
    Ok(saved)
}

/// Pause capture within a recording.
#[tauri::command]
pub fn pause_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.pause_recording()
}

/// Resume a paused recording.
#[tauri::command]
pub fn resume_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.resume_recording()
}

/// Save the doctor's edits to a record's transcript (autosave).
#[tauri::command]
pub fn update_transcript(
    store: State<'_, SharedStore>,
    id: String,
    transcript: String,
) -> Result<(), String> {

    match store.lock().update_transcript(&id, &transcript) {
        Ok(()) => {
            log::info!("[EDIT] {id} transcript updated");
            Ok(())
        }
        Err(e) => {
            log::warn!("[EDIT] {id} transcript update failed {e}");
            Err(e.to_string())
        }
    }
}

/// List saved encounters/sessions, newest first.
#[tauri::command]
pub fn list_records(store: State<'_, SharedStore>) -> Result<Vec<RecordSummary>, String> {
    store.lock().list_records().map_err(|e| e.to_string())
}

/// Load a full record (transcript included) by id; `None` if it's gone.
#[tauri::command]
pub fn open_record(store: State<'_, SharedStore>, id: String) -> Result<Option<Record>, String> {
    store.lock().open_record(&id).map_err(|e| e.to_string())
}

/// List a record's note versions, newest first; the `is_active` row is the current note and the rest are the revertable history. 
/// The frontend loads these after a record opens and after GENERATING→IDLE.
#[tauri::command]
pub fn list_notes(store: State<'_, SharedStore>, record_id: String) -> Result<Vec<Note>, String> {
    store
        .lock()
        .list_notes(&record_id)
        .map_err(|e| e.to_string())
}

/// Permanently delete a record and its notes.
#[tauri::command]
pub fn delete_record(store: State<'_, SharedStore>, id: String) -> Result<(), String> {
    store.lock().delete_record(&id).map_err(|e| e.to_string())
}

/// IDLE → GENERATING → IDLE: generate a SOAP note from the record's (edited) transcript and persist it as the new active version.
/// Streams `generation-token` events; resolves with the new note id (`None` if cancelled).
/// Guarded against an empty transcript.

/// `async` + `spawn_blocking`: generation blocks for seconds, so it runs on a blocking-pool thread rather than the IPC thread.
/// That keeps the IPC thread free to dispatch `cancel_generation` and stops the window from freezing for the whole generation.
#[tauri::command]
pub async fn generate_note(
    coordinator: State<'_, Arc<Coordinator>>,
    store: State<'_, SharedStore>,
    record_id: String,
) -> Result<Option<String>, String> {
    // crate::trial::ensure_active()?;
    let transcript = load_transcript(&store, &record_id)?;
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || coordinator.generate_note(&record_id, &transcript))
        .await
        .map_err(|e| e.to_string())?
}

/// Produce another note version for the record.
#[tauri::command]
pub async fn regenerate_note(
    coordinator: State<'_, Arc<Coordinator>>,
    store: State<'_, SharedStore>,
    record_id: String,
) -> Result<Option<String>, String> {
    // crate::trial::ensure_active()?;
    let transcript = load_transcript(&store, &record_id)?;
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || coordinator.generate_note(&record_id, &transcript))
        .await
        .map_err(|e| e.to_string())?
}

/// Cancel the in-flight generation; the partial note is discarded.
#[tauri::command]
pub fn cancel_generation(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.cancel_generation()
}

/// Autosave the clinician's in-place edits to a note.
#[tauri::command]
pub fn update_note(
    store: State<'_, SharedStore>,
    id: String,
    soap_data: String,
) -> Result<(), String> {
    match store.lock().update_note(&id, &soap_data) {
        Ok(()) => {
            log::info!("[EDIT] {id} generated notes updated");
            Ok(())
        }
        Err(e) => {
            log::warn!("[EDIT] {id} generated notes update failed {e}");
            Err(e.to_string())
        }
    }
}

/// Revert a record to an earlier note version, making it active again.
#[tauri::command]
pub fn revert_version(
    store: State<'_, SharedStore>,
    record_id: String,
    note_id: String,
) -> Result<(), String> {
    store
        .lock()
        .set_active_note(&record_id, &note_id)
        .map_err(|e| e.to_string())
}

/// Read the current settings. The `state` handle is injected by type; no JS args.
#[tauri::command]
pub fn get_settings(state: State<'_, SharedSettings>) -> Settings {
    state.get()
}

/// Current status.
/// Returns `"ready"` when the model is loaded, else `"loading"`.
#[tauri::command]
pub fn get_llm_status(engine: State<'_, Arc<LlmEngine>>) -> String {
    if engine.is_loaded() {
        "ready".to_string()
    } else {
        "loading".to_string()
    }
}

/// The gate holds the engine until the frontend reports it has mounted ([`frontend_ready`]), then warms once.
/// blueprint of PreloadGate
pub struct PreloadGate {
    stt: Arc<SttEngine>,
    stt_model_dirs: Vec<PathBuf>,
    engine: Arc<LlmEngine>,
    /// One-shot guard, but re-armable: the preload thread clears it when the load
    /// fails so Setup can call [`frontend_ready`] again once the weights exist.
    // started: AtomicBool,
    started: Arc<AtomicBool>,
}

/// Instance of Preload
impl PreloadGate {
    pub fn new(stt: Arc<SttEngine>, stt_model_dirs: Vec<PathBuf>, engine: Arc<LlmEngine>) -> Self {
        Self {
            stt,
            stt_model_dirs,
            engine,
            // started: AtomicBool::new(false),
            started: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// frontend loaded -> model weights loading (STT -> LLM)
#[tauri::command]
pub fn frontend_ready(app: AppHandle, gate: State<'_, PreloadGate>) {
    if gate.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let engine = gate.engine.clone();
    let stt = gate.stt.clone();
    let stt_model_dirs = gate.stt_model_dirs.clone();
    let started = gate.started.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let _ = app.emit("llm-status", serde_json::json!({ "status": "loading" }));
        if let Err(e) = stt.ensure_loaded(ModelKind::Parakeet, &stt_model_dirs) {
            log::warn!("[LOAD] STT preload failed (will retry on Record): {e}");
        }
        match engine.ensure_loaded() {
            Ok(()) => {
                log::info!(
                    "[LOAD] both models resident, status changed to READY ({:.1}s)",
                    t0.elapsed().as_secs_f32()
                );
                let _ = app.emit("llm-status", serde_json::json!({ "status": "ready" }));
            }
            Err(e) => {
                // Re-arm so a later `frontend_ready` actually runs. On a first run this
                // call arrives at App mount, before Setup has downloaded the weights, so
                // the failure is expected — Setup calls again once both models are on
                // disk and that second call is the one that primes (§8.2, §8.7).
                started.store(false, Ordering::SeqCst);
                log::warn!("LLM preload failed (will retry on first generation): {e}");
                let _ = app.emit(
                    "llm-status",
                    serde_json::json!({ "status": "error", "message": e.to_string() }),
                );
            }
        }
    });
}

/// Persist patched settings.
#[tauri::command]
// pub fn update_settings(state: State<'_, SharedSettings>, settings: Settings) -> Result<(), String> {
//     state.update(settings).map_err(|e| e.to_string())
// }
pub fn update_settings(
    state: State<'_, SharedSettings>,
    mut settings: Settings,
) -> Result<(), String> {
    // The `gpu` block (§8.8) is backend-owned: a payload missing the key would
    // deserialize to `pending` and wipe a completed probe. Keep ours.
    settings.gpu = state.get().gpu;
    state.update(settings).map_err(|e| e.to_string())
}

/// A microphone choice for the settings picker. 
/// Display-only metadata — the live cpal handle stays in the backend; `mic_device` persists the chosen `name` (`None` = system default).
#[derive(serde::Serialize)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

/// Settings view for the mic picker.
#[tauri::command]
pub fn list_input_devices() -> Result<Vec<InputDevice>, String> {
    crate::audio_toolkit::list_input_devices()
        .map(|devs| {
            devs.into_iter()
                .map(|d| InputDevice {
                    name: d.name,
                    is_default: d.is_default,
                })
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Submit doctor-typed feedback ("report a problem") through the telemetry seam — the "broke but didn't crash" channel that lands alongside crashes.
/// Routes to the same backend when built with `crash-reporting` + a DSN; a local log otherwise. 
/// The body is free text and NOT scrubbable, so the UI warns the clinician against including patient information.
#[tauri::command]
pub fn submit_feedback(message: String) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("feedback message is empty".to_string());
    }
    crate::telemetry::report_feedback(message);
    Ok(())
}


/// Initial stage, after installing the application and downloaded the model weights
#[tauri::command]
pub fn mark_setup_completed() {
    crate::telemetry::track_event("setup_completed", serde_json::json!({}));
}

// Beta expiry removed — the app no longer gates on a compiled-in end date.
// /// Temporary, trial status until July 31 2026
// #[tauri::command]
// pub fn trial_status() -> crate::trial::TrialStatus {
//     crate::trial::status()
// }

/// Record an app-update lifecycle event on-device, plus telemetry for the two failure stages.
#[tauri::command]
pub fn log_update_event(stage: String, message: Option<String>) {
    match stage.as_str() {
        "available" => log::info!("[UPDATE] update available"),
        "downloaded" => log::info!("[UPDATE] update downloaded"),
        "installed" => log::info!("[UPDATE] update installed"),
        "download_failed" => {
            let e = crate::telemetry::sanitize_error(message.as_deref().unwrap_or(""));
            log::warn!("[UPDATE] update download failed {e}");
            crate::telemetry::track_event(
                "update_download_failed",
                serde_json::json!({ "error": e }),
            );
        }
        "install_failed" => {
            let e = crate::telemetry::sanitize_error(message.as_deref().unwrap_or(""));
            log::warn!("[UPDATE] update install failed {e}");
            crate::telemetry::track_event(
                "update_install_failed",
                serde_json::json!({ "error": e }),
            );
        }
        other => log::warn!("[UPDATE] unknown update stage: {other}"),
    }
}

/// Load a record's transcript for generation, rejecting a missing record or an empty transcript.
fn load_transcript(store: &State<'_, SharedStore>, record_id: &str) -> Result<String, String> {
    let record = store
        .lock()
        .open_record(record_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("record {record_id} not found"))?;
    if record.transcript.trim().is_empty() {
        return Err("cannot generate a note from an empty transcript".to_string());
    }
    Ok(record.transcript)
}
