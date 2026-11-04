//! Адаптеры к ОС и внешнему окружению.

pub mod capture;
pub mod models;
pub mod paths;

pub use capture::{CaptureError, X11VideoRecorder};
pub use models::{DownloadProgress, MODELS, ModelError, ModelManager, ModelSpec};
pub use paths::{PathError, Paths};
