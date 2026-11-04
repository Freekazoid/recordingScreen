//! Экспорт записи через ffmpeg.
//!
//! Видео и аудиодорожки лежат раздельно; при экспорте они сводятся в один
//! контейнер с выбранным форматом, CRF и режимом звука.

use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Ошибки экспорта.
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("нет видеодорожки для экспорта")]
    NoVideo,

    #[error("файл не найден: {0}")]
    Missing(PathBuf),

    #[error("не удалось создать каталог {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("ffmpeg завершился с ошибкой: {0}")]
    Ffmpeg(String),

    #[error("не удалось запустить ffmpeg: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Параметры экспорта.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub video: PathBuf,
    pub system_audio: Option<PathBuf>,
    pub mic_audio: Option<PathBuf>,
    pub output: PathBuf,
    pub format: String,
    pub crf: u32,
    pub audio_mode: String,
}

/// Сводит видео и аудио в итоговый файл.
pub fn merge_recording(request: &ExportRequest) -> Result<PathBuf, MediaError> {
    if !request.video.exists() {
        return Err(MediaError::Missing(request.video.clone()));
    }
    if let Some(audio) = &request.system_audio
        && !audio.exists()
    {
        return Err(MediaError::Missing(audio.clone()));
    }
    if let Some(audio) = &request.mic_audio
        && !audio.exists()
    {
        return Err(MediaError::Missing(audio.clone()));
    }
    if let Some(parent) = request.output.parent() {
        std::fs::create_dir_all(parent).map_err(|source| MediaError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let args = build_args(request);
    let output = Command::new("ffmpeg").args(&args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MediaError::Ffmpeg(
            stderr.lines().rev().take(8).collect::<Vec<_>>().join(" | "),
        ));
    }
    Ok(request.output.clone())
}

/// Собирает аргументы ffmpeg для экспорта.
fn build_args(request: &ExportRequest) -> Vec<String> {
    let format = request.format.to_lowercase();
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        request.video.to_string_lossy().to_string(),
    ];

    let mut audio_inputs = Vec::new();
    let mut next_input = 1;
    if let Some(system) = &request.system_audio {
        args.push("-i".to_string());
        args.push(system.to_string_lossy().to_string());
        audio_inputs.push(next_input);
        next_input += 1;
    }
    if let Some(mic) = &request.mic_audio {
        args.push("-i".to_string());
        args.push(mic.to_string_lossy().to_string());
        audio_inputs.push(next_input);
    }

    let audio_enabled = request.audio_mode != "none" && !audio_inputs.is_empty();
    if audio_enabled {
        if audio_inputs.len() >= 2 {
            args.extend([
                "-filter_complex".to_string(),
                format!(
                    "[{}:a][{}:a]amix=inputs=2:normalize=0:duration=longest[aout]",
                    audio_inputs[0], audio_inputs[1]
                ),
                "-map".to_string(),
                "0:v:0".to_string(),
                "-map".to_string(),
                "[aout]".to_string(),
            ]);
        } else {
            args.extend([
                "-map".to_string(),
                "0:v:0".to_string(),
                "-map".to_string(),
                format!("{}:a:0", audio_inputs[0]),
            ]);
        }
    } else {
        args.extend(["-map".to_string(), "0:v:0".to_string(), "-an".to_string()]);
    }

    args.extend(video_args(&format, request.crf));
    if audio_enabled {
        args.extend(audio_args(&format));
    }
    args.extend(["-shortest".to_string(), "-y".to_string()]);
    args.push(request.output.to_string_lossy().to_string());
    args
}

/// Кодек видео по формату.
fn video_args(format: &str, crf: u32) -> Vec<String> {
    match format {
        "webm" => vec![
            "-c:v".to_string(),
            "libvpx-vp9".to_string(),
            "-crf".to_string(),
            ((crf as f32 / 51.0 * 63.0).round() as u32).to_string(),
            "-b:v".to_string(),
            "0".to_string(),
        ],
        "avi" => vec![
            "-c:v".to_string(),
            "mpeg4".to_string(),
            "-q:v".to_string(),
            ((crf as f32 / 51.0 * 30.0).round().clamp(2.0, 31.0) as u32).to_string(),
        ],
        _ => vec![
            "-c:v".to_string(),
            "libx264".to_string(),
            "-preset".to_string(),
            "medium".to_string(),
            "-crf".to_string(),
            crf.to_string(),
            "-pix_fmt".to_string(),
            "yuv420p".to_string(),
        ],
    }
}

/// Кодек аудио по формату.
fn audio_args(format: &str) -> Vec<String> {
    match format {
        "webm" => vec![
            "-c:a".to_string(),
            "libopus".to_string(),
            "-b:a".to_string(),
            "128k".to_string(),
        ],
        "avi" => vec![
            "-c:a".to_string(),
            "libmp3lame".to_string(),
            "-b:a".to_string(),
            "128k".to_string(),
        ],
        _ => vec![
            "-c:a".to_string(),
            "aac".to_string(),
            "-b:a".to_string(),
            "128k".to_string(),
        ],
    }
}

/// Проверяет, что файл существует и не пуст.
pub fn is_usable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.len() > 1024)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет сборку команды с двумя аудиодорожками.
    #[test]
    fn builds_mix_command() {
        let request = ExportRequest {
            video: PathBuf::from("/tmp/screen.mkv"),
            system_audio: Some(PathBuf::from("/tmp/system.wav")),
            mic_audio: Some(PathBuf::from("/tmp/mic.wav")),
            output: PathBuf::from("/tmp/out.mp4"),
            format: "mp4".to_string(),
            crf: 23,
            audio_mode: "aac".to_string(),
        };
        let args = build_args(&request);
        assert!(args.contains(&"-filter_complex".to_string()));
        assert!(args.iter().any(|arg| arg.contains("amix=inputs=2")));
        assert!(args.contains(&"aac".to_string()));
        assert_eq!(args.last(), Some(&"/tmp/out.mp4".to_string()));
    }

    /// Проверяет режим без звука.
    #[test]
    fn builds_silent_command() {
        let request = ExportRequest {
            video: PathBuf::from("/tmp/screen.mkv"),
            system_audio: Some(PathBuf::from("/tmp/system.wav")),
            mic_audio: None,
            output: PathBuf::from("/tmp/out.mkv"),
            format: "mkv".to_string(),
            crf: 28,
            audio_mode: "none".to_string(),
        };
        let args = build_args(&request);
        assert!(args.contains(&"-an".to_string()));
    }

    /// Реальная склейка синтетических файлов; запускать вручную.
    #[test]
    #[ignore = "требует ffmpeg"]
    fn merges_synthetic_recording() {
        let dir = tempfile::tempdir().expect("tempdir");
        let video = dir.path().join("video.mkv");
        let system = dir.path().join("system.wav");
        let mic = dir.path().join("mic.wav");
        let output = dir.path().join("out.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=10",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                &video.to_string_lossy(),
            ])
            .status()
            .expect("video");
        assert!(status.success());
        for (path, frequency) in [(&system, "440"), (&mic, "660")] {
            let status = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={frequency}:duration=1"),
                    "-c:a",
                    "pcm_s16le",
                    &path.to_string_lossy(),
                ])
                .status()
                .expect("audio");
            assert!(status.success());
        }

        let request = ExportRequest {
            video,
            system_audio: Some(system),
            mic_audio: Some(mic),
            output: output.clone(),
            format: "mp4".to_string(),
            crf: 23,
            audio_mode: "aac".to_string(),
        };
        merge_recording(&request).expect("merge");
        assert!(is_usable(&output));
    }
}
