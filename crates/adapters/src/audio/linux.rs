//! Linux-запись системного звука и микрофона через `pw-record`.
//!
//! Дорожки пишутся раздельно: `system.wav` (monitor sink) и `mic.wav`
//! (default source). Это основа channel-aware транскрипции.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use thiserror::Error;

/// Ошибки аудиозахвата.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("запись уже запущена")]
    AlreadyRunning,

    #[error("не найдены `wpctl`/`pactl` для определения аудиоустройств")]
    NoDeviceTools,

    #[error("не удалось определить устройство {0}")]
    NoDevice(String),

    #[error("не удалось создать каталог {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось запустить {track}: {source}")]
    Spawn {
        track: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось остановить {track}: {source}")]
    Stop {
        track: &'static str,
        #[source]
        source: std::io::Error,
    },
}

/// Результат запуска аудиодорожек.
#[derive(Debug, Clone)]
pub struct AudioStart {
    pub system: bool,
    pub mic: bool,
    pub warnings: Vec<String>,
}

/// Рекордер системного звука и микрофона.
#[derive(Debug, Default)]
pub struct LinuxAudioRecorder {
    system: Option<Child>,
    mic: Option<Child>,
}

impl LinuxAudioRecorder {
    /// Создаёт рекордер без запущенных процессов.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает `system.wav` и `mic.wav` в указанном каталоге сессии.
    pub fn start(&mut self, session_dir: &Path) -> Result<AudioStart, AudioError> {
        if self.system.is_some() || self.mic.is_some() {
            return Err(AudioError::AlreadyRunning);
        }
        std::fs::create_dir_all(session_dir).map_err(|source| AudioError::CreateDir {
            path: session_dir.to_path_buf(),
            source,
        })?;

        let sink = default_node("@DEFAULT_AUDIO_SINK@", "sink")?;
        let source = default_node("@DEFAULT_AUDIO_SOURCE@", "source")?;
        let system_path = session_dir.join("system.wav");
        let mic_path = session_dir.join("mic.wav");

        let mut warnings = Vec::new();
        self.system = spawn_pw_record(&format!("{sink}.monitor"), &system_path, 2, "system")
            .inspect_err(|error| warnings.push(error.to_string()))
            .ok();
        self.mic = spawn_pw_record(&source, &mic_path, 1, "mic")
            .inspect_err(|error| warnings.push(error.to_string()))
            .ok();

        if self.system.is_none() && self.mic.is_none() {
            return Err(AudioError::NoDeviceTools);
        }
        Ok(AudioStart {
            system: self.system.is_some(),
            mic: self.mic.is_some(),
            warnings,
        })
    }

    /// Останавливает обе дорожки.
    pub fn stop(&mut self) -> Result<(), AudioError> {
        stop_child(&mut self.system, "system")?;
        stop_child(&mut self.mic, "mic")
    }

    /// Проверяет, живы ли процессы записи.
    pub fn is_running(&mut self) -> bool {
        let system = child_running(&mut self.system);
        let mic = child_running(&mut self.mic);
        system || mic
    }
}

impl Drop for LinuxAudioRecorder {
    /// Гарантирует остановку дочерних процессов.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Запускает `pw-record` для указанного узла.
fn spawn_pw_record(
    target: &str,
    output: &Path,
    channels: u32,
    track: &'static str,
) -> Result<Child, AudioError> {
    Command::new("pw-record")
        .args([
            "--target",
            target,
            "--rate",
            "48000",
            "--channels",
            &channels.to_string(),
            "--format",
            "s16",
            &output.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| AudioError::Spawn { track, source })
}

/// Проверяет, что процесс ещё работает; завершившийся убирает из состояния.
fn child_running(child: &mut Option<Child>) -> bool {
    let Some(process) = child.as_mut() else {
        return false;
    };
    match process.try_wait() {
        Ok(Some(_)) => {
            *child = None;
            false
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Останавливает процесс через SIGINT и ждёт завершения.
fn stop_child(child: &mut Option<Child>, track: &'static str) -> Result<(), AudioError> {
    let Some(mut process) = child.take() else {
        return Ok(());
    };

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(process.id() as i32, libc::SIGINT);
        }
        for _ in 0..50 {
            match process.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(source) => return Err(AudioError::Stop { track, source }),
            }
        }
        process
            .kill()
            .map_err(|source| AudioError::Stop { track, source })?;
    }

    #[cfg(not(unix))]
    {
        process
            .kill()
            .map_err(|source| AudioError::Stop { track, source })?;
    }

    process
        .wait()
        .map_err(|source| AudioError::Stop { track, source })?;
    Ok(())
}

/// Определяет имя узла по умолчанию через `wpctl` или `pactl`.
fn default_node(target: &str, kind: &str) -> Result<String, AudioError> {
    if let Ok(output) = Command::new("wpctl").args(["inspect", target]).output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let line = line.trim().trim_start_matches('*').trim();
            if let Some(rest) = line.strip_prefix("node.name = ")
                && let Some(name) = rest
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
            {
                return Ok(name.to_string());
            }
        }
    }

    let command = if kind == "sink" {
        "get-default-sink"
    } else {
        "get-default-source"
    };
    if let Ok(output) = Command::new("pactl").args([command]).output()
        && output.status.success()
    {
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() {
            return Ok(name);
        }
    }

    Err(AudioError::NoDevice(kind.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет остановку без запущенных процессов.
    #[test]
    fn stop_without_start_is_ok() {
        let mut recorder = LinuxAudioRecorder::new();
        recorder.stop().expect("stop");
        assert!(!recorder.is_running());
    }

    /// Проверяет, что имя узла не пустое в текущем окружении.
    #[test]
    #[ignore = "требует PipeWire/PulseAudio"]
    fn finds_default_sink() {
        let sink = default_node("@DEFAULT_AUDIO_SINK@", "sink").expect("sink");
        assert!(!sink.is_empty());
    }

    /// Короткая реальная запись system/mic; запускать вручную.
    #[test]
    #[ignore = "требует PipeWire/PulseAudio и устройства"]
    fn records_audio_tracks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut recorder = LinuxAudioRecorder::new();
        let started = recorder.start(dir.path()).expect("start");
        assert!(started.system || started.mic);
        thread::sleep(Duration::from_secs(2));
        recorder.stop().expect("stop");
        assert!(dir.path().join("system.wav").exists() || dir.path().join("mic.wav").exists());
    }
}
