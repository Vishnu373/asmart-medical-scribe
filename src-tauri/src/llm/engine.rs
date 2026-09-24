//! In-process GGUF note generation over `llama-cpp-4` (design §8.2). CPU-only, no
//! server, no network — generation stays on-device (NFR-6).
//!
//! Like the STT engine, the native model sits behind the `NoteGenerator` trait so
//! the GENERATING state machine is testable without it; this file holds the one
//! part that needs the real llama.cpp binding and is verified by building/running
//! `cargo test` on Windows (the binding compiles native code; it is not exercised
//! on the Linux dev box). The streaming/cancel/persist orchestration around it
//! lives in `generator.rs`.

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use anyhow::{anyhow, Result};
use log::{info, warn};

// use llama_cpp_4::context::params::{LlamaContextParams, LlamaContextType};
use llama_cpp_4::context::params::LlamaContextParams;
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::params::LlamaModelParams;
use llama_cpp_4::model::{AddBos, LlamaModel, Special};
// use llama_cpp_4::mtp::MtpSession;
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;

use super::mtp::{self, MtpContexts, MtpDecoder};
use super::prompt;

/// The note-generation model (design §8.2). A single on-device model —
/// `gemma-4-E2B-it-UD-Q4_K_XL` — behind the `NoteGenerator` interface. Kept as an
/// enum (one variant today) so `prompt` / [`PrefixCache`] keep a
/// typed dispatch point if a second model is ever added. The installer bundles no
/// LLM; it is downloaded once at first-run Setup (D3, `models`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmModel {
    /// Gemma 3n E2B instruct, Unsloth dynamic Q4_K_XL (GGUF).
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

    /// The MTP draft GGUF resolved alongside [`file_name`] — a standalone 4-block
    /// `gemma4-assistant` model carrying the `nextn` projection heads, loaded as its
    /// own `LlamaModel` to back the speculative draft context.
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
const MAX_REASONING_TOKENS: i32 = 512; // separate cap for the <think> scratchpad (§8.3); observed
                                       // reasoning runs ~240–250 tokens, so 512 leaves headroom
const SAMPLE_TEMP: f32 = 0.2; // low temperature → near-deterministic, low hallucination

// /// Draft tokens the MTP head proposes per verification step (§8.2). 3 is upstream's
// /// own CLI default; the optimum is model/quant dependent, so B5 re-tunes it.
// const N_DRAFT_MAX: i32 = 3;
// ^ moved to `mtp::N_DRAFT`.

/// Decode buffers and the KV cache need headroom beyond the weights themselves
/// (§8.4). Demanded of the target only — the draft adds neither.
const WORKING_MARGIN: u64 = 2 * 1024 * 1024 * 1024;

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
    /// The MTP draft model backing the speculative draft context (§8.2). Loaded and
    /// dropped with the target: a note's context pair needs both, so one without the
    /// other is a failed load, not a slow one.
    draft_model: Mutex<Option<LlamaModel>>,
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
    /// Decode-phase threads (physical // 2); prefill left at the llama.cpp default
    /// (design §8.2 — decode is bandwidth-bound and stops scaling; prefill is not).
    /// `None` when the physical core count was unavailable — both phases then fall
    /// back to llama.cpp's own defaults rather than a guessed count.
    n_threads: Option<i32>,
    // n_threads: i32,
    /// Serializes [`ensure_loaded`] so the co-resident background preload (design
    /// §8.2 startup fix) and an early Generate can't both load the model at once.
    /// Held only across the load itself, never nested inside the `model` lock.
    load_lock: Mutex<()>,
    /// `MTP_ENABLED`, read once at construction. Off only for `MTP_ENABLED=0`; when
    /// off, the draft model is never loaded.
    mtp_enabled: bool,
}

impl LlmEngine {
    /// Create the engine for `kind`, resolving the model file across `model_dirs`
    /// (first existing wins). The model itself is not loaded until [`ensure_loaded`];
    /// `n_threads` (physical // 2, design §8.2) is applied to both decode and prefill
    /// (see [`new_context`]); `None` leaves both at the llama.cpp defaults.
    pub fn new(kind: LlmModel, model_dirs: Vec<PathBuf>, n_threads: Option<i32>) -> Result<Self> {
        let mut backend =
            LlamaBackend::init().map_err(|e| anyhow!("llama backend init failed: {e}"))?;
        // llama.cpp/ggml dump per-tensor load and context noise straight to C++ stderr,
        // bypassing Rust's `log` filter. `void_logs` installs a no-op callback via
        // `llama_log_set`, which forwards to `ggml_log_set` — covers both. Errors still
        // come back as `Result`s. Comment out to get the firehose back when debugging.
        backend.void_logs();
        let mtp_enabled = std::env::var("MTP_ENABLED").map_or(true, |v| v != "0");
        if mtp_enabled {
            info!("[LOAD] MTP enabled");
        } else {
            info!("[LOAD] MTP disabled (MTP_ENABLED=0)");
        }
        Ok(Self {
            backend,
            model: Mutex::new(None),
            draft_model: Mutex::new(None),
            prefix_cache: Mutex::new(None),
            kind,
            model_dirs,
            n_threads: n_threads.map(|n| n.max(1)),
            // n_threads: n_threads.max(1),
            load_lock: Mutex::new(()),
            mtp_enabled,
        })
    }

    pub fn model_kind(&self) -> LlmModel {
        self.kind
    }

    /// The note model. Deliberately not the MTP draft: the draft is a speedup whose
    /// absence must neither read as "still loading" (`get_llm_status`) nor send every
    /// later call back through the load body. It gets one attempt, beside the target.
    // /// Both models — the target and its MTP draft. `get_llm_status` reports "ready"
    // /// off this, and generation needs the pair, so a half-load is not loaded.
    pub fn is_loaded(&self) -> bool {
        // self.lock_model().is_some() && self.lock_draft_model().is_some()
        self.lock_model().is_some()
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
        // Each step below is gated on its own state, so a failure in one can never skip
        // another: a missing draft must still leave the target loaded and primed.
        // let target_was_loaded = self.lock_model().is_some();
        if self.lock_model().is_none() {
            let file = kind.file_name();
            let path = crate::models::resolve(file, &self.model_dirs).ok_or_else(|| {
                anyhow!(
                    "model file {file} not found in {:?} — the bundled model is missing, \
                     or (for the optional tier) it has not been downloaded yet",
                    self.model_dirs
                )
            })?;
            guard_available_ram(&path, WORKING_MARGIN)?;

            info!("[LOAD] loading SLM: {file}"); // §10.3
            let t_load = Instant::now();
            let params = LlamaModelParams::default(); // mmap default; CPU-only build
            let model = LlamaModel::load_from_file(&self.backend, &path, &params).map_err(|e| {
                // §10.3 `[LOAD] SLM load failed: {e}` (both sinks). Sanitized: the llama.cpp
                // load error embeds the GGUF path (username = PII).
                let msg = crate::telemetry::sanitize_error(&e.to_string());
                log::error!("[LOAD] SLM load failed: {msg}");
                crate::telemetry::track_event(
                    "slm_load_failed",
                    serde_json::json!({ "error": msg }),
                );
                anyhow!("failed to load LLM model {}: {e}", path.display())
            })?;
            *self.lock_model() = Some(model);
            info!(
                "[LOAD] SLM model loaded: {:.1}s", // §10.3
                t_load.elapsed().as_secs_f32()
            );
            // info!("Loaded LLM model: {:?}", kind);
        }

        // The prefix cache belongs to the target, so a draft-only load skips it — the
        // blob is already in memory from the launch that loaded the target.
        // if target_was_loaded {
        //     return Ok(());
        // }
        // Prime as soon as the target is in memory, gated on the cache itself rather than on
        // whether this call did the load: a retry after a failed draft load must still get
        // here, or the process primes on no launch at all.
        if self.prefix_cache.lock().unwrap().is_none() {
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
        }

        // The draft is a speedup, not a requirement, so its failure is non-fatal: a missing
        // or broken draft must not cost the target its prime — nor block the installer's
        // `--prime-kv` pass (§8.7), which runs before Setup has ever downloaded the draft.
        if self.mtp_enabled && self.lock_draft_model().is_none() {
            match self.load_draft(kind) {
                Ok(draft) => *self.lock_draft_model() = Some(draft),
                Err(e) => warn!("[LOAD] SLM draft unavailable ({e}) — no speculative decoding"),
            }
        }
        Ok(())
    }

    /// Load the draft into an engine whose target is already resident — the upgrade case,
    /// where Setup downloads the draft after launch loaded Gemma without it. No-op otherwise.
    pub fn load_draft_if_target_loaded(&self) {
        // Same lock as `ensure_loaded`: an in-flight preload finishes (loading the draft) first.
        let _load = self.load_lock.lock().unwrap_or_else(|p| p.into_inner());
        if !self.mtp_enabled || self.lock_model().is_none() || self.lock_draft_model().is_some() {
            return;
        }
        match self.load_draft(self.model_kind()) {
            Ok(draft) => *self.lock_draft_model() = Some(draft),
            Err(e) => warn!("[LOAD] SLM draft unavailable ({e}) — no speculative decoding"),
        }
    }

    /// Load the MTP draft model (design §8.x speculative decoding). Split out of
    /// `ensure_loaded` so its failure can be logged and swallowed there.
    fn load_draft(&self, kind: LlmModel) -> Result<LlamaModel> {
        let file = kind.draft_file_name();
        let path = crate::models::resolve(file, &self.model_dirs).ok_or_else(|| {
            anyhow!(
                "MTP draft model file {file} not found in {:?} — Setup downloads it \
                 beside the note model",
                self.model_dirs
            )
        })?;
        // No working margin: 93 MB of weights, and the draft context borrows the
        // target's KV cache instead of allocating a second one (see `new_draft_context`).
        guard_available_ram(&path, 0)?;

        info!("[LOAD] loading SLM draft: {file}"); // §10.3
        let t_load = Instant::now();
        let params = LlamaModelParams::default();
        let draft = LlamaModel::load_from_file(&self.backend, &path, &params).map_err(|e| {
            // Same sanitizing as the target: the llama.cpp load error embeds the GGUF path.
            let msg = crate::telemetry::sanitize_error(&e.to_string());
            log::error!("[LOAD] SLM draft load failed: {msg}");
            crate::telemetry::track_event(
                "slm_draft_load_failed",
                serde_json::json!({ "error": msg }),
            );
            anyhow!("failed to load the MTP draft model {}: {e}", path.display())
        })?;
        info!(
            "[LOAD] SLM draft model loaded: {:.1}s", // §10.3
            t_load.elapsed().as_secs_f32()
        );
        Ok(draft)
    }

    pub fn unload(&self) {
        *self.lock_model() = None;
        *self.lock_draft_model() = None;
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
        let kind = self.model_kind();
        let prompt = prompt::build_prompt(kind, transcript);

        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;
        // Optional by design (§8.2): a missing or broken draft costs the note its
        // speculation, never the note itself.
        // let draft_model = draft_guard
        //     .as_ref()
        //     .ok_or_else(|| anyhow!("MTP draft model is not loaded"))?;
        let draft_guard = self.lock_draft_model();

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
        // let prompt_budget = N_CTX as i32 - output_budget;
        // ^ left one spare cell, but every speculation round transiently occupies
        // `n_cur + 1 ..= n_cur + N_DRAFT_MAX` as well (§8.2), so a full-length note ran
        // the target out of KV slots mid-decode. Reserved unconditionally — three cells
        // are cheaper than a budget that depends on whether the draft loaded.
        // let prompt_budget = N_CTX as i32 - output_budget - N_DRAFT_MAX;
        let prompt_budget = N_CTX as i32 - output_budget - mtp::N_DRAFT;
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

        // let mut ctx = self.new_context(model)?;
        // let mut draft_ctx = draft_guard
        //     .as_ref()
        //     .map(|draft_model| self.new_draft_context(draft_model, &ctx))
        //     .transpose()?;
        // ^ two independent locals, where only the declaration order stopped the draft
        // outliving the cells it aliases. `Contexts` owns both and fixes the drop order.
        //
        // Built before the prefix restore: the draft context is wired into the target's
        // KV cache, so it must exist before anything writes cells into it.
        // let mut contexts = match draft_guard.as_ref() {
        //     Some(draft_model) => self.pair_with_draft(draft_model, self.new_context(model)?)?,
        //     None => Contexts::target_only(self.new_context(model)?),
        // };
        // let (ctx, draft_ctx) = contexts.split_mut();
        let target = self.new_context(model)?;
        let mut contexts = match (self.mtp_enabled, draft_guard.as_ref()) {
            (true, Some(draft_model)) => MtpContexts::with_draft(
                &self.backend,
                draft_model,
                target,
                self.n_threads,
                note_id,
            ),
            (true, None) => {
                warn!("[GENERATE] {note_id} draft model missing — generating with the main model only");
                MtpContexts::target_only(target)
            }
            (false, _) => MtpContexts::target_only(target),
        };
        // Restore the cached prefix KV if this prompt starts with exactly its
        // tokens; otherwise start from position 0 (full decode, the fallback).
        // let start = self.restore_prefix(ctx, kind, &tokens);
        let start = self.restore_prefix(contexts.target_mut(), kind, &tokens);
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
        // Every target decode runs through the decoder from here on. With a draft it
        // harvests the target's hidden states into MTP state as it goes, which is what
        // the drafting reads; without one it is a plain target decode and the loop
        // degenerates to one token per decode.
        // let mut decoder = match draft_ctx {
        //     Some(draft_ctx) => Decoder::Speculative(
        //         MtpSession::new(ctx, draft_ctx, 1, N_DRAFT_MAX)
        //             .map_err(|e| anyhow!("failed to create the MTP draft session: {e}"))?,
        //     ),
        //     None => {
        //         warn!("[GENERATE] {note_id} no MTP draft loaded — decoding without speculation");
        //         Decoder::Plain(ctx)
        //     }
        // };
        // let mut decoder = contexts.decoder(note_id)?;
        // let note = self.decode_and_generate(
        //     note_id,
        //     &mut decoder,
        //     …
        // );
        // if decoder.is_speculative() { info!(…) }
        // ^ a failed MTP session failed the note; `with_decoder` falls back to plain.
        let note = contexts.with_decoder(note_id, |decoder| {
            let note = self.decode_and_generate(
                note_id,
                decoder,
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
            );
            // Logged on every exit (done, cancelled, failed) once the MTP session existed.
            if decoder.is_speculative() {
                info!(
                    "[GENERATE] {note_id} MTP — {} drafted, {} accepted",
                    decoder.drafted, decoder.accepted
                );
            }
            note
        });
        let note = note?;
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
        // Priming is a one-off prefill with no note in flight, so it gets every physical core.
        if let Some(physical) = sysinfo::System::new().physical_core_count() {
            ctx.set_n_threads(ctx.n_threads(), physical as i32);
        }
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
    /// of the prompt prefix and the llama-cpp-sys-4 version (stamped by `build.rs`, since that
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
        // Stamped with llama-cpp-4's version before the sys fix; see build.rs:
        // let version = env!("LLAMA_CPP_4_VERSION");
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
        // Returns the bytes read; `0` means llama.cpp rejected the blob (a state from a
        // different model/params, or a layout it can't parse). §8.7
        let read = ctx.state_seq_set_data_ext(&pc.state, 0, 0);
        if read == 0 {
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
        // decoder: &mut Decoder<'_, '_>,
        decoder: &mut MtpDecoder<'_, '_>,
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
        decoder
            .decode(&mut batch)
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
        // Speculation state (§8.2). `verified` holds tokens the target has already
        // confirmed and decoded but not yet emitted; `seed` is the token sampled from the
        // newest logits, which is not in the KV yet and starts the next round. Everything
        // below still consumes one token at a time.
        let mut verified: VecDeque<LlamaToken> = VecDeque::new();
        let mut seed: Option<LlamaToken> = None;
        // // Speculation is a speedup, not a requirement (§8.2). A failed proposal latches it
        // // off for the rest of the note instead of failing the note: near-greedy decoding
        // // means a retry hits the same draft with the same state and fails identically.
        // let mut drafting = true;
        // ^ the draft-failure latch now lives in `MtpDecoder`.
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
                    // Whatever was drafted past this point continues the reasoning we
                    // are about to cut off, so drop the unemitted tail and roll the KV
                    // back to the last emitted token — that tail and the last round's
                    // rejected drafts both sit where the boundary is about to go.
                    info!(
                        "[GENERATE] {note_id} reasoning cap reached ({} tokens) — forcing {}",
                        s.max_reasoning_tokens, s.boundary
                    );
                    n_cur -= verified.len() as i32;
                    verified.clear();
                    seed = None; // the next sample comes from the injected boundary
                    decoder.rollback(n_cur)?;
                    // ^ was `rollback_kv(decoder, n_cur)?`.
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
                    decoder
                        .decode(&mut batch)
                        .map_err(|e| anyhow!("boundary injection decode failed: {e}"))?;
                    raw.push_str(s.boundary);
                    boundary_passed = true;
                    continue; // next sample reads the boundary's logits → first note token
                }
            }

            // let token = sampler.sample(session.target_context(), batch.n_tokens() - 1);
            // sampler.accept(token);
            // ^ superseded by the speculation round: the target still decides every token,
            //   but up to N_DRAFT_MAX + 1 of them now come out of a single decode (§8.2).
            if verified.is_empty() {
                // Only the prefill and a forced boundary leave `seed` empty; after a round
                // it holds the token sampled past the accepted run, already paid for.
                let id_last = match seed.take() {
                    Some(t) => t,
                    None => {
                        let t = sampler.sample(decoder.context(), batch.n_tokens() - 1);
                        sampler.accept(t);
                        t
                    }
                };
                // verified.push_back(id_last);
                //
                // // Before drafting, not after: last round's rejected drafts still occupy
                // // `n_cur..`, the positions this round is about to write, and the draft
                // // reads the target's cells (§8.2: it mirrors them), so it would otherwise
                // // draft a continuation of tokens the target already threw away.
                // rollback_kv(decoder, n_cur)?;
                // // let drafts = decoder.draft(n_cur, id_last)?;
                // // ^ a broken draft head took the note down with it, a failure mode the
                // // pre-speculation path did not have.
                // let drafts = if drafting {
                //     decoder.draft(n_cur, id_last).unwrap_or_else(|e| {
                //         drafting = false;
                //         warn!(
                //             "[GENERATE] {note_id} MTP draft failed ({e}) — finishing the note without speculation"
                //         );
                //         Vec::new()
                //     })
                // } else {
                //     Vec::new()
                // };
                //
                // batch.clear();
                // batch
                //     .add(id_last, n_cur, &[0], true)
                //     .map_err(|e| anyhow!("failed to add a token to the batch: {e}"))?;
                // for (i, d) in drafts.iter().enumerate() {
                //     batch
                //         .add(*d, n_cur + 1 + i as i32, &[0], true)
                //         .map_err(|e| anyhow!("failed to add a draft token to the batch: {e}"))?;
                // }
                // // No rollback here: `draft` leaves nothing behind — llama.cpp returns from
                // // `apply_ubatch` without touching a cell while the draft's cache mirrors
                // // the target's. The only cells ever in the way are the target's own
                // // rejected drafts, already cleared above before they could be drafted on.
                // decoder
                //     .decode(&mut batch)
                //     .map_err(|e| anyhow!("token decode failed: {e}"))?;
                //
                // // Row 0 of the logits is what follows the seed, row i + 1 what follows
                // // draft i. A row that agrees with its draft accepts it; the first
                // // disagreement — or the row past the last draft — is the target's own
                // // next token and seeds the next round, so nothing sampled is wasted and
                // // the emitted sequence is exactly what the target alone would produce.
                // let mut n_accepted = 0usize;
                // seed = Some(loop {
                //     let t = sampler.sample(decoder.context(), n_accepted as i32);
                //     sampler.accept(t);
                //     match drafts.get(n_accepted) {
                //         Some(d) if *d == t => {
                //             verified.push_back(t);
                //             n_accepted += 1;
                //         }
                //         _ => break t,
                //     }
                // });
                // if !drafts.is_empty() {
                //     // Resyncs the draft head's carried hidden state to the accepted prefix.
                //     // Skipped when nothing was proposed — there is no proposal to answer,
                //     // which is also every round of the no-draft path.
                //     decoder.accept(n_accepted)?;
                // }
                // // Everything in `verified` is now in the target's KV; `seed` is not.
                // n_cur += verified.len() as i32;
                // Draft → verify → accept lives in `mtp.rs`; `tokens` are already in the KV.
                let round = decoder.round(id_last, n_cur, &mut sampler)?;
                n_cur += round.tokens.len() as i32;
                verified.extend(round.tokens);
                seed = Some(round.next_seed);
            }
            let token = verified
                .pop_front()
                .ok_or_else(|| anyhow!("the speculation round produced no token"))?;
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

            // The speculation round already decoded this token — and the drafts that
            // rode with it — into the target, so the per-token decode is gone:
            // batch.clear();
            // batch
            //     .add(token, n_cur, &[0], true)
            //     .map_err(|e| anyhow!("failed to add a token to the batch: {e}"))?;
            // n_cur += 1;
            // session
            //     .decode_target_and_process(&mut batch)
            //     .map_err(|e| anyhow!("token decode failed: {e}"))?;
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
        // Decode (`n_threads`, physical // 2, §8.2) is memory-bandwidth-bound and regresses
        // past a fraction of the cores. Unset values fall back to llama.cpp defaults.
        let mut ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(N_CTX));
        if let Some(n) = self.n_threads {
            ctx_params = ctx_params.with_n_threads(n);
        }
        // Prefill is compute-bound, so it gets every physical core.
        if let Some(physical) = sysinfo::System::new().physical_core_count() {
            ctx_params = ctx_params.with_n_threads_batch(physical as i32);
        }
        model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| anyhow!("failed to create LLM context: {e}"))
    }

    // /// The MTP draft context for `model`, paired with the target `target` (§8.2).
    // ///
    // /// `gemma4-assistant` is a draft-only architecture: llama.cpp refuses to build a
    // /// context for it without `ctx_other`, and uses that pairing to map the draft's four
    // /// blocks onto the target's last two KV layers rather than allocating a second cache
    // /// — hence the matching `N_CTX`.
    // ///
    // /// The target is taken by value and handed back inside [`Contexts`]: llama.cpp keeps
    // /// its pointer in the draft's `ctx_other` and aliases its KV cells for the draft's
    // /// whole life, yet the returned context borrows nothing, so only co-ownership can
    // /// stop the target being dropped first.
    // // fn new_draft_context<'a>(&'a self, model: &'a LlamaModel, target: &LlamaContext<'_>)
    // //     -> Result<LlamaContext<'a>>
    // // ^ handed back a free-standing draft context: nothing in the type system tied it to
    // // the target it aliases, so a swap of the two `let`s in `generate` was a UAF.
    // fn pair_with_draft<'a>(
    //     &'a self,
    //     model: &'a LlamaModel,
    //     target: LlamaContext<'a>,
    // ) -> Result<Contexts<'a>> {
    //     let mut ctx_params = LlamaContextParams::default()
    //         .with_n_ctx(NonZeroU32::new(N_CTX))
    //         .with_ctx_type(LlamaContextType::Mtp)
    //         .with_ctx_other(&target)
    //         // Dead for this arch (the draft is not recurrent), but `MtpSession` validates
    //         // it against `n_draft_max` for the architectures that are.
    //         .with_n_rs_seq(N_DRAFT_MAX.max(4) as u32);
    //     if let Some(n) = self.n_threads {
    //         ctx_params = ctx_params.with_n_threads(n).with_n_threads_batch(n);
    //     }
    //     let draft = model
    //         .new_context(&self.backend, ctx_params)
    //         .map_err(|e| anyhow!("failed to create the MTP draft context: {e}"))?;
    //     Ok(Contexts {
    //         draft: Some(draft),
    //         target,
    //     })
    // }
    // ^ replaced by `mtp::MtpContexts::with_draft`.

    fn lock_model(&self) -> MutexGuard<'_, Option<LlamaModel>> {
        self.model.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock_draft_model(&self) -> MutexGuard<'_, Option<LlamaModel>> {
        self.draft_model.lock().unwrap_or_else(|p| p.into_inner())
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

// /// The target context plus, when a draft model is loaded, the MTP draft context built
// /// against it (§8.2). Owning both is a tie the borrow checker cannot express on its own:
// /// `LlamaModel::new_context` returns a context that borrows nothing, while llama.cpp
// /// stores the target's pointer in the draft's `ctx_other` and aliases the target's KV
// /// cells for as long as the draft lives. Field order is drop order — the draft goes
// /// first, so the cells it points at are still there.
// struct Contexts<'a> {
//     draft: Option<LlamaContext<'a>>,
//     target: LlamaContext<'a>,
// }
//
// impl<'a> Contexts<'a> {
//     /// A run with no speculation: the target alone.
//     fn target_only(target: LlamaContext<'a>) -> Self {
//         Self {
//             draft: None,
//             target,
//         }
//     }
//
//     /// Both halves at once — `MtpSession` needs them mutably together.
//     fn split_mut(&mut self) -> (&mut LlamaContext<'a>, Option<&mut LlamaContext<'a>>) {
//         (&mut self.target, self.draft.as_mut())
//     }
// }
//
// /// How the generate loop decodes on the target: with MTP speculation when a draft model
// /// is loaded, plain otherwise. The draft is a speedup, not a requirement (§8.2), so its
// /// absence must not cost the clinician the note — `Plain` proposes nothing and the
// /// round degenerates to one target-decided token per decode.
// enum Decoder<'ctx, 'model> {
//     Speculative(MtpSession<'ctx, 'model>),
//     Plain(&'ctx mut LlamaContext<'model>),
// }
//
// impl<'ctx, 'model> Decoder<'ctx, 'model> {
//     /// The target context — what every sample reads its logits from.
//     fn context(&self) -> &LlamaContext<'model> {
//         match self {
//             Self::Speculative(s) => s.target_context(),
//             Self::Plain(ctx) => ctx,
//         }
//     }
//
//     /// Decode one batch on the target, harvesting it into MTP state when speculating.
//     fn decode(&mut self, batch: &mut LlamaBatch) -> Result<()> {
//         match self {
//             Self::Speculative(s) => s
//                 .decode_target_and_process(batch)
//                 .map_err(|e| anyhow!("{e}")),
//             Self::Plain(ctx) => ctx.decode(batch).map_err(|e| anyhow!("{e}")),
//         }
//     }
//
//     /// Propose up to `N_DRAFT_MAX` continuations of `id_last`; none without a draft.
//     fn draft(&mut self, n_past: i32, id_last: LlamaToken) -> Result<Vec<LlamaToken>> {
//         match self {
//             Self::Speculative(s) => s
//                 .draft(0, n_past, id_last)
//                 .map_err(|e| anyhow!("MTP draft failed: {e}")),
//             Self::Plain(_) => Ok(Vec::new()),
//         }
//     }
//
//     /// Answer the outstanding proposal with how much of it the target kept.
//     fn accept(&mut self, n_accepted: usize) -> Result<()> {
//         match self {
//             Self::Speculative(s) => s
//                 .accept(0, n_accepted as u16)
//                 .map_err(|e| anyhow!("failed to accept {n_accepted} draft tokens: {e}")),
//             Self::Plain(_) => Ok(()),
//         }
//     }
// }
//
// /// Drop every target KV cell at or past `n_past` on sequence 0 (§8.2). What occupies those
// /// positions is the target's own work — last round's rejected drafts, or the unemitted tail
// /// at a forced boundary. The draft context marks no cells of its own: llama.cpp skips
// /// `apply_ubatch` entirely while its cache mirrors the target's, so it only ever reads.
// fn rollback_kv(decoder: &mut Decoder<'_, '_>, n_past: i32) -> Result<()> {
//     // let removed = match decoder { … }
//     // .map_err(…)?;
//     // if removed { Ok(()) } else { Err(anyhow!("the target KV cache refused …")) }
//     // ^ dead guard: `llama_kv_cache::seq_rm` returns true on every path. Only a recurrent
//     // cache can refuse a partial removal, and neither context has one.
//     match decoder {
//         Decoder::Speculative(s) => s.clear_target_kv_cache_seq(Some(0), Some(n_past as u32), None),
//         Decoder::Plain(ctx) => ctx.clear_kv_cache_seq(Some(0), Some(n_past as u32), None),
//     }
//     .map_err(|e| anyhow!("failed to clear the speculative KV range: {e}"))?;
//     // Ask the cells instead of trusting the return value. Stale cells past `n_past` are
//     // attended alongside the tokens that replace them, so a removal that did not take is a
//     // corrupt note, not a slow one.
//     let highest = decoder.context().kv_cache_seq_pos_max(0);
//     if highest >= n_past {
//         return Err(anyhow!(
//             "the target KV cache still holds position {highest} after the removal at {n_past}"
//         ));
//     }
//     Ok(())
// }
// ^ replaced by `mtp::MtpContexts` / `mtp::MtpDecoder`.

/// Fail the load if free RAM is below the model file size plus a working margin
/// (design §8.4): better a graceful error in IDLE than a mid-load OOM crash.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_superseded_blobs_deletes_only_other_prefix_kv_files() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("prefix_kv_gemma.gguf_abc123_0.7.0.bin");

        // The current blob, two superseded ones, a crashed write's leftover, and the
        // model weights sitting in the same dir.
        let stale = dir.path().join("prefix_kv_gemma.gguf_abc123_0.6.1.bin");
        let old_prompt = dir.path().join("prefix_kv_gemma.gguf_deadbe_0.7.0.bin");
        let leftover = dir.path().join("prefix_kv_gemma.gguf_abc123_0.7.0.tmp");
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

fn guard_available_ram(model_path: &Path, margin: u64) -> Result<()> {
    let model_bytes = std::fs::metadata(model_path)
        .map(|m| m.len())
        .map_err(|e| anyhow!("model file not found at {}: {e}", model_path.display()))?;
    let needed = model_bytes + margin;

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
