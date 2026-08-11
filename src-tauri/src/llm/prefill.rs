//! Transcript prefill during recording (design §8.9): each segment is decoded into the
//! note model's KV cache as it is produced, so Generate has only the closing turn tail
//! and the note itself left to do. Any failure falls back to [`LlmEngine::generate`]'s
//! normal path.
//!
//! **Why a thread.** `LlamaContext<'a>` borrows `&'a LlamaModel`, which lives inside
//! `LlmEngine`'s mutex, so keeping one context alive across a whole recording means
//! keeping the `MutexGuard` alive too. One thread owns both. The loop itself lives in
//! `engine.rs` ([`LlmEngine::run_prefill_loop`]) because it needs the engine's private
//! model/prefix-cache fields.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::Result;

use super::engine::LlmEngine;

/// Queue depth past which prefill gives up for the rest of the recording.
///
/// A backlog means prefill cannot keep pace with speech and will never catch up: at
/// Generate it would still be draining the queue while the clinician waits, which is
/// worse than not prefilling at all. Stopping hands Generate back its normal path.
const MAX_QUEUE_DEPTH: usize = 8;

/// Work for the prefill thread.
pub(crate) enum PrefillCmd {
    /// A transcribed segment: tokenize (no BOS) and decode it onto the live context.
    Segment { seq: u64, text: String },
    /// Run the note on that same context. Streamed back over `events` rather than
    /// through a callback: `generate`'s `on_token` is a borrowed `&dyn Fn`, which
    /// cannot cross a channel, so the *caller* invokes it as pieces arrive.
    Generate {
        record_id: String,
        note_id: String,
        transcript: String,
        cancel: Arc<AtomicBool>,
        events: Sender<GenEvent>,
    },
}

/// One streamed piece, then the finished note (or the error / `None` for cancelled).
pub(crate) enum GenEvent {
    Token(String),
    Done(Result<Option<String>>),
}

/// Handle to the prefill thread for one recording. Dropping it closes the command
/// channel, so the thread finishes its queue, releases the model guard, and exits.
pub(crate) struct PrefillSession {
    tx: Option<Sender<PrefillCmd>>,
    /// Queued-but-unhandled segments. Incremented on send, decremented by the loop —
    /// an unbounded channel plus this counter, never a `SyncSender`: blocking the STT
    /// sink is the one thing prefill must never do.
    depth: Arc<AtomicUsize>,
    /// Set when prefill has given up (backlog, decode failure, context budget). The
    /// loop breaks on it, so the model guard is released and Generate's normal path
    /// can take the lock.
    disabled: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PrefillSession {
    /// Spawn the prefill thread for one recording.
    pub(crate) fn spawn(engine: Arc<LlmEngine>) -> Self {
        let (tx, rx) = channel();
        let depth = Arc::new(AtomicUsize::new(0));
        let disabled = Arc::new(AtomicBool::new(false));

        let handle = {
            let (depth, disabled) = (depth.clone(), disabled.clone());
            thread::spawn(move || engine.run_prefill_loop(rx, depth, disabled))
        };

        Self {
            tx: Some(tx),
            depth,
            disabled,
            handle: Some(handle),
        }
    }

    /// Queue a finished segment. Never blocks and never fails loudly: prefill is an
    /// optimization, so every failure path here just stops prefilling.
    pub(crate) fn push_segment(&self, seq: u64, text: &str) {
        if self.disabled.load(Ordering::Relaxed) {
            return;
        }
        let queued = self.depth.fetch_add(1, Ordering::Relaxed) + 1;
        if queued > MAX_QUEUE_DEPTH {
            self.give_up("prefill cannot keep pace with speech");
            return;
        }
        let sent = self.tx.as_ref().is_some_and(|tx| {
            tx.send(PrefillCmd::Segment {
                seq,
                text: text.to_string(),
            })
            .is_ok()
        });
        if !sent {
            self.disabled.store(true, Ordering::Relaxed);
        }
    }

    /// Run the note on the live prefilled context, streaming pieces to `on_token`.
    ///
    /// `None` means "no usable session" — the caller runs the normal path. Any inner
    /// `Err` is the generation's own error and is returned as-is.
    pub(crate) fn generate(
        &self,
        record_id: &str,
        note_id: &str,
        transcript: &str,
        on_token: &dyn Fn(&str),
        cancel: &Arc<AtomicBool>,
    ) -> Option<Result<Option<String>>> {
        if self.disabled.load(Ordering::Relaxed) {
            return None;
        }
        let (etx, erx) = channel();
        self.tx
            .as_ref()?
            .send(PrefillCmd::Generate {
                record_id: record_id.to_string(),
                note_id: note_id.to_string(),
                transcript: transcript.to_string(),
                cancel: cancel.clone(),
                events: etx,
            })
            .ok()?;

        // Pump until Done. A closed channel means the thread died mid-note; nothing has
        // been persisted yet, so fall back and let the normal path generate it.
        while let Ok(event) = erx.recv() {
            match event {
                GenEvent::Token(piece) => on_token(&piece),
                GenEvent::Done(result) => return Some(result),
            }
        }
        None
    }

    fn give_up(&self, why: &str) {
        // `swap` so the line lands exactly once no matter how many segments pile in.
        if !self.disabled.swap(true, Ordering::Relaxed) {
            log::warn!("[PREFILL] stopped for this recording: {why}");
        }
    }
}

impl Drop for PrefillSession {
    fn drop(&mut self) {
        // Close the queue first, then join: the loop only exits once every Sender is
        // gone, so joining without this would block forever.
        self.tx = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The text to append to the prefilled prompt for one segment, or `None` for a segment
/// with nothing in it.
///
/// This must reproduce `orchestrator::pipeline::assemble_transcript` exactly — trimmed
/// segments, one space between them — because that is what Generate tokenizes and
/// compares against: a whitespace mismatch is silent, it just costs the entire prefill
/// at the LCP check.
pub(crate) fn segment_chunk(is_first: bool, text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(if is_first {
        text.to_string()
    } else {
        format!(" {text}")
    })
}

#[cfg(test)]
mod tests {
    use super::segment_chunk;

    /// A stand-in for `orchestrator::pipeline::assemble_transcript` (private to that
    /// module). If that join ever changes, this test keeps failing until `segment_chunk`
    /// follows it.
    fn assemble(segments: &[&str]) -> String {
        segments
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Concatenating the chunks in arrival order must reproduce the saved transcript
    /// byte for byte — otherwise Generate's LCP diverges at the first segment boundary
    /// and the whole prefill is wasted.
    fn chunks(segments: &[&str]) -> String {
        let mut out = String::new();
        for text in segments {
            if let Some(chunk) = segment_chunk(out.is_empty(), text) {
                out.push_str(&chunk);
            }
        }
        out
    }

    #[test]
    fn chunks_reassemble_into_the_saved_transcript() {
        let segments = [
            "Patient reports a headache.",
            "  No fever.  ",
            "",
            "   ",
            "Started two days ago.",
        ];
        assert_eq!(chunks(&segments), assemble(&segments));
        assert_eq!(
            chunks(&segments),
            "Patient reports a headache. No fever. Started two days ago."
        );
    }

    #[test]
    fn a_leading_blank_segment_does_not_claim_the_first_slot() {
        // The first *non-empty* segment must still be unprefixed, or every later
        // comparison is off by one space.
        let segments = ["   ", "Chest pain.", "Since Monday."];
        assert_eq!(chunks(&segments), assemble(&segments));
        assert_eq!(chunks(&segments), "Chest pain. Since Monday.");
    }

    #[test]
    fn blank_segments_are_skipped() {
        assert_eq!(segment_chunk(true, "  \n "), None);
        assert_eq!(segment_chunk(false, ""), None);
    }
}
