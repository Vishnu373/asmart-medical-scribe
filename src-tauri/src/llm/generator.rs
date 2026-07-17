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
