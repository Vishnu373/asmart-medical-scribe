use std::sync::{Mutex, MutexGuard};

use anyhow::Result;

/// The backend-owned recording state (design §6.6). The UI only *requests*
/// transitions; the coordinator decides whether each is legal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Processing,
}

impl RecordingState {
    /// Wire form emitted in `state-changed` (design §9.5). Kept uppercase to
    /// match the event contract the frontend listens for.
    pub fn as_str(self) -> &'static str {
        match self {
            RecordingState::Idle => "IDLE",
            RecordingState::Recording => "RECORDING",
            RecordingState::Processing => "PROCESSING",
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

struct Inner {
    state: RecordingState,
    paused: bool,
    /// `None` only while a stop is draining: the pipeline is moved out so the
    /// blocking drain runs without holding the state lock, letting a concurrent
    /// Start observe PROCESSING and be rejected (design §6.6).
    pipeline: Option<Box<dyn Pipeline>>,
}

/// Single-threaded coordinator that owns the recording state and serializes all
/// transitions, modeled on the reference transcription coordinator. State guards
/// reject illegal or duplicate transitions so rapid clicks / hotkey spam can't
/// corrupt the machine (design §6.6).
pub struct Coordinator {
    inner: Mutex<Inner>,
    emit: EmitFn,
}

impl Coordinator {
    pub fn new(pipeline: Box<dyn Pipeline>, emit: EmitFn) -> Self {
        Self {
            inner: Mutex::new(Inner {
                state: RecordingState::Idle,
                paused: false,
                pipeline: Some(pipeline),
            }),
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

    /// Build a coordinator plus handles to inspect the pipeline calls and the
    /// emitted events.
    fn build(pipeline: MockPipeline) -> (Coordinator, Arc<Mutex<Vec<AppEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let emit: EmitFn = Box::new(move |ev| sink.lock().unwrap().push(ev));
        (Coordinator::new(Box::new(pipeline), emit), events)
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
}
