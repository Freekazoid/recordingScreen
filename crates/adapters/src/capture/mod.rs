//! Захват экрана и звука.
//!
//! Первый адаптер — X11-видео через ffmpeg. Wayland-портал и аудиодорожки
//! добавляются следующими шагами, не меняя интерфейс `VideoRecorder`.

pub mod x11;

pub use x11::{CaptureError, X11VideoRecorder};
