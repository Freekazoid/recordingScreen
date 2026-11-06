//! Захват экрана и звука.
//!
//! Платформенные адаптеры скрыты за общими именами: на Linux это X11 и
//! Wayland-портал, на Windows — ffmpeg `gdigrab`, на macOS — ffmpeg
//! `avfoundation`. Интерфейс `VideoRecorder` одинаков, поэтому UI не знает,
//! какая ОС под ним.

pub mod segments;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod screenshot;
#[cfg(target_os = "linux")]
pub mod wayland;
#[cfg(windows)]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod x11;

#[cfg(target_os = "linux")]
mod portal_token;

pub use segments::{concat_segments, recover_all_segments, segment_pattern};

#[cfg(target_os = "macos")]
pub use macos::{MacVideoRecorder, list_windows, screenshot, take_screenshot};
#[cfg(target_os = "linux")]
pub use screenshot::take_screenshot;
#[cfg(target_os = "linux")]
pub use wayland::WaylandPortalRecorder;
#[cfg(windows)]
pub use windows::{WindowsVideoRecorder, list_windows, screenshot, take_screenshot};
#[cfg(target_os = "linux")]
pub use x11::{X11VideoRecorder, list_windows, screenshot};

/// Что выбирает пользователь в портале Wayland.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSource {
    Monitor,
    Window,
}

/// Сообщение, когда пользователь закрыл диалог портала без подтверждения.
pub const PORTAL_CANCELLED: &str = "доступ к экрану не предоставлен";

/// Ошибки запуска и остановки записи.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("запись уже запущена")]
    AlreadyRunning,

    #[error("не удалось создать каталог {path}: {source}")]
    CreateDir {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось запустить ffmpeg: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("не удалось остановить ffmpeg: {0}")]
    Stop(std::io::Error),
}

/// Окно для выбора режима «Программа».
#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    pub region: Region,
}

/// Прямоугольная область экрана в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Платформенный рекордер видео.
#[cfg(target_os = "linux")]
pub type VideoRecorder = x11::X11VideoRecorder;
/// Платформенный рекордер видео.
#[cfg(windows)]
pub type VideoRecorder = windows::WindowsVideoRecorder;
/// Платформенный рекордер видео.
#[cfg(target_os = "macos")]
pub type VideoRecorder = macos::MacVideoRecorder;

/// Заглушка Wayland-портала на Windows/macOS: путь выбирается только по
/// `WAYLAND_DISPLAY`, которого там нет, но тип нужен для общей UI-логики.
#[cfg(not(target_os = "linux"))]
#[derive(Debug, Default)]
pub struct WaylandPortalRecorder;

#[cfg(not(target_os = "linux"))]
impl WaylandPortalRecorder {
    /// Создаёт заглушку.
    pub fn new() -> Self {
        Self
    }

    /// Портал недоступен на этой ОС.
    pub fn start(&mut self, _output: &std::path::Path, _fps: u32) -> Result<(), String> {
        Err("Wayland-портал доступен только на Linux".to_string())
    }

    /// Портал недоступен на этой ОС.
    pub fn start_with_source(
        &mut self,
        _output: &std::path::Path,
        _fps: u32,
        _source: PortalSource,
        _crop: Option<Region>,
    ) -> Result<(), String> {
        Err("Wayland-портал доступен только на Linux".to_string())
    }

    /// Всегда `false`: записи нет.
    pub fn is_running(&mut self) -> bool {
        false
    }

    /// Нечего останавливать.
    pub fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Нечего останавливать.
    pub fn request_stop(&mut self) {}

    /// Результата запуска нет.
    pub fn take_start_result(&mut self) -> Option<Result<(), String>> {
        None
    }

    /// Ошибки нет.
    pub fn take_error(&self) -> Option<String> {
        None
    }

    /// Сбрасывает завершённое состояние.
    pub fn clear_finished(&mut self) {}

    /// Сбрасывает состояние принудительно.
    pub fn force_reset(&mut self) {}
}
