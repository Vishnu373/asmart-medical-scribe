//! Tauri command handlers (frontend → backend). See design §9.4.
//!
//! Commands are *requests*: the backend coordinator owns the state and its
//! guards reject illegal transitions, returning an `Err(String)` the frontend
//! surfaces (design §6.6/§9.4).

use tauri::State;

use crate::orchestrator::Coordinator;
use crate::store::{Record, RecordSummary, SharedStore};

/// Echoes a message back to the frontend with a prefix, proving the bridge works.
#[tauri::command]
pub fn ping(message: String) -> String {
    format!("pong: {message}")
}

/// IDLE → RECORDING (design §9.4). Rejected unless currently IDLE.
#[tauri::command]
pub fn start_recording(coordinator: State<'_, Coordinator>) -> Result<(), String> {
    coordinator.start_recording()
}

/// RECORDING → PROCESSING → IDLE: stop capture, drain the queue, return to IDLE.
/// Resolves with the saved record's id (`None` if the consult was empty) so the
/// UI can load it for editing and note generation (design §6.6).
#[tauri::command]
pub fn stop_recording(coordinator: State<'_, Coordinator>) -> Result<Option<String>, String> {
    coordinator.stop_recording()
}

/// Pause capture within a recording (stays RECORDING; design §6.6/§9.4).
#[tauri::command]
pub fn pause_recording(coordinator: State<'_, Coordinator>) -> Result<(), String> {
    coordinator.pause_recording()
}

/// Resume a paused recording.
#[tauri::command]
pub fn resume_recording(coordinator: State<'_, Coordinator>) -> Result<(), String> {
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

/// Permanently delete a record and its notes (cascade via FK; NFR-9, §9.4).
#[tauri::command]
pub fn delete_record(store: State<'_, SharedStore>, id: String) -> Result<(), String> {
    store.lock().delete_record(&id).map_err(|e| e.to_string())
}
