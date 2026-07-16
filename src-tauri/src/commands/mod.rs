//! Tauri command handlers (frontend → backend). See design §9.4.
//!
//! Commands are *requests*: the backend coordinator owns the state and its
//! guards reject illegal transitions, returning an `Err(String)` the frontend
//! surfaces (design §6.6/§9.4).

use std::sync::Arc;

use tauri::State;

use crate::llm::{LlmEngine, LlmModel};
use crate::orchestrator::Coordinator;
use crate::settings::{Settings, SharedSettings};
use crate::store::{Note, Record, RecordSummary, SharedStore};

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
    store
        .lock()
        .update_transcript(&id, &transcript)
        .map_err(|e| e.to_string())
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
    store.lock().list_notes(&record_id).map_err(|e| e.to_string())
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

/// IDLE → CORRECTING → IDLE: run the post-ASR correction pass over the record's
/// finalized transcript (design §6.7). Auto-invoked by the UI on Stop. Streams
/// `correction-suggestion` events and a terminal `correction-done`/`correction-error`;
/// resolves once the pass ends. Blocks note generation until it does (the sequencing
/// invariant). Like `generate_note`, runs on a blocking thread so the IPC thread stays
/// free to dispatch `cancel_generation`.
#[tauri::command]
pub async fn suggest_corrections(
    coordinator: State<'_, Arc<Coordinator>>,
    store: State<'_, SharedStore>,
    record_id: String,
) -> Result<(), String> {
    crate::trial::ensure_active()?;
    let transcript = load_transcript(&store, &record_id)?;
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || coordinator.suggest_corrections(&transcript))
        .await
        .map_err(|e| e.to_string())?
}

/// Cancel the in-flight generation *or* correction pass; a partial note is discarded,
/// a cancelled correction leaves the transcript plain (§8.4/§6.7).
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
    store
        .lock()
        .update_note(&id, &soap_data)
        .map_err(|e| e.to_string())
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
/// Returns `"ready"` when the model is loaded (or in swap mode, where it loads lazily
/// per generation so generation is available immediately), else `"loading"`.
#[tauri::command]
pub fn get_llm_status(
    engine: State<'_, Arc<LlmEngine>>,
    settings: State<'_, SharedSettings>,
) -> String {
    // Swap mode loads lazily per generation (no preload, so no `llm-status` event
    // fires) — generation is available immediately, so report "ready". The effective
    // mode must honor a residency *override*, which `resolve` applies without ever
    // writing `residency_mode`; reading that field raw would report "loading" forever
    // on a swap-by-override device (design §7).
    let swap = crate::residency::effective_mode(&settings.get())
        == Some(crate::residency::ResidencyMode::Swap);
    if engine.is_loaded() || swap {
        "ready".to_string()
    } else {
        "loading".to_string()
    }
}

/// Persist patched settings (§9.3/§9.4). The frontend sends the full object
/// (read-modify-write), so internal keys are preserved across the round-trip. The
/// value param is named `settings` to match the `invoke("update_settings", { settings })`
/// arg; the managed handle is the type-resolved `state` param.
#[tauri::command]
pub fn update_settings(
    state: State<'_, SharedSettings>,
    engine: State<'_, Arc<LlmEngine>>,
    settings: Settings,
) -> Result<(), String> {
    // Retarget the live note-generation engine so a `model_choice` change takes
    // effect without an app restart. `from_choice` resolves the tier the same way
    // startup does (explicit tier, else the RAM-fit default); `set_model` is a no-op
    // when unchanged and otherwise unloads the old model so the next note reloads
    // the new one. Uses the cached RAM probe, falling back to a fresh probe.
    let total_ram = settings
        .observed_total_ram
        .unwrap_or_else(crate::residency::probe_total_ram);
    let kind = LlmModel::from_choice(&settings.model_choice, total_ram);
    state.update(settings).map_err(|e| e.to_string())?;
    engine.set_model(kind);
    Ok(())
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
