pub mod audio;
pub mod vad;

pub use audio::{
    is_microphone_access_denied,
    is_no_input_device_error,
    list_input_devices,
    AudioRecorder,
    CpalDeviceInfo,
    FrameResampler
};
pub use vad::{SileroVad, SmoothedVad, VadFrame, VoiceActivityDetector};

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

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
