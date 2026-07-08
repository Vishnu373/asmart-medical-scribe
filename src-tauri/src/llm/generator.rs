//! Production `NoteGenerator`: drives the native `LlmEngine`, streams its tokens
//! to the UI, and persists the finished note. This is the glue layer (Tauri +
//! store), kept out of the engine and the coordinator so both stay testable —
//! the same split as `RealPipeline` for recording.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::orchestrator::{CorrectionSuggester, NoteGenerator};
use crate::store::SharedStore;

use super::correction;
use super::engine::LlmEngine;

pub struct RealNoteGenerator {
    app: AppHandle,
    engine: Arc<LlmEngine>,
    store: SharedStore,
    /// Residency Swap mode (design §7/§8.4): release the LLM after each generation
    /// so the next recording's STT model has the RAM. Co-resident leaves it warm.
    swap_mode: bool,
}

impl RealNoteGenerator {
    pub fn new(app: AppHandle, engine: Arc<LlmEngine>, store: SharedStore, swap_mode: bool) -> Self {
        Self {
            app,
            engine,
            store,
            swap_mode,
        }
    }
}

impl NoteGenerator for RealNoteGenerator {
    fn generate(
        &self,
        record_id: &str,
        transcript: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        // Stream each decoded piece to the UI as raw text; the frontend renders
        // the markdown once on completion (design §8.5).
        let app = self.app.clone();
        let result = self.engine.generate(
            transcript,
            &move |piece| {
                let _ = app.emit("generation-token", json!({ "text": piece }));
            },
            &cancel,
        );

        // In swap mode, free the LLM regardless of outcome so STT can reload for
        // the next recording. Done before surfacing an error so a failure can't
        // leave the model pinned.
        if self.swap_mode {
            self.engine.unload();
        }

        // A cancelled run discards the partial note — nothing is persisted (§8.4).
        let markdown = match result? {
            Some(text) => text,
            None => return Ok(None),
        };

        // Each generation is a new, immutable, active version (§8.5).
        let note = self.store.lock().insert_note(record_id, &markdown)?;
        Ok(Some(note.id))
    }
}

/// Production `CorrectionSuggester`: runs the §6.7 correction pass on the resident
/// `LlmEngine`, splits the streamed output into JSON-lines records, and emits each
/// one that survives the server-side guard as a `correction-suggestion` event. No
/// second model and no persistence — suggestions are transient until the clinician
/// accepts one (which patches the transcript via the existing autosave, §6.5).
pub struct RealCorrectionSuggester {
    app: AppHandle,
    engine: Arc<LlmEngine>,
    /// Residency Swap mode (design §7/§8.4): release the LLM after the pass so the
    /// still-warm STT model isn't forced to co-reside with it on the low-RAM machine
    /// swap mode protects. Co-resident leaves it warm for the imminent Generate.
    swap_mode: bool,
}

impl RealCorrectionSuggester {
    pub fn new(app: AppHandle, engine: Arc<LlmEngine>, swap_mode: bool) -> Self {
        Self {
            app,
            engine,
            swap_mode,
        }
    }
}

/// Parse one streamed line and, if it's a well-formed record whose `original` span
/// appears verbatim in the transcript, emit it. The in-transcript check is the
/// server-side half of the span-only invariant (§6.7) — enforced here, not just by
/// the prompt, so a phantom span can't reach the UI.
fn emit_suggestion(app: &AppHandle, line: &str, transcript: &str) {
    if let Some(s) = correction::parse_line(line) {
        if transcript.contains(&s.original) {
            let _ = app.emit("correction-suggestion", &s);
        }
    }
}

impl CorrectionSuggester for RealCorrectionSuggester {
    fn suggest(&self, transcript: &str, cancel: Arc<AtomicBool>) -> Result<Option<()>> {
        // Buffer the token stream and flush one record per newline as it arrives, so
        // suggestions appear the instant their line completes (§6.7 parse-as-you-go).
        let buf = Arc::new(Mutex::new(String::new()));
        let on_token = {
            let buf = buf.clone();
            let app = self.app.clone();
            let transcript = transcript.to_string();
            move |piece: &str| {
                let mut b = buf.lock().unwrap();
                b.push_str(piece);
                while let Some(nl) = b.find('\n') {
                    let line: String = b.drain(..=nl).collect();
                    emit_suggestion(&app, &line, &transcript);
                }
            }
        };

        let result = self.engine.suggest_corrections(transcript, &on_token, &cancel);

        // In swap mode, free the LLM regardless of outcome — the same discipline as
        // note generation (§8.4) — so an auto-on-Stop pass can't leave STT and the
        // LLM co-resident on a machine swap mode is meant to keep them apart on.
        if self.swap_mode {
            self.engine.unload();
        }

        match result {
            Ok(Some(_)) => {
                // Flush a final record with no trailing newline, then signal the end.
                let rest = buf.lock().unwrap().clone();
                emit_suggestion(&self.app, &rest, transcript);
                let _ = self.app.emit("correction-done", ());
                Ok(Some(()))
            }
            // Cancelled: the pass simply ends and the transcript stays plain (§6.7).
            Ok(None) => {
                let _ = self.app.emit("correction-done", ());
                Ok(None)
            }
            Err(e) => {
                let _ = self
                    .app
                    .emit("correction-error", json!({ "message": e.to_string() }));
                Err(e)
            }
        }
    }
}
