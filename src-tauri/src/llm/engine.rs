//! In-process GGUF note generation over `llama-cpp-2` (design §8.2). CPU-only, no
//! server, no network — generation stays on-device (NFR-6).
//!
//! Like the STT engine, the native model sits behind the `NoteGenerator` trait so
//! the GENERATING state machine is testable without it; this file holds the one
//! part that needs the real llama.cpp binding and is verified by building/running
//! `cargo test` on Windows (the binding compiles native code; it is not exercised
//! on the Linux dev box). The streaming/cancel/persist orchestration around it
//! lives in `generator.rs`.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::{anyhow, Result};
use log::{info, warn};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::LlamaStateSeqFlags;

use super::prefill::{self, GenEvent, PrefillCmd, PrefillSession};
use super::prompt;

/// The note-generation model (design §8.2). A single on-device model —
/// `gemma-4-E2B-it-UD-Q4_K_XL` — behind the `NoteGenerator` interface. Kept as an
/// enum (one variant today) so `prompt` / [`PrefixCache`] keep a
/// typed dispatch point if a second model is ever added. The installer bundles no
/// LLM; it is downloaded once at first-run Setup (D3, `models`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmModel {
    /// Gemma 4 E2B instruct, Unsloth dynamic Q4_K_XL (GGUF).
    Gemma,
}

impl LlmModel {
    /// The GGUF filename resolved under the models search dirs (D1: app-data
    /// download dir first, then the bundled resource dir). Must equal the R2 object's
    /// on-disk name; `models::LLM` downloads to exactly this name.
    pub fn file_name(self) -> &'static str {
        match self {
            LlmModel::Gemma => "gemma-4-E2B-it-UD-Q4_K_XL.gguf",
        }
    }
}

// Tuning constants (design §8.2 "set at implementation via benchmarking"): kept
// conservative so the context fits a realistic consult without reserving RAM the
// §7 budget needs.
const N_CTX: u32 = 8192; // prompt + transcript + reasoning + note; well under the model maxima
const MAX_OUTPUT_TOKENS: i32 = 1536; // ceiling for the SOAP note itself (post-reasoning)
const MAX_REASONING_TOKENS: i32 = 1024; // separate cap for the <think> scratchpad (§8.3) so a
                                        // verbose CoT can't eat the note's budget; tunable (§8.2)
const SAMPLE_TEMP: f32 = 0.2; // low temperature → near-deterministic, low hallucination

/// Sanity floor for a serialized prefix KV state (§8.7). The real one is ~16.5 MB, so this
/// only ever rejects a zero/garbage serialize — never a legitimately small prefix.
const MIN_PREFIX_KV_BYTES: usize = 64 * 1024;

/// Generate-path reasoning suppression (design §8.3): the boundary string that ends
/// the `<think>` block, plus a cap on the reasoning phase. The note is given its own
/// `max_tokens` *after* the boundary, so a long scratchpad can never truncate it.
struct Suppress<'a> {
    open: &'a str,
    boundary: &'a str,
    max_reasoning_tokens: i32,
}

/// Serialized KV state of the fixed prompt prefix (system + one-shot example, §8.3)
/// for one model. Restoring this into a fresh context skips re-decoding the prefix
/// on every note — the KV-cache reuse of §8.6. `prefix_tokens` pins which token
/// sequence the state was built from: a generation only reuses it when the full
/// prompt begins with exactly these tokens, which keeps cached and uncached notes
/// byte-identical (a tokenizer merge across the split boundary simply misses the
/// cache and falls back to a full decode).
struct PrefixCache {
    kind: LlmModel,
    prefix_tokens: Vec<LlamaToken>,
    state: Vec<u8>,
}

/// Owns the loaded GGUF model. The `LlamaBackend` is process-wide and created
/// once; the model is warmed at startup (co-resident, §7) and can be unloaded to
/// release RAM. Generation builds a fresh context each run,
/// restoring the cached prefix KV state into it (§8.6) when available.
pub struct LlmEngine {
    backend: LlamaBackend,
    model: Mutex<Option<LlamaModel>>,
    /// Cached prefix KV state (§8.6), primed on load ([`warmup`]) and dropped on
    /// [`unload`]/model change. Guarded separately from `model`; a fresh context is
    /// still built per note, so cancel/error can never leave stale tokens here.
    prefix_cache: Mutex<Option<PrefixCache>>,
    /// The model the engine loads. Immutable — there is one model, so it is fixed at
    /// construction (no live retargeting anymore).
    kind: LlmModel,
    /// Model-file search dirs, in priority order (D1): the app-data download dir
    /// first (optional models the doctor pulled), then the bundled resource dir.
    model_dirs: Vec<PathBuf>,
    /// Decode-phase threads, STT's half of the cores (design §8.2). `None` when the
    /// physical core count was unavailable: llama.cpp then picks both counts itself.
    n_threads: Option<i32>,
    // n_threads: i32,
    /// Prefill-phase threads (`n_threads_batch`), the cores STT's half leaves over
    /// (design §8.2). `None` applies `n_threads` to both phases.
    n_threads_batch: Option<i32>,
    /// Mirrors "is `model` `Some`?" without taking the model lock, which the prefill
    /// session holds for a whole recording while `is_loaded` sits on the UI's path.
    loaded: AtomicBool,
    /// The live prefill session while a recording is in flight (design §8.9). Its
    /// thread owns the model guard and one warm context.
    prefill: Mutex<Option<PrefillSession>>,
    /// Serializes [`ensure_loaded`] so the co-resident background preload (design
    /// §8.2 startup fix) and an early Generate can't both load the model at once.
    /// Held only across the load itself, never nested inside the `model` lock.
    load_lock: Mutex<()>,
}

impl LlmEngine {
    /// Create the engine for `kind`, resolving the model file across `model_dirs`
    /// (first existing wins). The model itself is not loaded until [`ensure_loaded`];
    /// `n_threads` (physical // 2, design §8.2) drives decode; `None` leaves both phases
    /// at the llama.cpp defaults. `n_threads_batch` is the prefill-phase count (design
    /// §8.2) — `None` applies `n_threads` to both.
    pub fn new(
        kind: LlmModel,
        model_dirs: Vec<PathBuf>,
        n_threads: Option<i32>,
        n_threads_batch: Option<i32>,
    ) -> Result<Self> {
        let mut backend =
            LlamaBackend::init().map_err(|e| anyhow!("llama backend init failed: {e}"))?;
        // llama.cpp/ggml dump per-tensor load and context noise straight to C++ stderr,
        // bypassing Rust's `log` filter. `void_logs` installs a no-op callback via
        // `llama_log_set`, which forwards to `ggml_log_set` — covers both. Errors still
        // come back as `Result`s. Comment out to get the firehose back when debugging.
        backend.void_logs();
        Ok(Self {
            backend,
            model: Mutex::new(None),
            prefix_cache: Mutex::new(None),
            kind,
            model_dirs,
            n_threads: n_threads.map(|n| n.max(1)),
            // n_threads: n_threads.max(1),
            n_threads_batch: n_threads_batch.map(|n| n.max(1)),
            loaded: AtomicBool::new(false),
            prefill: Mutex::new(None),
            load_lock: Mutex::new(()),
        })
    }

    pub fn model_kind(&self) -> LlmModel {
        self.kind
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// Load the model if it isn't already, after checking that enough RAM is free
    /// (design §8.4 load-time guard): the §7 budget is a decision on *total* RAM,
    /// but actual *available* RAM at generation time can be lower, so guard here
    /// to fail gracefully rather than risk a silent OOM.
    pub fn ensure_loaded(&self) -> Result<()> {
        if self.is_loaded() {
            return Ok(());
        }
        // Serialize concurrent loaders (background preload vs. an early Generate,
        // design §8.2). `is_loaded` → load isn't atomic on its own; take the load
        // lock and re-check under it so the model loads at most once. The lock is
        // separate from `model` and released before this returns, so it never nests.
        let _load = self.load_lock.lock().unwrap_or_else(|p| p.into_inner());
        if self.is_loaded() {
            return Ok(());
        }
        let kind = self.model_kind();
        let file = kind.file_name();
        let path = crate::models::resolve(file, &self.model_dirs).ok_or_else(|| {
            anyhow!(
                "model file {file} not found in {:?} — the bundled model is missing, \
                 or (for the optional tier) it has not been downloaded yet",
                self.model_dirs
            )
        })?;
        guard_available_ram(&path)?;

        info!("[LOAD] loading SLM: {file}"); // §10.3
        let t_load = Instant::now();
        // let params = LlamaModelParams::default(); // mmap default; CPU-only build
        // The default is n_gpu_layers = -1 (offload all), so with the Vulkan backend
        // now compiled in it must be pinned to 0 until B3 makes the choice (§8.8).
        let params = LlamaModelParams::default().with_n_gpu_layers(0);
        let model = LlamaModel::load_from_file(&self.backend, &path, &params).map_err(|e| {
            // §10.3 `[LOAD] SLM load failed: {e}` (both sinks). Sanitized: the llama.cpp
            // load error embeds the GGUF path (username = PII).
            let msg = crate::telemetry::sanitize_error(&e.to_string());
            log::error!("[LOAD] SLM load failed: {msg}");
            crate::telemetry::track_event("slm_load_failed", serde_json::json!({ "error": msg }));
            anyhow!("failed to load LLM model {}: {e}", path.display())
        })?;
        *self.lock_model() = Some(model);
        self.loaded.store(true, Ordering::Release);
        info!(
            "[LOAD] SLM model loaded: {:.1}s", // §10.3
            t_load.elapsed().as_secs_f32()
        );
        // info!("Loaded LLM model: {:?}", kind);

        // Warmup: the first inference after a load is slow (cold weights/buffers);
        // a tiny throwaway pass keeps the clinician's first real note at full
        // speed (design §8.4). Failure here is non-fatal — log and continue.
        // Timed separately from the weight load: priming decodes the whole fixed
        // prefix, so it is a real slice of startup and worth seeing on its own.
        // let t_warm = Instant::now();
        // if let Err(e) = self.warmup() {
        //     warn!("LLM warmup pass failed (non-fatal): {e}");
        // } else {
        //     info!(
        //         "[LOAD] SLM prefix KV cache primed in {:.1}s",
        //         t_warm.elapsed().as_secs_f32()
        //     );
        // }
        // Try the on-disk prefix KV first (§8.7) — reading the blob skips the prefix
        // decode entirely. Anything wrong with it (absent, stale prompt, short read)
        // falls through to priming, the in-memory-only path kept commented above.
        let t_warm = Instant::now();
        match self.load_prefix_kv() {
            Ok(()) => info!(
                "[LOAD] SLM prefix KV restored from disk in {:.2}s",
                t_warm.elapsed().as_secs_f32()
            ),
            Err(e) => {
                info!("[LOAD] SLM prefix KV not restored from disk ({e}) — priming");
                let t_warm = Instant::now();
                if let Err(e) = self.warmup() {
                    warn!("LLM warmup pass failed (non-fatal): {e}");
                } else {
                    info!(
                        "[LOAD] SLM prefix KV cache primed in {:.1}s",
                        t_warm.elapsed().as_secs_f32()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn unload(&self) {
        // Before anything else: the prefill thread holds the model guard, so taking it
        // below would block forever until that thread exits.
        self.end_prefill();
        self.loaded.store(false, Ordering::Release);
        *self.lock_model() = None;
        // Drop the cached prefix state with the model: it belongs to this model
        // (§8.6). The next load rebuilds it — from the blob when one is present (§8.7),
        // by priming otherwise.
        *self.prefix_cache.lock().unwrap() = None;
    }

    /// Generate a SOAP note from `transcript`, streaming each decoded piece to
    /// `on_token` and polling `cancel` between tokens. Returns the note markdown, or
    /// `None` if cancelled (the caller discards the partial, §8.4). The model reasons
    /// in a private `<think>` block first; only the note after
    /// [`prompt::REASONING_BOUNDARY`] is streamed and returned (§8.3).
    ///
    /// The prompt is built as the fixed prefix + this transcript's tail; when the
    /// prefix's KV state is cached (§8.6) it is restored into the fresh context and
    /// only the tail is decoded, so the prefix is never re-read. The full prompt is
    /// always tokenized and fed identically to the fallback path — the cache only
    /// skips *recomputing* the prefix's KV — so a cached note is byte-identical to
    /// an uncached one.
    pub fn generate(
        &self,
        record_id: &str,
        note_id: &str,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        // Generate-path instrumentation. Every value is a count or a duration — no
        // transcript text is ever logged (NFR-6/PHI). The per-phase and completion
        // timings are emitted inside `decode_and_generate` (§10.3 `[GENERATE]` rows).
        self.ensure_loaded()?;

        // A live prefill session already holds the transcript in a warm context, so run
        // the note there (design §8.9). `None` means no usable session and the normal
        // path below takes over.
        if let Some(result) =
            self.try_prefill_generate(record_id, note_id, transcript, on_token, cancel)
        {
            return result;
        }

        let kind = self.model_kind();
        let prompt = prompt::build_prompt(kind, transcript);

        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| anyhow!("failed to tokenize prompt: {e}"))?;
        // Reserve room for *both* phases: the KV cache holds prompt + reasoning +
        // note, so the prompt must leave MAX_REASONING_TOKENS + MAX_OUTPUT_TOKENS of
        // headroom under N_CTX. Reserving the note budget alone (or checking the
        // prompt alone) would let a verbose <think> block push the note past N_CTX and
        // truncate it mid-decode. Unchanged by caching — the tail still occupies the
        // same positions.
        let output_budget = MAX_REASONING_TOKENS + MAX_OUTPUT_TOKENS;
        let prompt_budget = N_CTX as i32 - output_budget;
        if tokens.len() as i32 >= prompt_budget {
            return Err(anyhow!(
                "transcript is too long for the model context ({} tokens; the prompt \
                 must stay under {prompt_budget} to leave room for the {output_budget}-token \
                 reasoning+note within the {N_CTX} context)",
                tokens.len()
            ));
        }

        // §10.3 `[GENERATE] {record_id} → {note_id}, note generation started — {input_tokens}`.
        // Emitted here (not in the generator) because the token count is only known after
        // tokenization. No transcript text is logged — only its char/token counts.
        info!(
            "[GENERATE] {record_id} → {note_id}, note generation started — {} input tokens",
            tokens.len()
        );

        let mut ctx = self.new_context(model)?;
        // Restore the cached prefix KV if this prompt starts with exactly its
        // tokens; otherwise start from position 0 (full decode, the fallback).
        let start = self.restore_prefix(&mut ctx, kind, &tokens);
        if start > 0 {
            info!(
                "[GENERATE] {note_id} prefix cache HIT — {start} of {} tokens restored, {} to prefill",
                tokens.len(),
                tokens.len() as i32 - start
            );
        } else {
            info!(
                "[GENERATE] {note_id} prefix cache MISS — all {} tokens must be prefilled",
                tokens.len()
            );
        }
        let note = self.decode_and_generate(
            note_id,
            &mut ctx,
            model,
            &tokens,
            start,
            MAX_OUTPUT_TOKENS,
            Some(Suppress {
                open: prompt::REASONING_OPEN,
                boundary: prompt::REASONING_BOUNDARY,
                max_reasoning_tokens: MAX_REASONING_TOKENS,
            }),
            on_token,
            cancel,
        )?;
        // Deterministic scrub of any reasoning marker the model echoed after the note
        // body (§8.5) — the streamed buffer may briefly flash it, but the persisted
        // note never carries it. Cancellation returns `None` and is passed through.
        Ok(note.map(|n| prompt::sanitize_note(&n)))
    }

    /// Prime the prefix cache (§8.6): decode the fixed prefix once and serialize the
    /// resulting context state so later notes can restore it instead of re-decoding.
    /// Called right after a load — this replaces the old throwaway warmup pass, and
    /// doubles as the warmup (the first real decode after a load is the slow one).
    /// The serialized state is also written to disk (§8.7) so the next launch can skip
    /// this decode entirely. Failure is non-fatal: generation falls back to a full
    /// per-note decode.
    fn warmup(&self) -> Result<()> {
        let kind = self.model_kind();
        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;

        let prefix_tokens = model
            .str_to_token(&prompt::prefix(kind), AddBos::Always)
            .map_err(|e| anyhow!("failed to tokenize prompt prefix: {e}"))?;

        let mut ctx = self.new_context(model)?;
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = prefix_tokens.len() as i32 - 1;
        for (i, token) in prefix_tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i as i32 == last)
                .map_err(|e| anyhow!("failed to fill prefix batch: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| anyhow!("prefix decode failed: {e}"))?;

        // Serialize the KV state for **sequence 0 only** (all prompt tokens live on
        // seq 0 — see the batch above). The sequence-scoped size is the cells actually
        // used (~the prefix), not the N_CTX maximum the whole-context `get_state_size`
        // reports — so priming doesn't briefly allocate and zero ~1 GB right after the
        // model load, which would spike RAM against the §7 co-resident budget.
        let mut state = vec![0u8; ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty())];
        let written = unsafe {
            ctx.state_seq_get_data_ext(state.as_mut_ptr(), 0, LlamaStateSeqFlags::empty())
        };
        state.truncate(written);
        // `state_seq_get_data_ext` reports 0 on internal failure and has no `Result`. An empty
        // (or absurdly short) blob writes and reads back fine, so nothing downstream would ever
        // re-prime it — `restore_prefix` just fails silently and every note pays the full
        // prefill forever. Bail instead: the caller's warmup branch is non-fatal, so this
        // degrades to one full decode rather than a permanently poisoned cache. §8.7
        if written < MIN_PREFIX_KV_BYTES {
            return Err(anyhow!(
                "prefix KV serialize returned {written} bytes — too short to be a real state"
            ));
        }
        info!(
            "[LOAD] prefix KV state = {:.1} MB",
            state.len() as f32 / (1024.0 * 1024.0)
        );

        // Keep the blob for the next launch (§8.7). Best-effort — a failed write only
        // means the next launch primes again.
        // Via `.tmp` + rename: a re-prime writes the same filename, and a direct write
        // truncates it first — an interrupted one would leave a short blob that reads
        // back fine and is never re-primed. Non-atomic original:
        // match std::fs::write(&path, &state) {
        if let Some(path) = self.prefix_kv_path() {
            let tmp = path.with_extension("tmp");
            match std::fs::write(&tmp, &state).and_then(|()| std::fs::rename(&tmp, &path)) {
                // Only prune once the replacement is on disk, never before.
                Ok(()) => Self::remove_superseded_blobs(&path),
                Err(e) => {
                    warn!("failed to write prefix KV blob: {e}");
                    let _ = std::fs::remove_file(&tmp); // leftover half-blob
                }
            }
        }

        *self.prefix_cache.lock().unwrap() = Some(PrefixCache {
            kind,
            prefix_tokens,
            state,
        });
        Ok(())
    }

    /// Populate the prefix cache from the on-disk blob (§8.7) instead of decoding the
    /// prefix. Only tokenizes the prefix (no context, no decode). Errors mean "no
    /// usable blob" and the caller primes instead.
    fn load_prefix_kv(&self) -> Result<()> {
        let kind = self.model_kind();
        let path = self
            .prefix_kv_path()
            .ok_or_else(|| anyhow!("no writable models dir"))?;
        let state = std::fs::read(&path)?;
        // Same floor as the write side: a blob this short can't restore, and treating it as
        // "no usable blob" re-primes and overwrites it instead of caching it forever.
        if state.len() < MIN_PREFIX_KV_BYTES {
            return Err(anyhow!("prefix KV blob is only {} bytes", state.len()));
        }

        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;
        let prefix_tokens = model
            .str_to_token(&prompt::prefix(kind), AddBos::Always)
            .map_err(|e| anyhow!("failed to tokenize prompt prefix: {e}"))?;
        drop(guard);

        *self.prefix_cache.lock().unwrap() = Some(PrefixCache {
            kind,
            prefix_tokens,
            state,
        });
        // A launch that hits the blob never reaches `warmup`, so prune here too — otherwise
        // an orphan survives forever once the new blob exists.
        Self::remove_superseded_blobs(&path);
        Ok(())
    }

    /// Where the prefix KV blob lives — the writable app-data models dir, named with a hash
    /// of the prompt prefix and the llama-cpp-sys-2 version (stamped by `build.rs`, since that
    /// crate vendors llama.cpp and owns the blob layout). A prompt edit
    /// or a dependency bump changes the name, so a stale blob is never read (the file simply
    /// isn't there). §8.7
    ///
    /// Takes no `kind`: it is `pub(crate)` since B3, and a caller-supplied kind could name a
    /// model this engine never loaded. `self.kind` makes that unrepresentable.
    // pub(crate) fn prefix_kv_path(&self, kind: LlmModel) -> Option<PathBuf> {
    pub(crate) fn prefix_kv_path(&self) -> Option<PathBuf> {
        use sha2::{Digest, Sha256};
        use std::fmt::Write;

        let kind = self.kind;
        let dir = self.model_dirs.first()?;
        let digest = Sha256::digest(prompt::prefix(kind).as_bytes());
        let mut hash = String::with_capacity(16);
        for b in &digest[..8] {
            let _ = write!(hash, "{b:02x}");
        }
        // Pre-version-stamp name, kept for reference:
        // Some(dir.join(format!("prefix_kv_{}_{hash}.bin", kind.file_name())))
        // Stamped with llama-cpp-2's version before the sys fix; see build.rs:
        // let version = env!("LLAMA_CPP_2_VERSION");
        let version = env!("LLAMA_CPP_SYS_2_VERSION");
        Some(dir.join(format!(
            "prefix_kv_{}_{hash}_{version}.bin",
            kind.file_name()
        )))
    }

    /// Delete every `prefix_kv_*` blob beside `current` — the ones a prompt edit or a
    /// dependency bump left unreadable, at ~16 MB apiece. Best-effort: a blob that won't
    /// unlink is wasted disk, not a failed load. §8.7
    fn remove_superseded_blobs(current: &Path) {
        let Some(dir) = current.parent() else { return };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path == current {
                continue;
            }
            let is_blob = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("prefix_kv_"));
            if is_blob {
                match std::fs::remove_file(&path) {
                    Ok(()) => info!(
                        "[LOAD] superseded prefix KV blob removed: {}",
                        path.display()
                    ),
                    Err(e) => warn!("failed to remove superseded prefix KV blob: {e}"),
                }
            }
        }
    }

    /// Restore the cached prefix state into `ctx` and return the token position to
    /// resume decoding from, or `0` (full decode) when the cache is absent, for a
    /// different model, doesn't prefix `tokens`, or fails to load. The full-prefix
    /// check keeps output identical to the uncached path (see [`generate`]).
    fn restore_prefix(&self, ctx: &mut LlamaContext, kind: LlmModel, tokens: &[LlamaToken]) -> i32 {
        let cache = self.prefix_cache.lock().unwrap();
        let Some(pc) = cache.as_ref() else { return 0 };
        let n = pc.prefix_tokens.len();
        if pc.kind != kind || tokens.len() <= n || tokens[..n] != pc.prefix_tokens[..] {
            return 0;
        }
        // Safety: `state` came from `state_seq_get_data_ext` on a context created with
        // the same model and params, restored onto the same sequence id (0) — the
        // binding's contract for restore.
        let ok = unsafe { ctx.state_seq_set_data_ext(&pc.state, 0, LlamaStateSeqFlags::empty()) };
        if !ok {
            // Restore failed — reset any partial state and decode the whole prompt.
            // Logged because this is the one silent way the cache stops paying off: the note
            // is still correct, just at full prefill cost, with the load-time log still
            // reporting a successful restore. §8.7
            warn!("[LOAD] prefix KV restore rejected by llama.cpp — full prefill for this note");
            ctx.clear_kv_cache();
            return 0;
        }
        n as i32
    }

    /// Restore the cached prefix into `ctx` on its own and return its tokens — the
    /// prefill session's version of [`restore_prefix`], which needs a full prompt to
    /// check against and there is no prompt yet mid-recording. `None` means no usable
    /// cache, and the session gives up (a note without the prefix cached is the normal
    /// path's problem, not prefill's).
    fn restore_prefix_state(
        &self,
        ctx: &mut LlamaContext,
        kind: LlmModel,
    ) -> Option<Vec<LlamaToken>> {
        let cache = self.prefix_cache.lock().unwrap();
        let pc = cache.as_ref()?;
        if pc.kind != kind {
            return None;
        }
        // Safety: same contract as `restore_prefix` — the blob came from
        // `state_seq_get_data_ext` on a context built from this model and params.
        let ok = unsafe { ctx.state_seq_set_data_ext(&pc.state, 0, LlamaStateSeqFlags::empty()) };
        if !ok {
            warn!("[PREFILL] prefix KV restore rejected by llama.cpp");
            ctx.clear_kv_cache();
            return None;
        }
        Some(pc.prefix_tokens.clone())
    }

    /// Decode `tokens[start..]` into `ctx` (positions `start..`, so a restored
    /// prefix lines up), then stream generated tokens until end-of-generation, the
    /// token cap, or cancellation. Shared by the cached and full-decode paths.
    ///
    /// `suppress` gates the chain-of-thought (design §8.3): when `Some` (the generate
    /// path), decoded pieces are buffered and **not** streamed until the boundary
    /// ([`prompt::REASONING_BOUNDARY`]) appears; only the note after it is streamed
    /// and returned, so the `<think>` reasoning is never shown or persisted. `None`
    /// streams every piece unfiltered.
    ///
    /// Three things keep the note from being truncated or lost:
    /// - **Plain-note fallback.** If the model skips the format — its first content is
    ///   not the `<think>` opener — there is no reasoning block, so streaming starts
    ///   immediately and every token counts against the *note* budget. (Counting a
    ///   plain note as reasoning is what used to cap it at `max_reasoning_tokens`.)
    /// - **Note budget after the boundary.** Once reasoning closes, the note gets its
    ///   own `max_tokens` regardless of how long the scratchpad ran.
    /// - **Reasoning cap → forced boundary.** If the `<think>` block runs past
    ///   `max_reasoning_tokens`, the boundary is force-decoded into the context so the
    ///   model stops reasoning and writes the note, rather than erroring out — under
    ///   near-greedy decoding that error would reproduce identically on every retry, a
    ///   permanent wall.
    ///
    /// The only remaining no-boundary case is the model ending its turn (EOG)
    /// mid-`<think>`: output that opened `<think>` but never closed it is pure
    /// scratchpad → error out rather than persist the reasoning as the note (§8.3).
    #[allow(clippy::too_many_arguments)]
    fn decode_and_generate(
        &self,
        note_id: &str,
        ctx: &mut LlamaContext,
        model: &LlamaModel,
        tokens: &[LlamaToken],
        start: i32,
        max_tokens: i32,
        suppress: Option<Suppress>,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        // Phase clock. `t_phase` is reset at each phase boundary; `t_gen` measures from
        // the top so the "first visible word" line reports what the clinician actually
        // waits for after clicking Generate.
        let t_gen = Instant::now();
        let mut t_phase = Instant::now();

        info!("[GENERATE] {note_id} prefill started"); // §10.3
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() as i32 - 1;
        for i in start..tokens.len() as i32 {
            batch
                .add(tokens[i as usize], i, &[0], i == last)
                .map_err(|e| anyhow!("failed to fill prompt batch: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| anyhow!("prompt decode failed: {e}"))?;
        let prefilled = tokens.len() as i32 - start;
        // §10.3 `[GENERATE] {note_id} prefill done — prefill duration {N}s` (tok/s kept
        // for on-device diagnostics).
        info!(
            "[GENERATE] {note_id} prefill done — {prefilled} tokens, prefill duration {:.2}s ({:.0} tok/s)",
            t_phase.elapsed().as_secs_f32(),
            rate(prefilled, t_phase.elapsed())
        );
        t_phase = Instant::now();
        // §10.3 `[GENERATE] {note_id} reasoning started` — only when the two-phase
        // (chain-of-thought) format is active; `suppress: None` streams with no reasoning.
        if suppress.is_some() {
            info!("[GENERATE] {note_id} reasoning started");
        }

        // Low temperature for near-deterministic, low-hallucination clinical text
        // (design §8.2/§8.3).
        let mut sampler =
            LlamaSampler::chain_simple([LlamaSampler::temp(SAMPLE_TEMP), LlamaSampler::greedy()]);

        let mut raw = String::new(); // full generation, including any reasoning block
        let mut note = String::new(); // the streamed/returned portion (post-boundary)

        // With no suppression the whole stream is the note from the first token.
        let mut boundary_passed = suppress.is_none();
        // Absolute next position: the prompt fills 0..tokens.len(), so generation
        // continues there regardless of how much of the prompt was cached.
        let mut n_cur = tokens.len() as i32;
        let mut note_tokens = 0; // counted against `max_tokens` (the note budget)
        let mut reasoning_tokens = 0; // counted against the reasoning cap, while suppressing

        // Cursor into `raw` for the boundary search: everything before it has already
        // been scanned and can't be part of a first match, so each token only searches
        // the newly-grown suffix instead of rescanning from 0 (avoids O(n²) on the
        // decode hot path). The boundary may straddle two pieces, so we back the cursor
        // up by `boundary.len() - 1` to keep the overlap where a match could complete.
        let mut scan_from = 0usize;
        // `boundary_passed` flips at three separate sites (forced boundary, plain-note
        // fallback, boundary found); logging the reasoning→note transition once from the
        // top of the loop covers all three without duplicating the line at each.
        let mut reasoning_logged = suppress.is_none();
        loop {
            if boundary_passed && !reasoning_logged {
                reasoning_logged = true;
                // §10.3 `[GENERATE] {note_id} reasoning done — reasoning duration {N}s`.
                info!(
                    "[GENERATE] {note_id} reasoning done — {reasoning_tokens} tokens, reasoning duration {:.1}s ({:.1} tok/s)",
                    t_phase.elapsed().as_secs_f32(),
                    rate(reasoning_tokens, t_phase.elapsed())
                );
                // §10.3 `[GENERATE] {note_id} perceived TTFT at {N}s` — what the clinician
                // waits before the first visible note word after clicking Generate.
                info!(
                    "[GENERATE] {note_id} perceived TTFT at {:.1}s",
                    t_gen.elapsed().as_secs_f32()
                );
                t_phase = Instant::now();
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(None); // partial note discarded by the caller
            }
            // The note gets its own `max_tokens` regardless of how long the reasoning
            // ran; the reasoning phase is separately capped so it can't consume the
            // context reserved for the note (§8.3).
            if boundary_passed {
                if note_tokens >= max_tokens {
                    break;
                }
            } else if let Some(s) = &suppress {
                if reasoning_tokens >= s.max_reasoning_tokens {
                    // Runaway scratchpad: the model is still reasoning past its cap. We
                    // do *not* break here — under near-greedy decoding that would hit
                    // the "produced only reasoning" error identically on every retry, a
                    // permanent wall that could never produce a note (§8.3). Instead
                    // force-close the `<think>` block by decoding the boundary tokens
                    // into the context and switch to streaming, so the cap means "stop
                    // thinking, write the note now" rather than "fail forever".
                    let forced = model
                        .str_to_token(s.boundary, AddBos::Never)
                        .map_err(|e| anyhow!("failed to tokenize reasoning boundary: {e}"))?;
                    batch.clear();
                    let last = forced.len() as i32 - 1;
                    for (j, t) in forced.iter().enumerate() {
                        batch
                            .add(*t, n_cur, &[0], j as i32 == last)
                            .map_err(|e| anyhow!("failed to inject the reasoning boundary: {e}"))?;
                        n_cur += 1;
                    }
                    ctx.decode(&mut batch)
                        .map_err(|e| anyhow!("boundary injection decode failed: {e}"))?;
                    raw.push_str(s.boundary);
                    boundary_passed = true;
                    continue; // next sample reads the boundary's logits → first note token
                }
            }

            let token = sampler.sample(ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if model.is_eog_token(token) {
                break;
            }

            let piece = model
                .token_to_str(token, Special::Tokenize)
                .map_err(|e| anyhow!("failed to decode a token: {e}"))?;
            // Some Gemma GGUFs don't mark <end_of_turn> as an end-of-generation token,
            // so is_eog_token misses it; under Special::Tokenize it then renders as the
            // literal tag and would both leak into the note and let generation run on to
            // max_tokens (wasted CPU). Each turn-control token is a single token → a
            // single complete piece, so an exact match ends the turn here with no
            // hold-back buffer, before the piece is streamed or appended.
            if piece == "<end_of_turn>" || piece == "<start_of_turn>" {
                break;
            }
            raw.push_str(&piece);

            if boundary_passed {
                on_token(&piece);
                note.push_str(&piece);
                note_tokens += 1;
            } else if let Some(s) = &suppress {
                let trimmed = raw.trim_start();
                if !trimmed.is_empty()
                    && !trimmed.starts_with(s.open)
                    && !s.open.starts_with(trimmed)
                {
                    // The model skipped the two-phase format: its first content is not
                    // the reasoning opener (and isn't a partial prefix of it still being
                    // formed), so there is no `<think>` block and everything so far is a
                    // plain note (the §8.3 fallback). Switch to note mode now — stream
                    // what's buffered and count it against the *note* budget, not the
                    // reasoning cap. Counting a plain note as reasoning is exactly what
                    // capped it at `max_reasoning_tokens` and truncated notes longer than
                    // that. Detection lands on the first content token, so `trimmed` is
                    // effectively that one token.
                    boundary_passed = true;
                    on_token(trimmed);
                    note.push_str(trimmed);
                    note_tokens += 1;
                } else if let Some(rel) = raw[scan_from..].find(s.boundary) {
                    // Reasoning closed: everything up to the boundary was the private
                    // scratchpad. Stream only the note text after it.
                    boundary_passed = true;
                    let idx = scan_from + rel;
                    let tail = raw[idx + s.boundary.len()..].trim_start();
                    if !tail.is_empty() {
                        on_token(tail);
                        note.push_str(tail);
                        note_tokens += 1;
                    }
                } else {
                    // Still inside the reasoning block, boundary not seen yet. It may
                    // straddle two pieces, so keep buffering and search from `scan_from`
                    // (which keeps the previous piece's tail overlap) rather than only
                    // this piece. Advance the cursor to just before where the next token
                    // could complete the boundary (the trailing overlap), then back off
                    // to a char boundary so the next slice can't split a multibyte char
                    // in the reasoning text.
                    reasoning_tokens += 1;
                    scan_from = raw.len().saturating_sub(s.boundary.len() - 1);
                    while scan_from > 0 && !raw.is_char_boundary(scan_from) {
                        scan_from -= 1;
                    }
                }
            }

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| anyhow!("failed to add a token to the batch: {e}"))?;
            n_cur += 1;
            ctx.decode(&mut batch)
                .map_err(|e| anyhow!("token decode failed: {e}"))?;
        }

        if boundary_passed {
            // §10.3 `[GENERATE] {note_id} note generation complete — {generated_token_count},
            // total {N}s, {tokens/s}`. `t_gen` (from the top of generation) is the total;
            // `t_phase` (reset at the note boundary) gives the true note decode rate.
            info!(
                "[GENERATE] {note_id} note generation complete — {note_tokens} tokens, total {:.1}s, {:.1} tok/s",
                t_gen.elapsed().as_secs_f32(),
                rate(note_tokens, t_phase.elapsed())
            );
            Ok(Some(note))
        } else if raw.contains(prompt::REASONING_OPEN) {
            // The model opened `<think>` and then ended its turn (EOG) before closing
            // it — the only way to land here now that the reasoning cap force-closes the
            // block instead of breaking. `raw` is the private scratchpad with no note
            // after it. Streaming or persisting that would turn the model's internal
            // reasoning into the clinician's saved note (a PHI-shaped leak), so fail
            // loudly instead — the caller persists nothing and the clinician regenerates.
            Err(anyhow!(
                "note generation produced only reasoning (no {:?} boundary); discarding \
                 the scratchpad rather than persisting it as a note",
                prompt::REASONING_BOUNDARY
            ))
        } else {
            // Degenerate output with no `<think>` and no note content routed inline (a
            // plain note is caught during the loop and streamed live). This is reached
            // only by all-whitespace or an unclosed partial `<think>` prefix at EOG —
            // return whatever there is rather than nothing (design §8.3 edge case).
            warn!(
                "reasoning boundary {:?} not found in generation; returning full output",
                suppress.as_ref().map(|s| s.boundary)
            );
            on_token(&raw);
            Ok(Some(raw))
        }
    }

    /// A fresh inference context sized to N_CTX on the engine's thread budget. One
    /// is built per note (and per prefix priming); the cached prefix state is
    /// restored into it, so nothing needs to hold a context across notes (§8.6).
    fn new_context<'a>(&'a self, model: &'a LlamaModel) -> Result<LlamaContext<'a>> {
        // llama.cpp uses `n_threads_batch` for batched (prefill) decode and `n_threads`
        // for single-token decode, so the two phases sit on disjoint cores with no
        // runtime switching (design §8.2). Unset → llama.cpp's own defaults.
        let mut ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(N_CTX));
        if let Some(n) = self.n_threads {
            ctx_params = ctx_params
                .with_n_threads(n)
                .with_n_threads_batch(self.n_threads_batch.unwrap_or(n));
        }
        // let ctx_params = LlamaContextParams::default()
        //     .with_n_ctx(NonZeroU32::new(N_CTX))
        //     .with_n_threads(self.n_threads)
        //     .with_n_threads_batch(self.n_threads);
        model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| anyhow!("failed to create LLM context: {e}"))
    }

    fn lock_model(&self) -> MutexGuard<'_, Option<LlamaModel>> {
        self.model.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- transcript prefill during recording (design §8.9) ----

    /// Start prefilling this recording's segments, replacing any previous session.
    /// Unconditional: an unknown core count just leaves llama.cpp its own thread counts.
    pub fn begin_prefill(self: &Arc<Self>) {
        let mut slot = self.prefill.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None; // drop the previous session (and its KV) before spawning a new one
        *slot = Some(PrefillSession::spawn(self.clone()));
    }

    /// Drop the session: its thread drains, releases the model guard, and exits.
    pub fn end_prefill(&self) {
        let mut slot = self.prefill.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }

    /// Queue a finished segment for prefill. Never blocks the STT sink.
    pub fn push_prefill_segment(&self, seq: u64, text: &str) {
        let slot = self.prefill.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(session) = slot.as_ref() {
            session.push_segment(seq, text);
        }
    }

    /// Route a note through the live session, or `None` to use the normal path.
    fn try_prefill_generate(
        &self,
        record_id: &str,
        note_id: &str,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Option<Result<Option<String>>> {
        let slot = self.prefill.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref()?
            .generate(record_id, note_id, transcript, on_token, cancel)
    }

    /// The prefill thread's body. Holds the model guard and one warm context for the
    /// life of the recording; see `llm::prefill` for why that has to be a thread.
    ///
    /// Every failure here is non-fatal by construction: it sets `disabled` and breaks,
    /// which releases the guard so [`generate`]'s normal path can take the lock.
    pub(crate) fn run_prefill_loop(
        &self,
        rx: Receiver<PrefillCmd>,
        depth: Arc<AtomicUsize>,
        disabled: Arc<AtomicBool>,
    ) {
        let give_up = |why: String| {
            if !disabled.swap(true, Ordering::Relaxed) {
                warn!("[PREFILL] stopped for this recording: {why}");
            }
        };

        if let Err(e) = self.ensure_loaded() {
            give_up(format!("model not loaded ({e})"));
            return;
        }
        let guard = self.lock_model();
        let Some(model) = guard.as_ref() else {
            give_up("model not loaded".into());
            return;
        };
        let kind = self.kind;

        let mut ctx = match self.new_context(model) {
            Ok(ctx) => ctx,
            Err(e) => return give_up(format!("context creation failed ({e})")),
        };
        // The prefix restore runs at record start, where nobody is waiting on it.
        let Some(prefix_tokens) = self.restore_prefix_state(&mut ctx, kind) else {
            return give_up("no usable prefix KV cache".into());
        };
        let prefix_len = prefix_tokens.len();
        let mut prefilled = prefix_tokens;

        // Same ceiling Generate enforces: the KV holds prompt + reasoning + note, so the
        // prompt must leave both output budgets free under N_CTX.
        let prompt_budget = (N_CTX as i32 - (MAX_REASONING_TOKENS + MAX_OUTPUT_TOKENS)) as usize;
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);

        for cmd in rx.iter() {
            match cmd {
                PrefillCmd::Segment { seq, text } => {
                    depth.fetch_sub(1, Ordering::Relaxed);
                    // `prefilled.len() == prefix_len` means nothing has been appended yet,
                    // so this is the transcript's first segment and takes no leading space.
                    let Some(chunk) = prefill::segment_chunk(prefilled.len() == prefix_len, &text)
                    else {
                        continue;
                    };

                    let t0 = Instant::now();
                    let tokens = match model.str_to_token(&chunk, AddBos::Never) {
                        Ok(t) => t,
                        Err(e) => {
                            give_up(format!("segment tokenization failed ({e})"));
                            break;
                        }
                    };
                    if prefilled.len() + tokens.len() >= prompt_budget {
                        // The consult outgrew the context. Generate's existing "transcript
                        // is too long" error stays the only user-facing behaviour — prefill
                        // must never turn a working consult into an error, or vice versa.
                        give_up(format!(
                            "transcript passed the {prompt_budget}-token prompt budget"
                        ));
                        break;
                    }

                    batch.clear();
                    let last = tokens.len() - 1;
                    let mut failed = None;
                    for (i, token) in tokens.iter().enumerate() {
                        let pos = (prefilled.len() + i) as i32;
                        if let Err(e) = batch.add(*token, pos, &[0], i == last) {
                            failed = Some(format!("batch fill failed ({e})"));
                            break;
                        }
                    }
                    if let Some(why) = failed {
                        give_up(why);
                        break;
                    }
                    if let Err(e) = ctx.decode(&mut batch) {
                        give_up(format!("segment decode failed ({e})"));
                        break;
                    }
                    prefilled.extend_from_slice(&tokens);

                    // Counts and durations only — no transcript text (NFR-6/PHI).
                    info!(
                        "[PREFILL] seq{seq}: {} tokens, prefill - {:.2}s, total - {} tokens",
                        tokens.len(),
                        t0.elapsed().as_secs_f32(),
                        prefilled.len() - prefix_len
                    );
                }
                PrefillCmd::Generate {
                    record_id,
                    note_id,
                    transcript,
                    cancel,
                    events,
                } => {
                    let Some(result) = self.prefill_generate(
                        &mut ctx,
                        model,
                        kind,
                        &mut prefilled,
                        &record_id,
                        &note_id,
                        &transcript,
                        &cancel,
                        &events,
                    ) else {
                        // The context is unusable. Drop `events` with no `Done` on it:
                        // the caller reads the closed channel as "no session" and runs
                        // the normal path, so the clinician still gets a note.
                        give_up("the live context could not be reused".into());
                        break;
                    };
                    // Roll the KV back to what `prefilled` describes: the note and the
                    // turn tail before it sit past that point and must not be reused by a
                    // regenerate. `Ok(false)` is a *refused* removal — the stale region
                    // survives, so the session can't be trusted either.
                    if !matches!(
                        ctx.clear_kv_cache_seq(Some(0), Some(prefilled.len() as u32), None),
                        Ok(true)
                    ) {
                        give_up("KV rollback after the note failed".into());
                        let _ = events.send(GenEvent::Done(result));
                        break;
                    }
                    let _ = events.send(GenEvent::Done(result));
                }
            }
        }
    }

    /// Generate the note on the recording's live context. Same prompt, same decode loop,
    /// same output as [`generate`] — the only difference is where the prefill came from.
    ///
    /// `None` means the context turned out to be unusable and the caller falls back to
    /// the normal path; `Some(Err)` is the generation's own error, surfaced as usual.
    #[allow(clippy::too_many_arguments)]
    fn prefill_generate(
        &self,
        ctx: &mut LlamaContext,
        model: &LlamaModel,
        kind: LlmModel,
        prefilled: &mut Vec<LlamaToken>,
        record_id: &str,
        note_id: &str,
        transcript: &str,
        cancel: &Arc<AtomicBool>,
        events: &Sender<GenEvent>,
    ) -> Option<Result<Option<String>>> {
        let prompt = prompt::build_prompt(kind, transcript);
        let tokens = match model.str_to_token(&prompt, AddBos::Always) {
            Ok(t) => t,
            Err(e) => return Some(Err(anyhow!("failed to tokenize prompt: {e}"))),
        };

        let output_budget = MAX_REASONING_TOKENS + MAX_OUTPUT_TOKENS;
        let prompt_budget = N_CTX as i32 - output_budget;
        if tokens.len() as i32 >= prompt_budget {
            // Byte-for-byte the error the normal path raises — prefill must never change
            // what the clinician sees, only how fast they see it.
            return Some(Err(anyhow!(
                "transcript is too long for the model context ({} tokens; the prompt \
                 must stay under {prompt_budget} to leave room for the {output_budget}-token \
                 reasoning+note within the {N_CTX} context)",
                tokens.len()
            )));
        }

        info!(
            "[GENERATE] {record_id} → {note_id}, note generation started — {} input tokens",
            tokens.len()
        );

        // Longest common prefix against what the recording prefilled. An edit anywhere in
        // the transcript costs only the tokens after it; an untouched transcript reuses
        // everything. `prefilled` is a prefix of the prompt in the no-edit case, so the
        // LCP is its whole length.
        let reuse = tokens
            .iter()
            .zip(prefilled.iter())
            .take_while(|(a, b)| a == b)
            .count();
        // Drop everything past the reuse point. A refused removal (`Ok(false)`) leaves
        // stale KV entries that would decode into a garbage note — abandon the session
        // and let the caller regenerate on a fresh context instead.
        if !matches!(
            ctx.clear_kv_cache_seq(Some(0), Some(reuse as u32), None),
            Ok(true)
        ) {
            return None;
        }
        prefilled.truncate(reuse);

        info!(
            "[GENERATE] {note_id} prefill session — {reuse} of {} tokens reused, {} to prefill",
            tokens.len(),
            tokens.len() - reuse
        );

        let on_token = |piece: &str| {
            let _ = events.send(GenEvent::Token(piece.to_string()));
        };
        let note = self.decode_and_generate(
            note_id,
            ctx,
            model,
            &tokens,
            reuse as i32,
            MAX_OUTPUT_TOKENS,
            Some(Suppress {
                open: prompt::REASONING_OPEN,
                boundary: prompt::REASONING_BOUNDARY,
                max_reasoning_tokens: MAX_REASONING_TOKENS,
            }),
            &on_token,
            cancel,
        );
        Some(note.map(|n| n.map(|n| prompt::sanitize_note(&n))))
    }
}

/// Tokens per second, guarding the degenerate zero-elapsed case so a fast phase
/// logs `0` rather than `inf`.
fn rate(tokens: i32, elapsed: std::time::Duration) -> f32 {
    let s = elapsed.as_secs_f32();
    if s <= 0.0 {
        0.0
    } else {
        tokens as f32 / s
    }
}

/// Fail the load if free RAM is below the model file size plus a working margin
/// (design §8.4): better a graceful error in IDLE than a mid-load OOM crash.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_superseded_blobs_deletes_only_other_prefix_kv_files() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("prefix_kv_gemma.gguf_abc123_0.1.150.bin");

        // The current blob, two superseded ones, a crashed write's leftover, and the
        // model weights sitting in the same dir.
        let stale = dir.path().join("prefix_kv_gemma.gguf_abc123_0.1.149.bin");
        let old_prompt = dir.path().join("prefix_kv_gemma.gguf_deadbe_0.1.150.bin");
        let leftover = dir.path().join("prefix_kv_gemma.gguf_abc123_0.1.150.tmp");
        let weights = dir.path().join(LlmModel::Gemma.file_name());
        for p in [&current, &stale, &old_prompt, &leftover, &weights] {
            std::fs::write(p, b"x").unwrap();
        }

        LlmEngine::remove_superseded_blobs(&current);

        assert!(current.exists(), "the current blob must survive");
        assert!(weights.exists(), "non-blob files must be untouched");
        assert!(!stale.exists());
        assert!(!old_prompt.exists());
        assert!(!leftover.exists());

        // Idempotent, and safe when the current blob is the only file left.
        LlmEngine::remove_superseded_blobs(&current);
        assert!(current.exists());
    }

    #[test]
    fn remove_superseded_blobs_ignores_a_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("no-such-dir").join("prefix_kv_x.bin");
        LlmEngine::remove_superseded_blobs(&gone); // must not panic
    }
}

fn guard_available_ram(model_path: &Path) -> Result<()> {
    let model_bytes = std::fs::metadata(model_path)
        .map(|m| m.len())
        .map_err(|e| anyhow!("model file not found at {}: {e}", model_path.display()))?;
    // Decode buffers/KV-cache need headroom beyond the weights themselves.
    const WORKING_MARGIN: u64 = 2 * 1024 * 1024 * 1024;
    let needed = model_bytes + WORKING_MARGIN;

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let available = sys.available_memory();
    if available < needed {
        return Err(anyhow!(
            "not enough free memory to load the note model: need ~{} MB, {} MB free",
            needed / (1024 * 1024),
            available / (1024 * 1024)
        ));
    }
    Ok(())
}
