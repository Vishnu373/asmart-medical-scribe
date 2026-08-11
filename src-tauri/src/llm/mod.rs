//! In-process GGUF SOAP note generation (design §8). B10.
//!
//! `prompt` builds the zero-shot SOAP prompt (pure, unit-tested). `engine` wraps
//! the native `llama-cpp-2` model — load/RAM-guard/warmup and the streaming,
//! cancellable decode loop (verified on Windows like the rest of the native
//! stack). `generator` is the production `NoteGenerator` that streams tokens to
//! the UI and persists the note; the coordinator (B7) drives it through the
//! `NoteGenerator` trait, so the GENERATING state machine stays testable with a
//! mock and no model.

//! `prefill` decodes each transcript segment into the note model's KV cache as it is
//! produced, so Generate only has the tail left to do (design §8.9).

mod engine;
mod generator;
mod prefill;
mod prompt;

pub use engine::{LlmEngine, LlmModel};
pub use generator::RealNoteGenerator;
