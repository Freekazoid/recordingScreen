//! Запись экрана Windows через ffmpeg `gdigrab`.
//!
//! GDI-захват доступен без установки драйверов и работает в обычной
//! пользовательской сессии. Для MVP этого достаточно; WGC добавим позже,
//! если понадобится захват защищённых/аппаратных окон.

use super::segments::concat_segments;
use super::{CaptureError, Region, WindowInfo};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};

/// Рекордер экрана Windows.
#[derive(Debug, Default)]
pub struct WindowsVideoRecorder {
    child: Option<Child>,
    output: Option<PathBuf>,
}

impl WindowsVideoRecorder {
    /// Создаёт рекордер без запущенного процесса.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает запись всего экрана.
    pub fn start(&mut self, output: &Path, fps: u32) -> Result<(), CaptureError> {
        self.start_region(output, fps, None)
    }

    /// Запускает запись области (None — весь экран).
    ///
    /// `gdigrab` умеет писать сразу в сегменты, поэтому длинная запись
    /// переживает падение и не требует перекодирования при склейке.
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
        let args = build_args(fps, output, region);
        let log_path = output.with_file_name("ffmpeg-video.log");
        let log = std::fs::File::create(&log_path).ok();
        let mut command = Command::new(crate::media::ffmpeg_binary());
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log.map(Stdio::from).unwrap_or_else(Stdio::null));
        let child = command.spawn()?;
        self.child = Some(child);
        self.output = Some(output.to_path_buf());
        Ok(())
    }

    /// Останавливает ffmpeg командой `q` и склеивает сегменты.
    ///
    /// На Windows нет SIGINT, поэтому используем штатный интерактивный
    /// выход ffmpeg: контейнер закрывается корректно, файл не теряется.
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let output = self.output.take();
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
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

impl Drop for WindowsVideoRecorder {
    /// Не оставляет ffmpeg висеть после выхода приложения.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Собирает аргументы ffmpeg для gdigrab.
fn build_args(fps: u32, output: &Path, region: Option<Region>) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-f".to_string(),
        "gdigrab".to_string(),
        "-framerate".to_string(),
        fps.to_string(),
    ];
    if let Some(region) = region {
        args.extend([
            "-offset_x".to_string(),
            region.x.to_string(),
            "-offset_y".to_string(),
            region.y.to_string(),
            "-video_size".to_string(),
            format!("{}x{}", region.width, region.height),
        ]);
    }
    args.extend([
        "-i".to_string(),
        "desktop".to_string(),
        // libx264/yuv420p требует чётные ширину и высоту, а рабочий стол
        // может быть, например, 1218x849.
        "-vf".to_string(),
        "scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string(),
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
    ]);
    args
}

/// Возвращает список видимых окон верхнего уровня.
///
/// Заголовки читаются в UTF-16 и конвертируются в UTF-8, поэтому
/// кириллица отображается корректно.
pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let mut windows: Vec<WindowInfo> = Vec::new();
    let pointer = &mut windows as *mut Vec<WindowInfo> as LPARAM;
    // SAFETY: указатель живёт до конца EnumWindows, callback пишет только
    // в этот вектор и не сохраняет ссылку.
    let ok = unsafe { EnumWindows(Some(enum_window), pointer) };
    if ok == 0 {
        return Err("EnumWindows завершился с ошибкой".to_string());
    }
    windows.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(windows)
}

/// Callback перечисления окон.
unsafe extern "system" fn enum_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` — указатель на Vec, переданный из `list_windows`.
    let windows = unsafe { &mut *(lparam as *mut Vec<WindowInfo>) };
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return TRUE;
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if style & WS_EX_TOOLWINDOW != 0 {
        return TRUE;
    }
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return TRUE;
    }
    let mut buffer = vec![0_u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if copied <= 0 {
        return TRUE;
    }
    let title = String::from_utf16_lossy(&buffer[..copied as usize]);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return TRUE;
    }
    let width = (rect.right - rect.left).max(0) as u32;
    let height = (rect.bottom - rect.top).max(0) as u32;
    if width < 20 || height < 20 {
        return TRUE;
    }
    windows.push(WindowInfo {
        id: hwnd as u64,
        title,
        region: Region {
            x: rect.left.max(0) as u32,
            y: rect.top.max(0) as u32,
            width,
            height,
        },
    });
    TRUE
}

/// Делает снимок всего экрана в PNG.
pub fn screenshot(output: &Path) -> Result<(), String> {
    let status = Command::new(crate::media::ffmpeg_binary())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "gdigrab",
            "-framerate",
            "1",
            "-i",
            "desktop",
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

/// Снимок экрана для выбора области.
pub fn take_screenshot(output: &Path) -> Result<PathBuf, String> {
    screenshot(output)?;
    Ok(output.to_path_buf())
}
