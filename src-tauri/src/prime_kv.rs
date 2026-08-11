//! Headless `--prime-kv` mode (design §8.7, §14.3): prefill the fixed prompt prefix and
//! write the KV blob to disk, then exit. Run by the installer after an update that changed
//! llama.cpp, so the ~22s prime is paid there instead of in the doctor's first session.
//!
//! No window, no Tauri app — the app-data paths come straight from the environment. Every
//! failure returns normally: a failed prime must never fail an install, because the app
//! still primes at launch (§8.4) if the blob is missing.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use log::{info, warn};

use crate::llm::{LlmEngine, LlmModel};

/// The bundle identifier, also the app-data folder name. Hardcoded because there is no
/// `AppHandle` here — resolving it properly is exactly the Tauri boot this path skips.
///
/// **Three hand-written copies exist and nothing cross-checks them:** `tauri.conf.json`
/// (authoritative), `installer-hooks.nsh`, and this const. Change all three together — a
/// stale copy here makes the prime look in a directory that doesn't exist and log the
/// `no model … — skipping` line, which is indistinguishable from a healthy fresh install.
/// The optimization would be dead with nothing in the log to say so.
const IDENTIFIER: &str = "com.asmartmedicalscribe.app";

/// True when the process was started as `--prime-kv`.
pub fn requested() -> bool {
    std::env::args().any(|a| a == "--prime-kv")
}

/// Prime the prefix KV cache and return. Always returns — the caller exits 0.
pub fn run() {
    init_logging();

    let Some(models_dir) = models_dir() else {
        warn!("[PRIME] APPDATA unset — skipping");
        return;
    };
    let kind = LlmModel::Gemma;
    if !models_dir.join(kind.file_name()).exists() {
        // Fresh install: the models aren't downloaded yet, so Setup owns the prime (§8.2).
        info!("[PRIME] no model in {} — skipping", models_dir.display());
        return;
    }

    // Same half-physical-cores budget as `lib.rs` (§8.2). Duplicated rather than shared: drift
    // only changes how fast this prime runs, never the bytes it writes.
    let n_threads = sysinfo::System::new()
        .physical_core_count()
        .map(|physical| (physical / 2).max(1) as i32);
    // No prefill here: this is a headless one-shot prime, there is no recording.
    let engine = match LlmEngine::new(kind, vec![models_dir], n_threads, None) {
        Ok(e) => e,
        Err(e) => {
            warn!("[PRIME] engine init failed: {e}");
            return;
        }
    };

    // Cheapest exit: an update that didn't change the prompt or llama.cpp keeps the same
    // blob name, so there is nothing to do and the installer waits milliseconds.
    if engine.prefix_kv_path().is_some_and(|p| p.exists()) {
        info!("[PRIME] prefix KV blob already present — nothing to do");
        return;
    }

    // `ensure_loaded` primes and writes the blob; the RAM is released when this process exits.
    // Its `Ok` is not proof of a blob: a warmup or blob-write failure is non-fatal there and
    // still returns `Ok`. Without a console this log is the only signal the prime produced
    // anything, so confirm the file rather than trust the return.
    // match engine.ensure_loaded() {
    //     Ok(()) => info!("[PRIME] done"),
    match engine.ensure_loaded() {
        Ok(()) if engine.prefix_kv_path().is_some_and(|p| p.exists()) => info!("[PRIME] done"),
        Ok(()) => warn!("[PRIME] primed but no blob on disk — the app will prime again at launch"),
        Err(e) => warn!("[PRIME] failed: {e}"),
    }
}

/// `%APPDATA%\<identifier>\models` — the writable models dir `models::model_dirs` resolves
/// first. Only that dir is searched: the blob is written there, and the bundled resource dir
/// isn't writable anyway.
fn models_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(IDENTIFIER).join("models"))
}

/// Append this run to `medscribe.log`, next to what the app itself writes. `windows_subsystem
/// = "windows"` means there is no console, so the file is the only output. Best-effort: no log
/// file is not a reason to skip the prime.
fn init_logging() {
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };
    let dir = PathBuf::from(local).join(IDENTIFIER).join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("medscribe.log"))
    else {
        return;
    };
    if log::set_boxed_logger(Box::new(FileLogger(Mutex::new(file)))).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Minimal `log` sink for the headless run — `tauri_plugin_log` needs a Tauri app, and this
/// only has to get the engine's `[LOAD]` lines into the file.
struct FileLogger(Mutex<std::fs::File>);

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if let Ok(mut file) = self.0.lock() {
            let _ = writeln!(file, "[{}][prime-kv] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.0.lock() {
            let _ = file.flush();
        }
    }
}
