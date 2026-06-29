//! Medical Scribe — Tauri 2 backend entry point.
//!
//! Modules are scaffolded empty in B1 and filled in per the implementation plan:
//! audio capture/VAD/STT are ported from the reference codebase (B3–B6); storage,
//! residency, note generation, hand-off and telemetry are built fresh.

mod audio_toolkit;
mod commands;
mod crypto;
mod handoff;
mod llm;
mod orchestrator;
mod residency;
mod segment;
mod settings;
mod store;
mod stt;
mod telemetry;

/// Builds and runs the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .invoke_handler(tauri::generate_handler![commands::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
