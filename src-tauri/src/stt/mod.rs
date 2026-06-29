//! Speech-to-text behind a `Transcriber` trait: Parakeet TDT v3 (multilingual
//! EN+FR, the v1 all-rounder) over `transcribe-rs`, plus a `MockTranscriber`
//! for tests. Ported and adapted from the reference STT manager (B5).
//!
//! Adaptation: the reference manager is wired to a Tauri `AppHandle`, a download
//! `ModelManager`, `specta` bindings and eight engines with GPU accelerators.
//! Here it is decoupled — the engine wrapper owns only the loaded model, an idle
//! watcher and language config, exposes a plain `transcribe(&[f32]) -> String`,
//! and is driven by the orchestrator (B7), not global settings.

mod engine;
mod mock;
mod text;
mod transcriber;

pub use engine::{ModelKind, SttEngine};
pub use mock::MockTranscriber;
pub use text::{apply_custom_words, filter_transcription_output};
pub use transcriber::Transcriber;
