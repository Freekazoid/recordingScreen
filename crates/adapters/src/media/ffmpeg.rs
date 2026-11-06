//! Экспорт записи через ffmpeg.
//!
//! Видео и аудиодорожки лежат раздельно; при экспорте они сводятся в один
//! контейнер с выбранным форматом, CRF и режимом звука.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Приоритет ffmpeg ниже, чем у окна и compositor.
#[cfg(unix)]
const FFMPEG_NICE: i32 = 10;

/// Создаёт команду ffmpeg с пониженным приоритетом.
///
/// Экспорт 1080p занимает все доступные ядра и может на несколько секунд
/// заблокировать отрисовку окна. Пониженный приоритет оставляет CPU
/// интерфейсу и compositor'у.
fn ffmpeg_command(args: &[String]) -> Command {
    let mut command = Command::new(crate::media::ffmpeg_binary());
    command.args(args);
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, FFMPEG_NICE);
            Ok(())
        });
    }
    command
}

/// Ограничивает число потоков экспорта, чтобы не забирать все ядра.
fn export_threads() -> usize {
    std::thread::available_parallelism()
        .map(|count| (count.get() / 2).clamp(2, 6))
        .unwrap_or(4)
}

/// Ошибки экспорта.
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("нет видеодорожки для экспорта")]
    NoVideo,

    #[error("нет аудиодорожки для экспорта")]
    NoAudio,

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
    pub audio_offset_ms: i64,
    pub mic_gain_db: f32,
}

/// Мягкое усиление тихой микрофонной дорожки.
const MIC_FILTER: &str = "highpass=f=80,dynaudnorm=f=500:m=15:p=0.95";

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
    let output = ffmpeg_command(&args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(MediaError::Ffmpeg(
            stderr.lines().rev().take(8).collect::<Vec<_>>().join(" | "),
        ));
    }
    Ok(request.output.clone())
}

/// Сводит только аудиодорожки в WAV (режим «Только аудио»).
pub fn merge_audio(
    system_audio: Option<PathBuf>,
    mic_audio: Option<PathBuf>,
    output: &Path,
    mic_gain_db: f32,
) -> Result<PathBuf, MediaError> {
    if system_audio.is_none() && mic_audio.is_none() {
        return Err(MediaError::NoAudio);
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|source| MediaError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-nostdin".to_string(),
        "-y".to_string(),
    ];
    let mut inputs = Vec::new();
    if let Some(system) = &system_audio {
        if !system.exists() {
            return Err(MediaError::Missing(system.clone()));
        }
        args.push("-i".to_string());
        args.push(system.to_string_lossy().to_string());
        inputs.push(0);
    }
    if let Some(mic) = &mic_audio {
        if !mic.exists() {
            return Err(MediaError::Missing(mic.clone()));
        }
        args.push("-i".to_string());
        args.push(mic.to_string_lossy().to_string());
        inputs.push(if system_audio.is_some() { 1 } else { 0 });
    }
    if inputs.len() >= 2 {
        args.extend([
            "-filter_complex".to_string(),
            format!(
                "[{}:a]anull[sys];[{}:a]volume={mic_gain_db:.1}dB,{MIC_FILTER}[mic];\
                 [sys][mic]amix=inputs=2:normalize=0:duration=longest[aout]",
                inputs[0], inputs[1]
            ),
            "-map".to_string(),
            "[aout]".to_string(),
        ]);
    } else if mic_audio.is_some() {
        args.extend([
            "-filter_complex".to_string(),
            format!(
                "[{}:a]volume={mic_gain_db:.1}dB,{MIC_FILTER}[aout]",
                inputs[0]
            ),
            "-map".to_string(),
            "[aout]".to_string(),
        ]);
    }
    args.extend([
        "-c:a".to_string(),
        "pcm_s16le".to_string(),
        "-y".to_string(),
        output.to_string_lossy().to_string(),
    ]);
    let result = ffmpeg_command(&args).output()?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(MediaError::Ffmpeg(
            stderr.lines().rev().take(8).collect::<Vec<_>>().join(" | "),
        ));
    }
    Ok(output.to_path_buf())
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
        "-threads".to_string(),
        export_threads().to_string(),
        "-i".to_string(),
        request.video.to_string_lossy().to_string(),
    ];

    let mut audio_inputs = Vec::new();
    let mut next_input = 1;
    let offset = request.audio_offset_ms as f64 / 1000.0;
    if let Some(system) = &request.system_audio {
        if offset.abs() > 0.001 {
            args.push("-itsoffset".to_string());
            args.push(format!("{offset:.3}"));
        }
        args.push("-i".to_string());
        args.push(system.to_string_lossy().to_string());
        audio_inputs.push(next_input);
        next_input += 1;
    }
    if let Some(mic) = &request.mic_audio {
        if offset.abs() > 0.001 {
            args.push("-itsoffset".to_string());
            args.push(format!("{offset:.3}"));
        }
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
                    "[{}:a]anull[sys];[{}:a]volume={:.1}dB,{MIC_FILTER}[mic];\
                     [sys][mic]amix=inputs=2:normalize=0:duration=longest[aout]",
                    audio_inputs[0], audio_inputs[1], request.mic_gain_db
                ),
                "-map".to_string(),
                "0:v:0".to_string(),
                "-map".to_string(),
                "[aout]".to_string(),
            ]);
        } else if request.mic_audio.is_some() {
            args.extend([
                "-filter_complex".to_string(),
                format!(
                    "[{}:a]volume={:.1}dB,{MIC_FILTER}[aout]",
                    audio_inputs[0], request.mic_gain_db
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

    args.extend(["-threads".to_string(), export_threads().to_string()]);
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
            audio_offset_ms: 120,
            mic_gain_db: 12.0,
        };
        let args = build_args(&request);
        assert!(args.contains(&"-filter_complex".to_string()));
        assert!(args.contains(&"-itsoffset".to_string()));
        assert!(args.iter().any(|arg| arg.contains("amix=inputs=2")));
        assert!(args.iter().any(|arg| arg.contains("dynaudnorm")));
        assert!(
            args.iter().filter(|arg| *arg == "-threads").count() >= 2,
            "экспорт должен ограничивать потоки"
        );
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
            audio_offset_ms: 0,
            mic_gain_db: 0.0,
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
        let status = Command::new(crate::media::ffmpeg_binary())
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
            let status = Command::new(crate::media::ffmpeg_binary())
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
            audio_offset_ms: 0,
            mic_gain_db: 0.0,
        };
        merge_recording(&request).expect("merge");
        assert!(is_usable(&output));
    }
}
