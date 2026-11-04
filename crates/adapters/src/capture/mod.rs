//! Захват экрана и звука.
//!
//! Первый адаптер — X11-видео через ffmpeg. Wayland-портал и аудиодорожки
//! добавляются следующими шагами, не меняя интерфейс `VideoRecorder`.

pub mod screenshot;
pub mod wayland;
pub mod x11;

pub use screenshot::take_screenshot;
pub use wayland::{PortalSource, WaylandPortalRecorder};
pub use x11::{CaptureError, WindowInfo, X11VideoRecorder, list_windows};

/// Прямоугольная область экрана в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
