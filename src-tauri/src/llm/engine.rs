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
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use super::prompt::build_prompt;

/// The note-generation model (design §8.2). The three doctor-facing tiers map
/// here: `best`→Mistral, `medium`→Phi (Q8), `okay`→PhiQ4. When no tier is chosen
/// the model is picked fit-to-machine on total RAM ([`for_total_ram`]). The first
/// two ship bundled; PhiQ4 is the optional on-demand download (D1, `models`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LlmModel {
    /// `best` (≥16 GB default): Mistral-7B-Instruct-v0.3 Q4_K_M (~4.4 GB). Bundled.
    Mistral,
    /// `medium` (<16 GB default): Phi-3.5-mini-instruct Q8_0 (~4.0 GB). Bundled.
    Phi,
    /// `okay`: Phi-3.5-mini-instruct Q4_K_M (~2.4 GB). Optional, downloaded on demand.
    PhiQ4,
}

const GIB: u64 = 1024 * 1024 * 1024;

// The §8.2 model-choice threshold: at/above this total RAM the larger LLM is
// selected. This module is the single source of truth for the model choice *and*
// its footprint (see [`LlmModel::footprint`]); the §7 residency budget reads them
// so it can never size co-residency for a different model than the engine loads.
const LLM_MODEL_THRESHOLD: u64 = 16 * GIB;

impl LlmModel {
    /// The §8.2 selection rule, keyed on total RAM (the same probe that drives §7).
    /// This is the default when the doctor hasn't picked a tier explicitly.
    pub fn for_total_ram(total_ram: u64) -> Self {
        if total_ram >= LLM_MODEL_THRESHOLD {
            LlmModel::Mistral
        } else {
            LlmModel::Phi
        }
    }

    /// Map a doctor-facing `model_choice` tier (§9.3) to a model, or `None` for an
    /// unrecognized value so the caller can fall back to the automatic pick.
    pub fn from_tier(choice: &str) -> Option<Self> {
        match choice {
            "best" => Some(LlmModel::Mistral),
            "medium" => Some(LlmModel::Phi),
            "okay" => Some(LlmModel::PhiQ4),
            _ => None,
        }
    }

    /// Resolve the model the engine should load: the explicitly chosen tier if it
    /// is recognized, otherwise the fit-to-machine default ([`for_total_ram`]).
    pub fn from_choice(choice: &str, total_ram: u64) -> Self {
        LlmModel::from_tier(choice).unwrap_or_else(|| LlmModel::for_total_ram(total_ram))
    }

    /// Approximate resident RAM footprint of the loaded GGUF (design §8.2). The
    /// residency feasibility calc (§7) reads this so its co-residency budget is
    /// for exactly the model [`for_total_ram`] will pick — one source of truth for
    /// the (choice, size) pair. Design-target estimates, validated in benchmarking.
    pub fn footprint(self) -> u64 {
        match self {
            LlmModel::Mistral => 22 * GIB / 5, // ~4.4 GB (Q4_K_M)
            LlmModel::Phi => 4 * GIB,          // ~4.0 GB (Q8_0)
            LlmModel::PhiQ4 => 12 * GIB / 5,   // ~2.4 GB (Q4_K_M)
        }
    }

    /// The GGUF filename resolved under the models search dirs (D1: app-data
    /// download dir first, then the bundled resource dir). These literals are the
    /// installer/download filenames; `models::OPTIONAL` keys off the same names.
    pub fn file_name(self) -> &'static str {
        match self {
            LlmModel::Mistral => "Mistral-7B-Instruct-v0.3-Q4_K_M.gguf",
            LlmModel::Phi => "phi-3.5-mini-instruct-Q8_0-worthdoing.gguf",
            LlmModel::PhiQ4 => "phi-3.5-mini-instruct-Q4_K_M-worthdoing.gguf",
        }
    }
}

// Tuning constants (design §8.2 "set at implementation via benchmarking"): kept
// conservative so the context fits a realistic consult without reserving RAM the
// §7 budget needs.
const N_CTX: u32 = 8192; // prompt + transcript + note; well under the model maxima
const MAX_OUTPUT_TOKENS: i32 = 1536; // generous ceiling for a SOAP note
const SAMPLE_TEMP: f32 = 0.2; // low temperature → near-deterministic, low hallucination

/// Owns the loaded GGUF model. The `LlamaBackend` is process-wide and created
/// once; the model is loaded lazily (swap mode) or at startup (co-resident) and
/// can be unloaded to release RAM. Generation builds a fresh context each run.
pub struct LlmEngine {
    backend: LlamaBackend,
    model: Mutex<Option<LlamaModel>>,
    kind: LlmModel,
    /// Model-file search dirs, in priority order (D1): the app-data download dir
    /// first (optional models the doctor pulled), then the bundled resource dir.
    model_dirs: Vec<PathBuf>,
    n_threads: i32,
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
            kind,
            model_dirs,
            n_threads: n_threads.max(1),
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
        let file = self.kind.file_name();
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
        info!("Loaded LLM model: {:?}", self.kind);

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
    }

    /// Generate a SOAP note from `transcript`, streaming each decoded piece to
    /// `on_token` and polling `cancel` between tokens. Returns the full note
    /// markdown, or `None` if cancelled (the caller discards the partial, §8.4).
    pub fn generate(
        &self,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        self.ensure_loaded()?;
        let prompt = build_prompt(self.kind, transcript);
        self.run(&prompt, MAX_OUTPUT_TOKENS, on_token, cancel)
    }

    /// A throwaway generation right after load to warm caches; output discarded.
    fn warmup(&self) -> Result<()> {
        let never = Arc::new(AtomicBool::new(false));
        let _ = self.run(&build_prompt(self.kind, "warmup"), 1, &|_| {}, &never)?;
        Ok(())
    }

    /// The decode loop. Tokenizes `prompt`, decodes greedily-ish at low
    /// temperature, streams pieces, and stops at end-of-generation, the token cap,
    /// or cancellation.
    fn run(
        &self,
        prompt: &str,
        max_tokens: i32,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        let guard = self.lock_model();
        let model = guard
            .as_ref()
            .ok_or_else(|| anyhow!("LLM model is not loaded"))?;

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(N_CTX))
            .with_n_threads(self.n_threads)
            .with_n_threads_batch(self.n_threads);
        let mut ctx = model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| anyhow!("failed to create LLM context: {e}"))?;

        let tokens = model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|e| anyhow!("failed to tokenize prompt: {e}"))?;
        // Reserve room for the note itself: the KV cache holds prompt + generated
        // tokens, so the prompt must leave at least `max_tokens` of headroom under
        // N_CTX. Checking the prompt alone would let a long-but-fitting transcript
        // stream a partial note and then fail mid-decode at position N_CTX.
        let prompt_budget = N_CTX as i32 - max_tokens;
        if tokens.len() as i32 >= prompt_budget {
            return Err(anyhow!(
                "transcript is too long for the model context ({} tokens; the prompt \
                 must stay under {prompt_budget} to leave room for the {max_tokens}-token \
                 note within the {N_CTX} context)",
                tokens.len()
            ));
        }

        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() as i32 - 1;
        for (i, token) in tokens.iter().enumerate() {
            batch
                .add(*token, i as i32, &[0], i as i32 == last)
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
        let mut n_cur = batch.n_tokens();
        let mut generated = 0;
        while generated < max_tokens {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None); // partial note discarded by the caller
            }

            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
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
