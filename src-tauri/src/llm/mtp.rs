//! MTP speculative decoding (design §8.2). The Gemma 4 MTP assistant drafts up to
//! [`N_DRAFT`] tokens from the target's hidden states; the target checks them all in
//! one decode and keeps the prefix that matches its own samples exactly. Without a
//! draft the same interface decodes one token per round.
//!
//! Draft sampling: upstream always takes the draft's top-1 token (a fixed sampler
//! inside `common/speculative.cpp`, not settable from Rust) — the same pick as the
//! planned temp 0.1 + greedy, since greedy ignores temperature.

use std::num::NonZeroU32;

use anyhow::{anyhow, Result};
use log::warn;

use llama_cpp_4::context::params::{LlamaContextParams, LlamaContextType};
use llama_cpp_4::context::LlamaContext;
use llama_cpp_4::llama_backend::LlamaBackend;
use llama_cpp_4::llama_batch::LlamaBatch;
use llama_cpp_4::model::LlamaModel;
use llama_cpp_4::mtp::{MtpSession, MtpSessionError};
use llama_cpp_4::sampling::LlamaSampler;
use llama_cpp_4::token::LlamaToken;

/// Draft tokens proposed per round.
pub(super) const N_DRAFT: i32 = 3;

/// The target context plus, when speculating, the draft context built against it.
/// llama.cpp keeps the target's pointer in the draft's `ctx_other` and aliases its KV
/// cells, so field order (= drop order) puts the draft first.
pub(super) struct MtpContexts<'a> {
    draft: Option<LlamaContext<'a>>,
    target: LlamaContext<'a>,
}

impl<'a> MtpContexts<'a> {
    /// No speculation: the target alone.
    pub(super) fn target_only(target: LlamaContext<'a>) -> Self {
        Self {
            draft: None,
            target,
        }
    }

    /// Build the draft context on `target`'s KV cache. Must run before anything is
    /// written into the target (prefix restore included). On failure the note runs on
    /// the target alone (§8.2).
    pub(super) fn with_draft(
        backend: &LlamaBackend,
        draft_model: &'a LlamaModel,
        target: LlamaContext<'a>,
        n_threads: Option<i32>,
        note_id: &str,
    ) -> Self {
        let mut params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(target.n_ctx()))
            .with_ctx_type(LlamaContextType::Mtp)
            .with_ctx_other(&target)
            // Unused by this (non-recurrent) arch, but `MtpSession` validates it.
            .with_n_rs_seq(N_DRAFT.max(4) as u32);
        if let Some(n) = n_threads {
            params = params.with_n_threads(n).with_n_threads_batch(n);
        }
        // let draft = draft_model
        //     .new_context(backend, params)
        //     .map_err(|e| anyhow!("failed to create the MTP draft context: {e}"))?;
        // Ok(Self {
        //     draft: Some(draft),
        //     target,
        // })
        // ^ a broken draft failed every note for as long as it stayed loaded.
        match draft_model.new_context(backend, params) {
            Ok(draft) => Self {
                draft: Some(draft),
                target,
            },
            Err(e) => {
                warn!("[GENERATE] {note_id} MTP draft context failed ({e}) — generating with the main model only");
                Self::target_only(target)
            }
        }
    }

    // /// Start the per-note decoder: speculative when a draft context exists, plain
    // /// otherwise.
    // pub(super) fn decoder(&mut self, note_id: &str) -> Result<MtpDecoder<'_, 'a>> {
    //     let mode = match self.draft.as_mut() {
    //         Some(draft) => Mode::Speculative(
    //             MtpSession::new(&mut self.target, draft, 1, N_DRAFT)
    //                 .map_err(|e| anyhow!("failed to create the MTP session: {e}"))?,
    //         ),
    //         None => Mode::Plain(&mut self.target),
    //     };
    //     Ok(MtpDecoder {
    //         drafting: matches!(mode, Mode::Speculative(_)),
    //         mode,
    //         note_id: note_id.to_owned(),
    //         batch: LlamaBatch::new(N_DRAFT as usize + 1, 1),
    //         drafted: 0,
    //         accepted: 0,
    //     })
    // }
    // ^ a failed session failed the note; falling back to plain can't be expressed
    // when the decoder is returned (the session's borrow of the target spans both arms).

    /// Run `f` with the per-note decoder: speculative when a draft context exists and
    /// the session builds, plain otherwise (§8.2).
    pub(super) fn with_decoder<R>(
        &mut self,
        note_id: &str,
        f: impl FnOnce(&mut MtpDecoder<'_, 'a>) -> R,
    ) -> R {
        if let Some(draft) = self.draft.as_mut() {
            match MtpSession::new(&mut self.target, draft, 1, N_DRAFT) {
                Ok(s) => return f(&mut MtpDecoder::new(Mode::Speculative(s), note_id)),
                Err(e) => warn!(
                    "[GENERATE] {note_id} MTP session failed ({e}) — generating with the main model only"
                ),
            }
        }
        f(&mut MtpDecoder::new(Mode::Plain(&mut self.target), note_id))
    }

    /// The target, for the prefix KV restore before [`Self::decoder`].
    pub(super) fn target_mut(&mut self) -> &mut LlamaContext<'a> {
        &mut self.target
    }
}

enum Mode<'c, 'm> {
    Speculative(MtpSession<'c, 'm>),
    Plain(&'c mut LlamaContext<'m>),
}

/// The tokens one round settled: `tokens` are in the target's KV at `n_cur..`;
/// `next_seed` is sampled but not yet decoded, and starts the next round.
pub(super) struct Round {
    pub tokens: Vec<LlamaToken>,
    pub next_seed: LlamaToken,
}

pub(super) struct MtpDecoder<'c, 'm> {
    mode: Mode<'c, 'm>,
    /// Off after a draft failure: the rest of the note decodes one token per round.
    drafting: bool,
    note_id: String,
    batch: LlamaBatch,
    /// Draft tokens proposed / kept this note.
    pub drafted: usize,
    pub accepted: usize,
}

impl<'c, 'm> MtpDecoder<'c, 'm> {
    fn new(mode: Mode<'c, 'm>, note_id: &str) -> Self {
        Self {
            drafting: matches!(mode, Mode::Speculative(_)),
            mode,
            note_id: note_id.to_owned(),
            batch: LlamaBatch::new(N_DRAFT as usize + 1, 1),
            drafted: 0,
            accepted: 0,
        }
    }

    /// Whether this note speculates at all (the MTP session was built).
    pub(super) fn is_speculative(&self) -> bool {
        matches!(self.mode, Mode::Speculative(_))
    }

    /// The target context — every sample reads its logits here.
    pub(super) fn context(&self) -> &LlamaContext<'m> {
        match &self.mode {
            Mode::Speculative(s) => s.target_context(),
            Mode::Plain(ctx) => ctx,
        }
    }

    /// Decode `batch` on the target; while drafting, also hand its hidden states to
    /// the draft head. Used for the prompt prefill and the forced `</think>`.
    pub(super) fn decode(&mut self, batch: &mut LlamaBatch) -> Result<()> {
        decode_on(&mut self.mode, &mut self.drafting, &self.note_id, batch)
    }

    /// One draft → verify → accept round. `seed` goes at `n_cur`, drafts after it.
    pub(super) fn round(
        &mut self,
        seed: LlamaToken,
        n_cur: i32,
        sampler: &mut LlamaSampler,
    ) -> Result<Round> {
        // Clear last round's rejected drafts before the draft reads the target's cells.
        self.rollback(n_cur)?;

        let drafts = match &mut self.mode {
            Mode::Speculative(s) if self.drafting => match s.draft(0, n_cur, seed) {
                Ok(d) => d,
                Err(e) => {
                    stop_drafting(&mut self.drafting, &self.note_id, &e);
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };

        self.batch.clear();
        for (i, t) in std::iter::once(seed)
            .chain(drafts.iter().copied())
            .enumerate()
        {
            self.batch
                .add(t, n_cur + i as i32, &[0], true)
                .map_err(|e| anyhow!("failed to fill the round batch: {e}"))?;
        }
        decode_on(
            &mut self.mode,
            &mut self.drafting,
            &self.note_id,
            &mut self.batch,
        )
        .map_err(|e| anyhow!("token decode failed: {e}"))?;

        // Row i is the target's pick after token i of the batch. Keep draft i while it
        // matches; the first mismatch (or the row past the last draft) is the next seed.
        let mut tokens = vec![seed];
        let mut n_accepted = 0usize;
        let next_seed = loop {
            let t = sampler.sample(self.context(), n_accepted as i32);
            sampler.accept(t);
            match drafts.get(n_accepted) {
                Some(d) if *d == t => {
                    tokens.push(t);
                    n_accepted += 1;
                }
                _ => break t,
            }
        };

        if !drafts.is_empty() {
            self.drafted += drafts.len();
            self.accepted += n_accepted;
            // Skipped if this round's decode already turned drafting off.
            if let (Mode::Speculative(s), true) = (&mut self.mode, self.drafting) {
                if let Err(e) = s.accept(0, n_accepted as u16) {
                    stop_drafting(&mut self.drafting, &self.note_id, &e);
                }
            }
        }
        Ok(Round { tokens, next_seed })
    }

    /// Drop every target KV cell at or past `n_past` on sequence 0.
    pub(super) fn rollback(&mut self, n_past: i32) -> Result<()> {
        match &mut self.mode {
            Mode::Speculative(s) => s.clear_target_kv_cache_seq(Some(0), Some(n_past as u32), None),
            Mode::Plain(ctx) => ctx.clear_kv_cache_seq(Some(0), Some(n_past as u32), None),
        }
        .map_err(|e| anyhow!("failed to roll back the KV cache: {e}"))?;
        // Stale cells left past `n_past` would be attended alongside their replacements —
        // a silently corrupt note — so check the cells rather than trust the call.
        let highest = self.context().kv_cache_seq_pos_max(0);
        if highest >= n_past {
            return Err(anyhow!(
                "the KV cache still holds position {highest} after rollback to {n_past}"
            ));
        }
        Ok(())
    }
}

/// Target decode shared by [`MtpDecoder::decode`] and the round; split out so the
/// round can pass its own batch field alongside the mode.
fn decode_on(
    mode: &mut Mode<'_, '_>,
    drafting: &mut bool,
    note_id: &str,
    batch: &mut LlamaBatch,
) -> Result<()> {
    match mode {
        Mode::Speculative(s) if *drafting => match s.decode_target_and_process(batch) {
            Ok(()) => Ok(()),
            Err(MtpSessionError::Decode(e)) => Err(anyhow!("{e}")),
            // The target decode succeeded; only the draft side broke.
            Err(e) => {
                stop_drafting(drafting, note_id, &e);
                Ok(())
            }
        },
        Mode::Speculative(s) => s.decode_target(batch).map_err(|e| anyhow!("{e}")),
        Mode::Plain(ctx) => ctx.decode(batch).map_err(|e| anyhow!("{e}")),
    }
}

fn stop_drafting(drafting: &mut bool, note_id: &str, e: &MtpSessionError) {
    *drafting = false;
    warn!("[GENERATE] {note_id} MTP draft failed ({e}) — finishing without speculation");
}
