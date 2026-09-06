use super::{VadFrame, VoiceActivityDetector};
use anyhow::Result;
use std::collections::VecDeque;

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
    // frames gets passed via Silero vad; decides whether to keep or discard
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> Result<VadFrame<'a>> {
        self.frame_buffer.push_back(frame.to_vec());
        while self.frame_buffer.len() > self.prefill_frames + self.onset_frames {
            self.frame_buffer.pop_front();
        }

        let is_voice = self.inner_vad.is_voice(frame)?;

        match (self.in_speech, is_voice) {
            // possible start - counting
            (false, true) => {
                self.onset_counter += 1;
                if self.onset_counter >= self.onset_frames {
                    self.in_speech = true;
                    self.hangover_counter = self.hangover_frames;
                    self.onset_counter = 0;

                    self.temp_out.clear();
                    for buf in &self.frame_buffer {
                        self.temp_out.extend(buf);
                    }
                    Ok(VadFrame::Speech(&self.temp_out))
                } else {
                    Ok(VadFrame::Noise)
                }
            }

            // ongoing Speech
            (true, true) => {
                self.hangover_counter = self.hangover_frames;
                Ok(VadFrame::Speech(frame))
            }

            // pause or end
            (true, false) => {
                if self.hangover_counter > 0 {
                    self.hangover_counter -= 1;
                    Ok(VadFrame::Speech(frame))
                } else {
                    self.in_speech = false;
                    Ok(VadFrame::Noise)
                }
            }

            // silence
            (false, false) => {
                self.onset_counter = 0;
                Ok(VadFrame::Noise)
            }
        }
    }

    // clears for next recording
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

    const FRAME_LEN: usize = 480;

    fn frame() -> Vec<f32> {
        vec![0.5; FRAME_LEN]
    }

    #[test]
    fn onset_requires_consecutive_voice_frames() {
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, true]), 0, 0, 2);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // 1st voice: not yet
        assert!(vad.push_frame(&f).unwrap().is_speech()); // 2nd voice: triggers
    }

    #[test]
    fn hangover_keeps_speech_after_silence() {
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, false, false]), 0, 1, 1);
        let f = frame();
        assert!(vad.push_frame(&f).unwrap().is_speech()); // triggers
        assert!(vad.push_frame(&f).unwrap().is_speech()); // hangover holds
        assert!(!vad.push_frame(&f).unwrap().is_speech()); // hangover exhausted
    }

    #[test]
    fn prefill_prepends_buffered_frames() {
        let mut vad = SmoothedVad::new(ScriptedVad::new([false, true]), 1, 0, 1);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech());
        match vad.push_frame(&f).unwrap() {
            VadFrame::Speech(buf) => assert_eq!(buf.len(), FRAME_LEN * 2),
            VadFrame::Noise => panic!("expected speech onset"),
        }
    }

    #[test]
    fn onset_frames_are_not_clipped_when_onset_exceeds_prefill() {
        let mut vad = SmoothedVad::new(ScriptedVad::new([true, true, true]), 0, 0, 3);
        let f = frame();
        assert!(!vad.push_frame(&f).unwrap().is_speech());
        assert!(!vad.push_frame(&f).unwrap().is_speech());
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
