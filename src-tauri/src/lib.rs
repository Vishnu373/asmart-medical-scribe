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
use settings::Settings;
use store::{SharedStore, Store};
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

            // Encrypted PHI store: the AES-256 key is wrapped by Windows DPAPI and
            // never persisted in the clear (design §10.1). Both the pipeline (which
            // saves a record on stop) and the records commands share this handle.
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let key = crypto::load_or_create_key(&data_dir.join("db.key"))
                .map_err(|e| e.to_string())?;
            let store = SharedStore::new(
                Store::open(&data_dir.join("clinical.db"), &key).map_err(|e| e.to_string())?,
            );

            // One-time model-residency decision (§7): probe total RAM, resolve the
            // co-resident-vs-swap mode (honoring any manual override), and cache it
            // to settings. Re-probing total RAM each launch only validates the
            // cache; the LLM hand-off (B10) consumes the cached mode.
            let settings_path = data_dir.join("settings.json");
            let mut app_settings = Settings::load(&settings_path).map_err(|e| e.to_string())?;
            let (mode, changed) = residency::resolve(&mut app_settings, residency::probe_total_ram());
            if changed {
                app_settings.save(&settings_path).map_err(|e| e.to_string())?;
            }
            log::info!("model residency mode: {}", mode.as_str());

            let handle = app.handle().clone();
            let pipeline = RealPipeline::new(
                handle.clone(),
                engine,
                vad_model_path,
                store.clone(),
                data_dir,
            );
            let coordinator = Coordinator::new(
                Box::new(pipeline),
                Box::new(move |event| emit_app_event(&handle, event)),
            );
            app.manage(coordinator);
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::start_recording,
            commands::stop_recording,
            commands::pause_recording,
            commands::resume_recording,
            commands::update_transcript,
            commands::list_records,
            commands::open_record,
            commands::delete_record,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
