//! Медиа-операции: склейка видео и аудиодорожек, экспорт.

pub mod ffmpeg;

pub use ffmpeg::{ExportRequest, MediaError, merge_audio, merge_recording};

use std::path::PathBuf;

/// Возвращает ffmpeg рядом с приложением или системный.
///
/// В macOS `.app` и Windows-порт пакет кладёт `ffmpeg` рядом с
/// исполняемым файлом; PATH дочернего процесса туда не указывает.
pub fn ffmpeg_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for name in ["ffmpeg.exe", "ffmpeg"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("ffmpeg")
}
