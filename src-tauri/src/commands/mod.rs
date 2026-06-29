//! Tauri command handlers (frontend → backend). See design §9.4.
//!
//! Commands are *requests*: the backend coordinator owns the state and its
//! guards reject illegal transitions, returning an `Err(String)` the frontend
//! surfaces (design §6.6/§9.4).

use tauri::State;

use crate::orchestrator::Coordinator;

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
#[tauri::command]
pub fn stop_recording(coordinator: State<'_, Coordinator>) -> Result<(), String> {
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
