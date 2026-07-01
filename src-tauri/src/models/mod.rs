//! Model distribution: on-disk resolution + model downloads (D1, D3).
//!
//! The installer ships **no** model weights (D3 lean installer). On first launch a
//! one-time Setup downloads the models the app requires — the RAM-fit default LLM
//! (Mistral "best" on ≥16 GB, Phi-3.5 Q8 "medium" on <16 GB; design §8.2) and the
//! Parakeet STT model — and the app is gated until they are present ([`setup_status`]).
//! Beyond that required set, all three LLM tiers — Mistral "best", Phi Q8 "medium",
//! Phi Q4 "okay" — are downloadable on demand from Settings so a doctor can pull a
//! different one. These downloads are the only network calls in the app (NFR-6
//! zero-egress is about *PHI*; this is model weights), and each is content-verified
//! before use. The LLMs are single GGUFs ([`download_model`]); Parakeet is a gzipped
//! tar of a *directory*, so it is verified then extracted ([`download_stt`]).
//!
//! Downloaded models land in the writable `app_data_dir/models`; [`resolve`] also
//! searches the read-only `resource_dir/models` after it, so a model bundled by a
//! future build would still be found.

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

/// The downloadable LLM tiers: all three — Mistral "best", and both Phi-3.5
/// quantizations ("medium" Q8, "okay" Q4). On a given build one may already be
/// bundled (the RAM-fit default), in which case `model_status` reports it present
/// and the UI never offers its download. A `static` (not `const`) so a reference
/// into it is `'static` and can move into the download thread.
pub static OPTIONAL: &[OptionalModel] = &[
    OptionalModel {
        tier: "best",
        url: "https://huggingface.co/bartowski/Mistral-7B-Instruct-v0.3-GGUF/resolve/main/Mistral-7B-Instruct-v0.3-Q4_K_M.gguf?download=true",
        // TODO(D1): pin the SHA-256 of the released file (see the "okay" note below).
        sha256: None,
    },
    OptionalModel {
        tier: "medium",
        url: "https://huggingface.co/worthdoing/Phi-3.5-mini-instruct-GGUF/resolve/main/phi-3.5-mini-instruct-Q8_0-worthdoing.gguf?download=true",
        // TODO(D1): pin the SHA-256 of the released file (see the "okay" note below).
        sha256: None,
    },
    OptionalModel {
        tier: "okay",
        url: "https://huggingface.co/worthdoing/Phi-3.5-mini-instruct-GGUF/resolve/main/phi-3.5-mini-instruct-Q4_K_M-worthdoing.gguf?download=true",
        // TODO(D1): pin the SHA-256 of the released file so a corrupted/partial or
        // swapped download is rejected. Left `None` until the checksum is captured;
        // until then integrity rests on HTTPS + the size check only.
        sha256: None,
    },
];

/// All doctor-facing tiers, for `model_status` presence reporting.
const ALL_TIERS: [&str; 3] = ["best", "medium", "okay"];

/// The Parakeet STT model download (D3). Unlike the LLM GGUFs this is a gzipped
/// tar of a *directory* of ONNX files, so the transfer is verified then extracted
/// (see [`download_stt`]). It is required for the app to function at all, so the
/// first-run setup pulls it before releasing the app (it is not an LLM tier —
/// there is no per-tier `model_choice` for STT). `tier` here is the event key the
/// download progress/done/error events carry, matching the LLM ones' `tier`.
pub struct SttDownload {
    /// The event key for this download's `model-download-*` events.
    pub tier: &'static str,
    /// tar.gz source URL. ⚠ Third-party host we do not control — rehost on our own
    /// storage and swap this before release (D3 open item).
    pub url: &'static str,
    pub sha256: Option<&'static str>,
    /// The directory name the extracted files must land under — the loader
    /// resolves [`ModelKind::dir_name`]. Kept in sync by a unit test.
    pub dir_name: &'static str,
}

/// The Parakeet int8 archive. The event key `"stt"` is distinct from every LLM
/// tier so the frontend can key its progress map by it.
pub static STT: SttDownload = SttDownload {
    tier: "stt",
    url: "https://blob.handy.computer/parakeet-v3-int8.tar.gz",
    sha256: Some("43d37191602727524a7d8c6da0eef11c4ba24320f5b4730f1a2497befc2efa77"),
    dir_name: "parakeet-tdt-0.6b-v3",
};

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

/// Whether the models the app *requires* to run are on disk (D3 first-run gate):
/// the RAM-fit default LLM and the Parakeet STT model. The frontend shows the
/// one-time Setup screen until `ready`.
#[derive(Serialize)]
pub struct SetupStatus {
    /// The RAM-fit LLM tier this machine needs (`best` on ≥16 GB, `medium` on
    /// <16 GB) — the tier the Setup screen downloads.
    pub llm_tier: String,
    pub llm_present: bool,
    pub stt_present: bool,
    /// Both required models present — the app can start.
    pub ready: bool,
}

/// Report whether the required models (RAM-fit LLM + Parakeet STT) are present so
/// the frontend can gate the app on first run (D3). The LLM tier is the §8.2
/// fit-to-machine default, independent of any `model_choice` the doctor later sets.
#[tauri::command]
pub fn setup_status(app: AppHandle) -> Result<SetupStatus, String> {
    let dirs = model_dirs(&app).map_err(|e| e.to_string())?;
    let llm = LlmModel::for_total_ram(crate::residency::probe_total_ram());
    let llm_present = resolve(llm.file_name(), &dirs).is_some();
    let stt_present = resolve(STT.dir_name, &dirs).is_some();
    Ok(SetupStatus {
        llm_tier: llm.tier().to_string(),
        llm_present,
        stt_present,
        ready: llm_present && stt_present,
    })
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

/// Start downloading the Parakeet STT model (D3). Mirrors [`download_model`] —
/// returns once the worker is spawned; progress and the terminal result arrive as
/// the same `model-download-*` events, keyed by `STT.tier` (`"stt"`). Unlike an LLM
/// GGUF the archive is a tar.gz of a directory, so the worker verifies then
/// *extracts* it (see [`download_stt_to`]).
#[tauri::command]
pub fn download_stt(app: AppHandle) -> Result<(), String> {
    let dest_dir = model_dirs(&app).map_err(|e| e.to_string())?.remove(0); // app-data/models
    let tier = STT.tier.to_string();

    // Claim the download; reject a concurrent one (same guard as the LLM tiers).
    {
        let mut guard = IN_FLIGHT.lock().unwrap();
        let set = guard.get_or_insert_with(HashSet::new);
        if !set.insert(tier.clone()) {
            return Err("the speech model is already downloading".to_string());
        }
    }

    std::thread::spawn(move || {
        let result = download_stt_to(&app, &dest_dir);
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

/// Stream the Parakeet tarball to a `.part` in `dest_dir`, verify it, extract the
/// ONNX files into `dest_dir/<dir_name>`, then remove the archive (D3). A failure
/// discards the partial so a retry starts clean.
fn download_stt_to(app: &AppHandle, dest_dir: &Path) -> Result<()> {
    let part = dest_dir.join("parakeet-v3-int8.tar.gz.part");
    let downloaded = stream_verified(app, STT.tier, STT.url, STT.sha256, &part)?;
    let extract = extract_model_dir(&part, dest_dir, STT.dir_name);
    // The verified archive is large; drop it whether extraction succeeded or not
    // (on failure the caller retries the whole download).
    let _ = fs::remove_file(&part);
    extract?;
    emit_complete(app, STT.tier, downloaded);
    Ok(())
}

/// Extract a gzipped tar at `archive` into `dest_dir/<dir_name>` (D3). The tarball
/// may wrap the model files in a top-level folder whose name differs from what the
/// loader resolves (the archive uses `parakeet-tdt-0.6b-v3-int8`; the loader wants
/// `ModelKind::dir_name` = `parakeet-tdt-0.6b-v3`). We unpack to a staging dir,
/// pick the real model root (a single wrapping subdir, else the staging dir
/// itself), and rename it into place — so the files always land under `dir_name`.
fn extract_model_dir(archive: &Path, dest_dir: &Path, dir_name: &str) -> Result<()> {
    let staging = dest_dir.join(format!(".{dir_name}.staging"));
    let _ = fs::remove_dir_all(&staging); // clear any interrupted prior extract
    fs::create_dir_all(&staging).map_err(|e| anyhow!("create staging dir: {e}"))?;

    let f = File::open(archive).map_err(|e| anyhow!("open archive: {e}"))?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    ar.unpack(&staging).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        anyhow!("extract archive: {e}")
    })?;

    // If everything is nested in one wrapping directory, that's the model root;
    // otherwise the files sit at the staging root.
    let root = single_subdir(&staging)?.unwrap_or_else(|| staging.clone());

    let dest = dest_dir.join(dir_name);
    let _ = fs::remove_dir_all(&dest); // replace any prior (e.g. corrupt) model
    fs::rename(&root, &dest).map_err(|e| anyhow!("finalize extracted model: {e}"))?;
    let _ = fs::remove_dir_all(&staging); // no-op if `root` *was* staging
    Ok(())
}

/// The sole subdirectory of `dir` when it contains exactly one entry and that
/// entry is a directory; `None` otherwise (files at the root, or multiple entries).
/// Distinguishes a tarball that wraps its files in a folder from one that doesn't.
fn single_subdir(dir: &Path) -> Result<Option<PathBuf>> {
    let mut entries = fs::read_dir(dir)
        .map_err(|e| anyhow!("read staging dir: {e}"))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| anyhow!("read staging entry: {e}"))?;
    if entries.len() != 1 {
        return Ok(None);
    }
    let entry = entries.remove(0);
    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
        Ok(Some(entry.path()))
    } else {
        Ok(None)
    }
}

/// Download an LLM GGUF to a `.part` file in `dest_dir`, verify it, then atomically
/// rename into place (D1). The streaming/verify is shared with the STT download via
/// [`stream_verified`]; here the verified bytes are the final file, so we just
/// rename and emit the closing 100% tick.
fn download_to(app: &AppHandle, spec: &OptionalModel, file: &str, dest_dir: &Path) -> Result<()> {
    let part = dest_dir.join(format!("{file}.part"));
    let downloaded = stream_verified(app, spec.tier, spec.url, spec.sha256, &part)?;
    fs::rename(&part, dest_dir.join(file)).map_err(|e| anyhow!("finalize download: {e}"))?;
    emit_complete(app, spec.tier, downloaded);
    Ok(())
}

/// Stream `url` to `part`, hashing as we go and emitting throttled
/// `model-download-progress` under `tier`. Verifies the transfer completed (size,
/// when the server gives a Content-Length) and the pinned SHA-256 (when known).
/// Leaves the verified bytes at `part` for the caller to finalize (rename into
/// place, or extract); removes `part` on any failure so a retry starts clean.
/// Returns the byte count downloaded.
fn stream_verified(
    app: &AppHandle,
    tier: &str,
    url: &str,
    sha256: Option<&str>,
    part: &Path,
) -> Result<u64> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| anyhow!("download request failed: {e}"))?;
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut out = File::create(part).map_err(|e| anyhow!("create temp file: {e}"))?;
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
                    tier: tier.to_string(),
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
    // partial body would otherwise be finalized and load as a corrupt file later.
    // When the server gave a Content-Length, require we got all of it.
    if total > 0 && downloaded != total {
        let _ = fs::remove_file(part);
        bail!("incomplete download ({downloaded} of {total} bytes) — download discarded");
    }

    if let Some(expected) = sha256 {
        let got = hex_lower(&hasher.finalize());
        if !got.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(part);
            bail!("checksum mismatch (expected {expected}, got {got}) — download discarded");
        }
    }

    Ok(downloaded)
}

/// Emit a final 100% `model-download-progress` so the UI lands on complete before
/// the terminal `model-download-done`.
fn emit_complete(app: &AppHandle, tier: &str, downloaded: u64) {
    let _ = app.emit(
        "model-download-progress",
        DownloadProgress {
            tier: tier.to_string(),
            downloaded,
            total: downloaded,
        },
    );
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
    use crate::stt::ModelKind;

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
    fn stt_catalog_dir_name_matches_the_loader() {
        // The extracted directory must equal what the STT engine resolves, or a
        // downloaded Parakeet model would never be found. Guards against drift.
        assert_eq!(STT.dir_name, ModelKind::Parakeet.dir_name());
    }

    #[test]
    fn single_subdir_detects_a_wrapping_folder() {
        let root = tempfile::tempdir().unwrap();

        // One wrapping subdir → returned as the model root.
        let inner = root.path().join("parakeet-tdt-0.6b-v3-int8");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("encoder.onnx"), b"x").unwrap();
        assert_eq!(single_subdir(root.path()).unwrap(), Some(inner));

        // A second entry at the root → no single wrapping dir.
        fs::write(root.path().join("stray.onnx"), b"x").unwrap();
        assert_eq!(single_subdir(root.path()).unwrap(), None);
    }

    #[test]
    fn single_subdir_is_none_when_files_sit_at_the_root() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("encoder.onnx"), b"x").unwrap();
        assert_eq!(single_subdir(root.path()).unwrap(), None);
    }

    #[test]
    fn hex_lower_is_zero_padded_lowercase() {
        assert_eq!(hex_lower(&[0x00, 0x0a, 0xff]), "000aff");
    }
}
