use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;

/// The backend-owned app state (design §6.6 + §8.4). The UI only *requests*
/// transitions; the coordinator decides whether each is legal. Note generation
/// is a distinct `GENERATING` state reachable only from `IDLE`, so recording is
/// blocked while a note is being produced and vice-versa.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Processing,
    /// The §6.7 post-ASR correction pass. Reachable only from `IDLE` and, like
    /// `Generating`, it blocks note generation — the two never run at once, so
    /// correction is always sequenced *before* Generate.
    Correcting,
    Generating,
}

impl RecordingState {
    /// Wire form emitted in `state-changed` (design §9.5). Kept uppercase to
    /// match the event contract the frontend listens for.
    pub fn as_str(self) -> &'static str {
        match self {
            RecordingState::Idle => "IDLE",
            RecordingState::Recording => "RECORDING",
            RecordingState::Processing => "PROCESSING",
            RecordingState::Correcting => "CORRECTING",
            RecordingState::Generating => "GENERATING",
        }
    }
}

/// An event the coordinator pushes to the UI. Only the two lifecycle events the
/// coordinator itself owns; `transcript-segment` / `input-level` originate inside
/// the pipeline (see `pipeline.rs`) and are emitted there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// `state-changed{state}` — IDLE / RECORDING / PROCESSING (design §9.5).
    StateChanged(RecordingState),
    /// `error{code,message}` — a recoverable failure surfaced to the UI.
    Error { code: String, message: String },
}

/// Sink the coordinator emits through. Boxed (not a Tauri `Emitter`) so the
/// state machine is testable without a running app; production supplies a
/// closure that maps `AppEvent` onto `app.emit(...)` (see `pipeline.rs`).
pub type EmitFn = Box<dyn Fn(AppEvent) + Send + Sync>;

/// The capture+transcription pipeline the coordinator drives. Abstracted behind
/// a trait so the state machine can be unit-tested with a mock, while production
/// wires the real cpal/VAD/STT stack (`RealPipeline`). Methods take `&mut self`
/// and are only ever called by the single coordinator, one at a time.
pub trait Pipeline: Send {
    /// Spin up capture and the STT worker. Returns once running — must not block
    /// for the duration of the recording (design §6.6 "Start — spin up").
    fn start(&mut self) -> Result<()>;
    /// Stop capture, tail-flush the open segment, and drain the worker. Blocks
    /// until every in-flight segment has been transcribed (design §6.6 "Stop").
    /// Returns the id of the persisted `records` row, or `None` when nothing was
    /// transcribed (an empty consult isn't saved).
    fn stop(&mut self) -> Result<Option<String>>;
    /// Gate capture without tearing the pipeline down (pause/resume mid-consult).
    fn set_paused(&mut self, paused: bool);
}

/// Produces a SOAP note from a finalized transcript (design §8). Abstracted
/// behind a trait so the GENERATING state machine is unit-testable with a mock,
/// while production wires the in-process `llama-cpp-2` model (`RealNoteGenerator`).
///
/// `generate` is long-running and synchronous; the coordinator calls it with the
/// state lock released so a concurrent `cancel_generation` can flip `cancel`. The
/// implementation streams `generation-token` events and persists the finished
/// note itself, returning the new note id, or `None` if it was cancelled (the
/// partial note is discarded, design §8.4).
pub trait NoteGenerator: Send + Sync {
    fn generate(
        &self,
        record_id: &str,
        transcript: &str,
        cancel: Arc<AtomicBool>,
    ) -> Result<Option<String>>;
}

/// Runs the §6.7 post-ASR correction pass over a finalized transcript. Abstracted
/// behind a trait so the `CORRECTING` state machine is unit-testable with a mock,
/// while production wires the resident LLM (`RealCorrectionSuggester`).
///
/// Like `NoteGenerator::generate`, `suggest` is long-running and synchronous; the
/// coordinator calls it with the state lock released so a concurrent
/// `cancel_generation` can flip `cancel`. The implementation streams
/// `correction-suggestion` events and the terminal `correction-done` /
/// `correction-error` itself, returning `Some(())` when the pass completed, `None`
/// if it was cancelled. The feature is additive, so a completed and a cancelled pass
/// are both non-errors to the coordinator.
pub trait CorrectionSuggester: Send + Sync {
    fn suggest(&self, transcript: &str, cancel: Arc<AtomicBool>) -> Result<Option<()>>;
}

struct Inner {
    state: RecordingState,
    paused: bool,
    /// `None` only while a stop is draining: the pipeline is moved out so the
    /// blocking drain runs without holding the state lock, letting a concurrent
    /// Start observe PROCESSING and be rejected (design §6.6).
    pipeline: Option<Box<dyn Pipeline>>,
    /// Set while GENERATING; `cancel_generation` flips it and the running
    /// generator polls it. Cleared when generation ends.
    cancel: Option<Arc<AtomicBool>>,
}

/// Single-threaded coordinator that owns the recording state and serializes all
/// transitions, modeled on the reference transcription coordinator. State guards
/// reject illegal or duplicate transitions so rapid clicks / hotkey spam can't
/// corrupt the machine (design §6.6).
pub struct Coordinator {
    inner: Mutex<Inner>,
    generator: Box<dyn NoteGenerator>,
    suggester: Box<dyn CorrectionSuggester>,
    emit: EmitFn,
}

impl Coordinator {
    pub fn new(
        pipeline: Box<dyn Pipeline>,
        generator: Box<dyn NoteGenerator>,
        suggester: Box<dyn CorrectionSuggester>,
        emit: EmitFn,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: RecordingState::Idle,
                paused: false,
                pipeline: Some(pipeline),
                cancel: None,
            }),
            generator,
            suggester,
            emit,
        }
    }

    pub fn state(&self) -> RecordingState {
        self.lock().state
    }

    /// IDLE → RECORDING. Rejected unless currently IDLE. On a pipeline failure the
    /// machine stays IDLE and an `error` event is surfaced.
    pub fn start_recording(&self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Idle {
            return Err(reject("start_recording", inner.state));
        }
        let pipeline = inner
            .pipeline
            .as_mut()
            .expect("pipeline is present whenever the machine is IDLE");
        if let Err(e) = pipeline.start() {
            let msg = e.to_string();
            self.fail("recording_start_failed", &msg);
            return Err(msg);
        }
        inner.state = RecordingState::Recording;
        inner.paused = false;
        // Emit while still holding the lock so the state change and its
        // notification are atomic: under rapid start/stop the next command can't
        // slip in and emit out of order (design §6.6 "UI can't desync"). The emit
        // closure only calls app.emit and never re-enters the coordinator, so
        // holding the lock here can't deadlock.
        (self.emit)(AppEvent::StateChanged(RecordingState::Recording));
        Ok(())
    }

    /// RECORDING → PROCESSING (drain) → IDLE. Rejected unless currently RECORDING.
    /// The pipeline is moved out for the drain so the lock isn't held while it
    /// blocks, and a Start arriving mid-drain sees PROCESSING and is rejected.
    pub fn stop_recording(&self) -> Result<Option<String>, String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Recording {
            return Err(reject("stop_recording", inner.state));
        }
        inner.state = RecordingState::Processing;
        inner.paused = false;
        let mut pipeline = inner
            .pipeline
            .take()
            .expect("pipeline is present whenever the machine is RECORDING");
        // PROCESSING is emitted atomically with the state change; the lock is
        // then released only for the blocking drain (so a concurrent Start sees
        // PROCESSING and is rejected), and reacquired to emit IDLE atomically.
        (self.emit)(AppEvent::StateChanged(RecordingState::Processing));
        drop(inner);

        let result = pipeline.stop();

        let mut inner = self.lock();
        inner.pipeline = Some(pipeline);
        inner.state = RecordingState::Idle;
        (self.emit)(AppEvent::StateChanged(RecordingState::Idle));
        match result {
            Ok(record_id) => Ok(record_id),
            Err(e) => {
                // The drain steps swallow their own errors; the only failure
                // `stop()` surfaces is persisting the record (the pipeline has
                // already spilled the transcript to a recovery file), so report
                // it as a save failure rather than a generic drain failure.
                let msg = e.to_string();
                self.fail("save_failed", &msg);
                Err(msg)
            }
        }
    }

    /// Pause capture within a recording. Rejected unless RECORDING and not
    /// already paused. Does not emit `state-changed` — the design event contract
    /// (§9.5) has no PAUSED state, so the machine stays RECORDING.
    pub fn pause_recording(&self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Recording || inner.paused {
            return Err(reject("pause_recording", inner.state));
        }
        inner.paused = true;
        inner.pipeline.as_mut().unwrap().set_paused(true);
        Ok(())
    }

    /// Resume a paused recording. Rejected unless RECORDING and currently paused.
    pub fn resume_recording(&self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Recording || !inner.paused {
            return Err(reject("resume_recording", inner.state));
        }
        inner.paused = false;
        inner.pipeline.as_mut().unwrap().set_paused(false);
        Ok(())
    }

    /// IDLE → GENERATING → IDLE: produce a SOAP note from `transcript` and persist
    /// it (design §8.4). Rejected unless IDLE, so it can't run mid-recording and a
    /// second Generate is ignored while one is in flight. Mirrors `stop_recording`:
    /// the state lock is released for the blocking generation (letting a concurrent
    /// `cancel_generation` flip the flag) and reacquired to emit IDLE atomically.
    /// Resolves with the new note's id, or `None` if it was cancelled.
    pub fn generate_note(
        &self,
        record_id: &str,
        transcript: &str,
    ) -> Result<Option<String>, String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Idle {
            return Err(reject("generate_note", inner.state));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.state = RecordingState::Generating;
        inner.cancel = Some(cancel.clone());
        (self.emit)(AppEvent::StateChanged(RecordingState::Generating));
        drop(inner);

        let result = self.generator.generate(record_id, transcript, cancel);

        let mut inner = self.lock();
        inner.state = RecordingState::Idle;
        inner.cancel = None;
        (self.emit)(AppEvent::StateChanged(RecordingState::Idle));
        match result {
            Ok(note_id) => Ok(note_id),
            Err(e) => {
                let msg = e.to_string();
                self.fail("generation_failed", &msg);
                Err(msg)
            }
        }
    }

    /// IDLE → CORRECTING → IDLE: run the §6.7 post-ASR correction pass over
    /// `transcript`, streaming `correction-suggestion` events. Rejected unless IDLE,
    /// so it can't run mid-recording and note generation is blocked until it ends —
    /// the "sequenced, never concurrent with Generate" invariant. Mirrors
    /// `generate_note`: the lock is released for the blocking pass (so a concurrent
    /// `cancel_generation` can flip the flag) and reacquired to emit IDLE atomically.
    /// The pass is additive — a completed or cancelled run both resolve `Ok`; only a
    /// model error surfaces as `Err`.
    pub fn suggest_corrections(&self, transcript: &str) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.state != RecordingState::Idle {
            return Err(reject("suggest_corrections", inner.state));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.state = RecordingState::Correcting;
        inner.cancel = Some(cancel.clone());
        (self.emit)(AppEvent::StateChanged(RecordingState::Correcting));
        drop(inner);

        let result = self.suggester.suggest(transcript, cancel);

        let mut inner = self.lock();
        inner.state = RecordingState::Idle;
        inner.cancel = None;
        (self.emit)(AppEvent::StateChanged(RecordingState::Idle));
        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                // Correction is strictly additive (§6.7): a failure leaves a plain,
                // editable transcript. Log it, but do NOT emit the global `error`
                // AppEvent — that would surface a toast for a feature meant to fail
                // silently. The suggester has already emitted `correction-error`,
                // which the UI ignores.
                let msg = e.to_string();
                log::error!("correction_failed: {msg}");
                Err(msg)
            }
        }
    }

    /// Signal the in-flight generation *or* correction pass to stop; it returns to
    /// IDLE on its own (design §8.4/§6.7). Rejected unless GENERATING or CORRECTING.
    /// Only flips the shared flag — the running pass observes it and unwinds; a
    /// cancelled note discards its partial, a cancelled correction leaves the
    /// transcript plain. Shared cancel path per §6.7 ("cancelable via the existing
    /// generation-cancel path").
    pub fn cancel_generation(&self) -> Result<(), String> {
        let inner = self.lock();
        if inner.state != RecordingState::Generating && inner.state != RecordingState::Correcting {
            return Err(reject("cancel_generation", inner.state));
        }
        if let Some(cancel) = &inner.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn fail(&self, code: &str, message: &str) {
        log::error!("{code}: {message}");
        (self.emit)(AppEvent::Error {
            code: code.to_string(),
            message: message.to_string(),
        });
    }

    /// Recover from a poisoned lock: a panic between transitions leaves the state
    /// readable and consistent, so we keep going rather than wedge the app.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn reject(action: &str, state: RecordingState) -> String {
    format!("{action} rejected: illegal in state {}", state.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Shared, inspectable record of what the coordinator asked the pipeline to
    /// do (the coordinator owns the `Box<dyn Pipeline>`, so the test keeps a
    /// clone of these handles to assert against afterward).
    #[derive(Default)]
    struct Calls {
        started: usize,
        stopped: usize,
        paused: Vec<bool>,
    }

    struct MockPipeline {
        calls: Arc<Mutex<Calls>>,
        fail_start: bool,
        fail_stop: bool,
        /// The record id a successful stop reports (None = empty consult).
        record_id: Option<String>,
    }

    impl MockPipeline {
        fn new(calls: Arc<Mutex<Calls>>) -> Self {
            Self {
                calls,
                fail_start: false,
                fail_stop: false,
                record_id: Some("rec-1".to_string()),
            }
        }
    }

    impl Pipeline for MockPipeline {
        fn start(&mut self) -> Result<()> {
            self.calls.lock().unwrap().started += 1;
            if self.fail_start {
                anyhow::bail!("mic open failed");
            }
            Ok(())
        }
        fn stop(&mut self) -> Result<Option<String>> {
            self.calls.lock().unwrap().stopped += 1;
            if self.fail_stop {
                anyhow::bail!("disk full");
            }
            Ok(self.record_id.clone())
        }
        fn set_paused(&mut self, paused: bool) {
            self.calls.lock().unwrap().paused.push(paused);
        }
    }

    /// A note generator with configurable behavior. By default it returns a saved
    /// note id immediately; `block_until_cancel` makes it spin until cancelled (to
    /// test cancellation), and `fail` makes it error.
    #[derive(Default)]
    struct MockGenerator {
        record_id: Option<String>,
        block_until_cancel: bool,
        fail: bool,
    }

    impl MockGenerator {
        fn saved() -> Self {
            Self {
                record_id: Some("note-1".to_string()),
                ..Default::default()
            }
        }
    }

    impl NoteGenerator for MockGenerator {
        fn generate(
            &self,
            _record_id: &str,
            _transcript: &str,
            cancel: Arc<AtomicBool>,
        ) -> Result<Option<String>> {
            if self.fail {
                anyhow::bail!("model load failed");
            }
            if self.block_until_cancel {
                // Spin (bounded) until cancel_generation flips the flag.
                for _ in 0..10_000 {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(None); // partial note discarded
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                panic!("cancel was never signalled");
            }
            Ok(self.record_id.clone())
        }
    }

    /// A correction suggester with configurable behavior, mirroring `MockGenerator`.
    /// By default it completes immediately; `block_until_cancel` spins until
    /// cancelled (to test the shared cancel path), and `fail` makes it error.
    #[derive(Default)]
    struct MockSuggester {
        block_until_cancel: bool,
        fail: bool,
    }

    impl CorrectionSuggester for MockSuggester {
        fn suggest(&self, _transcript: &str, cancel: Arc<AtomicBool>) -> Result<Option<()>> {
            if self.fail {
                anyhow::bail!("correction model failed");
            }
            if self.block_until_cancel {
                for _ in 0..10_000 {
                    if cancel.load(Ordering::Relaxed) {
                        return Ok(None); // cancelled: transcript stays plain
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                panic!("cancel was never signalled");
            }
            Ok(Some(()))
        }
    }

    /// Build a coordinator plus handles to inspect the pipeline calls and the
    /// emitted events. Uses a no-op generator/suggester unless a `build_with*`
    /// variant is used.
    fn build(pipeline: MockPipeline) -> (Coordinator, Arc<Mutex<Vec<AppEvent>>>) {
        build_with(pipeline, MockGenerator::saved(), MockSuggester::default())
    }

    fn build_with(
        pipeline: MockPipeline,
        generator: MockGenerator,
        suggester: MockSuggester,
    ) -> (Coordinator, Arc<Mutex<Vec<AppEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let emit: EmitFn = Box::new(move |ev| sink.lock().unwrap().push(ev));
        (
            Coordinator::new(
                Box::new(pipeline),
                Box::new(generator),
                Box::new(suggester),
                emit,
            ),
            events,
        )
    }

    #[test]
    fn start_then_stop_walks_idle_recording_processing_idle() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, events) = build(MockPipeline::new(calls.clone()));

        assert_eq!(co.state(), RecordingState::Idle);
        co.start_recording().unwrap();
        assert_eq!(co.state(), RecordingState::Recording);
        // Stop reports the persisted record id back to the caller (the command
        // resolves with it so the UI can later save edits / generate a note).
        assert_eq!(co.stop_recording().unwrap(), Some("rec-1".to_string()));
        assert_eq!(co.state(), RecordingState::Idle);

        let c = calls.lock().unwrap();
        assert_eq!((c.started, c.stopped), (1, 1));
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Recording),
                AppEvent::StateChanged(RecordingState::Processing),
                AppEvent::StateChanged(RecordingState::Idle),
            ]
        );
    }

    #[test]
    fn duplicate_start_is_rejected() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, _events) = build(MockPipeline::new(calls.clone()));

        co.start_recording().unwrap();
        assert!(co.start_recording().is_err()); // already RECORDING
        assert_eq!(co.state(), RecordingState::Recording);
        assert_eq!(calls.lock().unwrap().started, 1); // pipeline started once
    }

    #[test]
    fn stop_without_recording_is_rejected() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, _events) = build(MockPipeline::new(calls.clone()));

        assert!(co.stop_recording().is_err());
        assert_eq!(co.state(), RecordingState::Idle);
        assert_eq!(calls.lock().unwrap().stopped, 0);
    }

    #[test]
    fn start_failure_keeps_idle_and_emits_error() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut p = MockPipeline::new(calls.clone());
        p.fail_start = true;
        let (co, events) = build(p);

        assert!(co.start_recording().is_err());
        assert_eq!(co.state(), RecordingState::Idle);
        assert_eq!(
            *events.lock().unwrap(),
            vec![AppEvent::Error {
                code: "recording_start_failed".to_string(),
                message: "mic open failed".to_string(),
            }]
        );
    }

    #[test]
    fn stop_failure_still_returns_to_idle() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut p = MockPipeline::new(calls.clone());
        p.fail_stop = true;
        let (co, events) = build(p);

        co.start_recording().unwrap();
        assert!(co.stop_recording().is_err());
        assert_eq!(co.state(), RecordingState::Idle); // recovered, not wedged
        // Still walked through PROCESSING → IDLE, plus the error.
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Recording),
                AppEvent::StateChanged(RecordingState::Processing),
                AppEvent::StateChanged(RecordingState::Idle),
                AppEvent::Error {
                    code: "save_failed".to_string(),
                    message: "disk full".to_string(),
                },
            ]
        );
    }

    #[test]
    fn pause_and_resume_gate_the_pipeline_without_changing_state() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, events) = build(MockPipeline::new(calls.clone()));

        co.start_recording().unwrap();
        co.pause_recording().unwrap();
        assert!(co.pause_recording().is_err()); // already paused
        co.resume_recording().unwrap();
        assert!(co.resume_recording().is_err()); // not paused

        assert_eq!(co.state(), RecordingState::Recording);
        assert_eq!(calls.lock().unwrap().paused, vec![true, false]);
        // No state-changed beyond the initial RECORDING — PAUSED isn't a wire state.
        assert_eq!(
            *events.lock().unwrap(),
            vec![AppEvent::StateChanged(RecordingState::Recording)]
        );
    }

    #[test]
    fn pause_is_rejected_when_idle() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, _events) = build(MockPipeline::new(calls.clone()));

        assert!(co.pause_recording().is_err());
        assert!(co.resume_recording().is_err());
        assert!(calls.lock().unwrap().paused.is_empty());
    }

    #[test]
    fn generate_walks_idle_generating_idle_and_returns_note_id() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, events) = build(MockPipeline::new(calls));

        // Resolves with the persisted note id so the UI can edit / revert it.
        assert_eq!(
            co.generate_note("rec-1", "patient reports a headache").unwrap(),
            Some("note-1".to_string())
        );
        assert_eq!(co.state(), RecordingState::Idle);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Generating),
                AppEvent::StateChanged(RecordingState::Idle),
            ]
        );
    }

    #[test]
    fn generate_is_rejected_unless_idle() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, _events) = build(MockPipeline::new(calls));

        co.start_recording().unwrap(); // now RECORDING
        assert!(co.generate_note("rec-1", "t").is_err());
        assert_eq!(co.state(), RecordingState::Recording);
    }

    #[test]
    fn generation_failure_returns_to_idle_and_emits_error() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut g = MockGenerator::saved();
        g.fail = true;
        let (co, events) = build_with(MockPipeline::new(calls), g, MockSuggester::default());

        assert!(co.generate_note("rec-1", "t").is_err());
        assert_eq!(co.state(), RecordingState::Idle); // recovered, not wedged
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Generating),
                AppEvent::StateChanged(RecordingState::Idle),
                AppEvent::Error {
                    code: "generation_failed".to_string(),
                    message: "model load failed".to_string(),
                },
            ]
        );
    }

    #[test]
    fn cancel_is_rejected_when_not_generating() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, _events) = build(MockPipeline::new(calls));
        assert!(co.cancel_generation().is_err()); // IDLE, nothing to cancel
    }

    #[test]
    fn correction_walks_idle_correcting_idle() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let (co, events) = build(MockPipeline::new(calls));

        co.suggest_corrections("patient reports a headache").unwrap();
        assert_eq!(co.state(), RecordingState::Idle);
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Correcting),
                AppEvent::StateChanged(RecordingState::Idle),
            ]
        );
    }

    #[test]
    fn generate_is_blocked_while_correcting() {
        // The "sequenced, never concurrent" invariant (§6.7): a Generate arriving
        // mid-correction is rejected because the machine is CORRECTING, not IDLE.
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut s = MockSuggester::default();
        s.block_until_cancel = true;
        let (co, _events) = build_with(MockPipeline::new(calls), MockGenerator::saved(), s);
        let co = Arc::new(co);

        let worker = {
            let co = co.clone();
            std::thread::spawn(move || co.suggest_corrections("t"))
        };
        while co.state() != RecordingState::Correcting {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(co.generate_note("rec-1", "t").is_err()); // blocked: not IDLE

        // The shared cancel path unwinds the correction pass back to IDLE.
        co.cancel_generation().unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(co.state(), RecordingState::Idle);
    }

    #[test]
    fn correction_failure_returns_to_idle_without_a_global_error_event() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut s = MockSuggester::default();
        s.fail = true;
        let (co, events) = build_with(MockPipeline::new(calls), MockGenerator::saved(), s);

        assert!(co.suggest_corrections("t").is_err());
        assert_eq!(co.state(), RecordingState::Idle); // recovered, not wedged
        // Strictly additive (§6.7): no global `error` AppEvent, so no UI toast — only
        // the state walk back to IDLE. The suggester's own `correction-error` (ignored
        // by the UI) is the sole failure signal.
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                AppEvent::StateChanged(RecordingState::Correcting),
                AppEvent::StateChanged(RecordingState::Idle),
            ]
        );
    }

    #[test]
    fn cancel_generation_signals_the_running_generation() {
        // Concurrency: generate_note blocks in the generator (lock released) while
        // cancel_generation runs on the main thread and flips the flag.
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut g = MockGenerator::saved();
        g.block_until_cancel = true;
        let (co, _events) = build_with(MockPipeline::new(calls), g, MockSuggester::default());
        let co = Arc::new(co);

        let worker = {
            let co = co.clone();
            std::thread::spawn(move || co.generate_note("rec-1", "t"))
        };

        // Wait until the generation is actually in flight, then cancel it.
        while co.state() != RecordingState::Generating {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        co.cancel_generation().unwrap();

        // Cancelled generation discards the partial note (returns None) and the
        // machine settles back at IDLE.
        assert_eq!(worker.join().unwrap().unwrap(), None);
        assert_eq!(co.state(), RecordingState::Idle);
    }
}
