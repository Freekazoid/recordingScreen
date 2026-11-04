//! Адаптеры к ОС и внешнему окружению.

pub mod capture;
pub mod paths;

pub use capture::{CaptureError, X11VideoRecorder};
pub use paths::{PathError, Paths};
