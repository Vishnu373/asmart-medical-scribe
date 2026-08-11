//! Production `NoteGenerator`: drives the native `LlmEngine`, streams its tokens
//! to the UI, and persists the finished note. This is the glue layer (Tauri +
//! store), kept out of the engine and the coordinator so both stay testable —
//! the same split as `RealPipeline` for recording.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::orchestrator::NoteGenerator;
use crate::store::SharedStore;

use super::engine::LlmEngine;

pub struct RealNoteGenerator {
    app: AppHandle,
    engine: Arc<LlmEngine>,
    store: SharedStore,
}

impl RealNoteGenerator {
    pub fn new(app: AppHandle, engine: Arc<LlmEngine>, store: SharedStore) -> Self {
        Self { app, engine, store }
    }
}

impl NoteGenerator for RealNoteGenerator {
    fn generate(
        &self,
        record_id: &str,
        transcript: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<String>> {
        // Pre-mint the note id so it can be logged at generation start and reused
        // verbatim as the notes.id row on insert (§10.3 4c). `record_id` is the
        // incoming reference to the record this note belongs to. The `[GENERATE] …
        // note generation started` line is emitted inside the engine, where the input
        // token count is known.
        let notes_id = crate::store::new_id();

        // Stream each decoded piece to the UI as raw text; the frontend renders
        // the markdown once on completion (design §8.5).
        let app = self.app.clone();
        let result = self.engine.generate(
            record_id,
            &notes_id,
            transcript,
            &move |piece| {
                let _ = app.emit("generation-token", json!({ "text": piece }));
            },
            &cancel,
        );

        // A cancelled run discards the partial note — nothing is persisted (§8.4).
        let markdown = match result {
            Ok(Some(text)) => text,
            Ok(None) => return Ok(None),
            Err(e) => {
                // §10.3 `[GENERATE] {note_id}, note generation failed {e}` (both sinks).
                // Sanitized: a model/decode error can embed the GGUF path.
                let msg = crate::telemetry::sanitize_error(&e.to_string());
                log::error!("[GENERATE] {notes_id}, note generation failed {msg}");
                crate::telemetry::track_event("generation_failed", json!({ "error": msg }));
                return Err(e);
            }
        };

        // Each generation is a new, immutable, active version (§8.5).
        let note = self
            .store
            .lock()
            .insert_note(&notes_id, record_id, &markdown)?;
        Ok(Some(note.id))
    }
}
