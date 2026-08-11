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

/// Every thread count in the app, split from one physical-core probe (§8.2).
struct ThreadSplit {
    /// ORT intra/inter-op pool for STT.
    stt: usize,
    /// llama.cpp single-token decode. The same share as STT, which is idle at
    /// Generate time — but a separate field, so the STT debug override cannot
    /// silently retune the LLM.
    decode: usize,
    /// llama.cpp batch threads, prefilling transcript segments during the
    /// recording (§8.9) — the cores STT does not take.
    prefill: usize,
}

/// The thread split: half the physical cores to STT and decode, the rest to LLM
/// prefill. The *core count* is probed once and cached; the split itself is derived
/// every launch, so changing it here reaches machines that have already probed.
/// `None` = core count unavailable, so every consumer keeps its own default — a
/// machine we cannot measure gets the stock behaviour, not a guess.
/// See docs/implementation-stt-thread-management.md.
fn thread_split(settings: &mut Settings) -> Option<ThreadSplit> {
    // Debug knob, read before the probe so it still works on a machine we cannot
    // measure. Overrides the STT share only — never cached, never seen by the LLM.
    let stt_override = std::env::var("STT_THREAD_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0);

    let cached = settings.physical_cores;
    let physical = match cached {
        Some(n) => n,
        None => {
            let n = sysinfo::System::new().physical_core_count()?;
            settings.physical_cores = Some(n);
            n
        }
    };

    // Integer division floors, by decision: 7 cores → 3 for STT/decode, 4 for prefill.
    // Derived here rather than cached — the cache holds cores, not threads (§3.1).
    let decode = (physical / 2).max(1);
    let prefill = (physical - decode).max(1);
    let stt = stt_override.unwrap_or(decode);

    log::info!(
        "[THREADS] {physical} physical cores {}",
        if cached.is_some() { "(cached)" } else { "detected" }
    );
    if stt_override.is_some() {
        log::info!("[STT] thread allocation override: {stt} threads from STT_THREAD_COUNT");
    } else {
        log::info!("[STT] thread allocated: {stt} threads");
    }
    log::info!("[LLM] thread allocated: {decode} decode, {prefill} prefill");

    Some(ThreadSplit {
        stt,
        decode,
        prefill,
    })
}

/// Size the ORT global thread pool. With a global pool set, `DisablePerSessionThreads`
/// makes every ORT session — Parakeet's three and Silero's — draw from it instead of
/// sizing its own to the physical core count.
// Phase 1 (experiment): the count came from `ASMART_STT_THREADS` /
// `ASMART_STT_INTER_THREADS`, and an unset var meant no pool at all. Both are now
// resolved by `thread_split` above.
// fn init_ort_thread_pool() {
//     let intra = std::env::var("ASMART_STT_THREADS")
//         .ok()
//         .and_then(|v| v.parse::<usize>().ok())
//         .filter(|n| *n > 0);
//     let Some(intra) = intra else { return };
//
//     let inter = std::env::var("ASMART_STT_INTER_THREADS")
//         .ok()
//         .and_then(|v| v.parse::<usize>().ok())
//         .filter(|n| *n > 0)
//         .unwrap_or(intra);
fn init_ort_thread_pool(intra: usize) {
    // Pinned to the intra count; `transcribe-rs` enables parallel execution, so leaving
    // the inter-op pool uncapped would leak past the limit we just set.
    let inter = intra;

    let opts = match ort::environment::GlobalThreadPoolOptions::default()
        .with_intra_threads(intra)
        .and_then(|o| o.with_inter_threads(inter))
    {
        Ok(o) => o,
        Err(e) => {
            log::warn!("[STT] ORT thread pool config failed: {e}");
            return;
        }
    };

    // A `false` return means an ORT environment already existed and ours was ignored.
    // let committed = ort::init().with_global_thread_pool(opts).commit();
    // log::info!("[STT] ORT global pool intra={intra} inter={inter} committed={committed}");
    let _ = ort::init().with_global_thread_pool(opts).commit();
}

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
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // Phase 1: the pool was sized here, first thing in setup. It now depends on
            // settings, so the call moved below `Settings::load`.
            // init_ort_thread_pool();

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
            let mut app_settings = Settings::load(&settings_path).map_err(|e| e.to_string())?;

            // Must precede any ORT session (STT preload, SileroVad) — the environment is
            // first-write-wins. May write the thread counts on first run, hence after the
            // load and before the save.
            let probed_before = app_settings.physical_cores;
            let threads = thread_split(&mut app_settings);
            if let Some(t) = &threads {
                init_ort_thread_pool(t.stt);
            }
            // `SharedSettings::new` only wraps the value — it never writes. Without this the
            // probed count stays in memory and every launch re-probes.
            //
            // Only the first run mutates the count, and only that one key is written back:
            // a whole-struct save on every launch would overwrite a settings.json that
            // `load` could not parse, taking mic_device and the gpu cache with it.
            // if let Err(e) = app_settings.save(&settings_path) {
            //     log::warn!("[STT] settings save failed (non-fatal): {e}");
            // }
            if app_settings.physical_cores != probed_before {
                if let Err(e) = app_settings.patch_physical_cores(&settings_path) {
                    log::warn!("[THREADS] settings save failed (non-fatal): {e}");
                }
            }

            // deleting old models - applications running v.0.1.1
            if let Err(e) = models::cleanup_retired_weights(app.handle()) {
                log::warn!("retired-weight cleanup failed (non-fatal): {e}");
            }

            // Note-generation model: Gemma. Co-resident always warmed once at startup and kept resident for the life of the process.
            let llm_model = LlmModel::Gemma;
            let model_dirs = models::model_dirs(app.handle()).map_err(|e| e.to_string())?;

            // `None` on both when the core count is unavailable, leaving llama.cpp its
            // own defaults.
            let n_threads = threads.as_ref().map(|t| t.decode as i32);
            let n_threads_batch = threads.as_ref().map(|t| t.prefill as i32);

            // managed state for the LLM sharing -> model, location, thread count
            let llm_engine = Arc::new(
                LlmEngine::new(llm_model, model_dirs.clone(), n_threads, n_threads_batch)
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
            // 7. LLM engine — segments are pushed to its prefill session as they land
            let pipeline = RealPipeline::new(
                handle.clone(),
                engine,
                vad_model_path,
                model_dirs,
                store.clone(),
                data_dir,
                llm_engine.clone(),
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
