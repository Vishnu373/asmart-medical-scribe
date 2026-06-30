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

use llm::{LlmEngine, LlmModel, RealNoteGenerator};
use orchestrator::{emit_app_event, Coordinator, RealPipeline};
use residency::ResidencyMode;
use settings::Settings;
use store::{SharedStore, Store};
use stt::SttEngine;

/// How long the STT model sits unused before the idle-watcher unloads it
/// (design §6.4). Kept warm across back-to-back consults; released when the app
/// sits idle.
const STT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Builds and runs the Tauri application.
pub fn run() {
    // Crash reporting (§10.3) is compiled out by default (offline, NFR-6) and a
    // no-op unless the `crash-reporting` feature + a DSN are present.
    telemetry::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
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
            let total_ram = residency::probe_total_ram();
            let (mode, changed) = residency::resolve(&mut app_settings, total_ram);
            if changed {
                app_settings.save(&settings_path).map_err(|e| e.to_string())?;
            }
            log::info!("model residency mode: {}", mode.as_str());

            // In-process note-generation model (§8). The model is picked by the
            // same RAM probe (§8.2); residency mode decides when it loads — warmed
            // at startup when co-resident, loaded per generation when swapping.
            let llm_model = LlmModel::for_total_ram(total_ram);
            let models_dir = app.path().resource_dir()?.join("models");
            let n_threads = std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(4);
            let llm_engine =
                Arc::new(LlmEngine::new(llm_model, models_dir, n_threads).map_err(|e| e.to_string())?);
            let swap_mode = mode == ResidencyMode::Swap;
            if !swap_mode {
                // Co-resident: warm the model now so the first Generate is instant.
                // A failure here (e.g. model not yet bundled) is non-fatal — the
                // first generation retries and surfaces the error then.
                if let Err(e) = llm_engine.ensure_loaded() {
                    log::warn!("LLM preload failed (will retry on first generation): {e}");
                }
            }

            let handle = app.handle().clone();
            let pipeline = RealPipeline::new(
                handle.clone(),
                engine,
                vad_model_path,
                store.clone(),
                data_dir,
            );
            let generator =
                RealNoteGenerator::new(handle.clone(), llm_engine, store.clone(), swap_mode);
            let coordinator = Coordinator::new(
                Box::new(pipeline),
                Box::new(generator),
                Box::new(move |event| emit_app_event(&handle, event)),
            );
            // Managed as an `Arc` so the async `generate_note` command can move an
            // owned handle onto a blocking thread (keeping the IPC thread free for
            // `cancel_generation` during the multi-second generation; §8.4).
            app.manage(Arc::new(coordinator));
            app.manage(store);

            // EMR hand-off hotkey (§8.6): the rebindable paste key (default Alt+P)
            // emits `handoff-requested`; the overlay window (F7) shows the picker.
            // A registration failure (e.g. the combo is already taken) is non-fatal.
            if let Err(e) = handoff::register_paste_hotkey(app.handle(), &app_settings.paste_hotkey)
            {
                log::warn!(
                    "paste hotkey '{}' not registered: {e}",
                    app_settings.paste_hotkey
                );
            }

            // Settings (§9.3) managed for the `get_settings`/`update_settings`
            // commands (§9.4). Takes ownership of the (residency-resolved) settings
            // and their path so the Settings view (F6) can read and persist them.
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
            commands::update_settings,
            commands::list_input_devices,
            handoff::paste_section,
            handoff::rebind_paste_hotkey,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
