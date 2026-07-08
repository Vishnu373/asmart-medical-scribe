//! Recording orchestrator & state machine (B7). A single backend coordinator
//! owns IDLE → RECORDING → PROCESSING (design §6.6) and serializes every
//! transition; the UI only *requests* them via the §9.4 recording commands.
//! State guards reject illegal/duplicate transitions so click/hotkey spam can't
//! corrupt the machine.
//!
//! `coordinator` is the Tauri-free, unit-tested state machine (driven through a
//! `Pipeline` trait). `pipeline` is the production `RealPipeline` that wires the
//! real cpal capture → VAD segmenter → STT worker stack and emits the §9.5
//! events. PROCESSING is also the seam where phase-two note generation will run.

mod coordinator;
mod pipeline;

pub use coordinator::{
    AppEvent, Coordinator, CorrectionSuggester, NoteGenerator, Pipeline, RecordingState,
};
pub use pipeline::{emit_app_event, RealPipeline};
