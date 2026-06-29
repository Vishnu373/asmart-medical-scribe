//! Streaming segmenter (B6): turns the continuous 16 kHz stream into numbered
//! speech segments and runs them through STT to emit live `transcript-segment`
//! events (design §6.3/§6.5).
//!
//! New code built on the ported pieces: the `Segmenter` (capture thread) cuts
//! segments at VAD pause boundaries onto an mpsc queue; `spawn_stt_worker`
//! (transcription thread) drains the queue through a `Transcriber` and pushes
//! `{seq, text}` to a sink in order. The two threads are decoupled by the queue
//! so a slow model never stalls capture (NFR-1). The orchestrator (B7) owns the
//! channel and supplies the real Tauri emit sink.

mod segmenter;
mod worker;

pub use segmenter::{Segment, Segmenter, SegmenterConfig};
pub use worker::{spawn_stt_worker, SegmentError, SttWorkerHandle, TranscriptSegment};
