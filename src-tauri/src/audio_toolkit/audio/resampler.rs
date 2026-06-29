use rubato::{FftFixedIn, Resampler};
use std::time::Duration;

// Make this a constant you can tweak
const RESAMPLER_CHUNK_SIZE: usize = 1024;

/// Streaming resampler that converts an input sample stream to a fixed output
/// rate, emitting fixed-duration mono frames. When in/out rates match it becomes
/// a pass-through framer.
pub struct FrameResampler {
    resampler: Option<FftFixedIn<f32>>,
    chunk_in: usize,
    in_buf: Vec<f32>,
    frame_samples: usize,
    pending: Vec<f32>,
}

impl FrameResampler {
    pub fn new(in_hz: usize, out_hz: usize, frame_dur: Duration) -> Self {
        let frame_samples = ((out_hz as f64 * frame_dur.as_secs_f64()).round()) as usize;
        assert!(frame_samples > 0, "frame duration too short");

        // Use fixed chunk size instead of GCD-based
        let chunk_in = RESAMPLER_CHUNK_SIZE;

        let resampler = (in_hz != out_hz).then(|| {
            FftFixedIn::<f32>::new(in_hz, out_hz, chunk_in, 1, 1)
                .expect("Failed to create resampler")
        });

        Self {
            resampler,
            chunk_in,
            in_buf: Vec::with_capacity(chunk_in),
            frame_samples,
            pending: Vec::with_capacity(frame_samples),
        }
    }

    pub fn push(&mut self, mut src: &[f32], mut emit: impl FnMut(&[f32])) {
        if self.resampler.is_none() {
            self.emit_frames(src, &mut emit);
            return;
        }

        while !src.is_empty() {
            let space = self.chunk_in - self.in_buf.len();
            let take = space.min(src.len());
            self.in_buf.extend_from_slice(&src[..take]);
            src = &src[take..];

            if self.in_buf.len() == self.chunk_in {
                if let Ok(out) = self
                    .resampler
                    .as_mut()
                    .unwrap()
                    .process(&[&self.in_buf[..]], None)
                {
                    self.emit_frames(&out[0], &mut emit);
                }
                self.in_buf.clear();
            }
        }
    }

    pub fn finish(&mut self, mut emit: impl FnMut(&[f32])) {
        // Process any remaining input samples
        if let Some(ref mut resampler) = self.resampler {
            if !self.in_buf.is_empty() {
                // Pad with zeros to reach chunk size
                self.in_buf.resize(self.chunk_in, 0.0);
                if let Ok(out) = resampler.process(&[&self.in_buf[..]], None) {
                    self.emit_frames(&out[0], &mut emit);
                }
            }
        }

        // Emit any remaining pending frame (padded with zeros)
        if !self.pending.is_empty() {
            self.pending.resize(self.frame_samples, 0.0);
            emit(&self.pending);
            self.pending.clear();
        }

        // Reset for the next recording: one FrameResampler lives for the whole
        // worker, so leftover input bytes or the resampler's internal FFT/overlap
        // state would otherwise leak the previous consult's tail into the next.
        self.in_buf.clear();
        if let Some(ref mut resampler) = self.resampler {
            resampler.reset();
        }
    }

    fn emit_frames(&mut self, mut data: &[f32], emit: &mut impl FnMut(&[f32])) {
        while !data.is_empty() {
            let space = self.frame_samples - self.pending.len();
            let take = space.min(data.len());
            self.pending.extend_from_slice(&data[..take]);
            data = &data[take..];

            if self.pending.len() == self.frame_samples {
                emit(&self.pending);
                self.pending.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 second of 48 kHz audio should resample to ~1 second of 16 kHz audio.
    #[test]
    fn downsamples_48k_to_16k() {
        let input = vec![0.25f32; 48_000];
        let mut out = Vec::new();
        let mut rs = FrameResampler::new(48_000, 16_000, Duration::from_millis(30));
        rs.push(&input, |frame| out.extend_from_slice(frame));
        rs.finish(|frame| out.extend_from_slice(frame));

        // Expect ~16k samples; allow a few frames of slack for chunk buffering
        // and the zero-padded final frame.
        let frame = (16_000f64 * 0.03) as i64; // 480
        let diff = (out.len() as i64 - 16_000).abs();
        assert!(
            diff <= 3 * frame,
            "got {} samples, expected ~16000 (diff {diff})",
            out.len()
        );
    }

    /// Equal rates: pass-through framing, length preserved up to the final
    /// zero-padded frame.
    #[test]
    fn passthrough_when_rates_match() {
        let input = vec![0.1f32; 16_000];
        let mut out = Vec::new();
        let mut rs = FrameResampler::new(16_000, 16_000, Duration::from_millis(30));
        rs.push(&input, |frame| out.extend_from_slice(frame));
        rs.finish(|frame| out.extend_from_slice(frame));

        let frame = (16_000f64 * 0.03) as usize; // 480
        assert!(
            out.len() >= 16_000 && out.len() <= 16_000 + frame,
            "got {} samples, expected 16000..={}",
            out.len(),
            16_000 + frame
        );
    }

    /// A second recording on the same resampler must not inherit the first's
    /// tail: finish() clears in_buf and resets the resampler, so cycle two
    /// produces the same output length as if it ran on a fresh resampler.
    #[test]
    fn finish_resets_state_between_recordings() {
        let mut rs = FrameResampler::new(48_000, 16_000, Duration::from_millis(30));

        // First recording, then finish.
        rs.push(&vec![0.25f32; 48_000], |_| {});
        rs.finish(|_| {});

        // Second recording on the same resampler.
        let mut reused = Vec::new();
        rs.push(&vec![0.25f32; 48_000], |frame| reused.extend_from_slice(frame));
        rs.finish(|frame| reused.extend_from_slice(frame));

        // Same input on a brand-new resampler.
        let mut fresh = Vec::new();
        let mut rs2 = FrameResampler::new(48_000, 16_000, Duration::from_millis(30));
        rs2.push(&vec![0.25f32; 48_000], |frame| fresh.extend_from_slice(frame));
        rs2.finish(|frame| fresh.extend_from_slice(frame));

        assert_eq!(
            reused.len(),
            fresh.len(),
            "reused resampler emitted {} samples, fresh emitted {}",
            reused.len(),
            fresh.len()
        );
    }

    /// Frames are emitted at the configured size while streaming.
    #[test]
    fn emits_fixed_size_frames() {
        let mut sizes = Vec::new();
        let mut rs = FrameResampler::new(16_000, 16_000, Duration::from_millis(30));
        rs.push(&vec![0.0f32; 5_000], |frame| sizes.push(frame.len()));
        assert!(!sizes.is_empty());
        assert!(sizes.iter().all(|&s| s == 480));
    }
}
