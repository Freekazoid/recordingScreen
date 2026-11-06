//! Запись экрана macOS через ffmpeg AVFoundation.
//!
//! AVFoundation отдаёт экран как видеоустройство «Capture screen N».
//! Запись идёт сегментами, как на других платформах; при первом запуске
//! macOS попросит разрешение Screen Recording.

use super::segments::concat_segments;
use super::{CaptureError, Region, WindowInfo};
use crate::media::ffmpeg_binary;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Рекордер экрана macOS.
#[derive(Debug, Default)]
pub struct MacVideoRecorder {
    child: Option<Child>,
    output: Option<PathBuf>,
}

impl MacVideoRecorder {
    /// Создаёт рекордер без запущенного процесса.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает запись всего экрана.
    pub fn start(&mut self, output: &Path, fps: u32) -> Result<(), CaptureError> {
        self.start_region(output, fps, None)
    }

    /// Запускает запись области (None — весь экран).
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
        let screen = screen_device()?;
        let args = build_args(screen, fps, output, region);
        let log_path = output.with_file_name("ffmpeg-video.log");
        let log = std::fs::File::create(&log_path).ok();
        let mut command = Command::new(ffmpeg_binary());
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null));
        let mut child = command.spawn()?;
        // Даём ffmpeg время сообщить об отсутствии разрешения Screen Recording.
        std::thread::sleep(std::time::Duration::from_millis(700));
        if let Some(status) = child.try_wait().map_err(CaptureError::Stop)? {
            let detail = std::fs::read_to_string(&log_path).unwrap_or_default();
            let tail = detail
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(CaptureError::Spawn(std::io::Error::other(format!(
                "ffmpeg завершился ({status}): {tail}"
            ))));
        }
        self.child = Some(child);
        self.output = Some(output.to_path_buf());
        Ok(())
    }

    /// Останавливает ffmpeg командой `q` и склеивает сегменты.
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let output = self.output.take();
        // SIGINT даёт ffmpeg корректно закрыть текущий сегмент.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let mut stopped = false;
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

impl Drop for MacVideoRecorder {
    /// Не оставляет ffmpeg висеть после выхода.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Индекс устройства «Capture screen N» в AVFoundation.
fn screen_device() -> Result<usize, CaptureError> {
    let output = Command::new(ffmpeg_binary())
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output()
        .map_err(CaptureError::Spawn)?;
    let text = String::from_utf8_lossy(&output.stderr);
    let mut in_video = false;
    for line in text.lines() {
        if line.contains("AVFoundation video devices:") {
            in_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices:") {
            in_video = false;
            continue;
        }
        if !in_video {
            continue;
        }
        let Some(start) = line.rfind('[') else {
            continue;
        };
        let Some(end) = line[start..].find(']') else {
            continue;
        };
        let Ok(index) = line[start + 1..start + end].trim().parse::<usize>() else {
            continue;
        };
        let name = line[start + end + 1..].trim();
        if name.to_lowercase().contains("capture screen") {
            return Ok(index);
        }
    }
    Err(CaptureError::Spawn(std::io::Error::other(
        "AVFoundation не нашёл «Capture screen»; разрешите Screen Recording в настройках macOS",
    )))
}

/// Собирает аргументы ffmpeg для AVFoundation.
fn build_args(screen: usize, fps: u32, output: &Path, region: Option<Region>) -> Vec<String> {
    let mut filter = match region {
        Some(region) => format!(
            "crop={}:{}:{}:{}",
            region.width, region.height, region.x, region.y
        ),
        None => "null".to_string(),
    };
    filter.push_str(",scale=trunc(iw/2)*2:trunc(ih/2)*2");
    vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "avfoundation".to_string(),
        "-framerate".to_string(),
        fps.to_string(),
        "-capture_cursor".to_string(),
        "0".to_string(),
        "-i".to_string(),
        screen.to_string(),
        "-vf".to_string(),
        filter,
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

/// Выбор окна на macOS пока не реализован: для этого нужен CGWindowList
/// или ScreenCaptureKit, а не AVFoundation.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    Ok(Vec::new())
}

/// Делает снимок экрана встроенным `screencapture`.
pub fn take_screenshot(output: &Path) -> Result<PathBuf, String> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("каталог {}: {error}", parent.display()))?;
    }
    let output_arg = output.to_string_lossy().to_string();
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", output_arg.as_str()])
        .status()
        .map_err(|error| format!("screencapture: {error}"))?;
    if status.success() {
        Ok(output.to_path_buf())
    } else {
        Err(format!("screencapture завершился: {status}"))
    }
}

/// Снимок экрана для выбора области.
pub fn screenshot(output: &Path) -> Result<(), String> {
    take_screenshot(output).map(|_| ())
}
