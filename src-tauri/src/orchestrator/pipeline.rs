use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::audio_toolkit::{AudioRecorder, SileroVad, SmoothedVad};
use crate::llm::LlmEngine;
use crate::segment::{spawn_stt_worker, Segmenter, SegmenterConfig, SttWorkerHandle};
use crate::store::SharedStore;
use crate::stt::{ModelKind, SttEngine, Transcriber};

use super::coordinator::{AppEvent, Pipeline};

// VAD smoothing (30-ms frames). Tunable; chosen so a brief gap doesn't chop a
// sentence and the leading syllable isn't clipped (prefill ≥ onset − 1, see B4).
const VAD_THRESHOLD: f32 = 0.5;
const VAD_PREFILL_FRAMES: usize = 8; // ~240 ms pre-roll prepended on onset
const VAD_HANGOVER_FRAMES: usize = 24; // ~720 ms held after the detector goes quiet
const VAD_ONSET_FRAMES: usize = 3; // ~90 ms of voice required to start speech

// Language stamped on a saved record. Until settings/detection are wired (F6),
// consults are saved as English; the doctor can't yet change this. (Divergence
// from design §9.2, which expects `en`/`fr` from settings — recorded in the log.)
const DEFAULT_LANGUAGE: &str = "en";

/// The live capture → segment → transcribe pipeline. Built fresh on each
/// `start()` and fully torn down on `stop()`; only the STT model (`SttEngine`)
/// is long-lived and stays warm across recordings (design §6.4/§6.6).
pub struct RealPipeline {
    app: AppHandle,
    engine: Arc<SttEngine>,
    vad_model_path: PathBuf,
    /// Model-file search dirs (D1 resolver order): the STT model is resolved from
    /// these on each `start()` so the bundled Parakeet model is loaded before
    /// capture begins, rather than assumed already loaded.
    model_dirs: Vec<PathBuf>,
    store: SharedStore,
    /// App data dir; on a DB save failure the transcript is spilled here as a
    /// recoverable `.txt` so a finished consult is never lost (it lived only in
    /// memory until this point).
    data_dir: PathBuf,
    /// Note-generation engine. The pipeline only touches its prefill session: start one
    /// per recording and push each finished segment at it (design §8.9).
    llm: Arc<LlmEngine>,
    running: Option<Running>,
}

/// The per-recording threads/handles, dropped in order on stop.
struct Running {
    recorder: AudioRecorder,
    segmenter: Arc<Mutex<Segmenter>>,
    worker: SttWorkerHandle,
    paused: Arc<AtomicBool>,
    /// Transcribed segment text, accumulated in `seq` order by the worker sink and
    /// joined into the saved transcript on stop (design §9.6: the document, not
    /// the transient segments, is what's persisted).
    transcript: Arc<Mutex<Vec<String>>>,
}

impl RealPipeline {
    pub fn new(
        app: AppHandle,
        engine: Arc<SttEngine>,
        vad_model_path: PathBuf,
        model_dirs: Vec<PathBuf>,
        store: SharedStore,
        data_dir: PathBuf,
        llm: Arc<LlmEngine>,
    ) -> Self {
        Self {
            app,
            engine,
            vad_model_path,
            model_dirs,
            store,
            data_dir,
            llm,
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
    fn start(&mut self, id: &str) -> Result<()> {
        // Ensure the STT model is loaded before capture starts, resolving the
        // bundled Parakeet model from the model dirs (no-op if already warm). Fail
        // here — before spinning up capture threads — so a missing model surfaces
        // as a clean start error rather than a per-segment transcription failure.
        self.engine
            .ensure_loaded(ModelKind::Parakeet, &self.model_dirs)
            .map_err(|e| anyhow!("failed to load STT model: {e}"))?;

        // Capture → segmenter queue → STT worker → UI.
        let (seg_tx, seg_rx) = std::sync::mpsc::channel();
        let segmenter = Arc::new(Mutex::new(Segmenter::new(
            Box::new(self.build_vad()?),
            SegmenterConfig::default(),
            seg_tx,
        )));

        let transcript = Arc::new(Mutex::new(Vec::<String>::new()));

        // Start this recording's prefill session before the first segment can land, so no
        // segment is silently dropped on the floor. Replaces any previous session.
        self.llm.begin_prefill();

        let worker = {
            let app = self.app.clone();
            let transcript = transcript.clone();
            let llm = self.llm.clone();
            let transcriber: Arc<dyn Transcriber> = self.engine.clone();
            spawn_stt_worker(seg_rx, transcriber, move |res| match res {
                Ok(ts) => {
                    // Accumulate for the final saved transcript (the worker emits
                    // in seq order, so push order is seq order), then notify the UI.
                    if let Ok(mut t) = transcript.lock() {
                        t.push(ts.text.clone());
                    }
                    let _ = app.emit(
                        "transcript-segment",
                        json!({ "seq": ts.seq, "text": ts.text }),
                    );
                    // Second destination after the UI (design §6.5). Queued, never inline:
                    // prefilling here would hold segment N+1's transcription behind
                    // segment N's prefill, serializing the two phases §8.9 overlaps.
                    llm.push_prefill_segment(ts.seq, &ts.text);
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
            })
            .with_error_callback({
                // §10.3 `[RECORD] {record_id} audio device failed mid-recording`
                // (+ telemetry + a UI notification). The bare catalog line is on-device
                // only and the telemetry carries no error string: a cpal device error can
                // embed the mic name, which is PII and never leaves the device (§10.3).
                let app = self.app.clone();
                let record_id = id.to_string();
                move |_err: String| {
                    log::error!("[RECORD] {record_id} audio device failed mid-recording");
                    crate::telemetry::track_event("audio_device_failed", json!({}));
                    let _ = app.emit(
                        "error",
                        json!({
                            "code": "audio_device_failed",
                            "message": "The microphone stopped working during recording.",
                        }),
                    );
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
            transcript,
        });
        Ok(())
    }

    fn stop(&mut self, id: &str) -> Result<Option<String>> {
        let Some(running) = self.running.take() else {
            return Ok(None);
        };
        let Running {
            mut recorder,
            segmenter,
            worker,
            transcript,
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

        // 5. Assemble the finalized transcript and persist the encounter. An empty
        //    consult (silence / nothing recognized) isn't saved.
        let segments = match transcript.lock() {
            Ok(t) => t.clone(),
            Err(p) => p.into_inner().clone(),
        };
        let text = assemble_transcript(&segments);
        if text.is_empty() {
            return Ok(None);
        }
        // Label starts empty; the doctor titles the encounter in the Records view.
        // Uses the id pre-minted on Start so the row matches the logged record id.
        match self
            .store
            .lock()
            .create_record(id, "", DEFAULT_LANGUAGE, &text)
        {
            Ok(record) => Ok(Some(record.id)),
            Err(e) => {
                // The transcript only ever lived in memory — don't let a DB error
                // throw away a whole consult. Spill it to a recoverable file and
                // fold that location into the error the UI surfaces.
                match save_fallback_transcript(&self.data_dir, &text) {
                    Ok(path) => Err(anyhow!(
                        "failed to save record: {e}; transcript preserved at {}",
                        path.display()
                    )),
                    Err(write_err) => Err(anyhow!(
                        "failed to save record: {e}; \
                         AND failed to write fallback transcript: {write_err}"
                    )),
                }
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(running) = &self.running {
            running.paused.store(paused, Ordering::Relaxed);
        }
    }
}

/// Join the transcribed segments into the finalized transcript blob persisted on
/// the record (design §9.6: segments are transient transport; the document is the
/// blob). The worker already skips empty results; trimming here is defensive.
fn assemble_transcript(segments: &[String]) -> String {
    segments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Last-resort recovery when the encrypted DB insert fails: write the assembled
/// transcript to a timestamped `.txt` in the data dir and return its path. NB:
/// this file is plaintext PHI outside the SQLCipher DB — acceptable only because
/// the alternative is silently losing the consult; the doctor re-imports/deletes
/// it. Stays on the same device (no egress).
fn save_fallback_transcript(dir: &Path, text: &str) -> Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("recovered-transcript-{ts}.txt"));
    std::fs::write(&path, text)?;
    Ok(path)
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

#[cfg(test)]
mod tests {
    use super::assemble_transcript;

    #[test]
    fn assembles_segments_into_one_blob() {
        let segments = vec![
            "Patient reports a headache.".to_string(),
            "  No fever.  ".to_string(),
            String::new(), // defensive: skipped
            "Started two days ago.".to_string(),
        ];
        assert_eq!(
            assemble_transcript(&segments),
            "Patient reports a headache. No fever. Started two days ago."
        );
    }

    #[test]
    fn empty_or_blank_segments_assemble_to_empty() {
        assert_eq!(assemble_transcript(&[]), "");
        assert_eq!(assemble_transcript(&["   ".to_string()]), "");
    }
}
