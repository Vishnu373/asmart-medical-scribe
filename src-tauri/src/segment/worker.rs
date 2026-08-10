use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::audio_toolkit::TARGET_SAMPLE_RATE;
use crate::stt::Transcriber;

use super::Segment;

/// A transcribed segment ready for the UI: the sequence number fixes its order,
/// the text is appended to the on-screen transcript (design §9.5 `transcript-segment`).
pub struct TranscriptSegment {
    pub seq: u64,
    pub text: String,
}

/// A segment that failed to transcribe. Carries the `seq` so the orchestrator
/// (B7) can react — reload the model (a B5 native panic unloads it, so every
/// later segment would otherwise error) and emit `error{code,message}` (§9.5) —
/// instead of the failure being buried in the log and the rest of the consult
/// silently lost.
pub struct SegmentError {
    pub seq: u64,
    pub message: String,
}

/// Handle to the running STT worker thread.
///
/// **Shutdown contract:** the worker only exits once the queue closes, i.e. once
/// every `Sender` (held by the `Segmenter`) is dropped. So to drain and wait,
/// drop the `Segmenter`/`Sender` **first**, then call [`SttWorkerHandle::join`].
/// Dropping the handle on its own simply *detaches* the worker (it finishes when
/// the channel closes) — deliberately not a join, so teardown can't deadlock if
/// the handle is dropped while a `Sender` is still alive.
pub struct SttWorkerHandle {
    handle: Option<JoinHandle<()>>,
}

impl SttWorkerHandle {
    /// Wait for the worker to drain the remaining segments and exit. Only call
    /// this once the `Sender` is dropped, otherwise it blocks forever (B7's
    /// PROCESSING→IDLE drain drops the segmenter, then joins).
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the transcription worker (the "hands"): it drains `rx`, runs each
/// segment through `transcriber`, and pushes the result to `sink` in sequence
/// order. A single worker over a FIFO channel makes ordering inherent — there is
/// no reorder buffer because completion can't outrun the queue (design §6.3).
///
/// `sink` is a plain callback rather than a Tauri `Emitter` so this stays
/// testable; B7 supplies a closure that emits `transcript-segment` on `Ok` and
/// reloads + emits `error{code,message}` on `Err`. The sink carries a `Result`
/// (not just the success type) precisely so a mid-consult engine failure is
/// surfaced to the orchestrator rather than swallowed to the log.
pub fn spawn_stt_worker<F>(
    rx: Receiver<Segment>,
    transcriber: Arc<dyn Transcriber>,
    mut sink: F,
) -> SttWorkerHandle
where
    F: FnMut(Result<TranscriptSegment, SegmentError>) + Send + 'static,
{
    let handle = thread::spawn(move || {
        // Per-consult accumulators: the loop spans exactly one consult, since the channel
        // closes when B7 drops the segmenter on stop.
        let (mut segments, mut speech_s, mut transcribe_s) = (0u64, 0.0f32, 0.0f32);
        let mut slowest: Option<(u64, f32)> = None;

        for segment in rx.iter() {
            // STT cost visibility (docs/implementation-stt-thread-management.md §3.6).
            let audio_s = segment.audio.len() as f32 / TARGET_SAMPLE_RATE as f32;
            let t0 = Instant::now();
            let result = transcriber.transcribe(&segment.audio);
            let secs = t0.elapsed().as_secs_f32();
            log::info!(
                "[STT] seq{}: speech - {audio_s:.1}s, transcribe - {secs:.2}s",
                segment.seq
            );

            segments += 1;
            speech_s += audio_s;
            transcribe_s += secs;
            if slowest.is_none_or(|(_, s)| secs > s) {
                slowest = Some((segment.seq, secs));
            }

            // Phase 1: RTF per segment — a ratio you had to invert to read, and totals you
            // had to sum by hand. Replaced by the durations above plus the summary below.
            // let ms = t0.elapsed().as_secs_f32() * 1000.0;
            // log::info!(
            //     "[STT] seq={} audio={audio_s:.2}s transcribe={ms:.0}ms rtf={:.3}",
            //     segment.seq,
            //     ms / 1000.0 / audio_s
            // );

            // match transcriber.transcribe(&segment.audio) {   // pre-experiment: untimed
            match result {
                Ok(text) if !text.trim().is_empty() => {
                    sink(Ok(TranscriptSegment {
                        seq: segment.seq,
                        text,
                    }));
                }
                Ok(_) => {} // empty/whitespace transcription: nothing to append
                Err(e) => sink(Err(SegmentError {
                    seq: segment.seq,
                    message: e.to_string(),
                })),
            }
        }

        // Channel closed: the consult is over. Skipped at zero so an all-silence
        // recording does not log a row of zeroes.
        // `slowest` is set on the same iteration that increments `segments`, so
        // `Some` and `segments > 0` are the same condition — matching on it drops a
        // fallback that could only ever print a fake row.
        // if segments > 0 {
        //     let (seq, secs) = slowest.unwrap_or((0, 0.0));
        if let Some((seq, secs)) = slowest {
            log::info!(
                "[STT] transcription done: segments - {segments}, speech duration - {speech_s:.1}s, \
                 transcribe duration - {transcribe_s:.1}s, slowest seq - (seq{seq}, {secs:.2}s)"
            );
        }
    });

    SttWorkerHandle {
        handle: Some(handle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::MockTranscriber;
    use anyhow::anyhow;
    use std::sync::mpsc::channel;
    use std::sync::Mutex;

    fn audio() -> Vec<f32> {
        vec![0.1; 10]
    }

    /// Send `count` segments (seq 0..count) and run them through `transcriber`,
    /// returning the Ok results as `(seq, text)` and the Err results as `seq`.
    fn run(
        count: u64,
        transcriber: Arc<dyn Transcriber>,
    ) -> (Vec<(u64, String)>, Vec<u64>) {
        let (tx, rx) = channel();
        for seq in 0..count {
            tx.send(Segment {
                seq,
                audio: audio(),
            })
            .unwrap();
        }
        drop(tx); // close the queue so the worker drains and exits

        let oks = Arc::new(Mutex::new(Vec::new()));
        let errs = Arc::new(Mutex::new(Vec::new()));
        let (so, se) = (oks.clone(), errs.clone());

        spawn_stt_worker(rx, transcriber, move |res| match res {
            Ok(ts) => so.lock().unwrap().push((ts.seq, ts.text)),
            Err(e) => se.lock().unwrap().push(e.seq),
        })
        .join();

        let oks = oks.lock().unwrap().clone();
        let errs = errs.lock().unwrap().clone();
        (oks, errs)
    }

    #[test]
    fn emits_results_in_sequence_order() {
        let t: Arc<dyn Transcriber> =
            Arc::new(MockTranscriber::with_responses(["alpha", "bravo", "charlie"]));
        let (oks, errs) = run(3, t);
        assert!(errs.is_empty());
        assert_eq!(
            oks,
            vec![
                (0, "alpha".to_string()),
                (1, "bravo".to_string()),
                (2, "charlie".to_string()),
            ]
        );
    }

    #[test]
    fn empty_transcriptions_are_not_emitted() {
        // Only the first segment yields text; the mock then returns "" (default).
        let t: Arc<dyn Transcriber> = Arc::new(MockTranscriber::with_responses(["hello"]));
        let (oks, errs) = run(3, t);
        assert!(errs.is_empty());
        assert_eq!(oks, vec![(0, "hello".to_string())]);
    }

    #[test]
    fn transcribe_errors_are_surfaced_with_their_seq() {
        // A transcriber that always fails: every segment's error must reach the
        // sink (so B7 can reload + emit error{...}), not be swallowed.
        struct FailingTranscriber;
        impl Transcriber for FailingTranscriber {
            fn transcribe(&self, _audio: &[f32]) -> anyhow::Result<String> {
                Err(anyhow!("engine panicked"))
            }
        }
        let (oks, errs) = run(3, Arc::new(FailingTranscriber));
        assert!(oks.is_empty());
        assert_eq!(errs, vec![0, 1, 2]);
    }
}
