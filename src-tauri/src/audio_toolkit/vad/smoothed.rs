use super::{VadFrame, VoiceActivityDetector};
use anyhow::Result;
use std::collections::VecDeque;

/// Wraps a boolean VAD with onset, hangover and prefill smoothing so brief gaps
/// don't chop speech and the leading syllable isn't clipped:
/// - `onset_frames`: consecutive voice frames required before speech starts.
/// - `hangover_frames`: voice frames emitted after the detector goes quiet.
/// - `prefill_frames`: buffered pre-speech frames prepended on onset.
pub struct SmoothedVad {
    inner_vad: Box<dyn VoiceActivityDetector>,
    prefill_frames: usize,
    hangover_frames: usize,
    onset_frames: usize,

    frame_buffer: VecDeque<Vec<f32>>,
    hangover_counter: usize,
    onset_counter: usize,
    in_speech: bool,

    temp_out: Vec<f32>,
}

impl SmoothedVad {
    pub fn new(
        inner_vad: Box<dyn VoiceActivityDetector>,
        prefill_frames: usize,
        hangover_frames: usize,
        onset_frames: usize,
    ) -> Self {
        Self {
            inner_vad,
            prefill_frames,
            hangover_frames,
            onset_frames,
            frame_buffer: VecDeque::new(),
            hangover_counter: 0,
            onset_counter: 0,
            in_speech: false,
            temp_out: Vec::new(),
        }
    }
}

impl VoiceActivityDetector for SmoothedVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        // 1. Buffer every incoming frame for possible pre-roll. The cap must
        // cover both the prefill preroll AND the onset_frames voice frames seen
        // before the trigger fires, otherwise the earliest onset frames are
        // evicted and the leading syllable is clipped (e.g. onset=3, prefill=0
        // would lose the first ~60 ms of every utterance).
        self.frame_buffer.push_back(frame.to_vec());
        while self.frame_buffer.len() > self.prefill_frames + self.onset_frames {
            self.frame_buffer.pop_front();
        }

        // 2. Delegate to the wrapped boolean VAD
        let is_voice = self.inner_vad.is_voice(frame)?;

        match (self.in_speech, is_voice) {
            // Potential start of speech - need to accumulate onset frames
            (false, true) => {
                self.onset_counter += 1;
                if self.onset_counter >= self.onset_frames {
                    // We have enough consecutive voice frames to trigger speech
                    self.in_speech = true;
                    self.hangover_counter = self.hangover_frames;
                    self.onset_counter = 0; // Reset for next time

                    // Collect prefill + current frame
                    self.temp_out.clear();
                    for buf in &self.frame_buffer {
                        self.temp_out.extend(buf);
                    }
                    Ok(VadFrame::Speech(&self.temp_out))
                } else {
                    // Not enough frames yet, still silence
                    Ok(VadFrame::Noise)
                }
            }

            // Ongoing Speech
            (true, true) => {
                self.hangover_counter = self.hangover_frames;
                Ok(VadFrame::Speech(frame))
            }

            // End of Speech or interruption during onset phase
            (true, false) => {
                if self.hangover_counter > 0 {
                    self.hangover_counter -= 1;
                    Ok(VadFrame::Speech(frame))
                } else {
                    self.in_speech = false;
                    Ok(VadFrame::Noise)
                }
            }

            // Silence or broken onset sequence
            (false, false) => {
                self.onset_counter = 0; // Reset onset counter on silence
                Ok(VadFrame::Noise)
            }
        }
    }

    fn reset(&mut self) {
        self.frame_buffer.clear();
        self.hangover_counter = 0;
        self.onset_counter = 0;
        self.in_speech = false;
        self.temp_out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boolean VAD driven by a fixed script of voice/silence decisions, so the
    /// smoothing logic can be tested without the ONNX model.
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
            if self.script.pop_front().unwrap_or(false) {
                Ok(VadFrame::Speech(frame))
            } else {
                Ok(VadFrame::Noise)
            }
        }
    }

    const FRAME_LEN: usize = 480; // 30 ms @ 16 kHz

    fn frame() -> Vec<f32> {
        vec![0.5; FRAME_LEN]
    }

    #[test]
    fn onset_requires_consecutive_voice_frames() {
        // onset = 2: a single voice frame must not trigger speech.
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, true]), 0, 0, 2);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // 1st voice: not yet
        assert!(vad.push_frame(&f).unwrap().is_speech()); // 2nd voice: triggers
    }

    #[test]
    fn hangover_keeps_speech_after_silence() {
        // onset = 1, hangover = 1.
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, false, false]), 0, 1, 1);
        let f = frame();
        assert!(vad.push_frame(&f).unwrap().is_speech()); // triggers
        assert!(vad.push_frame(&f).unwrap().is_speech()); // hangover holds
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // hangover exhausted
    }

    #[test]
    fn prefill_prepends_buffered_frames() {
        // prefill = 1, onset = 1: onset output is prefill + current = 2 frames.
        let mut vad = SmoothedVad::new(ScriptedVad::new([false, true]), 1, 0, 1);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // silence, buffered
        match vad.push_frame(&f).unwrap() {
            VadFrame::Speech(buf) => assert_eq!(buf.len(), FRAME_LEN * 2),
            VadFrame::Noise => panic!("expected speech onset"),
        }
    }

    #[test]
    fn onset_frames_are_not_clipped_when_onset_exceeds_prefill() {
        // onset = 3, prefill = 0: the trigger must emit all 3 accumulated voice
        // frames, not just the last one (regression for the buffer-cap clip).
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, true, true]), 0, 0, 3);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // 1st voice: buffered
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // 2nd voice: buffered
        match vad.push_frame(&f).unwrap() {
            VadFrame::Speech(buf) => assert_eq!(buf.len(), FRAME_LEN * 3),
            VadFrame::Noise => panic!("expected speech onset"),
        }
    }

    #[test]
    fn silence_stays_noise() {
        let mut vad = SmoothedVad::new(ScriptedVad::new([false, false, false]), 0, 0, 1);
        let f = frame();
        for _ in 0..3 {
            assert!(!vad.push_frame(&f).unwrap().is_speech());
        }
    }
}
