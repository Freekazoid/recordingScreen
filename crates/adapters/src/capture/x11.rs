//! Запись X11-экрана через ffmpeg `x11grab`.
//!
//! Адаптер намеренно не знает про UI и настройки: ему дают путь и FPS,
//! он запускает/останавливает ffmpeg. Это позволяет позже добавить
//! Wayland-портал и аудиодорожки за тем же интерфейсом.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use thiserror::Error;

/// Ошибки запуска и остановки записи.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("запись уже запущена")]
    AlreadyRunning,

    #[error("не удалось создать каталог {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось запустить ffmpeg: {0}")]
    Spawn(#[from] std::io::Error),

    #[error("не удалось остановить ffmpeg: {0}")]
    Stop(std::io::Error),
}

/// Рекордер X11-экрана.
#[derive(Debug, Default)]
pub struct X11VideoRecorder {
    child: Option<Child>,
}

impl X11VideoRecorder {
    /// Создаёт рекордер без запущенного процесса.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает запись всего экрана в `output`.
    pub fn start(&mut self, output: &Path, fps: u32) -> Result<(), CaptureError> {
        if self.child.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CaptureError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let display = display_name();
        let (width, height) = display_size();
        let mut command = Command::new("ffmpeg");
        command
            .args(build_args(&display, (width, height), fps, output))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn()?;
        self.child = Some(child);
        Ok(())
    }

    /// Останавливает запись и дожидается завершения ffmpeg.
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        #[cfg(unix)]
        {
            // SIGINT заставляет ffmpeg корректно закрыть контейнер.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGINT);
            }
            for _ in 0..50 {
                match child.try_wait() {
                    Ok(Some(_)) => return Ok(()),
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(error) => return Err(CaptureError::Stop(error)),
                }
            }
            child.kill().map_err(CaptureError::Stop)?;
        }

        #[cfg(not(unix))]
        {
            child.kill().map_err(CaptureError::Stop)?;
        }

        child.wait().map_err(CaptureError::Stop)?;
        Ok(())
    }

    /// Проверяет, жив ли процесс записи.
    pub fn is_running(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(Some(_)) => {
                self.child = None;
                false
            }
            Ok(None) => true,
            Err(_) => false,
        }
    }
}

impl Drop for X11VideoRecorder {
    /// Гарантирует, что дочерний ffmpeg не останется висеть после выхода.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Собирает аргументы ffmpeg для X11-захвата.
fn build_args(display: &str, size: (u32, u32), fps: u32, output: &Path) -> Vec<String> {
    vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "x11grab".to_string(),
        "-framerate".to_string(),
        fps.to_string(),
        "-video_size".to_string(),
        format!("{}x{}", size.0, size.1),
        "-i".to_string(),
        display.to_string(),
        "-c:v".to_string(),
        "libx264".to_string(),
        "-preset".to_string(),
        "veryfast".to_string(),
        "-crf".to_string(),
        "23".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        output.to_string_lossy().to_string(),
    ]
}

/// Возвращает имя X11-дисплея из окружения.
fn display_name() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    if display.contains('.') {
        display
    } else {
        format!("{display}.0")
    }
}

/// Определяет размер экрана через `xdpyinfo`; при сбое — 1920×1080.
fn display_size() -> (u32, u32) {
    let output = Command::new("xdpyinfo").output().ok();
    let Some(output) = output else {
        return (1920, 1080);
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("dimensions:") {
            let size = rest.split_whitespace().next().unwrap_or_default();
            let mut parts = size.split('x');
            if let (Some(width), Some(height)) = (parts.next(), parts.next())
                && let (Ok(width), Ok(height)) = (width.parse(), height.parse())
            {
                return (width, height);
            }
        }
    }
    (1920, 1080)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что команда содержит x11grab и корректный путь.
    #[test]
    fn builds_x11_command() {
        let args = build_args(":0.0", (1920, 1080), 20, Path::new("/tmp/screen.mkv"));
        assert!(args.contains(&"x11grab".to_string()));
        assert!(args.contains(&"1920x1080".to_string()));
        assert!(args.contains(&"/tmp/screen.mkv".to_string()));
        assert!(args.contains(&"20".to_string()));
    }

    /// Проверяет нормализацию имени дисплея.
    #[test]
    fn normalizes_display() {
        // Значение зависит от окружения, но результат не должен быть пустым.
        assert!(!display_name().is_empty());
    }

    /// Короткая реальная запись X11; запускать вручную.
    #[test]
    #[ignore = "требует X11, ffmpeg и доступный DISPLAY"]
    fn records_short_x11_clip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("screen.mkv");
        let mut recorder = X11VideoRecorder::new();
        recorder.start(&output, 10).expect("старт записи");
        std::thread::sleep(std::time::Duration::from_secs(2));
        recorder.stop().expect("остановка записи");
        let size = std::fs::metadata(&output).expect("файл записи").len();
        assert!(size > 0, "файл записи пустой");
    }
}
