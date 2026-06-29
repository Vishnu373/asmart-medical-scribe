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

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use orchestrator::{emit_app_event, Coordinator, RealPipeline};
use stt::SttEngine;

/// How long the STT model sits unused before the idle-watcher unloads it
/// (design §6.4). Kept warm across back-to-back consults; released when the app
/// sits idle.
const STT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Builds and runs the Tauri application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .setup(|app| {
            // The STT model is long-lived (warm across recordings); the per-
            // recording capture/segment/worker threads are spun up by the
            // pipeline on each Start (design §6.6).
            let engine = Arc::new(SttEngine::new(STT_IDLE_TIMEOUT));

            // Bundled VAD model lives under the app's resource dir. (STT model
            // preload/asset bundling is wired in a later phase; until then a
            // recording surfaces an `error` event rather than transcribing.)
            let vad_model_path = app
                .path()
                .resource_dir()?
                .join("models")
                .join("silero_vad.onnx");

            let handle = app.handle().clone();
            let pipeline = RealPipeline::new(handle.clone(), engine, vad_model_path);
            let coordinator = Coordinator::new(
                Box::new(pipeline),
                Box::new(move |event| emit_app_event(&handle, event)),
            );
            app.manage(coordinator);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::start_recording,
            commands::stop_recording,
            commands::pause_recording,
            commands::resume_recording,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
