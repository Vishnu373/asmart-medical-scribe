//! Low-level audio: capture (cpal), resampling (rubato → 16 kHz mono f32) and a
//! spectrum level meter. Ported and adapted from the reference STT toolkit (B3).
//! Voice-activity detection lands in B4.

pub mod audio;
pub mod vad;

pub use audio::{
    is_microphone_access_denied, is_no_input_device_error, list_input_devices, read_wav_samples,
    save_wav_file, verify_wav_file, AudioRecorder, CpalDeviceInfo, FrameResampler,
};
pub use vad::{SileroVad, SmoothedVad, VadFrame, VoiceActivityDetector};

/// Sample rate the STT pipeline expects: 16 kHz mono f32. Capture runs at the
/// device's native rate and `FrameResampler` downsamples to this.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Returns the CPAL host for the current platform. Linux prefers the ALSA host;
/// every other platform uses the default.
pub fn get_cpal_host() -> cpal::Host {
    #[cfg(target_os = "linux")]
    {
        cpal::host_from_id(cpal::HostId::Alsa).unwrap_or_else(|_| cpal::default_host())
    }
    #[cfg(not(target_os = "linux"))]
    {
        cpal::default_host()
    }
}
