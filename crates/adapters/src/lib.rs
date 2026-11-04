//! Адаптеры к ОС и внешнему окружению.

pub mod audio;
pub mod capture;
pub mod media;
pub mod models;
pub mod paths;

pub use audio::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, LinuxAudioRecorder,
    read_levels,
};
pub use capture::{
    CaptureError, PortalSource, Region, WaylandPortalRecorder, WindowInfo, X11VideoRecorder,
    list_windows, take_screenshot,
};
pub use media::{ExportRequest, MediaError, merge_audio, merge_recording};
pub use models::{DownloadProgress, MODELS, ModelError, ModelManager, ModelSpec};
pub use paths::{PathError, Paths};
