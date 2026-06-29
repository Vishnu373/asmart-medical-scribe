//! Tauri command handlers (frontend → backend). See design §9.4.
//!
//! B1 ships only `ping`, which proves the frontend↔backend bridge is wired.

/// Echoes a message back to the frontend with a prefix, proving the bridge works.
#[tauri::command]
pub fn ping(message: String) -> String {
    format!("pong: {message}")
}
