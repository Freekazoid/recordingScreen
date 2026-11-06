//! Сегментированная запись и склейка сегментов.
//!
//! И видео X11, и GStreamer на Wayland пишут сегменты по 60 секунд. После
//! штатной остановки и при восстановлении после падения сегменты
//! склеиваются в итоговый `screen.mkv` без перекодирования.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Возвращает шаблон имени сегментов рядом с итоговым файлом.
pub fn segment_pattern(output: &Path) -> PathBuf {
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("screen");
    output.with_file_name(format!("{stem}-%03d.mkv"))
}

/// Склеивает сегменты в `output`; если сегментов нет, ничего не делает.
pub fn concat_segments(output: &Path) -> Result<(), String> {
    let Some(parent) = output.parent() else {
        return Ok(());
    };
    let stem = output
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("screen");
    let prefix = format!("{stem}-");
    let mut segments = Vec::new();
    if let Ok(entries) = fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) && name.ends_with(".mkv") {
                segments.push(entry.path());
            }
        }
    }
    if segments.is_empty() {
        return Ok(());
    }
    segments.sort();
    let list = parent.join("segments.txt");
    let mut content = String::new();
    for segment in &segments {
        let path = segment.display().to_string();
        // В списке concat-демуксера обратный слэш — escape-символ, поэтому
        // на Windows пути переводим в прямой слэш, который ffmpeg понимает.
        #[cfg(windows)]
        let path = path.replace('\\', "/");
        let path = path.replace('\'', "'\\''");
        content.push_str(&format!("file '{path}'\n"));
    }
    fs::write(&list, content).map_err(|error| format!("список сегментов: {error}"))?;
    let result = Command::new(crate::media::ffmpeg_binary())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            &list.to_string_lossy(),
            "-c",
            "copy",
            &output.to_string_lossy(),
        ])
        .output()
        .map_err(|error| format!("ffmpeg concat: {error}"))?;
    let _ = fs::remove_file(&list);
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "склейка сегментов: {}",
            stderr.lines().rev().take(6).collect::<Vec<_>>().join(" | ")
        ));
    }
    for segment in segments {
        let _ = fs::remove_file(segment);
    }
    Ok(())
}

/// Восстанавливает сегменты во всех сессиях каталога записей.
///
/// Возвращает число успешно склеенных сессий и ошибки.
pub fn recover_all_segments(recordings_dir: &Path) -> (usize, Vec<String>) {
    let mut recovered = 0;
    let mut errors = Vec::new();
    let Ok(entries) = fs::read_dir(recordings_dir) else {
        return (0, errors);
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let output = dir.join("screen.mkv");
        if output.exists() {
            continue;
        }
        let has_segments = fs::read_dir(&dir)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with("screen-") && name.ends_with(".mkv")
            });
        if !has_segments {
            continue;
        }
        match concat_segments(&output) {
            Ok(()) => recovered += 1,
            Err(error) => errors.push(format!("{}: {error}", dir.display())),
        }
    }
    (recovered, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет шаблон сегментов.
    #[test]
    fn builds_segment_pattern() {
        assert_eq!(
            segment_pattern(Path::new("/tmp/session/screen.mkv")),
            Path::new("/tmp/session/screen-%03d.mkv")
        );
    }

    /// Проверяет, что без сегментов склейка не падает.
    #[test]
    fn concat_without_segments_is_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        concat_segments(&dir.path().join("screen.mkv")).expect("concat");
    }

    /// Проверяет восстановление сегментов после «падения».
    #[test]
    #[ignore = "требует ffmpeg"]
    fn recovers_segments_after_crash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = dir.path().join("session");
        std::fs::create_dir_all(&session).expect("mkdir");
        for index in 0..2 {
            let output = session.join(format!("screen-{index:03}.mkv"));
            let status = Command::new(crate::media::ffmpeg_binary())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=160x120:rate=10",
                    "-t",
                    "1",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    &output.to_string_lossy(),
                ])
                .status()
                .expect("ffmpeg");
            assert!(status.success());
        }
        let (recovered, errors) = recover_all_segments(dir.path());
        assert_eq!(recovered, 1, "{errors:?}");
        assert!(session.join("screen.mkv").exists());
    }
}
