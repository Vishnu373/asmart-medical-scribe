//! ASmart Medical Scribe — Tauri 2 backend entry point.
//!
//! Modules are scaffolded empty in B1 and filled in per the implementation plan:
//! audio capture/VAD/STT are ported from the reference codebase (B3–B6); storage,
//! note generation, hand-off and telemetry are built fresh.

mod audio_toolkit;
mod commands;
mod crypto;
mod handoff;
mod llm;
mod models;
mod orchestrator;
mod segment;
mod settings;
mod store;
mod stt;
mod telemetry;
mod trial;

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;
use tauri_plugin_log::{Target, TargetKind};

use llm::{LlmEngine, LlmModel, RealNoteGenerator};
use orchestrator::{emit_app_event, Coordinator, RealPipeline};
use settings::Settings;
use store::{SharedStore, Store};
use stt::SttEngine;

/// How long the STT model sits unused before the idle-watcher unloads it
/// (design §6.4). Kept warm across back-to-back consults; released when the app
/// sits idle.
///
/// `Duration::ZERO` disables the idle unload entirely (see `stt::engine::idle_watch`,
/// which skips the unload check when the timeout is zero). Both models are now loaded
/// once at `frontend_ready` and stay resident for the life of the process: the ~12 GB
/// budget assumes a 16 GB floor, and paying a multi-second reload after five idle
/// minutes cost far more than the RAM it returned.
const STT_IDLE_TIMEOUT: Duration = Duration::ZERO;
// const STT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Builds and runs the Tauri application.
pub fn run() {
    // Crash reporting (§10.3) is compiled out by default (offline, NFR-6) and a
    // no-op unless the `crash-reporting` feature + a DSN are present.
    telemetry::init();
    telemetry::track_event("app_launched", serde_json::json!({}));

    tauri::Builder::default()
        // Allow-list: every crate is muted to Warn (genuine failures still surface),
        // only ours keeps Info. `CARGO_CRATE_NAME` is the [lib] name, so a rename
        // can't silently mute us. Covers `transcribe_rs`/ONNX and every other chatty
        // dependency. Native C++ stderr bypasses this — see `LlmEngine::new`.
        .plugin(
            tauri_plugin_log::Builder::new()
                // Persist the on-device log (§10.3) to a named file in the app log
                // dir so the §10.3 catalog lines survive across runs, alongside the
                // dev-time stdout stream. Set explicitly (rather than relying on the
                // plugin default) so the filename is deterministic for support.
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
        // .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            log::info!("[startup] application starting");
            // The STT model is long-lived (warm across recordings); the per-
            // recording capture/segment/worker threads are spun up by the
            // pipeline on each Start (design §6.6).
            let engine = Arc::new(SttEngine::new(STT_IDLE_TIMEOUT));

            // Bundled VAD model lives under the app's resource dir. (The STT
            // model is resolved across the D1 model dirs and loaded by the
            // pipeline on Start — see `RealPipeline::start`.)
            let vad_model_path = app
                .path()
                .resource_dir()?
                .join("models")
                .join("silero_vad_v4.onnx");

            // Encrypted PHI store: the AES-256 key is wrapped by Windows DPAPI and
            // never persisted in the clear (design §10.1). Both the pipeline (which
            // saves a record on stop) and the records commands share this handle.
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let key =
                crypto::load_or_create_key(&data_dir.join("db.key")).map_err(|e| e.to_string())?;
            let store = SharedStore::new(
                Store::open(&data_dir.join("clinical.db"), &key).map_err(|e| e.to_string())?,
            );

            // Config-only settings (§9.3): mic/VAD/idle. Loaded here so the Settings
            // view (F6) can read and persist them; also owned by `SharedSettings` below.
            let settings_path = data_dir.join("settings.json");
            let app_settings = Settings::load(&settings_path).map_err(|e| e.to_string())?;

            // One-time upgrade migration (§4d): drop the retired multi-tier LLM GGUFs
            // (Mistral/Phi) a pre-§3 install left in the download dir, so an upgraded
            // device isn't stuck carrying ~7–11 GB of dead weights. Best-effort.
            if let Err(e) = models::cleanup_retired_weights(app.handle()) {
                log::warn!("retired-weight cleanup failed (non-fatal): {e}");
            }

            // In-process note-generation model (§8). One model now — Gemma (§3
            // single-model refactor). Co-resident always (§7): warmed once at startup
            // and kept resident for the life of the process. The model file is resolved
            // across the download dir then the bundled resource dir (D1); the installer
            // bundles no LLM, so it comes from the download dir.
            let llm_model = LlmModel::Gemma;
            let model_dirs = models::model_dirs(app.handle()).map_err(|e| e.to_string())?;
            // LLM thread count (design §8.2): token-by-token decode is memory-bandwidth-
            // bound and stops scaling — often regresses — past a fraction of the cores,
            // so use physical // 2. The same count caps prefill (see `engine::new_context`)
            // so it can't fall to the llama.cpp default of 4. When the physical count is
            // unavailable, estimate it as half the logical count (assume 2-way SMT) so the
            // fallback still targets physical // 2 rather than 2× it.
            let n_threads = sysinfo::System::new()
                .physical_core_count()
                .or_else(|| {
                    std::thread::available_parallelism()
                        .map(|n| n.get() / 2)
                        .ok()
                })
                .map(|physical| (physical / 2).max(1) as i32)
                .unwrap_or(1);
            let llm_engine = Arc::new(
                LlmEngine::new(llm_model, model_dirs.clone(), n_threads)
                    .map_err(|e| e.to_string())?,
            );
            // Co-resident wants a warm model so the first Generate is instant, but the
            // multi-GB GGUF load + warmup is seconds long, and starting it here — in
            // `setup`, before the webview paints — starved WebView2's first paint and
            // froze the window ("not responding") on launch, even on a background
            // thread. So the warm is deferred: the gate holds the engine until the
            // frontend reports it has fully mounted (`frontend_ready`), then warms once
            // off the main thread. The UI shows a "preparing" status until the
            // `llm-status` event flips to `ready`. (design §8.2, startup fix.)
            // STT is preloaded here too, and *before* the LLM: it is the smaller model
            // and the one the clinician needs the instant they press Record. It used to
            // load lazily inside `RealPipeline::start`, which put its multi-second load
            // on the critical path of the first Start of every session.
            app.manage(commands::PreloadGate::new(
                engine.clone(),
                model_dirs.clone(),
                llm_engine.clone(),
            ));

            // Managed so the app-exit hook (below) can unload STT on shutdown
            // (Phase 2, §7). The pipeline takes ownership of the original `engine`
            // Arc on the next line, so the exit hook resolves this clone from state.
            app.manage(engine.clone());

            let handle = app.handle().clone();
            let pipeline = RealPipeline::new(
                handle.clone(),
                engine,
                vad_model_path,
                model_dirs,
                store.clone(),
                data_dir,
            );
            // Managed so the `get_llm_status` command can report load readiness (§8.2
            // startup fix): it and the generator share the same `Arc<LlmEngine>`.
            app.manage(llm_engine.clone());
            let generator = RealNoteGenerator::new(handle.clone(), llm_engine, store.clone());
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

            // EMR hand-off (§8.6): the Alt+P global hotkey + no-activate picker
            // overlay are deferred. v1 hand-off is manual — the clinician copies a
            // SOAP section (per-section Copy button → `copy_to_clipboard`) and
            // pastes it into the EMR with Ctrl+V. The `register_paste_hotkey` /
            // `rebind_paste_hotkey` / `paste_section` machinery (B11) stays in place,
            // dormant, for when the overlay is built. So no global shortcut is
            // registered at startup.

            // Settings (§9.3) managed for the `get_settings`/`update_settings`
            // commands (§9.4). Takes ownership of the settings and their path so the
            // Settings view (F6) can read and persist them.
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
            commands::trial_status,
            models::download_llm,
            models::setup_status,
            models::download_stt,
            handoff::paste_section,
            handoff::rebind_paste_hotkey,
            handoff::copy_to_clipboard,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // Both models stay resident for the whole session and are unloaded only
        // here, on the final (non-cancellable) exit (Phase 2, §7). The OS reclaims
        // process RAM regardless, so this is a graceful, explicit release. Both
        // engines are managed `Arc`s; both `unload()`s are idempotent.
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                handle.state::<Arc<SttEngine>>().unload();
                handle.state::<Arc<LlmEngine>>().unload();
                log::info!("[LAUNCH] application closed");
            }
        });
}
