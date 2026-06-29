use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::audio_toolkit::{AudioRecorder, SileroVad, SmoothedVad};
use crate::segment::{spawn_stt_worker, Segmenter, SegmenterConfig, SttWorkerHandle};
use crate::stt::{SttEngine, Transcriber};

use super::coordinator::{AppEvent, Pipeline};

// VAD smoothing (30-ms frames). Tunable; chosen so a brief gap doesn't chop a
// sentence and the leading syllable isn't clipped (prefill ≥ onset − 1, see B4).
const VAD_THRESHOLD: f32 = 0.5;
const VAD_PREFILL_FRAMES: usize = 8; // ~240 ms pre-roll prepended on onset
const VAD_HANGOVER_FRAMES: usize = 24; // ~720 ms held after the detector goes quiet
const VAD_ONSET_FRAMES: usize = 3; // ~90 ms of voice required to start speech

/// The live capture → segment → transcribe pipeline. Built fresh on each
/// `start()` and fully torn down on `stop()`; only the STT model (`SttEngine`)
/// is long-lived and stays warm across recordings (design §6.4/§6.6).
pub struct RealPipeline {
    app: AppHandle,
    engine: Arc<SttEngine>,
    vad_model_path: PathBuf,
    running: Option<Running>,
}

/// The per-recording threads/handles, dropped in order on stop.
struct Running {
    recorder: AudioRecorder,
    segmenter: Arc<Mutex<Segmenter>>,
    worker: SttWorkerHandle,
    paused: Arc<AtomicBool>,
}

impl RealPipeline {
    pub fn new(app: AppHandle, engine: Arc<SttEngine>, vad_model_path: PathBuf) -> Self {
        Self {
            app,
            engine,
            vad_model_path,
            running: None,
        }
    }

    fn build_vad(&self) -> Result<SmoothedVad> {
        let silero = SileroVad::new(&self.vad_model_path, VAD_THRESHOLD)?;
        Ok(SmoothedVad::new(
            Box::new(silero),
            VAD_PREFILL_FRAMES,
            VAD_HANGOVER_FRAMES,
            VAD_ONSET_FRAMES,
        ))
    }
}

impl Pipeline for RealPipeline {
    fn start(&mut self) -> Result<()> {
        // Capture → segmenter queue → STT worker → UI.
        let (seg_tx, seg_rx) = std::sync::mpsc::channel();
        let segmenter = Arc::new(Mutex::new(Segmenter::new(
            Box::new(self.build_vad()?),
            SegmenterConfig::default(),
            seg_tx,
        )));

        let worker = {
            let app = self.app.clone();
            let transcriber: Arc<dyn Transcriber> = self.engine.clone();
            spawn_stt_worker(seg_rx, transcriber, move |res| match res {
                Ok(ts) => {
                    let _ = app.emit("transcript-segment", json!({ "seq": ts.seq, "text": ts.text }));
                }
                Err(e) => {
                    let _ = app.emit(
                        "error",
                        json!({ "code": "transcription_failed", "message": e.message }),
                    );
                }
            })
        };

        let paused = Arc::new(AtomicBool::new(false));

        let recorder = AudioRecorder::new()
            .map_err(|e| anyhow!("failed to create recorder: {e}"))?
            .with_frame_callback({
                let segmenter = segmenter.clone();
                let paused = paused.clone();
                move |frame: &[f32]| {
                    if paused.load(Ordering::Relaxed) {
                        return; // gated by pause; drop the frame, keep capturing
                    }
                    if let Ok(mut seg) = segmenter.lock() {
                        if let Err(e) = seg.push_frame(frame) {
                            log::warn!("segmenter dropped a frame: {e}");
                        }
                    }
                }
            })
            .with_level_callback({
                let app = self.app.clone();
                move |buckets: Vec<f32>| {
                    let _ = app.emit("input-level", json!({ "level": buckets }));
                }
            });

        let mut recorder = recorder;
        recorder
            .open(None) // default input device; settings-driven device is a later phase
            .map_err(|e| anyhow!("failed to open microphone: {e}"))?;
        recorder
            .start()
            .map_err(|e| anyhow!("failed to start capture: {e}"))?;

        // Only now that the pipeline is fully up: keep the model loaded for the
        // whole consult (even through long silence). Setting this earlier would
        // pin the model if any step above failed — the idle-watcher skips
        // unloading while recording is true.
        self.engine.set_recording(true);

        self.running = Some(Running {
            recorder,
            segmenter,
            worker,
            paused,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let Some(running) = self.running.take() else {
            return Ok(());
        };
        let Running {
            mut recorder,
            segmenter,
            worker,
            ..
        } = running;

        // 1. Stop capture; this tail-flushes the resampler's frames through the
        //    frame callback into the segmenter (design §6.6 step 1).
        let _ = recorder.stop();
        // 2. Shut the capture worker and release its clone of the frame callback,
        //    then drop the recorder so its own callback Arc is released too —
        //    leaving `segmenter` as the sole owner.
        let _ = recorder.close();
        drop(recorder);

        // 3. Flush the open segment, then drop the segmenter so its Sender closes
        //    and the STT worker's queue ends.
        if let Ok(mut seg) = segmenter.lock() {
            seg.finish();
        }
        drop(segmenter);

        // 4. Drain: the worker transcribes every remaining segment, then exits.
        worker.join();

        // Model stays warm; the idle-watcher unloads it later (design §6.6).
        self.engine.set_recording(false);
        Ok(())
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(running) = &self.running {
            running.paused.store(paused, Ordering::Relaxed);
        }
    }
}

/// Map an `AppEvent` from the coordinator onto a Tauri `emit`. This is the seam
/// where the testable state machine meets the real frontend bridge (design §9.5).
pub fn emit_app_event(app: &AppHandle, event: AppEvent) {
    match event {
        AppEvent::StateChanged(state) => {
            let _ = app.emit("state-changed", json!({ "state": state.as_str() }));
        }
        AppEvent::Error { code, message } => {
            let _ = app.emit("error", json!({ "code": code, "message": message }));
        }
    }
}
