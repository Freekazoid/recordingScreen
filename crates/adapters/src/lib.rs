//! Адаптеры к ОС и внешнему окружению.

pub mod audio;
pub mod capture;
pub mod media;
pub mod models;
pub mod paths;
#[cfg(target_os = "linux")]
mod portal;

pub use audio::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioRecorder, AudioStart,
    AudioTrackAnalysis, LinuxAudioRecorder, analyze_track, read_levels,
};
#[cfg(target_os = "linux")]
pub use capture::X11VideoRecorder;
pub use capture::{
    CaptureError, PORTAL_CANCELLED, PortalSource, Region, VideoRecorder, WaylandPortalRecorder,
    WindowInfo, concat_segments, list_windows, recover_all_segments, segment_pattern,
    take_screenshot,
};
pub use media::{ExportRequest, MediaError, merge_audio, merge_recording};
pub use models::{
    DownloadProgress, MODELS, ModelError, ModelManager, ModelSpec, resolve_models_root,
};
pub use paths::{PathError, Paths};
