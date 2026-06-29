use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::audio_toolkit::vad::{VadFrame, VoiceActivityDetector};
use crate::audio_toolkit::TARGET_SAMPLE_RATE;

/// One finished speech segment: a slice of 16 kHz mono audio plus the monotonic
/// sequence number that fixes its place in the transcript (design §6.3/§6.5).
pub struct Segment {
    pub seq: u64,
    pub audio: Vec<f32>,
}

/// Segment-length bounds in samples (design §6.3 "Edge cases").
#[derive(Clone, Copy, Debug)]
pub struct SegmenterConfig {
    /// Segments shorter than this at a pause boundary are discarded as blips.
    pub min_samples: usize,
    /// A speaker who never pauses is force-cut once the open segment reaches
    /// this many samples, bounding latency (NFR-1) and memory (NFR-5).
    pub max_samples: usize,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        let rate = TARGET_SAMPLE_RATE as usize;
        Self {
            min_samples: rate / 5, // 0.2 s
            max_samples: rate * 25, // 25 s
        }
    }
}

/// Consumes 16 kHz frames, runs them through the (smoothed) VAD, accumulates
/// speech into the current segment, and cuts a numbered segment onto the queue
/// at each pause boundary or max-length cap. The capture thread drives this; the
/// STT worker (see `worker.rs`) drains the queue, so a slow model never stalls
/// capture (design §6.3).
pub struct Segmenter {
    vad: Box<dyn VoiceActivityDetector>,
    config: SegmenterConfig,
    tx: Sender<Segment>,
    current: Vec<f32>,
    in_speech: bool,
    /// True when the open buffer is the remainder of an utterance that was
    /// force-cut by the max-cap (not a fresh segment). Its trailing piece must
    /// bypass the blip floor, or a speaker who runs just past the cap loses the
    /// tail of the sentence.
    cap_split: bool,
    next_seq: u64,
}

impl Segmenter {
    pub fn new(
        vad: Box<dyn VoiceActivityDetector>,
        config: SegmenterConfig,
        tx: Sender<Segment>,
    ) -> Self {
        Self {
            vad,
            config,
            tx,
            current: Vec::new(),
            in_speech: false,
            cap_split: false,
            next_seq: 0,
        }
    }

    /// Feed one 30 ms frame. Speech frames extend the open segment; the first
    /// silence after speech closes it; exceeding `max_samples` force-cuts it
    /// while staying open for the continuing speech.
    pub fn push_frame(&mut self, frame: &[f32]) -> Result<()> {
        match self.vad.push_frame(frame)? {
            VadFrame::Speech(samples) => {
                self.current.extend_from_slice(samples);
                self.in_speech = true;
                if self.current.len() >= self.config.max_samples {
                    // Max-cap cut: emit but keep accumulating — the speaker is
                    // still talking, this isn't a real pause boundary. The
                    // remainder is part of the same utterance, so don't let the
                    // blip floor drop its tail at the eventual pause.
                    self.cut(true);
                    self.cap_split = true;
                }
            }
            VadFrame::Noise => {
                if self.in_speech {
                    // Bypass the min-floor only if this segment is a cap-split
                    // remainder; a fresh standalone blip is still discarded.
                    self.cut(self.cap_split);
                    self.in_speech = false;
                    self.cap_split = false;
                }
            }
        }
        Ok(())
    }

    /// Tail flush on Stop: force-cut any still-open segment so the final words
    /// aren't lost, then reset the VAD and sequence counter for the next
    /// recording (design §6.3).
    pub fn finish(&mut self) {
        self.cut(true);
        self.in_speech = false;
        self.cap_split = false;
        // Renumber from 0 for the next recording: each consult's transcript is
        // an independent seq-ordered list (design §9.5), so a reused Segmenter
        // must not carry the prior consult's count forward.
        self.next_seq = 0;
        self.vad.reset();
    }

    /// Push the accumulated segment onto the queue. With `force` the min-length
    /// floor is bypassed (Stop tail flush); otherwise a too-short segment is
    /// dropped as a blip and no sequence number is consumed.
    fn cut(&mut self, force: bool) {
        if self.current.is_empty() {
            return;
        }
        if !force && self.current.len() < self.config.min_samples {
            self.current.clear();
            return;
        }
        let audio = std::mem::take(&mut self.current);
        let seq = self.next_seq;
        self.next_seq += 1;
        // A send error means the worker/receiver is gone (shutdown); drop the
        // segment rather than panic.
        let _ = self.tx.send(Segment { seq, audio });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::{channel, Receiver};

    const FRAME_LEN: usize = 480; // 30 ms @ 16 kHz

    /// A VAD driven by a fixed script of speech/silence decisions, so the
    /// segmenter's cutting logic can be tested without the ONNX model or the
    /// smoothing layer (those are exercised in B4).
    struct ScriptedVad {
        script: VecDeque<bool>,
    }

    impl ScriptedVad {
        fn new(decisions: impl IntoIterator<Item = bool>) -> Box<dyn VoiceActivityDetector> {
            Box::new(Self {
                script: decisions.into_iter().collect(),
            })
        }
    }

    impl VoiceActivityDetector for ScriptedVad {
        fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
            Ok(if self.script.pop_front().unwrap_or(false) {
                VadFrame::Speech(frame)
            } else {
                VadFrame::Noise
            })
        }
    }

    fn frame() -> Vec<f32> {
        vec![0.5; FRAME_LEN]
    }

    /// Drive a script through a segmenter and collect the emitted segments.
    fn run(
        decisions: impl IntoIterator<Item = bool>,
        config: SegmenterConfig,
        finish: bool,
    ) -> Vec<Segment> {
        let decisions: Vec<bool> = decisions.into_iter().collect();
        let (tx, rx): (Sender<Segment>, Receiver<Segment>) = channel();
        let mut seg = Segmenter::new(ScriptedVad::new(decisions.clone()), config, tx);
        let f = frame();
        for _ in &decisions {
            seg.push_frame(&f).unwrap();
        }
        if finish {
            seg.finish();
        }
        drop(seg); // drop the Sender so rx ends
        rx.iter().collect()
    }

    #[test]
    fn cuts_a_segment_at_each_pause_boundary() {
        let cfg = SegmenterConfig {
            min_samples: 1,
            max_samples: usize::MAX,
        };
        // speech×3, pause, speech×2, pause.
        let segs = run([true, true, true, false, true, true, false], cfg, false);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].seq, 0);
        assert_eq!(segs[0].audio.len(), 3 * FRAME_LEN);
        assert_eq!(segs[1].seq, 1);
        assert_eq!(segs[1].audio.len(), 2 * FRAME_LEN);
    }

    #[test]
    fn min_floor_discards_blips_without_consuming_a_seq() {
        let cfg = SegmenterConfig {
            min_samples: 2 * FRAME_LEN + 1, // a single frame is too short
            max_samples: usize::MAX,
        };
        // 1-frame blip (dropped), then a real 3-frame segment.
        let segs = run([true, false, true, true, true, false], cfg, false);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].seq, 0); // blip didn't consume seq 0
        assert_eq!(segs[0].audio.len(), 3 * FRAME_LEN);
    }

    #[test]
    fn max_cap_force_cuts_a_non_pausing_speaker() {
        let cfg = SegmenterConfig {
            min_samples: 1,
            max_samples: FRAME_LEN, // every frame hits the cap
        };
        let segs = run([true, true, true, false], cfg, false);
        assert_eq!(segs.len(), 3);
        for (i, s) in segs.iter().enumerate() {
            assert_eq!(s.seq, i as u64);
            assert_eq!(s.audio.len(), FRAME_LEN);
        }
    }

    #[test]
    fn cap_split_remainder_is_not_dropped_by_min_floor() {
        let cfg = SegmenterConfig {
            min_samples: 2 * FRAME_LEN, // a lone 1-frame tail is below the floor
            max_samples: 2 * FRAME_LEN, // cap after two frames
        };
        // 3 frames of unbroken speech then a pause: the cap cuts the first 2,
        // and the 1-frame remainder must still be emitted (same utterance).
        let segs = run([true, true, true, false], cfg, false);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].audio.len(), 2 * FRAME_LEN); // capped chunk
        assert_eq!(segs[1].seq, 1);
        assert_eq!(segs[1].audio.len(), FRAME_LEN); // tail kept despite < min
    }

    #[test]
    fn finish_force_flushes_open_segment_below_min_floor() {
        let cfg = SegmenterConfig {
            min_samples: 10 * FRAME_LEN, // larger than the tail
            max_samples: usize::MAX,
        };
        // One short speech frame, no closing pause — Stop must still emit it.
        let segs = run([true], cfg, true);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].seq, 0);
        assert_eq!(segs[0].audio.len(), FRAME_LEN);
    }

    #[test]
    fn reused_segmenter_renumbers_from_zero_each_recording() {
        let cfg = SegmenterConfig {
            min_samples: 1,
            max_samples: usize::MAX,
        };
        let (tx, rx): (Sender<Segment>, Receiver<Segment>) = channel();
        // Two consults, one frame of speech each, through the SAME segmenter.
        let mut seg = Segmenter::new(ScriptedVad::new([true, true]), cfg, tx);
        let f = frame();

        seg.push_frame(&f).unwrap(); // consult 1
        seg.finish();
        seg.push_frame(&f).unwrap(); // consult 2
        seg.finish();
        drop(seg);

        let segs: Vec<Segment> = rx.iter().collect();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].seq, 0);
        assert_eq!(segs[1].seq, 0); // second consult restarts at 0, not 1
    }

    #[test]
    fn silence_only_emits_nothing() {
        let cfg = SegmenterConfig::default();
        let segs = run([false, false, false], cfg, true);
        assert!(segs.is_empty());
    }
}
