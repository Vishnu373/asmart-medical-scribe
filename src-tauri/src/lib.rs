//! ASmart Medical Scribe — Tauri 2 backend entry point.

mod audio_toolkit;
mod commands;
mod crypto;
mod handoff;
mod llm;
mod models;
mod orchestrator;
mod prime_kv;
mod segment;
mod settings;
mod store;
mod stt;
mod telemetry;
// Beta expiry removed — `trial.rs` stays on disk, uncompiled, in case the gate returns.
// mod trial;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;
use tauri_plugin_log::{Target, TargetKind};

use llm::{LlmEngine, LlmModel, RealNoteGenerator};
use orchestrator::{emit_app_event, Coordinator, RealPipeline};
use settings::Settings;
use store::{SharedStore, Store};
use stt::SttEngine;


/// `Duration::ZERO` disables the idle unload entirely for STT.
const STT_IDLE_TIMEOUT: Duration = Duration::ZERO;

/// Builds and runs the Tauri application.
pub fn run() {
    // Installer post-install step (§8.7): prime the prefix KV blob and exit without ever
    // building the app. Checked before anything else so no window or plugin is created.
    if prime_kv::requested() {
        prime_kv::run();
        return;
    }

    telemetry::init();
    telemetry::track_event("application_started", serde_json::json!({}));

    tauri::Builder::default()
        // logging plugin
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir {
                        file_name: Some("medscribe".into()),
                    }),
                ])
                .level(log::LevelFilter::Warn)
                .level_for(env!("CARGO_CRATE_NAME"), log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            log::info!(
                "[LAUNCH] application started — v{}, {}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS
            );

            let engine = Arc::new(SttEngine::new(STT_IDLE_TIMEOUT));

            // Bundling VAD model with STT
            let vad_model_path = app
                .path()
                .resource_dir()?
                .join("models")
                .join("silero_vad_v4.onnx");


            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            // DPAPI-wrapped DB key
            let key = crypto::load_or_create_key(&data_dir.join("db.key")).map_err(|e| {
                let msg = telemetry::sanitize_error(&e.to_string());
                log::error!("[DB] DPAPI key unwrap failed {msg}");
                telemetry::track_event("db_key_unwrap_failed", serde_json::json!({ "error": msg }));
                e.to_string()
            })?;

            // clinical.db - encrypted PHI database
            let store = SharedStore::new(
                Store::open(&data_dir.join("clinical.db"), &key).map_err(|e| e.to_string())?,
            );

            // settings.json - user settings
            let settings_path = data_dir.join("settings.json");
            let app_settings = Settings::load(&settings_path).map_err(|e| e.to_string())?;


            // deleting old models - applications running v.0.1.1
            if let Err(e) = models::cleanup_retired_weights(app.handle()) {
                log::warn!("retired-weight cleanup failed (non-fatal): {e}");
            }

            // Note-generation model: Gemma. Co-resident always warmed once at startup and kept resident for the life of the process.
            let llm_model = LlmModel::Gemma;
            let model_dirs = models::model_dirs(app.handle()).map_err(|e| e.to_string())?;

            // 1. LLM thread count - Option 1 -> using half of physical cores for both prefill and decode 
            // 2. Option 2 -> when physical cores unavailable, llama.cpp pick its own default. 
            let n_threads = sysinfo::System::new()
                .physical_core_count()
                .map(|physical| (physical / 2).max(1) as i32);

            // managed state for the LLM sharing -> model, location, thread count
            let llm_engine = Arc::new(
                LlmEngine::new(llm_model, model_dirs.clone(), n_threads)
                    .map_err(|e| e.to_string())?,
            );


            // managed state for the LLM sharing -> STT engine, location, llm engine
            app.manage(commands::PreloadGate::new(
                engine.clone(),
                model_dirs.clone(),
                llm_engine.clone(),
            ));

            // managed state for closing - unloads STT
            app.manage(engine.clone());

            // Assembling the recording pipeline (audio -> transcript)
            // 1. events to UI
            // 2. STT engine
            // 3. VAD model
            // 4. STT model folder location
            // 5. save the recording
            // 6. location of dumping transcript in case DB fails
            let handle = app.handle().clone();
            let pipeline = RealPipeline::new(
                handle.clone(),
                engine,
                vad_model_path,
                model_dirs,
                store.clone(),
                data_dir,
            );

            // managed state for closing - unloads LLM
            app.manage(llm_engine.clone());

            // note generation struct
            let generator = RealNoteGenerator::new(handle.clone(), llm_engine, store.clone());


            // Coordinator
            // 1. recording pipeline
            // 2. note generation struct
            // 3. state machine
            let coordinator = Coordinator::new(
                Box::new(pipeline),
                Box::new(generator),
                Box::new(move |event| emit_app_event(&handle, event)),
            );

            app.manage(Arc::new(coordinator));
            app.manage(store);

            // managed state for settings - 1. app settings, settings file location
            app.manage(settings::SharedSettings::new(app_settings, settings_path));
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
            commands::list_notes,
            commands::delete_record,
            commands::generate_note,
            commands::regenerate_note,
            commands::cancel_generation,
            commands::update_note,
            commands::revert_version,
            commands::get_settings,
            commands::get_llm_status,
            commands::frontend_ready,
            commands::update_settings,
            commands::list_input_devices,
            commands::submit_feedback,
            commands::mark_setup_completed,
            // commands::trial_status,
            commands::log_update_event,
            models::download_llm,
            models::setup_status,
            models::download_stt,
            handoff::paste_section,
            handoff::rebind_paste_hotkey,
            handoff::copy_to_clipboard,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // The OS reclaims process RAM regardless, so this is a graceful, explicit release. Both
        // engines are managed `Arc`s; both `unload()`s are idempotent.
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                handle.state::<Arc<SttEngine>>().unload();
                log::info!("[CLOSE] STT model unloaded");

                handle.state::<Arc<LlmEngine>>().unload();
                log::info!("[CLOSE] SLM model unloaded");
                
                log::info!("[CLOSE] application closed");
            }
        });
}
