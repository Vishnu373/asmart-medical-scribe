//! In-process GGUF note generation over `llama-cpp-4` (design §8.2). CPU-only, no
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

// These read `llama_cpp_2::…` before the migration — only the crate name changed,
// every module path below is identical (migration M2).
use llama_cpp_4::context::params::{LlamaContextParams, LlamaContextType};
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel, Special};
// Speculative decoding (§8.10). `llama-cpp-2` had no equivalent — this is what the
// migration to `llama-cpp-4` was for.
use llama_cpp_4::mtp::{MtpSession, MtpSessionConfig};
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;
// llama-cpp-4 takes the state-seq flags as a plain `u32`, so the newtype is gone:
// use llama_cpp_2::LlamaStateSeqFlags;

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

    /// The speculative-decoding draft GGUF's filename (spec-decoding B1). Beside
    /// [`file_name`](Self::file_name) for the same reason: the download
    /// (`models::DRAFT`) and the loader must agree on the name in one place.
    pub fn draft_file_name(self) -> &'static str {
        match self {
            LlmModel::Gemma => "gemma4-e2b-draft.gguf",
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

/// Speculative-decoding draft length (design §8.10, spec-decoding decision #1): how many
/// tokens the draft head guesses per round. A fixed constant — not tuned at runtime, not a
/// setting. Independently matches upstream llama.cpp's own `n_draft_max` CLI default.
const K_DRAFT: i32 = 3;

/// Set to `1` to run the plain single-token decode loop even when the draft model loaded
/// (spec-decoding decision #11). Hidden, undocumented for users: benchmarking needs an A/B
/// on one machine and support needs a way to rule the feature out of a slowness report.
const NO_SPECULATIVE_ENV: &str = "MEDSCRIBE_NO_SPECULATIVE";

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

/// The draft head's harvest failed after a successful target decode: the KV is correct,
/// so the note continues on the plain path rather than failing (Background #6).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct DraftHarvestFailed(String);

/// What [`LlmEngine::decode_and_generate`] decodes through: the plain target context, or
/// an [`MtpSession`] that has exclusively borrowed it (design §8.10, spec-decoding B7).
///
/// The whole difference speculative decoding makes is *how one round advances*; every
/// other line of the generation loop — the reasoning boundary scan, the plain-note
/// fallback, the budgets, the turn-end checks — is byte-for-byte the same code on both
/// paths. Confining the fork to this enum is what makes "the note is identical either
/// way" structural rather than something a test has to keep proving.
enum Decoder<'a, 'ctx, 'model> {
    Plain(&'a mut LlamaContext<'model>),
    Mtp(&'a mut MtpSession<'ctx, 'model>),
}

impl<'a, 'ctx, 'model> Decoder<'a, 'ctx, 'model> {
    fn ctx(&self) -> &LlamaContext<'model> {
        match self {
            Decoder::Plain(c) => &**c,
            Decoder::Mtp(s) => s.target_context(),
        }
    }

    fn ctx_mut(&mut self) -> &mut LlamaContext<'model> {
        match self {
            Decoder::Plain(c) => &mut **c,
            Decoder::Mtp(s) => s.target_context_mut(),
        }
    }

    fn is_speculative(&self) -> bool {
        matches!(self, Decoder::Mtp(_))
    }

    /// Decode one batch on the target context.
    ///
    /// The MTP arm additionally hands the batch to upstream (`process`), which harvests
    /// the hidden state that seeds the *next* draft. Skipping it does not fail — the head
    /// just drafts from a stale position and the acceptance rate collapses silently — so
    /// every target decode on the speculative path must go through here, the prompt
    /// prefill included.
    fn decode(&mut self, batch: &mut LlamaBatch) -> Result<()> {
        match self {
            Decoder::Plain(c) => c.decode(batch).map_err(|e| anyhow!("{e}")),
            // Decoder::Mtp(s) => s
            //     .decode_target_and_process(batch)
            //     .map_err(|e| anyhow!("{e}")),
            // Split so the two halves fail differently: the target decode wrote the KV the
            // note is built on, but the harvest only seeds the draft head (Background #6).
            Decoder::Mtp(s) => {
                s.decode_target(batch).map_err(|e| anyhow!("{e}"))?;
                s.process(batch)
                    .map_err(|e| DraftHarvestFailed(format!("{e}")).into())
            }
        }
    }

    /// Decode one batch on the target only. For use once drafting is off: the harvest
    /// exists solely to seed the *next* draft, so past that point it is cost with no upside.
    fn decode_target_only(&mut self, batch: &mut LlamaBatch) -> Result<()> {
        match self {
            Decoder::Plain(c) => c.decode(batch).map_err(|e| anyhow!("{e}")),
            Decoder::Mtp(s) => s.decode_target(batch).map_err(|e| anyhow!("{e}")),
        }
    }
}

/// Speculative-decoding counters for one note (spec-decoding B8). Counts only — never
/// transcript or note text (NFR-6). The number that decides whether the feature pays for
/// itself is `accepted / drafted`; below roughly 30% the draft work costs more than it
/// saves, which is the stop condition B8 exists to test.
#[derive(Default)]
struct SpecStats {
    /// Rounds *attempted*: incremented at the top of `speculative_round`, so one that bails
    /// before committing anything still counts.
    rounds: u32,
    drafted: u32,
    accepted: u32,
    /// Target forward passes spent on rounds. One per round, which is the whole point:
    /// `committed / target_passes` is how many tokens each expensive weight read bought.
    target_passes: u32,
    committed: u32,
}

impl SpecStats {
    fn acceptance_pct(&self) -> f32 {
        if self.drafted == 0 {
            0.0
        } else {
            100.0 * self.accepted as f32 / self.drafted as f32
        }
    }

    fn tokens_per_pass(&self) -> f32 {
        if self.target_passes == 0 {
            0.0
        } else {
            self.committed as f32 / self.target_passes as f32
        }
    }
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
    /// The speculative-decoding draft head (design §8.10). Not a standalone LM: it consumes
    /// the target's hidden state and *shares the target's KV cache*, so it is only ever used
    /// through an `MtpSession` holding both contexts. Loaded in [`ensure_loaded`] straight
    /// after the target and with the same [`LlamaModelParams`] — §8.8's one-backend-decision
    /// rule (decision #7). `None` when the file is absent or would not load, which is not an
    /// error: the plain decode loop runs exactly as it does today.
    draft: Mutex<Option<LlamaModel>>,
    /// Whether speculative decoding is live for this process. True only when the draft
    /// loaded, the MTP session validated against the target, and [`NO_SPECULATIVE_ENV`] is
    /// unset. The single flag every path reads; anything that goes wrong clears it
    /// ([`disable_speculative`]) and the note is produced the way it is today (§8.10).
    speculative: AtomicBool,
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
            draft: Mutex::new(None),
            speculative: AtomicBool::new(false),
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
        // Resolved before the guard, not after: the draft is co-resident (decision #6), so
        // its bytes belong in the §8.4 floor. `None` (not downloaded) just means the floor
        // is the target's, as it was before speculative decoding.
        // The env switch is read here, not only in `load_draft` below: the guard runs first,
        // so the floor would otherwise charge for a draft decision #11 never loads.
        let draft_path = crate::models::resolve(kind.draft_file_name(), &self.model_dirs)
            .filter(|_| !speculative_off_by_env());
        // guard_available_ram(&path)?;
        guard_available_ram(&path, draft_path.as_deref())?;

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

        // Speculative decoding (design §8.10). Same `params` as the target, deliberately:
        // §8.8 decides the compute backend once and everything the engine loads follows it
        // (decision #7) — a draft on CPU under a GPU target would be the slower of the two
        // and invert the whole trade. Nothing here can fail the load.
        self.load_draft(draft_path.as_deref(), &params);

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
        // The draft goes with the target: it is only meaningful paired to that model's
        // context, so leaving it resident would hold 93 MiB describing nothing (§8.10).
        self.speculative.store(false, Ordering::Release);
        *self.lock_draft() = None;
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
        // Fires when a retry discards an already-streamed partial, so the UI can clear its
        // buffer instead of concatenating two notes (§9.5).
        on_restart: &dyn Fn(),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        // Generate-path instrumentation. Every value is a count or a duration — no
        // transcript text is ever logged (NFR-6/PHI). The per-phase and completion
        // timings are emitted inside `decode_and_generate` (§10.3 `[GENERATE]` rows).
        self.ensure_loaded()?;

        // A live prefill session already holds the transcript in a warm context, so run
        // the note there (design §8.9). `None` means no usable session and the normal
        // path below takes over.
        // if let Some(result) =
        //     self.try_prefill_generate(record_id, note_id, transcript, on_token, cancel)
        // Threaded `on_restart`: an abandoned session may already have streamed a partial.
        if let Some(result) =
            self.try_prefill_generate(record_id, note_id, transcript, on_token, on_restart, cancel)
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

        // Held beside the model guard, in that order everywhere, so the two locks can
        // never be taken in opposite orders by two threads (§8.10).
        let draft_guard = self.lock_draft();
        let draft_model = draft_guard.as_ref();

        let note = match self.generate_on_fresh_context(
            note_id,
            model,
            draft_model,
            kind,
            &tokens,
            on_token,
            cancel,
        ) {
            Err(e) if e.downcast_ref::<PoisonedContext>().is_some() => {
                // A refused speculative rollback: the KV describes tokens the model never
                // chose, so the context is thrown away rather than trusted. One fresh
                // context with the feature off gives the clinician the note they were
                // waiting for — slower, never wrong, and never an error (§8.10).
                self.disable_speculative(&format!("{e}"));
                // The abandoned partial is already on screen; drop it before re-streaming.
                on_restart();
                self.generate_on_fresh_context(
                    note_id,
                    model,
                    draft_model,
                    kind,
                    &tokens,
                    on_token,
                    cancel,
                )?
            }
            other => other?,
        };
        // Deterministic scrub of any reasoning marker the model echoed after the note
        // body (§8.5) — the streamed buffer may briefly flash it, but the persisted
        // note never carries it. Cancellation returns `None` and is passed through.
        Ok(note.map(|n| prompt::sanitize_note(&n)))
    }

    /// Run one note on a fresh context: restore the prefix, pair the draft to it when
    /// speculative decoding is live, then decode.
    ///
    /// Factored out of [`generate`] so a poisoned context can be thrown away and the note
    /// retried on a clean one without duplicating the prefix restore or its logging. The
    /// `'m` on both models and on `self` is load-bearing: `MtpSession` stores both contexts
    /// as `&mut LlamaContext<'model>`, and `&mut` is invariant, so the two contexts must be
    /// built from borrows the compiler can put in one region.
    #[allow(clippy::too_many_arguments)]
    fn generate_on_fresh_context<'m>(
        &'m self,
        note_id: &str,
        model: &'m LlamaModel,
        draft_model: Option<&'m LlamaModel>,
        kind: LlmModel,
        tokens: &[LlamaToken],
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        let mut ctx = self.new_context(model)?;
        // Built *before* the restore below, and that order is load-bearing: the draft context
        // shares the target's KV cells and llama.cpp resizes — hence resets — them as it
        // constructs (`llama-kv-cache.cpp:141`), so building it second erases the prefix.
        let prebuilt_draft_ctx = draft_model
            .filter(|_| self.is_speculative())
            .map(|dm| self.new_draft_context(dm, &ctx));
        // Restore the cached prefix KV if this prompt starts with exactly its
        // tokens; otherwise start from position 0 (full decode, the fallback).
        let start = self.restore_prefix(&mut ctx, kind, tokens);
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
        let suppress = || {
            Some(Suppress {
                open: prompt::REASONING_OPEN,
                boundary: prompt::REASONING_BOUNDARY,
                max_reasoning_tokens: MAX_REASONING_TOKENS,
            })
        };

        // Speculative only when the draft is live *and* everything it needs builds here.
        // Every other outcome falls through to the plain loop below, which is always a
        // correct answer (Background #6).
        // if let Some(dm) = draft_model.filter(|_| self.is_speculative()) {
        //     match self.new_draft_context(dm, &ctx) {
        if let Some(built) = prebuilt_draft_ctx {
            match built {
                Ok(mut draft_ctx) => {
                    // let config = MtpSessionConfig::new(1, K_DRAFT).with_p_min(0.0);
                    // One constructor so `p_min` cannot drift per call site.
                    let config = mtp_config();
                    // No nextn restore here: `ctx` dies with this call, unlike prefill's.
                    match MtpSession::new_with_config(&mut ctx, &mut draft_ctx, config) {
                        Ok(mut session) => {
                            return self.decode_and_generate(
                                note_id,
                                Decoder::Mtp(&mut session),
                                model,
                                tokens,
                                start,
                                MAX_OUTPUT_TOKENS,
                                suppress(),
                                on_token,
                                cancel,
                            );
                        }
                        Err(e) => self.disable_speculative(&format!("mtp session rejected: {e}")),
                    }
                }
                Err(e) => {
                    self.disable_speculative(&format!("draft context could not be built: {e}"))
                }
            }
        }
        self.decode_and_generate(
            note_id,
            Decoder::Plain(&mut ctx),
            model,
            tokens,
            start,
            MAX_OUTPUT_TOKENS,
            suppress(),
            on_token,
            cancel,
        )
    }

    /// Load the draft head and stand up the MTP session that pairs it to the target
    /// (design §8.10, spec-decoding B3). Returns nothing and raises nothing: every outcome
    /// other than success leaves `speculative` false, and a `false` flag is a complete,
    /// correct app — the note is produced by the loop that produces it today.
    ///
    /// Called from [`ensure_loaded`] with the *target's* `params`, after the target is in
    /// `self.model`: the session validates the two against each other, so the target has to
    /// be loadable first.
    fn load_draft(&self, draft_path: Option<&Path>, params: &LlamaModelParams) {
        if speculative_off_by_env() {
            info!("[LOAD] {NO_SPECULATIVE_ENV} set — speculative decoding off"); // §10.3
            return;
        }
        let Some(path) = draft_path else {
            // Not an error and not a retry: the draft is an optional download (§8.10) and a
            // device that never got it runs at today's speed forever, which is fine.
            info!("[LOAD] draft model not present — speculative decoding off for this session");
            return;
        };

        let t_draft = Instant::now();
        let draft = match LlamaModel::load_from_file(&self.backend, path, params) {
            Ok(m) => m,
            Err(e) => {
                // Sanitized for the same reason the target's load error is: the llama.cpp
                // message embeds the GGUF path, and the path embeds the Windows username.
                let msg = crate::telemetry::sanitize_error(&e.to_string());
                warn!("[LOAD] draft model load failed: {msg} — speculative decoding off for this session");
                return;
            }
        };
        info!(
            "[LOAD] draft model loaded: {:.1}s", // §10.3
            t_draft.elapsed().as_secs_f32()
        );
        *self.lock_draft() = Some(draft);

        // Prove the pairing here, at load, rather than the first time a doctor clicks
        // Generate: a mismatch found mid-note would be a failure the clinician waits
        // through, and this is the phase that exists to make that impossible.
        let t_session = Instant::now();
        match self.validate_mtp_session() {
            Ok(()) => {
                self.speculative.store(true, Ordering::Release);
                info!(
                    "[LOAD] mtp session ready: n_draft_max={K_DRAFT}, validated in {:.2}s", // §10.3
                    t_session.elapsed().as_secs_f32()
                );
            }
            Err(e) => {
                warn!("[LOAD] mtp session rejected: {e} — speculative decoding off");
                *self.lock_draft() = None; // 93 MiB that can never be used
            }
        }
    }

    /// Build the target/draft context pair and an [`MtpSession`] over them once, at load,
    /// to prove the pairing works before a clinician ever waits on it (spec-decoding B3).
    ///
    /// **Constructing the session *is* the validation.** `MtpSession::new_with_config`
    /// runs llama-cpp-4's own compatibility gate internally — the draft's
    /// `n_embd_out` against the target's `n_embd` (B0's 1536 ↔ 1536 finding), the two
    /// context types, and the sequence/batch capacities. The crate's `validate_contexts`
    /// is private, so there is nothing else to call and nothing to re-check by hand.
    ///
    /// Everything is dropped on the way out. That is not waste avoided by keeping it: the
    /// session is `!Send`/`!Sync` and `&mut`-borrows **both** contexts for its whole life,
    /// so it can never be stored on the engine beside them (a self-referential struct).
    /// B7 rebuilds it in the frame that owns the target context.
    fn validate_mtp_session(&self) -> Result<()> {
        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("target model is not loaded"))?;
        let draft_guard = self.lock_draft();
        let draft_model = draft_guard
            .as_ref()
            .ok_or_else(|| anyhow!("draft model is not loaded"))?;

        // Full N_CTX on purpose: a pairing validated at a smaller context could still fail
        // at the size a real note uses, which is the failure this phase exists to catch
        // early. The duration is logged so B8 can weigh it against the §8.7 fast launch.
        let mut target_ctx = self.new_context(model)?;
        let mut draft_ctx = self.new_draft_context(draft_model, &target_ctx)?;

        // `p_min(0.0)` is decision #2 in the crate's own terms: no probability floor, no
        // relaxed acceptance. The knob exists; it stays off, so nothing the draft invents
        // can enter clinical text on a probability argument.
        // `new(1, K_DRAFT)`: the first argument is the *count* of sequence slots, not a
        // sequence id — one slot, so B7 drafts on seq 0.
        // let config = MtpSessionConfig::new(1, K_DRAFT).with_p_min(0.0);
        // Same constructor the two generate paths use, so validation can't drift from them.
        let config = mtp_config();
        let session = MtpSession::new_with_config(&mut target_ctx, &mut draft_ctx, config)
            .map_err(|e| anyhow!("{e}"))?;
        // Explicit, so nobody later reads the bare `?` as an accident: the session exists
        // to be validated and freed, and its `Drop` releases the native session.
        drop(session);
        Ok(())
    }

    // Was: "`n_rs_seq` is the recurrent-state depth the post-round rollback needs" —
    // true only where the arch supports rollback, which `gemma4-assistant` does not.
    /// The draft head's context: MTP type, paired to the target's (design §8.10).
    ///
    /// `with_ctx_other` is not a nicety here. llama.cpp throws `Gemma4Assistant requires
    /// ctx_other to be set` when a `gemma4-assistant` context is built without it
    /// (`llama-context.cpp:146`), because the head consumes the target's hidden state and
    /// rides its KV layers rather than keeping a cache of its own. `n_seq_max` must
    /// *equal* the session's `n_seq`, not merely cover it. `n_rs_seq` is the recurrent
    /// rollback depth, set for correctness where it is honoured but clamped to 0 for
    /// `gemma4-assistant` (`llama-context.cpp:105`, `llama-arch.cpp:1025`).
    fn new_draft_context<'a>(
        &'a self,
        draft: &'a LlamaModel,
        target: &LlamaContext<'_>,
    ) -> Result<LlamaContext<'a>> {
        let mut params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_ctx_type(LlamaContextType::Mtp)
            .with_n_seq_max(1)
            .with_n_rs_seq(K_DRAFT.max(4) as u32)
            .with_ctx_other(target);
        // Decision #9: a round is draft, then verify, strictly in sequence — the two never
        // run at once, so the draft takes the same budget rather than a slice carved out of
        // it, which would leave cores idle for half of every round.
        if let Some(n) = self.n_threads {
            params = params
                .with_n_threads(n)
                .with_n_threads_batch(self.n_threads_batch.unwrap_or(n));
        }
        draft
            .new_context(&self.backend, params)
            .map_err(|e| anyhow!("failed to create the draft context: {e}"))
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
        // Pre-migration, through llama-cpp-2's flag newtype and raw destination pointer:
        // let mut state = vec![0u8; ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty())];
        // let written = unsafe {
        //     ctx.state_seq_get_data_ext(state.as_mut_ptr(), 0, LlamaStateSeqFlags::empty())
        // };
        // Flags `0` = no PARTIAL_ONLY: the full sequence state, as before.
        let mut state = vec![0u8; ctx.state_seq_get_size_ext(0, 0)];
        let written = ctx.state_seq_get_data_ext(&mut state, 0, 0);
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
        // let version = env!("LLAMA_CPP_SYS_2_VERSION");   // pre-llama-cpp-4 migration
        let version = env!("LLAMA_CPP_SYS_4_VERSION");
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
        // `state` came from `state_seq_get_data_ext` on a context created with the same
        // model and params, restored onto the same sequence id (0) — llama.cpp rejects
        // the blob rather than misreading it if that ever stops holding.
        // Pre-migration this was `unsafe` and returned a bool:
        // let ok = unsafe { ctx.state_seq_set_data_ext(&pc.state, 0, LlamaStateSeqFlags::empty()) };
        // llama-cpp-4 returns the bytes read instead — 0 is the failure signal.
        let ok = ctx.state_seq_set_data_ext(&pc.state, 0, 0) > 0;
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
        // Same contract as `restore_prefix` — the blob came from `state_seq_get_data_ext`
        // on a context built from this model and params.
        // let ok = unsafe { ctx.state_seq_set_data_ext(&pc.state, 0, LlamaStateSeqFlags::empty()) };
        let ok = ctx.state_seq_set_data_ext(&pc.state, 0, 0) > 0;
        if !ok {
            warn!("[PREFILL] prefix KV restore rejected by llama.cpp");
            ctx.clear_kv_cache();
            return None;
        }
        Some(pc.prefix_tokens.clone())
    }

    /// Decode one batch, keeping the note alive when only the draft half fails: a
    /// [`DraftHarvestFailed`] left the KV correct, so it ends drafting, not the note.
    fn decode_step(
        engine: &LlmEngine,
        decoder: &mut Decoder<'_, '_, '_>,
        batch: &mut LlamaBatch,
        speculative_live: &mut bool,
        what: &str,
    ) -> Result<()> {
        let decoded = if *speculative_live {
            decoder.decode(batch)
        } else {
            decoder.decode_target_only(batch)
        };
        match decoded {
            Ok(()) => Ok(()),
            Err(e) if e.downcast_ref::<DraftHarvestFailed>().is_some() => {
                engine.disable_speculative(&format!("{e}"));
                *speculative_live = false;
                Ok(())
            }
            Err(e) => Err(anyhow!("{what} failed: {e}")),
        }
    }

    /// Decode `tokens[start..]` through `decoder` (positions `start..`, so a restored
    /// prefix lines up), then stream generated tokens until end-of-generation, the
    /// token cap, or cancellation. Shared by the cached and full-decode paths.
    ///
    /// `decoder` decides only *how a round advances* — one token at a time, or `carry`
    /// plus the drafts a speculative round got the target to agree to (§8.10). Everything
    /// below that choice is one shared body, which is what makes the two paths produce the
    /// same note rather than merely being tested to.
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
        // ctx: &mut LlamaContext,
        mut decoder: Decoder<'_, '_, '_>,
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
        // Declared before the prompt prefill, which is itself a decode that can lose the
        // draft head; see the doc comment further down for what this flag is for.
        let mut speculative_live = decoder.is_speculative();
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() as i32 - 1;
        for i in start..tokens.len() as i32 {
            batch
                .add(tokens[i as usize], i, &[0], i == last)
                .map_err(|e| anyhow!("failed to fill prompt batch: {e}"))?;
        }
        // ctx.decode(&mut batch).map_err(|e| anyhow!("prompt decode failed: {e}"))?;
        // Through the decoder: on the speculative path this is also what seeds the draft
        // head's hidden state, so the first round drafts from the last prompt token rather
        // than from nothing (§8.10).
        // decoder
        //     .decode(&mut batch)
        //     .map_err(|e| anyhow!("prompt decode failed: {e}"))?;
        // Via `decode_step` so a harvest failure here ends drafting instead of the note.
        Self::decode_step(self, &mut decoder, &mut batch, &mut speculative_live, "prompt decode")?;
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
        // Speculative-decoding counters for this note (B8). Left at zero on the plain path.
        let mut stats = SpecStats::default();
        // Captured up front: a round can turn the feature off mid-note, and the completion
        // line should still say the note was produced on the speculative path.
        let speculative_used = decoder.is_speculative();
        // Local mirror of the engine flag, for *this* note. A round that fails leaves the
        // session with a proposal still pending, so retrying would fail identically every
        // round for the rest of the note — burning a draft attempt per token to produce
        // the same output. One failure ends drafting here and now.
        // let mut speculative_live = speculative_used;
        // Hoisted above the prompt prefill, which is already a decode that can lose the head.

        // §10.3, spec-decoding B8. One place, called at every exit: a cancelled or degenerate
        // note still spent draft work. Takes `&SpecStats` so `&mut stats` stays free for the
        // round.
        let log_spec_stats = |stats: &SpecStats| {
            if speculative_used {
                info!(
                    "[GENERATE] {note_id} speculative — {}/{} accepted ({:.0}%), {} rounds attempted, {:.2} tokens per pass",
                    stats.accepted,
                    stats.drafted,
                    stats.acceptance_pct(),
                    stats.rounds,
                    stats.tokens_per_pass()
                );
            }
        };
        // `carry` is the target's *own* next token — sampled from the logits the last
        // decode left behind, and not yet in the KV cache. The plain path commits exactly
        // it each round; the speculative path commits it plus every draft that matched
        // what the target would have written anyway (§8.10, decision #2). Hoisting the
        // sample out of the loop is what lets both paths share every line below it.
        let mut carry = sampler.sample(decoder.ctx(), batch.n_tokens() - 1);
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
                log_spec_stats(&stats); // a cancelled note still spent draft work (B8)
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
                    // ctx.decode(&mut batch)
                    // One decoder call covers both contexts: there is a single KV cache,
                    // and `process` re-seeds the draft head from the injected boundary so
                    // its next guesses are about the note rather than the scratchpad.
                    // decoder
                    //     .decode(&mut batch)
                    //     .map_err(|e| anyhow!("boundary injection decode failed: {e}"))?;
                    // Via `decode_step` so a harvest failure here ends drafting, not the note.
                    Self::decode_step(
                        self,
                        &mut decoder,
                        &mut batch,
                        &mut speculative_live,
                        "boundary injection decode",
                    )?;
                    raw.push_str(s.boundary);
                    boundary_passed = true;
                    // The injection moved the logits, so the token we were holding is no
                    // longer the target's own next choice — re-derive it. Not unit-testable
                    // (needs a loaded model); exercised by the B8 benchmark run.
                    carry = sampler.sample(decoder.ctx(), batch.n_tokens() - 1);
                    continue; // next round reads the boundary's logits → first note token
                }
            }

            // ---- produce this round's committed tokens ----
            //
            // Plain: exactly `carry`, decoded after routing, exactly as before.
            // Speculative: `carry` plus the accepted drafts, already decoded and trimmed.
            let mut spec_round = None;
            if speculative_live {
                if let Decoder::Mtp(session) = &mut decoder {
                    match speculative_round(
                        &mut **session,
                        &sampler,
                        &mut batch,
                        carry,
                        n_cur,
                        &mut stats,
                    ) {
                        Ok(round) => spec_round = Some(round),
                        Err(e) if e.downcast_ref::<PoisonedContext>().is_some() => return Err(e),
                        Err(e) => {
                            // Every non-poisoning exit of the round is one where the verify
                            // batch never reached the KV, so the note is unaffected: turn the
                            // feature off and let this round fall through to the plain step
                            // below (Background #6).
                            self.disable_speculative(&format!("{e}"));
                            speculative_live = false;
                            spec_round = None;
                        }
                    }
                }
            }
            let committed = match &spec_round {
                Some((tokens, _)) => tokens.as_slice(),
                None => std::slice::from_ref(&carry),
            };

            // Every committed token runs the *same* five per-token concerns the single
            // token used to, in order. A round can straddle the end of the reasoning block
            // and the first note tokens, so none of this can move to per-round.
            let mut kept = 0usize; // committed tokens actually accepted into the output
            let mut stop = false; // end of turn / note budget — leave the outer loop
            for &token in committed {
                sampler.accept(token);
                if model.is_eog_token(token) {
                    stop = true;
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
                    stop = true;
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

                kept += 1;
                // Budgets are checked here, per committed token, not once per round —
                // otherwise a round could carry the note up to K_DRAFT tokens past its cap
                // (§8.10). The plain path breaks at exactly the same token it always did.
                if boundary_passed {
                    if note_tokens >= max_tokens {
                        stop = true;
                        break;
                    }
                } else if let Some(s) = &suppress {
                    if reasoning_tokens >= s.max_reasoning_tokens {
                        // Stop consuming the round and re-enter the loop, where the forced
                        // boundary is injected at the position we actually reached.
                        break;
                    }
                }
            }

            // ---- advance ----
            match spec_round {
                Some((tokens, next)) => {
                    stats.committed += kept as u32;
                    // Anything committed but not kept (a turn end, a cap, a filled budget
                    // mid-round) is already in the KV and must come back out, or the next
                    // decode writes over a position that is still occupied.
                    if kept < tokens.len() {
                        let keep_upto = n_cur + kept as i32;
                        // trim_target(&mut decoder, keep_upto)?;
                        // A refusal only harms what decodes next; on the round that ends the
                        // note nothing does, so the stale entries die with the context.
                        match trim_target(&mut decoder, keep_upto) {
                            Ok(()) => {}
                            Err(e) if stop => {
                                warn!(
                                    "[GENERATE] {note_id} rollback refused on the final round; the note is complete, keeping it: {e}"
                                );
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    n_cur += kept as i32;
                    carry = next;
                }
                None => {
                    // The plain step, unchanged: the token is decoded *after* routing, so
                    // a turn end still costs no decode at all.
                    if kept == 1 {
                        batch.clear();
                        batch
                            .add(carry, n_cur, &[0], true)
                            .map_err(|e| anyhow!("failed to add a token to the batch: {e}"))?;
                        n_cur += 1;
                        // decoder
                        //     .decode(&mut batch)
                        //     .map_err(|e| anyhow!("token decode failed: {e}"))?;
                        // Via `decode_step` so a harvest failure here ends drafting, not the note.
                        Self::decode_step(
                            self,
                            &mut decoder,
                            &mut batch,
                            &mut speculative_live,
                            "token decode",
                        )?;
                        carry = sampler.sample(decoder.ctx(), batch.n_tokens() - 1);
                    }
                }
            }
            if stop {
                break;
            }
        }

        if boundary_passed {
            // §10.3 `[GENERATE] {note_id} note generation complete — {generated_token_count},
            // total {N}s, {tokens/s}`. `t_gen` (from the top of generation) is the total;
            // `t_phase` (reset at the note boundary) gives the true note decode rate.
            info!(
                "[GENERATE] {note_id} note generation complete — {note_tokens} tokens, total {:.1}s, {:.1} tok/s, speculative={}",
                t_gen.elapsed().as_secs_f32(),
                rate(note_tokens, t_phase.elapsed()),
                if speculative_used { "on" } else { "off" }
            );
            // §10.3, spec-decoding B8. The acceptance rate is the number that decides
            // whether the feature pays for itself; the tok/s comparison is made by running
            // the same transcript twice, once with MEDSCRIBE_NO_SPECULATIVE set.
            // if speculative_used {
            //     info!(
            //         "[GENERATE] {note_id} speculative — {}/{} accepted ({:.0}%), {} rounds, {:.2} tokens per pass",
            //         stats.accepted,
            //         stats.drafted,
            //         stats.acceptance_pct(),
            //         stats.rounds,
            //         stats.tokens_per_pass()
            //     );
            // }
            // Moved into `log_spec_stats` so the other exits report it too.
            log_spec_stats(&stats);
            Ok(Some(note))
        } else if raw.contains(prompt::REASONING_OPEN) {
            // The model opened `<think>` and then ended its turn (EOG) before closing
            // it — the only way to land here now that the reasoning cap force-closes the
            // block instead of breaking. `raw` is the private scratchpad with no note
            // after it. Streaming or persisting that would turn the model's internal
            // reasoning into the clinician's saved note (a PHI-shaped leak), so fail
            // loudly instead — the caller persists nothing and the clinician regenerates.
            log_spec_stats(&stats); // the draft work was spent even though the note failed
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
            log_spec_stats(&stats); // degenerate output still cost draft work
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

    fn lock_draft(&self) -> MutexGuard<'_, Option<LlamaModel>> {
        self.draft.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether speculative decoding is live (design §8.10). The decode loop reads this to
    /// choose between the drafted round and the plain single-token step; `false` is always
    /// a valid answer and always produces the same note.
    pub fn is_speculative(&self) -> bool {
        self.speculative.load(Ordering::Acquire)
    }

    // Was: "for the rest of this process" / "one-way on purpose" — `load_draft` re-arms
    // the flag after an `unload()`/reload, so the real scope is the session, not the process.
    /// Turn speculative decoding off for the rest of this session and say why once.
    ///
    /// The single choke point for Background #6 ("failure means run like today, never fail
    /// the note"): a decode error, a refused KV trim, or prefill falling behind all end
    /// here rather than raising. Nothing here re-arms it — a context that has already
    /// mis-stepped once is not something to re-arm mid-consult; only [`load_draft`],
    /// loading the draft model afresh, sets it back to true.
    pub fn disable_speculative(&self, why: &str) {
        if self.speculative.swap(false, Ordering::AcqRel) {
            warn!("[GENERATE] speculative decoding off for this session: {why}");
        }
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
        on_restart: &dyn Fn(),
        cancel: &Arc<AtomicBool>,
    ) -> Option<Result<Option<String>>> {
        let slot = self.prefill.lock().unwrap_or_else(|p| p.into_inner());
        // slot.as_ref()?
        //     .generate(record_id, note_id, transcript, on_token, cancel)
        // Split so a session that gave up mid-note signals the restart before the caller
        // re-streams the note on the normal path.
        let result = slot
            .as_ref()?
            .generate(record_id, note_id, transcript, on_token, cancel);
        if result.is_none() {
            on_restart();
        }
        result
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
            // §8.10: the `MtpSession` is built per note inside `prefill_generate` and
            // dropped when it returns, so a give-up here can never leave one alive against
            // a KV cache that no longer describes anything.
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
        // Same lock order as everywhere else: model, then draft. Held for the recording so
        // the draft context can be built beside the target one — `MtpSession` needs both
        // borrows in a single region, which a per-note guard could not provide.
        let draft_guard = self.lock_draft();
        let kind = self.kind;

        let mut ctx = match self.new_context(model) {
            Ok(ctx) => ctx,
            Err(e) => return give_up(format!("context creation failed ({e})")),
        };
        // Built once for the recording, not per note: it costs a context allocation and
        // nothing about it changes between notes. A failure is not a prefill failure —
        // speculative decoding goes off and the recording keeps prefilling (§8.10).
        //
        // Keep this above `restore_prefix_state`: the draft shares the target's KV cells and
        // resets them as it constructs (`llama-kv-cache.cpp:141`), so the reverse order would
        // silently drop the restored prefix.
        let mut draft_ctx = match draft_guard.as_ref().filter(|_| self.is_speculative()) {
            Some(dm) => match self.new_draft_context(dm, &ctx) {
                Ok(c) => Some(c),
                Err(e) => {
                    self.disable_speculative(&format!("draft context could not be built: {e}"));
                    None
                }
            },
            None => None,
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
                        draft_ctx.as_mut(),
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
    fn prefill_generate<'m>(
        &self,
        ctx: &mut LlamaContext<'m>,
        // Same `'m` as `ctx`, and not optional styling: `MtpSession` stores both contexts
        // as `&mut LlamaContext<'model>`, and `&mut` is invariant, so the two must come
        // from borrows the compiler can place in one region.
        draft_ctx: Option<&mut LlamaContext<'m>>,
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
        let suppress = || {
            Some(Suppress {
                open: prompt::REASONING_OPEN,
                boundary: prompt::REASONING_BOUNDARY,
                max_reasoning_tokens: MAX_REASONING_TOKENS,
            })
        };
        // A poisoned context is the one error that is not the note's: rejected drafts
        // survived a refused rollback, so this context is abandoned and `None` sends the
        // caller to the normal path, which builds a fresh one. The clinician gets a
        // slower note rather than a wrong one or an error (§8.10).
        let finish = |note: Result<Option<String>>| match note {
            Err(e) if e.downcast_ref::<PoisonedContext>().is_some() => {
                self.disable_speculative(&format!("{e}"));
                None
            }
            other => Some(other.map(|n| n.map(|n| prompt::sanitize_note(&n)))),
        };

        if let Some(d) = draft_ctx.filter(|_| self.is_speculative()) {
            // let config = MtpSessionConfig::new(1, K_DRAFT).with_p_min(0.0);
            // One constructor so `p_min` cannot drift per call site.
            let config = mtp_config();
            // Carried out of the match so the session's `&mut ctx` is gone before the restore.
            let note = match MtpSession::new_with_config(&mut *ctx, &mut *d, config) {
                Ok(mut session) => {
                    // return finish(self.decode_and_generate(
                    let note = self.decode_and_generate(
                        note_id,
                        Decoder::Mtp(&mut session),
                        model,
                        &tokens,
                        reuse as i32,
                        MAX_OUTPUT_TOKENS,
                        suppress(),
                        &on_token,
                        cancel,
                    );
                    drop(session); // releases the `&mut` borrow of `ctx`
                    Some(note)
                }
                // The session would not build. Keep the recording's prefill — that is worth
                // far more than the draft — and fall through to the plain loop.
                // Err(e) => self.disable_speculative(&format!("mtp session rejected: {e}")),
                Err(e) => {
                    self.disable_speculative(&format!("mtp session rejected: {e}"));
                    None
                }
            };
            // The session turns nextn on and nothing turns it off; `ctx` is the recording's,
            // so every later segment decode and plain-path note would carry it (§8.10).
            ctx.set_embeddings_pre_norm(false, false);
            if let Some(note) = note {
                return finish(note);
            }
        }
        finish(self.decode_and_generate(
            note_id,
            Decoder::Plain(ctx),
            model,
            &tokens,
            reuse as i32,
            MAX_OUTPUT_TOKENS,
            suppress(),
            &on_token,
            cancel,
        ))
    }
}

/// The MTP session config for every call site: `p_min = 0.0` is decision #2 — exact
/// verification only, so no probability floor can let a drafted token into the note.
fn mtp_config() -> MtpSessionConfig {
    MtpSessionConfig::new(1, K_DRAFT).with_p_min(0.0)
}

/// The one failure that means "this context can no longer be trusted" rather than "this
/// note failed": a KV rollback refused *after* a partly-rejected speculative round
/// (Background #4). Rejected drafts are then still sitting in the cache, so every token
/// decoded afterwards would be conditioned on text the target never chose.
///
/// **This is a deliberate departure from the plan's B7 step 4**, which said to clear
/// `speculative` and finish the note on the plain loop. That would produce a note built on
/// tokens the model never wrote — silently wrong clinical text, which §8.10 forbids far
/// more strongly than it forbids a slow note. Callers instead throw the context away and
/// regenerate on a fresh one with speculative decoding off, so the clinician still gets a
/// correct note and still never sees an error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PoisonedContext(String);

/// Roll the target KV back to `keep_upto` tokens, treating a refusal as poisoning.
///
/// `Ok(false)` is not a warning — it means llama.cpp declined the removal and the stale
/// entries survived. §8.9 already treats a refused trim as fatal to its session for exactly
/// this reason, and a speculative round has the same exposure.
fn trim_target(decoder: &mut Decoder<'_, '_, '_>, keep_upto: i32) -> Result<()> {
    let trimmed = decoder
        .ctx_mut()
        .clear_kv_cache_seq(Some(0), Some(keep_upto as u32), None);
    if matches!(trimmed, Ok(true)) {
        Ok(())
    } else {
        Err(PoisonedContext(format!(
            "speculative rollback to {keep_upto} was refused by llama.cpp ({trimmed:?})"
        ))
        .into())
    }
}

/// The acceptance rule — decision #2, and the whole safety argument of the feature.
///
/// A guess is kept only when it equals the token the target's *own* sampler produces at
/// that position, and testing stops at the first mismatch: a token drafted from a
/// continuation that will not exist says nothing about what the target wanted. Returns the
/// number of leading drafts kept and the target's own next token, which the following round
/// carries.
///
/// `target_at(i)` reads the target's choice for the position after batch row `i`. It is a
/// closure rather than the sampler itself so this rule can be tested without llama.cpp —
/// it is the one piece of the loop that decides what reaches clinical text. Calls are lazy:
/// a mismatch at the first slot costs one, not `K_DRAFT + 1`.
fn accept_prefix(
    drafts: &[LlamaToken],
    mut target_at: impl FnMut(i32) -> LlamaToken,
) -> (usize, LlamaToken) {
    let mut accepted = 0usize;
    let mut next = target_at(0);
    while accepted < drafts.len() && drafts[accepted] == next {
        accepted += 1;
        next = target_at(accepted as i32);
    }
    (accepted, next)
}

/// One speculative round (design §8.10, spec-decoding B7).
///
/// At entry the target KV holds `n_cur` tokens (positions `0..n_cur-1`) and `carry` — the
/// target's own next token — is not yet in it. The round returns the tokens committed
/// (always `carry`, then every accepted draft) and the next `carry`.
///
/// The acceptance rule is decision #2 and the whole safety argument: a draft is kept only
/// when it equals the token the *target's own sampler* produces at that position, using the
/// same `sampler.sample` call the plain loop uses. Testing stops at the first mismatch,
/// because a token drafted from a continuation that will not exist is meaningless.
///
/// **Failure contract.** A plain `Err` promises the caller that the KV still ends at `n_cur`,
/// which is what lets it retry the round on the plain path. Once the verify batch is decoded
/// that is no longer true, so every exit after it returns [`PoisonedContext`] instead.
fn speculative_round(
    session: &mut MtpSession<'_, '_>,
    sampler: &LlamaSampler,
    batch: &mut LlamaBatch,
    carry: LlamaToken,
    n_cur: i32,
    stats: &mut SpecStats,
) -> Result<(Vec<LlamaToken>, LlamaToken)> {
    stats.rounds += 1;

    // `n_past` is what is already in the target KV; `id_last` is the token just sampled and
    // not yet in it. Both are exactly the round invariant above.
    let drafts = session
        .draft(0, n_cur, carry)
        .map_err(|e| anyhow!("draft failed: {e}"))?;

    // No guesses this round (upstream can return an empty proposal). Nothing is pending, so
    // `accept` must not be called; fall back to a plain one-token step through the session
    // so the hidden state stays in step.
    if drafts.is_empty() {
        batch.clear();
        batch
            .add(carry, n_cur, &[0], true)
            .map_err(|e| anyhow!("failed to fill the verify batch: {e}"))?;
        // session
        //     .decode_target_and_process(batch)
        //     .map_err(|e| anyhow!("verify decode failed: {e}"))?;
        // Split, because the two halves leave different KV states: `process` runs after the
        // batch has landed, so its failure cannot fall back to the plain step (see below).
        session
            .decode_target(batch)
            .map_err(|e| anyhow!("verify decode failed: {e}"))?;
        session.process(batch).map_err(|e| {
            PoisonedContext(format!("verify process failed after the target decode: {e}"))
        })?;
        stats.target_passes += 1;
        let next = sampler.sample(session.target_context(), 0);
        return Ok((vec![carry], next));
    }

    // One batch, logits on every row: `carry@n_cur` plus each guess after it. This is the
    // saving — the target reads its 3.18 GB of weights once and answers for all of them.
    batch.clear();
    batch
        .add(carry, n_cur, &[0], true)
        .map_err(|e| anyhow!("failed to fill the verify batch: {e}"))?;
    for (i, d) in drafts.iter().enumerate() {
        batch
            .add(*d, n_cur + 1 + i as i32, &[0], true)
            .map_err(|e| anyhow!("failed to fill the verify batch: {e}"))?;
    }
    // session
    //     .decode_target_and_process(batch)
    //     .map_err(|e| anyhow!("verify decode failed: {e}"))?;
    // Same split as above, and here it is the whole safety of the fallback: past this line
    // `carry` *and* every guess are in the KV, so nothing may return a recoverable error.
    session
        .decode_target(batch)
        .map_err(|e| anyhow!("verify decode failed: {e}"))?;
    session.process(batch).map_err(|e| {
        PoisonedContext(format!("verify process failed after the target decode: {e}"))
    })?;
    stats.target_passes += 1;
    stats.drafted += drafts.len() as u32;

    // Row `i` holds the target's own choice for the position *after* row `i`'s token, so
    // row 0 answers for `carry`, row 1 for the first guess, and so on.
    let (accepted, next) = accept_prefix(&drafts, |i| sampler.sample(session.target_context(), i));
    stats.accepted += accepted as u32;

    // Tell upstream how far the target agreed. This is *not* a KV operation — it copies the
    // hidden-state row for the last committed position so the next round drafts from there
    // (`common/speculative.cpp`, `common_speculative_impl_draft_mtp::accept`). The plan's
    // claim that it also trims the shared cache is wrong; the trim below is still ours.
    // session
    //     .accept(0, accepted as u16)
    //     .map_err(|e| anyhow!("accept failed: {e}"))?;
    // Poisoning, not a plain error: the verify batch is already in the KV and the trim below
    // has not run, so the plain step would build the note on top of the rejected drafts.
    session
        .accept(0, accepted as u16)
        .map_err(|e| PoisonedContext(format!("accept failed: {e}")))?;

    // Rejected guesses are in the cache and have to come out. Nothing to do when every
    // guess was accepted — the cache already ends exactly at the committed length.
    if accepted < drafts.len() {
        let keep_upto = n_cur + 1 + accepted as i32;
        let trimmed = session.clear_target_kv_cache_seq(Some(0), Some(keep_upto as u32), None);
        if !matches!(trimmed, Ok(true)) {
            return Err(PoisonedContext(format!(
                "speculative rollback to {keep_upto} was refused by llama.cpp ({trimmed:?})"
            ))
            .into());
        }
    }

    let mut committed = Vec::with_capacity(accepted + 1);
    committed.push(carry);
    committed.extend_from_slice(&drafts[..accepted]);
    Ok((committed, next))
}

/// Whether [`NO_SPECULATIVE_ENV`]'s value turns speculative decoding off (decision #11).
///
/// Presence, not equality to `"1"`: someone who exports the variable at all means "off",
/// and the alternative is a benchmark that silently keeps the feature on because the shell
/// was handed `true` or `yes`. `0` and the empty string are the two spellings that read as
/// "leave it alone", so `MEDSCRIBE_NO_SPECULATIVE=0` is a usable way to say "on".
fn speculative_disabled_by_env(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// The switch as the process sees it. Shared by the RAM guard and `load_draft`, which must
/// agree: a floor that charges for a draft the switch skips fails the load for nothing.
fn speculative_off_by_env() -> bool {
    speculative_disabled_by_env(std::env::var(NO_SPECULATIVE_ENV).ok().as_deref())
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
    fn no_speculative_env_is_read_as_presence_not_as_the_literal_one() {
        // Unset (and the two "leave it on" spellings) keep the feature available.
        assert!(!speculative_disabled_by_env(None));
        assert!(!speculative_disabled_by_env(Some("")));
        assert!(!speculative_disabled_by_env(Some("0")));
        // Anything else is off — a benchmark run must never silently keep it on because
        // the shell was handed something other than `1`.
        assert!(speculative_disabled_by_env(Some("1")));
        assert!(speculative_disabled_by_env(Some("true")));
        assert!(speculative_disabled_by_env(Some("yes")));
    }

    #[test]
    fn draft_length_satisfies_the_mtp_session_bounds() {
        // llama-cpp-4 rejects the session outright outside these — a K_DRAFT edit that
        // broke them would surface as "mtp session rejected" on a doctor's machine
        // rather than here. Only K_DRAFT is checkable: `n_rs_seq` is clamped to 0 for
        // `gemma4-assistant` (`llama-context.cpp:105`), so no value of it can fail.
        assert!(
            (1..=4096).contains(&K_DRAFT),
            "MAX_SPECULATIVE_DRAFT_TOKENS"
        );
        // Was a tautology — `K_DRAFT.max(4) >= K_DRAFT` holds for every K_DRAFT:
        // assert!(K_DRAFT.max(4) >= K_DRAFT, "n_rs_seq must cover n_draft_max");
    }

    /// Records every slot the rule asked about, so laziness can be asserted directly.
    fn run_accept(drafts: &[i32], targets: &[i32]) -> (usize, i32, Vec<i32>) {
        use std::cell::RefCell;
        let asked = RefCell::new(Vec::new());
        let drafts: Vec<LlamaToken> = drafts.iter().copied().map(LlamaToken).collect();
        let (accepted, next) = accept_prefix(&drafts, |i| {
            asked.borrow_mut().push(i);
            LlamaToken(targets[i as usize])
        });
        (accepted, next.0, asked.into_inner())
    }

    #[test]
    fn acceptance_stops_at_the_first_mismatch() {
        // The plan's worked example: two guesses match, the third does not, and the
        // target's own choice at that slot becomes the next round's carry.
        let (accepted, next, asked) = run_accept(&[10, 11, 12], &[10, 11, 99, 0]);
        assert_eq!(accepted, 2);
        assert_eq!(
            next, 99,
            "the target's own token replaces the rejected guess"
        );
        assert_eq!(
            asked,
            vec![0, 1, 2],
            "no slot past the mismatch is consulted"
        );
    }

    #[test]
    fn acceptance_keeps_every_guess_when_the_target_agrees_throughout() {
        let (accepted, next, asked) = run_accept(&[10, 11, 12], &[10, 11, 12, 42]);
        assert_eq!(accepted, 3);
        assert_eq!(next, 42);
        assert_eq!(asked, vec![0, 1, 2, 3]);
    }

    #[test]
    fn acceptance_rejects_the_first_guess_for_one_sample() {
        // The worst case, and the cost side of the trade: one token committed, same as the
        // plain loop, and only one slot consulted.
        let (accepted, next, asked) = run_accept(&[10, 11, 12], &[77, 0, 0, 0]);
        assert_eq!(accepted, 0);
        assert_eq!(next, 77);
        assert_eq!(
            asked,
            vec![0],
            "a rejected slot must not cost the later samples"
        );
    }

    #[test]
    fn acceptance_with_no_drafts_still_yields_the_targets_own_token() {
        let (accepted, next, asked) = run_accept(&[], &[55]);
        assert_eq!(accepted, 0);
        assert_eq!(next, 55);
        assert_eq!(asked, vec![0]);
    }

    #[test]
    fn spec_stats_report_zero_rather_than_dividing_by_zero() {
        let empty = SpecStats::default();
        assert_eq!(empty.acceptance_pct(), 0.0);
        assert_eq!(empty.tokens_per_pass(), 0.0);

        // Three rounds of K=3: 9 drafted, 6 kept, so 6 + 3 carries = 9 committed over 3
        // target passes — three tokens per expensive weight read instead of one.
        let s = SpecStats {
            rounds: 3,
            drafted: 9,
            accepted: 6,
            target_passes: 3,
            committed: 9,
        };
        assert!((s.acceptance_pct() - 66.666_67).abs() < 0.01);
        assert!((s.tokens_per_pass() - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn remove_superseded_blobs_ignores_a_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("no-such-dir").join("prefix_kv_x.bin");
        LlmEngine::remove_superseded_blobs(&gone); // must not panic
    }
}

// fn guard_available_ram(model_path: &Path) -> Result<()> {
fn guard_available_ram(model_path: &Path, draft_path: Option<&Path>) -> Result<()> {
    let model_bytes = std::fs::metadata(model_path)
        .map(|m| m.len())
        .map_err(|e| anyhow!("model file not found at {}: {e}", model_path.display()))?;
    // The draft is co-resident with the target (spec-decoding decision #6), so its
    // weights are part of the floor: without this a machine that only just clears the
    // target passes the guard and then OOMs on the draft. Absent draft → 0 (§8.10).
    let draft_bytes = draft_path
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);
    // Decode buffers/KV-cache need headroom beyond the weights themselves.
    const WORKING_MARGIN: u64 = 2 * 1024 * 1024 * 1024;
    // let needed = model_bytes + WORKING_MARGIN;
    let needed = model_bytes + draft_bytes + WORKING_MARGIN;

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
