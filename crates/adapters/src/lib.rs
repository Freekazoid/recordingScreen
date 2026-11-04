//! Адаптеры к ОС и внешнему окружению.

pub mod audio;
pub mod capture;
pub mod models;
pub mod paths;

pub use audio::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, LinuxAudioRecorder,
};
pub use capture::{CaptureError, X11VideoRecorder};
pub use models::{DownloadProgress, MODELS, ModelError, ModelManager, ModelSpec};
pub use paths::{PathError, Paths};
