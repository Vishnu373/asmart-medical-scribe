use anyhow::Result;

/// One STT backend behind a single call. Implemented by the real `SttEngine`
/// (Parakeet/Whisper over transcribe-rs) and by `MockTranscriber` for tests.
/// The streaming segmenter (B6) and orchestrator (B7) depend only on this trait.
pub trait Transcriber: Send + Sync {
    /// Transcribe 16 kHz mono f32 audio to text. Empty input yields an empty
    /// string rather than an error.
    fn transcribe(&self, audio: &[f32]) -> Result<String>;
}
