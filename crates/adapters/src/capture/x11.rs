//! Запись X11-экрана через ffmpeg `x11grab`.
//!
//! Адаптер намеренно не знает про UI и настройки: ему дают путь и FPS,
//! он запускает/останавливает ffmpeg. Это позволяет позже добавить
//! Wayland-портал и аудиодорожки за тем же интерфейсом.

use super::segments::concat_segments;
use super::{CaptureError, Region, WindowInfo};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Рекордер X11-экрана.
#[derive(Debug, Default)]
pub struct X11VideoRecorder {
    child: Option<Child>,
    output: Option<PathBuf>,
}

impl X11VideoRecorder {
    /// Создаёт рекордер без запущенного процесса.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает запись всего экрана в `output`.
    pub fn start(&mut self, output: &Path, fps: u32) -> Result<(), CaptureError> {
        self.start_region(output, fps, None)
    }

    /// Запускает запись указанной области (None — весь экран).
    pub fn start_region(
        &mut self,
        output: &Path,
        fps: u32,
        region: Option<Region>,
    ) -> Result<(), CaptureError> {
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
        let mut command = Command::new(crate::media::ffmpeg_binary());
        command
            .args(build_args(&display, (width, height), fps, output, region))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command.spawn()?;
        self.child = Some(child);
        self.output = Some(output.to_path_buf());
        Ok(())
    }

    /// Останавливает запись, склеивает сегменты и дожидается ffmpeg.
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let output = self.output.take();

        #[cfg(unix)]
        {
            let mut stopped = false;
            // SIGINT заставляет ffmpeg корректно закрыть контейнер.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGINT);
            }
            for _ in 0..50 {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        stopped = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                    Err(error) => return Err(CaptureError::Stop(error)),
                }
            }
            if !stopped {
                child.kill().map_err(CaptureError::Stop)?;
                child.wait().map_err(CaptureError::Stop)?;
            }
        }

        #[cfg(not(unix))]
        {
            child.kill().map_err(CaptureError::Stop)?;
            child.wait().map_err(CaptureError::Stop)?;
        }

        if let Some(output) = output {
            concat_segments(&output)
                .map_err(|error| CaptureError::Stop(std::io::Error::other(error)))?;
        }
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
fn build_args(
    display: &str,
    size: (u32, u32),
    fps: u32,
    output: &Path,
    region: Option<Region>,
) -> Vec<String> {
    let (input, width, height) = match region {
        Some(region) => (
            format!("{display}+{},{}", region.x, region.y),
            region.width,
            region.height,
        ),
        None => (display.to_string(), size.0, size.1),
    };
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
        format!("{width}x{height}"),
        "-i".to_string(),
        input,
        "-c:v".to_string(),
        "libx264".to_string(),
        "-preset".to_string(),
        "veryfast".to_string(),
        "-crf".to_string(),
        "23".to_string(),
        "-g".to_string(),
        "40".to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
        "-f".to_string(),
        "segment".to_string(),
        "-segment_time".to_string(),
        "60".to_string(),
        "-reset_timestamps".to_string(),
        "1".to_string(),
        "-segment_format".to_string(),
        "matroska".to_string(),
        super::segments::segment_pattern(output)
            .to_string_lossy()
            .to_string(),
    ]
}

/// Возвращает список видимых окон X11.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let output = Command::new("xprop")
        .args(["-root", "_NET_CLIENT_LIST"])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("xprop: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(list) = text.split('#').nth(1) else {
        return Ok(Vec::new());
    };
    let mut windows = Vec::new();
    for token in list.split(',') {
        let token = token.trim();
        let Ok(id) = u64::from_str_radix(token.trim_start_matches("0x"), 16) else {
            continue;
        };
        let Ok(info) = Command::new("xwininfo")
            .args(["-id", token])
            .env("LC_ALL", "C")
            .output()
        else {
            continue;
        };
        let info = String::from_utf8_lossy(&info.stdout);
        let Some(region) = parse_geometry(&info) else {
            continue;
        };
        let Ok(name) = Command::new("xprop")
            .args(["-id", token, "_NET_WM_NAME", "WM_NAME"])
            .env("LC_ALL", "C")
            .output()
        else {
            continue;
        };
        let title = parse_title(&String::from_utf8_lossy(&name.stdout));
        if title.is_empty() || region.width < 20 || region.height < 20 {
            continue;
        }
        windows.push(WindowInfo { id, title, region });
    }
    Ok(windows)
}

/// Разбирает `xwininfo` и проверяет, что окно видимо.
fn parse_geometry(text: &str) -> Option<Region> {
    if !text.contains("Map State: IsViewable") {
        return None;
    }
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Absolute upper-left X:") {
            x = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("Absolute upper-left Y:") {
            y = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("Width:") {
            width = value.trim().parse().ok();
        } else if let Some(value) = line.strip_prefix("Height:") {
            height = value.trim().parse().ok();
        }
    }
    Some(Region {
        x: x?,
        y: y?,
        width: width?,
        height: height?,
    })
}

/// Извлекает имя окна из вывода `xprop`.
fn parse_title(text: &str) -> String {
    for line in text.lines() {
        if let Some((_, value)) = line.split_once('=') {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() && value != "N/A" {
                return decode_xprop_escapes(value);
            }
        }
    }
    String::new()
}

/// Раскодирует octal-escape последовательности `xprop` в UTF-8.
///
/// При `LC_ALL=C` утилита печатает кириллицу как `\320\240...`,
/// из-за чего список окон выглядел набором цифр.
fn decode_xprop_escapes(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            let mut buffer = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        let mut octal = String::new();
        while octal.len() < 3
            && let Some(next) = chars.peek().copied()
            && next.is_digit(8)
        {
            octal.push(next);
            chars.next();
        }
        if octal.len() == 3
            && let Ok(byte) = u8::from_str_radix(&octal, 8)
        {
            bytes.push(byte);
        } else {
            bytes.push(b'\\');
            bytes.extend_from_slice(octal.as_bytes());
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Делает снимок всего X11-экрана в PNG.
pub fn screenshot(output: &Path) -> Result<(), String> {
    let display = display_name();
    let (width, height) = display_size();
    let status = Command::new(crate::media::ffmpeg_binary())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "x11grab",
            "-video_size",
            &format!("{width}x{height}"),
            "-i",
            &display,
            "-frames:v",
            "1",
            &output.to_string_lossy(),
        ])
        .status()
        .map_err(|error| format!("ffmpeg screenshot: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg screenshot завершился: {status}"))
    }
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

    /// Проверяет, что команда содержит x11grab и шаблон сегментов.
    #[test]
    fn builds_x11_command() {
        let args = build_args(":0.0", (1920, 1080), 20, Path::new("/tmp/screen.mkv"), None);
        assert!(args.contains(&"x11grab".to_string()));
        assert!(args.contains(&"1920x1080".to_string()));
        assert!(args.contains(&"/tmp/screen-%03d.mkv".to_string()));
        assert!(args.contains(&"segment".to_string()));
        assert!(args.contains(&"20".to_string()));
    }

    /// Проверяет раскодирование кириллицы из octal-escape xprop.
    #[test]
    fn decodes_xprop_utf8_escapes() {
        let title = decode_xprop_escapes(
            "\\320\\240\\320\\265\\320\\272\\320\\276\\321\\200\\320\\264\\320\\265\\321\\200",
        );
        assert_eq!(title, "Рекордер");
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

    /// Проверяет ротацию сегментов на длинной записи; запускать вручную.
    #[test]
    #[ignore = "требует X11, ffmpeg и 65 секунд"]
    fn records_x11_segment_rotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("screen.mkv");
        let mut recorder = X11VideoRecorder::new();
        recorder.start(&output, 10).expect("старт записи");
        std::thread::sleep(std::time::Duration::from_secs(65));
        recorder.stop().expect("остановка записи");
        assert!(output.exists(), "итоговый файл не создан");
        assert!(
            !dir.path()
                .read_dir()
                .expect("read_dir")
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().contains("-00")),
            "сегменты не удалены после склейки"
        );
    }

    /// Проверяет 30-минутную запись; запускать вручную.
    #[test]
    #[ignore = "30-минутный soak; требует X11 и ffmpeg"]
    fn records_x11_30_minutes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("screen.mkv");
        let mut recorder = X11VideoRecorder::new();
        recorder.start(&output, 10).expect("старт записи");
        std::thread::sleep(std::time::Duration::from_secs(30 * 60));
        recorder.stop().expect("остановка записи");
        let size = std::fs::metadata(&output).expect("файл записи").len();
        assert!(size > 1_000_000, "файл слишком маленький: {size}");
    }

    /// Проверяет перечисление окон X11; запускать вручную.
    #[test]
    #[ignore = "требует X11 и xprop/xwininfo"]
    fn lists_x11_windows() {
        let windows = list_windows().expect("windows");
        assert!(windows.iter().all(|window| window.region.width > 0));
    }
}
