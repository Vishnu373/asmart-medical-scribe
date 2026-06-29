use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use anyhow::Result;

use super::Transcriber;

/// A `Transcriber` that returns canned text, for unit/integration tests of the
/// segmenter and orchestrator without loading a real model. Mirrors the role of
/// the reference `transcription_mock`.
///
/// Returns queued responses in order; once they run out it falls back to
/// `default`. Empty audio yields an empty string (matching the real engine).
pub struct MockTranscriber {
    responses: Mutex<VecDeque<String>>,
    default: String,
    calls: AtomicUsize,
}

impl MockTranscriber {
    /// Always return `default` (echo-style mock).
    pub fn new(default: impl Into<String>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            default: default.into(),
            calls: AtomicUsize::new(0),
        }
    }

    /// Return each response in order, then `default` once exhausted.
    pub fn with_responses<I, S>(responses: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            responses: Mutex::new(responses.into_iter().map(Into::into).collect()),
            default: String::new(),
            calls: AtomicUsize::new(0),
        }
    }

    /// How many times `transcribe` has been called.
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl Transcriber for MockTranscriber {
    fn transcribe(&self, audio: &[f32]) -> Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if audio.is_empty() {
            return Ok(String::new());
        }
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.default.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_mock_returns_default_and_counts_calls() {
        let m = MockTranscriber::new("hello");
        assert_eq!(m.transcribe(&[0.1, 0.2]).unwrap(), "hello");
        assert_eq!(m.transcribe(&[0.3]).unwrap(), "hello");
        assert_eq!(m.call_count(), 2);
    }

    #[test]
    fn queued_responses_drain_in_order_then_default() {
        let m = MockTranscriber::with_responses(["one", "two"]);
        assert_eq!(m.transcribe(&[1.0]).unwrap(), "one");
        assert_eq!(m.transcribe(&[1.0]).unwrap(), "two");
        assert_eq!(m.transcribe(&[1.0]).unwrap(), ""); // exhausted -> default ""
    }

    #[test]
    fn empty_audio_yields_empty_string() {
        let m = MockTranscriber::new("ignored");
        assert_eq!(m.transcribe(&[]).unwrap(), "");
        assert_eq!(m.call_count(), 1);
    }
}
