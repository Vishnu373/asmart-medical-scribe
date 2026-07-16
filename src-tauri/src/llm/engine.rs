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

use super::correction;
use super::prompt;

/// The note-generation model (design §8.2). A single on-device model —
/// `gemma-4-E2B-it-UD-Q4_K_XL` — behind the `NoteGenerator` interface. Kept as an
/// enum (one variant today) so `prompt` / `correction` / [`PrefixCache`] keep a
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
const N_CTX: u32 = 8192; // prompt + transcript + note; well under the model maxima
const MAX_OUTPUT_TOKENS: i32 = 1536; // generous ceiling for a SOAP note
const MAX_CORRECTION_TOKENS: i32 = 512; // JSON-lines suggestions are short; decode-light (§6.7)
const SAMPLE_TEMP: f32 = 0.2; // low temperature → near-deterministic, low hallucination

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
/// once; the model is loaded lazily (swap mode) or at startup (co-resident) and
/// can be unloaded to release RAM. Generation builds a fresh context each run,
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
    n_threads: i32,
    /// Serializes [`ensure_loaded`] so the co-resident background preload (design
    /// §8.2 startup fix) and an early Generate can't both load the model at once.
    /// Held only across the load itself, never nested inside the `model` lock.
    load_lock: Mutex<()>,
}

impl LlmEngine {
    /// Create the engine for `kind`, resolving the model file across `model_dirs`
    /// (first existing wins). The model itself is not loaded until [`ensure_loaded`];
    /// `n_threads` is scaled to the machine's physical cores (design §8.2).
    pub fn new(kind: LlmModel, model_dirs: Vec<PathBuf>, n_threads: i32) -> Result<Self> {
        let backend = LlamaBackend::init().map_err(|e| anyhow!("llama backend init failed: {e}"))?;
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

        let params = LlamaModelParams::default(); // mmap default; CPU-only build
        let model = LlamaModel::load_from_file(&self.backend, &path, &params)
            .map_err(|e| anyhow!("failed to load LLM model {}: {e}", path.display()))?;
        *self.lock_model() = Some(model);
        info!("Loaded LLM model: {:?}", kind);

        // Warmup: the first inference after a load is slow (cold weights/buffers);
        // a tiny throwaway pass keeps the clinician's first real note at full
        // speed (design §8.4). Failure here is non-fatal — log and continue.
        if let Err(e) = self.warmup() {
            warn!("LLM warmup pass failed (non-fatal): {e}");
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
    /// `on_token` and polling `cancel` between tokens. Returns the full note
    /// markdown, or `None` if cancelled (the caller discards the partial, §8.4).
    ///
    /// The prompt is built as the fixed prefix + this transcript's tail; when the
    /// prefix's KV state is cached (§8.7) it is restored into the fresh context and
    /// only the tail is decoded, so the prefix is never re-read. The full prompt is
    /// always tokenized and fed identically to the fallback path — the cache only
    /// skips *recomputing* the prefix's KV — so a cached note is byte-identical to
    /// an uncached one.
    pub fn generate(
        &self,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
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
        // Reserve room for the note itself: the KV cache holds prompt + generated
        // tokens, so the prompt must leave at least `MAX_OUTPUT_TOKENS` of headroom
        // under N_CTX. Checking the prompt alone would let a long-but-fitting
        // transcript stream a partial note and then fail mid-decode at position
        // N_CTX. Unchanged by caching — the tail still occupies the same positions.
        let prompt_budget = N_CTX as i32 - MAX_OUTPUT_TOKENS;
        if tokens.len() as i32 >= prompt_budget {
            return Err(anyhow!(
                "transcript is too long for the model context ({} tokens; the prompt \
                 must stay under {prompt_budget} to leave room for the {MAX_OUTPUT_TOKENS}-token \
                 note within the {N_CTX} context)",
                tokens.len()
            ));
        }

        let mut ctx = self.new_context(model)?;
        // Restore the cached prefix KV if this prompt starts with exactly its
        // tokens; otherwise start from position 0 (full decode, the fallback).
        let start = self.restore_prefix(&mut ctx, kind, &tokens);
        self.decode_and_generate(&mut ctx, model, &tokens, start, MAX_OUTPUT_TOKENS, on_token, cancel)
    }

    /// Run the post-ASR correction pass (design §6.7) over `transcript` on the
    /// resident model, streaming each decoded piece to `on_token` (the caller
    /// splits the stream into JSON-lines records) and polling `cancel`. Returns the
    /// full raw output, or `None` if cancelled.
    ///
    /// Reuses the same streaming decode as [`generate`] but with the correction
    /// prompt and a smaller token cap (suggestions are short). The correction prompt
    /// is not the SOAP prefix, so the prefix cache (§8.7) can't apply — the whole
    /// prompt is decoded from position 0.
    pub fn suggest_corrections(
        &self,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        self.ensure_loaded()?;
        let kind = self.model_kind();
        let prompt = correction::build_prompt(kind, transcript);

        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| anyhow!("failed to tokenize correction prompt: {e}"))?;
        let prompt_budget = N_CTX as i32 - MAX_CORRECTION_TOKENS;
        if tokens.len() as i32 >= prompt_budget {
            return Err(anyhow!(
                "transcript is too long for the correction pass ({} tokens; the prompt must \
                 stay under {prompt_budget} within the {N_CTX} context)",
                tokens.len()
            ));
        }

        let mut ctx = self.new_context(model)?;
        self.decode_and_generate(&mut ctx, model, &tokens, 0, MAX_CORRECTION_TOKENS, on_token, cancel)
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
        // model load, which would spike RAM at the §7 co-resident/swap threshold.
        let mut state = vec![0u8; ctx.state_seq_get_size_ext(0, LlamaStateSeqFlags::empty())];
        let written = unsafe { ctx.state_seq_get_data_ext(state.as_mut_ptr(), 0, LlamaStateSeqFlags::empty()) };
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
    #[allow(clippy::too_many_arguments)]
    fn decode_and_generate(
        &self,
        ctx: &mut LlamaContext,
        model: &LlamaModel,
        tokens: &[LlamaToken],
        start: i32,
        max_tokens: i32,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() as i32 - 1;
        for i in start..tokens.len() as i32 {
            batch
                .add(tokens[i as usize], i, &[0], i == last)
                .map_err(|e| anyhow!("failed to fill prompt batch: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| anyhow!("prompt decode failed: {e}"))?;

        // Low temperature for near-deterministic, low-hallucination clinical text
        // (design §8.2/§8.3).
        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(SAMPLE_TEMP),
            LlamaSampler::greedy(),
        ]);

        let mut out = String::new();
        // Absolute next position: the prompt fills 0..tokens.len(), so generation
        // continues there regardless of how much of the prompt was cached.
        let mut n_cur = tokens.len() as i32;
        let mut generated = 0;
        while generated < max_tokens {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None); // partial note discarded by the caller
            }

            let token = sampler.sample(ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if model.is_eog_token(token) {
                break;
            }

            let piece = model
                .token_to_str(token, Special::Tokenize)
                .map_err(|e| anyhow!("failed to decode a token: {e}"))?;
            on_token(&piece);
            out.push_str(&piece);

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| anyhow!("failed to add a token to the batch: {e}"))?;
            n_cur += 1;
            generated += 1;
            ctx.decode(&mut batch)
                .map_err(|e| anyhow!("token decode failed: {e}"))?;
        }

        Ok(Some(out))
    }

    /// A fresh inference context sized to N_CTX on the engine's thread budget. One
    /// is built per note (and per prefix priming); the cached prefix state is
    /// restored into it, so nothing needs to hold a context across notes (§8.7).
    fn new_context<'a>(&'a self, model: &'a LlamaModel) -> Result<LlamaContext<'a>> {
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
