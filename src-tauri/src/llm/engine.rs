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
use std::sync::atomic::{AtomicBool, Ordering};
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
}

// Tuning constants (design §8.2 "set at implementation via benchmarking"): kept
// conservative so the context fits a realistic consult without reserving RAM the
// §7 budget needs.
const N_CTX: u32 = 8192; // prompt + transcript + reasoning + note; well under the model maxima
const MAX_OUTPUT_TOKENS: i32 = 1536; // ceiling for the SOAP note itself (post-reasoning)
const MAX_REASONING_TOKENS: i32 = 1024; // separate cap for the <think> scratchpad (§8.3) so a
                                        // verbose CoT can't eat the note's budget; tunable (§8.2)
const SAMPLE_TEMP: f32 = 0.2; // low temperature → near-deterministic, low hallucination

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
/// on every note — the KV-cache reuse of §8.7. `prefix_tokens` pins which token
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
/// restoring the cached prefix KV state into it (§8.7) when available.
pub struct LlmEngine {
    backend: LlamaBackend,
    model: Mutex<Option<LlamaModel>>,
    /// Cached prefix KV state (§8.7), primed on load ([`warmup`]) and dropped on
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
    n_threads: i32,
    /// Serializes [`ensure_loaded`] so the co-resident background preload (design
    /// §8.2 startup fix) and an early Generate can't both load the model at once.
    /// Held only across the load itself, never nested inside the `model` lock.
    load_lock: Mutex<()>,
}

impl LlmEngine {
    /// Create the engine for `kind`, resolving the model file across `model_dirs`
    /// (first existing wins). The model itself is not loaded until [`ensure_loaded`];
    /// `n_threads` (physical // 2, design §8.2) is applied to both decode and prefill
    /// (see [`new_context`]).
    pub fn new(kind: LlmModel, model_dirs: Vec<PathBuf>, n_threads: i32) -> Result<Self> {
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
            n_threads: n_threads.max(1),
            load_lock: Mutex::new(()),
        })
    }

    pub fn model_kind(&self) -> LlmModel {
        self.kind
    }

    pub fn is_loaded(&self) -> bool {
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
        let params = LlamaModelParams::default(); // mmap default; CPU-only build
        let model = LlamaModel::load_from_file(&self.backend, &path, &params).map_err(|e| {
            // §10.3 `[LOAD] SLM load failed: {e}` (both sinks). Sanitized: the llama.cpp
            // load error embeds the GGUF path (username = PII).
            let msg = crate::telemetry::sanitize_error(&e.to_string());
            log::error!("[LOAD] SLM load failed: {msg}");
            crate::telemetry::track_event("slm_load_failed", serde_json::json!({ "error": msg }));
            anyhow!("failed to load LLM model {}: {e}", path.display())
        })?;
        *self.lock_model() = Some(model);
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
        let t_warm = Instant::now();
        if let Err(e) = self.warmup() {
            warn!("LLM warmup pass failed (non-fatal): {e}");
        } else {
            info!(
                "[LOAD] SLM prefix KV cache primed in {:.1}s",
                t_warm.elapsed().as_secs_f32()
            );
        }
        Ok(())
    }

    pub fn unload(&self) {
        *self.lock_model() = None;
        // Drop the cached prefix state with the model: it belongs to this model
        // (and would be re-primed cold on the next load, §8.7).
        *self.prefix_cache.lock().unwrap() = None;
    }

    /// Generate a SOAP note from `transcript`, streaming each decoded piece to
    /// `on_token` and polling `cancel` between tokens. Returns the note markdown, or
    /// `None` if cancelled (the caller discards the partial, §8.4). The model reasons
    /// in a private `<think>` block first; only the note after
    /// [`prompt::REASONING_BOUNDARY`] is streamed and returned (§8.3).
    ///
    /// The prompt is built as the fixed prefix + this transcript's tail; when the
    /// prefix's KV state is cached (§8.7) it is restored into the fresh context and
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

    /// Prime the prefix cache (§8.7): decode the fixed prefix once and serialize the
    /// resulting context state so later notes can restore it instead of re-decoding.
    /// Called right after a load — this replaces the old throwaway warmup pass, and
    /// doubles as the warmup (the first real decode after a load is the slow one).
    /// Failure is non-fatal: generation falls back to a full per-note decode.
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
        // seq 0 — see the batch below). The sequence-scoped size is the cells actually
        // used (~the prefix), not the N_CTX maximum the whole-context `get_state_size`
        // reports — so priming doesn't briefly allocate and zero ~1 GB right after the
        // model load, which would spike RAM against the §7 co-resident budget.
        let mut state = vec![0u8; ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty())];
        let written = unsafe {
            ctx.state_seq_get_data_ext(state.as_mut_ptr(), 0, LlamaStateSeqFlags::empty())
        };
        state.truncate(written);

        *self.prefix_cache.lock().unwrap() = Some(PrefixCache {
            kind,
            prefix_tokens,
            state,
        });
        Ok(())
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
    /// restored into it, so nothing needs to hold a context across notes (§8.7).
    fn new_context<'a>(&'a self, model: &'a LlamaModel) -> Result<LlamaContext<'a>> {
        // Both phases run on the tuned thread count (physical // 2, design §8.2): decode
        // (`n_threads`) is memory-bandwidth-bound and regresses past a fraction of the
        // cores, and we cap prefill (`n_threads_batch`) at the same count rather than let
        // it fall to the llama.cpp default of 4 — 4 would throttle the transcript-tail
        // prefill (the uncached part of every note) on any many-core machine.
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_threads(self.n_threads)
            .with_n_threads_batch(self.n_threads);
        model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| anyhow!("failed to create LLM context: {e}"))
    }

    fn lock_model(&self) -> MutexGuard<'_, Option<LlamaModel>> {
        self.model.lock().unwrap_or_else(|p| p.into_inner())
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
