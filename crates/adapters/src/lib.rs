//! Адаптеры к ОС и внешнему окружению.

pub mod audio;
pub mod capture;
pub mod media;
pub mod models;
pub mod paths;
mod portal;

pub use audio::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, AudioTrackAnalysis,
    LinuxAudioRecorder, analyze_track, read_levels,
};
pub use capture::{
    CaptureError, PORTAL_CANCELLED, PortalSource, Region, WaylandPortalRecorder, WindowInfo,
    X11VideoRecorder, concat_segments, list_windows, recover_all_segments, segment_pattern,
    take_screenshot,
};
pub use media::{ExportRequest, MediaError, merge_audio, merge_recording};
pub use models::{DownloadProgress, MODELS, ModelError, ModelManager, ModelSpec};
pub use paths::{PathError, Paths};
