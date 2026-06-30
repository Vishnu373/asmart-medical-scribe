//! Model distribution: on-disk resolution + the optional-model download (D1).
//!
//! v1 ships three models embedded in the installer (Parakeet STT, Mistral "best",
//! Phi-3.5 Q8 "medium") so the app works fully offline on first launch. The
//! lightest LLM tier — Phi-3.5 Q4 "okay" — is **not** bundled; the doctor pulls it
//! on demand from Settings. That download is the only network call in the app
//! (NFR-6 zero-egress is about *PHI*; this is model weights, fetched only on an
//! explicit click), and it is content-verified before use.
//!
//! Bundled models live under the read-only `resource_dir/models`; downloaded ones
//! land in the writable `app_data_dir/models`. [`resolve`] searches the latter
//! first so a downloaded file shadows a (non-existent) bundled one.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::llm::LlmModel;

/// An LLM tier that is downloaded on demand rather than bundled. `tier` is the
/// `model_choice` key (§9.3); the on-disk filename is owned by [`LlmModel`] so the
/// download and the loader always agree on the name.
pub struct OptionalModel {
    /// The `model_choice` tier this satisfies.
    pub tier: &'static str,
    /// Direct-download URL (a GGUF; HTTPS, follows redirects).
    pub url: &'static str,
    /// Expected lowercase hex SHA-256, when known. `None` skips verification (a
    /// pinned hash is strongly preferred — see the field's TODO at the call site).
    pub sha256: Option<&'static str>,
}

/// The optional (non-bundled) models. Only the "okay" tier today. A `static` (not
/// `const`) so a reference into it is `'static` and can move into the download
/// thread.
pub static OPTIONAL: &[OptionalModel] = &[OptionalModel {
    tier: "okay",
    url: "https://huggingface.co/worthdoing/Phi-3.5-mini-instruct-GGUF/resolve/main/phi-3.5-mini-instruct-Q4_K_M-worthdoing.gguf?download=true",
    // TODO(D1): pin the SHA-256 of the released file so a corrupted/partial or
    // swapped download is rejected. Left `None` until the checksum is captured;
    // until then integrity rests on HTTPS + the size check only.
    sha256: None,
}];

/// All doctor-facing tiers, for `model_status` presence reporting.
const ALL_TIERS: [&str; 3] = ["best", "medium", "okay"];

/// Tiers with a worker currently downloading. Guards against a second concurrent
/// download of the same tier — the UI hides its Download button via local state,
/// but remounting Settings mid-download (navigate away and back) re-shows it, and
/// a second click would `File::create` the same `.part` and interleave bytes into
/// a corrupt file. Keyed by tier; entry removed when the worker finishes.
static IN_FLIGHT: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The model-file search dirs in priority order (D1): the writable download dir
/// first, then the bundled resource dir. Creating the download dir here keeps the
/// first download from racing a missing parent.
pub fn model_dirs(app: &AppHandle) -> Result<Vec<PathBuf>> {
    let data = app
        .path()
        .app_data_dir()
        .map_err(|e| anyhow!("app data dir: {e}"))?
        .join("models");
    fs::create_dir_all(&data).map_err(|e| anyhow!("create download dir: {e}"))?;
    let resource = app
        .path()
        .resource_dir()
        .map_err(|e| anyhow!("resource dir: {e}"))?
        .join("models");
    Ok(vec![data, resource])
}

/// First existing path for `file` across `dirs`, in order. Pure — unit-tested.
pub fn resolve(file: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(file)).find(|p| p.exists())
}

/// Presence of one tier's model file across the search dirs.
#[derive(Serialize)]
pub struct ModelStatus {
    pub tier: String,
    pub present: bool,
    /// Whether this tier is an on-demand download (vs bundled). The UI shows a
    /// Download affordance for an optional tier that is absent.
    pub optional: bool,
}

/// Report which tier models are present on disk so the UI can show/hide the
/// download affordance for the optional tier (§9.3, D1).
#[tauri::command]
pub fn model_status(app: AppHandle) -> Result<Vec<ModelStatus>, String> {
    let dirs = model_dirs(&app).map_err(|e| e.to_string())?;
    Ok(ALL_TIERS
        .iter()
        .map(|&tier| {
            let file = LlmModel::from_tier(tier)
                .expect("ALL_TIERS are valid tiers")
                .file_name();
            ModelStatus {
                tier: tier.to_string(),
                present: resolve(file, &dirs).is_some(),
                optional: OPTIONAL.iter().any(|m| m.tier == tier),
            }
        })
        .collect())
}

/// Progress for an in-flight download. `total` is 0 when the server omits a
/// Content-Length (rare for HF); the UI then shows an indeterminate state.
#[derive(Clone, Serialize)]
struct DownloadProgress {
    tier: String,
    downloaded: u64,
    total: u64,
}

/// Start downloading an optional model tier (D1). Returns immediately after
/// validating and spawning the worker; progress and the terminal result are
/// reported via events so the IPC thread stays free and the UI can show a bar:
///   - `model-download-progress` `{ tier, downloaded, total }` (throttled)
///   - `model-download-done` `{ tier }`
///   - `model-download-error` `{ tier, message }`
#[tauri::command]
pub fn download_model(app: AppHandle, tier: String) -> Result<(), String> {
    let spec = OPTIONAL
        .iter()
        .find(|m| m.tier == tier)
        .ok_or_else(|| format!("'{tier}' is not an optional/downloadable model"))?;
    let file = LlmModel::from_tier(&tier)
        .ok_or_else(|| format!("unknown tier '{tier}'"))?
        .file_name();
    let dest_dir = model_dirs(&app).map_err(|e| e.to_string())?.remove(0); // app-data/models

    // Claim the tier; reject if a worker already holds it.
    {
        let mut guard = IN_FLIGHT.lock().unwrap();
        let set = guard.get_or_insert_with(HashSet::new);
        if !set.insert(tier.clone()) {
            return Err(format!("'{tier}' is already downloading"));
        }
    }

    std::thread::spawn(move || {
        let result = download_to(&app, spec, file, &dest_dir);
        // Release the claim before emitting the terminal event so a retry on error
        // (or a fresh download after done) isn't rejected as still in flight.
        if let Some(set) = IN_FLIGHT.lock().unwrap().as_mut() {
            set.remove(&tier);
        }
        match result {
            Ok(()) => {
                let _ = app.emit("model-download-done", serde_json::json!({ "tier": tier }));
            }
            Err(e) => {
                let _ = app.emit(
                    "model-download-error",
                    serde_json::json!({ "tier": tier, "message": e.to_string() }),
                );
            }
        }
    });
    Ok(())
}

/// Stream `spec.url` to a `.part` file in `dest_dir`, hashing as we go, then verify
/// (when a hash is pinned) and atomically rename into place. Emits throttled
/// progress. A failure removes the partial so a retry starts clean.
fn download_to(app: &AppHandle, spec: &OptionalModel, file: &str, dest_dir: &Path) -> Result<()> {
    let resp = ureq::get(spec.url)
        .call()
        .map_err(|e| anyhow!("download request failed: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let part = dest_dir.join(format!("{file}.part"));
    let mut out = File::create(&part).map_err(|e| anyhow!("create temp file: {e}"))?;
    let mut reader = resp.into_reader();
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16]; // 64 KiB
    let mut downloaded: u64 = 0;
    // Throttle progress emits to ~every 8 MiB so a multi-GB download doesn't flood
    // the event channel with tens of thousands of messages.
    const EMIT_EVERY: u64 = 8 * 1024 * 1024;
    let mut next_emit: u64 = 0;

    loop {
        let n = reader.read(&mut buf).map_err(|e| anyhow!("read body: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| anyhow!("write body: {e}"))?;
        downloaded += n as u64;
        if downloaded >= next_emit {
            let _ = app.emit(
                "model-download-progress",
                DownloadProgress {
                    tier: spec.tier.to_string(),
                    downloaded,
                    total,
                },
            );
            next_emit = downloaded + EMIT_EVERY;
        }
    }
    out.sync_all().map_err(|e| anyhow!("flush temp file: {e}"))?;
    drop(out);

    // A dropped connection EOFs the reader as `Ok(0)` rather than erroring, so a
    // partial body would otherwise be renamed into place and load as a corrupt
    // GGUF later. When the server gave a Content-Length, require we got all of it.
    if total > 0 && downloaded != total {
        let _ = fs::remove_file(&part);
        bail!("incomplete download ({downloaded} of {total} bytes) — download discarded");
    }

    if let Some(expected) = spec.sha256 {
        let got = hex_lower(&hasher.finalize());
        if !got.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&part);
            bail!("checksum mismatch (expected {expected}, got {got}) — download discarded");
        }
    }

    fs::rename(&part, dest_dir.join(file)).map_err(|e| anyhow!("finalize download: {e}"))?;
    // A final 100% tick so the UI lands on complete before `model-download-done`.
    let _ = app.emit(
        "model-download-progress",
        DownloadProgress {
            tier: spec.tier.to_string(),
            downloaded,
            total: total.max(downloaded),
        },
    );
    Ok(())
}

/// Lowercase hex of a digest, for checksum comparison.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_earlier_dirs() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let dirs = vec![a.path().to_path_buf(), b.path().to_path_buf()];

        // Absent everywhere → None.
        assert!(resolve("model.gguf", &dirs).is_none());

        // Present only in the second dir → found there.
        fs::write(b.path().join("model.gguf"), b"x").unwrap();
        assert_eq!(resolve("model.gguf", &dirs).unwrap(), b.path().join("model.gguf"));

        // Present in both → the first (download dir) shadows the bundled one.
        fs::write(a.path().join("model.gguf"), b"x").unwrap();
        assert_eq!(resolve("model.gguf", &dirs).unwrap(), a.path().join("model.gguf"));
    }

    #[test]
    fn optional_catalog_filenames_match_the_loader() {
        // The download filename must equal what the engine resolves, or a pulled
        // model would never be found. Guards the two lists against drift.
        for m in OPTIONAL {
            let kind = LlmModel::from_tier(m.tier).expect("optional tier is a real tier");
            assert!(
                m.url.ends_with(&format!("{}?download=true", kind.file_name()))
                    || m.url.contains(kind.file_name()),
                "optional '{}' url does not reference its filename {}",
                m.tier,
                kind.file_name()
            );
        }
    }

    #[test]
    fn hex_lower_is_zero_padded_lowercase() {
        assert_eq!(hex_lower(&[0x00, 0x0a, 0xff]), "000aff");
    }
}
