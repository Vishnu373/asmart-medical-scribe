//! Tauri command handlers (frontend → backend). See design §9.4.
//!
//! Commands are *requests*: the backend coordinator owns the state and its
//! guards reject illegal transitions, returning an `Err(String)` the frontend
//! surfaces (design §6.6/§9.4).

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

/// IDLE → RECORDING (design §9.4). Rejected unless currently IDLE.
#[tauri::command]
pub fn start_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    crate::trial::ensure_active()?;
    coordinator.start_recording()?;
    crate::telemetry::track_event("session_started", serde_json::json!({}));
    Ok(())
}

/// RECORDING → PROCESSING → IDLE: stop capture, drain the queue, return to IDLE.
/// Resolves with the saved record's id (`None` if the consult was empty) so the
/// UI can load it for editing and note generation (design §6.6).
#[tauri::command]
pub fn stop_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<Option<String>, String> {
    let saved = coordinator.stop_recording()?;
    crate::telemetry::track_event(
        "session_completed",
        serde_json::json!({ "saved": saved.is_some() }),
    );
    Ok(saved)
}

/// Pause capture within a recording (stays RECORDING; design §6.6/§9.4).
#[tauri::command]
pub fn pause_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.pause_recording()
}

/// Resume a paused recording.
#[tauri::command]
pub fn resume_recording(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.resume_recording()
}

/// Save the doctor's edits to a record's transcript (autosave, NFR-8; §9.4).
#[tauri::command]
pub fn update_transcript(
    store: State<'_, SharedStore>,
    id: String,
    transcript: String,
) -> Result<(), String> {
    // §10.3 `[EDIT] {record_id} transcript updated / update failed` (on-device only —
    // the id and any DB/IO error, never the transcript text).
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

/// List saved encounters, newest first (FR-13; §9.4).
#[tauri::command]
pub fn list_records(store: State<'_, SharedStore>) -> Result<Vec<RecordSummary>, String> {
    store.lock().list_records().map_err(|e| e.to_string())
}

/// Load a full record (transcript included) by id; `None` if it's gone (§9.4).
#[tauri::command]
pub fn open_record(store: State<'_, SharedStore>, id: String) -> Result<Option<Record>, String> {
    store.lock().open_record(&id).map_err(|e| e.to_string())
}

/// List a record's note versions, newest first; the `is_active` row is the
/// current note and the rest are the revertable history (§8.5). The frontend
/// loads these after a record opens and after GENERATING→IDLE (design §9.5).
#[tauri::command]
pub fn list_notes(store: State<'_, SharedStore>, record_id: String) -> Result<Vec<Note>, String> {
    store
        .lock()
        .list_notes(&record_id)
        .map_err(|e| e.to_string())
}

/// Permanently delete a record and its notes (cascade via FK; NFR-9, §9.4).
#[tauri::command]
pub fn delete_record(store: State<'_, SharedStore>, id: String) -> Result<(), String> {
    store.lock().delete_record(&id).map_err(|e| e.to_string())
}

/// IDLE → GENERATING → IDLE: generate a SOAP note from the record's (edited)
/// transcript and persist it as the new active version (§8.4). Streams
/// `generation-token` events; resolves with the new note id (`None` if cancelled).
/// Guarded against an empty transcript (§8.1).
///
/// `async` + `spawn_blocking`: generation blocks for seconds, so it runs on a
/// blocking-pool thread (with an owned `Arc<Coordinator>`) rather than the IPC
/// thread. That keeps the IPC thread free to dispatch `cancel_generation` and
/// stops the window from freezing for the whole generation (§8.4).
#[tauri::command]
pub async fn generate_note(
    coordinator: State<'_, Arc<Coordinator>>,
    store: State<'_, SharedStore>,
    record_id: String,
) -> Result<Option<String>, String> {
    crate::trial::ensure_active()?;
    let transcript = load_transcript(&store, &record_id)?;
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || coordinator.generate_note(&record_id, &transcript))
        .await
        .map_err(|e| e.to_string())?
}

/// Produce another note version for the record (§8.1). Identical to
/// `generate_note` — each (re)generation creates a new retained, revertable
/// version — but exposed separately to match the §9.4 command contract.
#[tauri::command]
pub async fn regenerate_note(
    coordinator: State<'_, Arc<Coordinator>>,
    store: State<'_, SharedStore>,
    record_id: String,
) -> Result<Option<String>, String> {
    crate::trial::ensure_active()?;
    let transcript = load_transcript(&store, &record_id)?;
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || coordinator.generate_note(&record_id, &transcript))
        .await
        .map_err(|e| e.to_string())?
}

/// Cancel the in-flight generation; the partial note is discarded (§8.4).
#[tauri::command]
pub fn cancel_generation(coordinator: State<'_, Arc<Coordinator>>) -> Result<(), String> {
    coordinator.cancel_generation()
}

/// Autosave the clinician's in-place edits to a note (§8.5).
#[tauri::command]
pub fn update_note(
    store: State<'_, SharedStore>,
    id: String,
    soap_data: String,
) -> Result<(), String> {
    // §10.3 `[EDIT] {note_id} generated notes updated / update failed` (on-device only —
    // the id and any DB/IO error, never the note text).
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

/// Revert a record to an earlier note version, making it active again (§8.5).
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

/// Read the current settings (§9.3/§9.4). The `state` handle is injected by type;
/// no JS args.
#[tauri::command]
pub fn get_settings(state: State<'_, SharedSettings>) -> Settings {
    state.get()
}

/// Current note-model readiness for the UI's "preparing" hint (design §8.2 startup
/// fix, §9.5). Queried once at mount to seed the state before the async `llm-status`
/// event flips it — the co-resident preload emits `loading` before the webview has a
/// listener, so a mount query is how the UI reliably learns it is still warming.
/// Returns `"ready"` when the model is loaded, else `"loading"`.
#[tauri::command]
pub fn get_llm_status(engine: State<'_, Arc<LlmEngine>>) -> String {
    if engine.is_loaded() {
        "ready".to_string()
    } else {
        "loading".to_string()
    }
}

/// Deferred co-resident LLM preload (§8.2 startup fix). Built in `setup` but *not*
/// run there: warming the multi-GB GGUF (mmap + warmup decode) inside `setup`
/// starves WebView2's first paint, so Windows ghosts the window as "not
/// responding" on launch even though the warm runs off the main thread. The gate
/// holds the engine until the frontend reports it has mounted ([`frontend_ready`]),
/// then warms once.
pub struct PreloadGate {
    /// Loaded first — see [`frontend_ready`].
    stt: Arc<SttEngine>,
    /// Model search dirs (D1) the STT load resolves Parakeet against.
    stt_model_dirs: Vec<PathBuf>,
    engine: Arc<LlmEngine>,
    /// Flipped the first time [`frontend_ready`] fires, so re-mounts don't re-warm.
    started: AtomicBool,
}

impl PreloadGate {
    pub fn new(stt: Arc<SttEngine>, stt_model_dirs: Vec<PathBuf>, engine: Arc<LlmEngine>) -> Self {
        Self {
            stt,
            stt_model_dirs,
            engine,
            started: AtomicBool::new(false),
        }
    }
}

/// Frontend → backend: the React app has finished mounting (§8.2 startup fix).
/// Kicks off the co-resident model warm exactly once, on a background thread,
/// emitting the same `llm-status` loading/ready/error events the mount query seeds
/// from. Deferring the warm to here — rather than `setup` — keeps the heavy GGUF
/// load off the launch path so the window paints before it starts. A no-op if a
/// prior call already started it (dev remounts / HMR). A load failure is non-fatal:
/// the first generation retries and surfaces the error then.
#[tauri::command]
pub fn frontend_ready(app: AppHandle, gate: State<'_, PreloadGate>) {
    if gate.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let engine = gate.engine.clone();
    let stt = gate.stt.clone();
    let stt_model_dirs = gate.stt_model_dirs.clone();
    std::thread::spawn(move || {
        let t0 = Instant::now();
        let _ = app.emit("llm-status", serde_json::json!({ "status": "loading" }));
        // STT first, then the SLM. A failure here is non-fatal — `RealPipeline::start`
        // still calls `ensure_loaded` and will surface the error on Record.
        if let Err(e) = stt.ensure_loaded(ModelKind::Parakeet, &stt_model_dirs) {
            log::warn!("[LOAD] STT preload failed (will retry on Record): {e}");
        }
        match engine.ensure_loaded() {
            Ok(()) => {
                // §10.3 co-resident ready line. (Catalog tags this `[CLOSE]`, which is a
                // doc typo — it is a load-completion event, so it carries `[LOAD]`.)
                log::info!(
                    "[LOAD] both models resident, status changed to READY ({:.1}s)",
                    t0.elapsed().as_secs_f32()
                );
                let _ = app.emit("llm-status", serde_json::json!({ "status": "ready" }));
            }
            Err(e) => {
                log::warn!("LLM preload failed (will retry on first generation): {e}");
                let _ = app.emit(
                    "llm-status",
                    serde_json::json!({ "status": "error", "message": e.to_string() }),
                );
            }
        }
    });
}

/// Persist patched settings (§9.3/§9.4). The frontend sends the full object
/// (read-modify-write), so internal keys are preserved across the round-trip. The
/// value param is named `settings` to match the `invoke("update_settings", { settings })`
/// arg; the managed handle is the type-resolved `state` param.
#[tauri::command]
pub fn update_settings(state: State<'_, SharedSettings>, settings: Settings) -> Result<(), String> {
    // There is one note model now, so settings changes never retarget the engine —
    // this only persists them (§3 single-model refactor).
    state.update(settings).map_err(|e| e.to_string())
}

/// A microphone choice for the settings picker (FR-12). Display-only metadata —
/// the live cpal handle stays in the backend; `mic_device` persists the chosen
/// `name` (`None` = system default).
#[derive(serde::Serialize)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

/// Enumerate capture devices so the Settings view can populate the mic picker
/// (§9.3 `mic_device`, FR-12). Wraps the existing audio-toolkit enumeration,
/// dropping the non-serializable cpal handle.
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

/// Submit doctor-typed feedback ("report a problem") through the telemetry seam
/// (§10.3) — the "broke but didn't crash" channel that lands alongside crashes.
/// Routes to the same backend when built with `crash-reporting` + a DSN; a local
/// log otherwise. The body is free text and NOT scrubbable, so the UI warns the
/// clinician against including patient information.
#[tauri::command]
pub fn submit_feedback(message: String) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("feedback message is empty".to_string());
    }
    crate::telemetry::report_feedback(message);
    Ok(())
}

/// Mark first-run setup as complete (implementation.md §3) — a deliberate,
/// PHI-free product event fired once, when the final required model download lands
/// and the app becomes usable. The frontend calls this from the setup screen's
/// completion transition, which only runs on a genuine first run (a later launch
/// finds the models present and skips setup entirely).
#[tauri::command]
pub fn mark_setup_completed() {
    crate::telemetry::track_event("setup_completed", serde_json::json!({}));
}

/// Report the compiled-in beta trial verdict (implementation.md §1). The frontend
/// calls this on startup and, once `expired`, shows the expired screen instead of
/// the app. Pure/local — the date is baked into the binary and compared to the
/// system clock (see `crate::trial`).
#[tauri::command]
pub fn trial_status() -> crate::trial::TrialStatus {
    crate::trial::status()
}

/// Record an app-update lifecycle event (§10.3 `[UPDATE]` rows) on-device, plus
/// telemetry for the two failure stages. The updater is driven entirely from the
/// frontend (`useUpdateCheck` / `UpdateButton`), so the app logs these from where it
/// drives the update rather than a plugin callback. `message` carries the JS error
/// string for the failure stages; it is sanitized before either sink (§10.3 — it
/// can embed a profile path). No PHI: update events are binary-only (§14).
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

/// Load a record's transcript for generation, rejecting a missing record or an
/// empty transcript (§8.1 "Generate is disabled when the transcript is empty").
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
