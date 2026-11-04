//! Linux-запись системного звука и микрофона через `pw-record`.
//!
//! Дорожки пишутся раздельно: `system.wav` (monitor sink) и `mic.wav`
//! (default source или выбранное устройство). Это основа channel-aware
//! транскрипции и preflight-проверки уровня сигнала.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Ошибки аудиозахвата.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("запись уже запущена")]
    AlreadyRunning,

    #[error("не найдены аудиоустройства (pw-dump/pactl)")]
    NoDevices,

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

    #[error("ошибка проверки звука: {0}")]
    Probe(String),
}

/// Тип аудиоустройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceKind {
    Mic,
    System,
}

/// Аудиоустройство для выбора в настройках.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub label: String,
    pub kind: AudioDeviceKind,
}

/// Результат короткой проверки сигнала.
#[derive(Debug, Clone, Copy)]
pub struct AudioProbe {
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub silent: bool,
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

    /// Возвращает список доступных источников и monitor-устройств.
    pub fn list_devices() -> Result<Vec<AudioDevice>, AudioError> {
        let mut devices = Vec::new();
        if let Ok(output) = Command::new("pw-dump").output()
            && output.status.success()
            && let Ok(value) = serde_json::from_slice::<Value>(&output.stdout)
        {
            devices = parse_devices(&value);
        }
        if devices.is_empty() {
            devices = list_pactl_devices();
        }
        if devices.is_empty() {
            return Err(AudioError::NoDevices);
        }
        Ok(devices)
    }

    /// Запускает `system.wav` и `mic.wav` в указанном каталоге сессии.
    pub fn start(
        &mut self,
        session_dir: &Path,
        system_target: Option<&str>,
        mic_target: Option<&str>,
    ) -> Result<AudioStart, AudioError> {
        if self.system.is_some() || self.mic.is_some() {
            return Err(AudioError::AlreadyRunning);
        }
        std::fs::create_dir_all(session_dir).map_err(|source| AudioError::CreateDir {
            path: session_dir.to_path_buf(),
            source,
        })?;

        let system = resolve_target(system_target, AudioDeviceKind::System)?;
        let mic = resolve_target(mic_target, AudioDeviceKind::Mic)?;
        let system_path = session_dir.join("system.wav");
        let mic_path = session_dir.join("mic.wav");

        let mut warnings = Vec::new();
        self.system = spawn_pw_record(&system, &system_path, 2, "system")
            .inspect_err(|error| warnings.push(error.to_string()))
            .ok();
        self.mic = spawn_pw_record(&mic, &mic_path, 1, "mic")
            .inspect_err(|error| warnings.push(error.to_string()))
            .ok();

        if self.system.is_none() && self.mic.is_none() {
            return Err(AudioError::NoDevices);
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
        child_running(&mut self.system) || child_running(&mut self.mic)
    }

    /// Записывает 1 секунду с выбранного устройства и считает RMS/peak.
    pub fn probe(target: Option<&str>, kind: AudioDeviceKind) -> Result<AudioProbe, AudioError> {
        let resolved = resolve_target(target, kind)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "screenrecorder-probe-{}-{stamp}.wav",
            std::process::id()
        ));
        let child = spawn_pw_record(&resolved, &path, 2, "probe")?;
        thread::sleep(Duration::from_secs(1));
        stop_child(&mut Some(child), "probe")?;
        let result = read_probe(&path);
        let _ = std::fs::remove_file(&path);
        result
    }
}

impl Drop for LinuxAudioRecorder {
    /// Гарантирует остановку дочерних процессов.
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Разбирает JSON `pw-dump` в список устройств.
fn parse_devices(value: &Value) -> Vec<AudioDevice> {
    let mut devices = Vec::new();
    let Some(objects) = value.as_array() else {
        return devices;
    };
    for object in objects {
        let Some(id) = object.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let Some(props) = object
            .get("info")
            .and_then(|info| info.get("props"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        let Some(class) = props.get("media.class").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = props.get("node.name").and_then(Value::as_str) else {
            continue;
        };
        let description = props
            .get("node.description")
            .and_then(Value::as_str)
            .unwrap_or(name);
        match class {
            "Audio/Source" if !name.ends_with(".monitor") => {
                push_device(
                    &mut devices,
                    AudioDevice {
                        id: id.to_string(),
                        label: description.to_string(),
                        kind: AudioDeviceKind::Mic,
                    },
                );
            }
            "Audio/Sink" => {
                push_device(
                    &mut devices,
                    AudioDevice {
                        id: id.to_string(),
                        label: format!("{description} (системный)"),
                        kind: AudioDeviceKind::System,
                    },
                );
            }
            _ => {}
        }
    }
    devices.sort_by_key(|device| match device.kind {
        AudioDeviceKind::Mic => 0,
        AudioDeviceKind::System => 1,
    });
    devices
}

/// Добавляет устройство, если такого id ещё нет.
fn push_device(devices: &mut Vec<AudioDevice>, device: AudioDevice) {
    if !devices.iter().any(|item| item.id == device.id) {
        devices.push(device);
    }
}

/// Фолбэк через `pactl`, если `pw-dump` недоступен.
fn list_pactl_devices() -> Vec<AudioDevice> {
    let mut devices = Vec::new();
    if let Ok(output) = Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.nth(1) else {
                continue;
            };
            if name.ends_with(".monitor") {
                continue;
            }
            push_device(
                &mut devices,
                AudioDevice {
                    id: name.to_string(),
                    label: name.to_string(),
                    kind: AudioDeviceKind::Mic,
                },
            );
        }
    }
    if let Ok(output) = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let Some(name) = parts.nth(1) else {
                continue;
            };
            push_device(
                &mut devices,
                AudioDevice {
                    id: format!("{name}.monitor"),
                    label: format!("{name} (системный)"),
                    kind: AudioDeviceKind::System,
                },
            );
        }
    }
    devices
}

/// Разрешает выбранный id или устройство по умолчанию.
fn resolve_target(target: Option<&str>, kind: AudioDeviceKind) -> Result<String, AudioError> {
    if let Some(value) = target.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(value.to_string());
    }
    match kind {
        AudioDeviceKind::Mic => default_node("@DEFAULT_AUDIO_SOURCE@", "source"),
        AudioDeviceKind::System => default_node("@DEFAULT_AUDIO_SINK@", "sink"),
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
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("id ")
                && let Some(id) = rest.split(',').next()
                && !id.trim().is_empty()
            {
                return Ok(id.trim().to_string());
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

/// Читает WAV и считает RMS/peak в dBFS.
fn read_probe(path: &Path) -> Result<AudioProbe, AudioError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| AudioError::Probe(format!("не удалось прочитать WAV: {error}")))?;
    let spec = reader.spec();
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err(AudioError::Probe("ожидается PCM s16 WAV".to_string()));
    }
    let mut sum = 0.0_f64;
    let mut count = 0_u64;
    let mut peak = 0.0_f32;
    for sample in reader.samples::<i16>() {
        let value = sample
            .map_err(|error| AudioError::Probe(format!("ошибка чтения сэмпла: {error}")))?
            as f32
            / 32768.0;
        sum += (value as f64).powi(2);
        count += 1;
        peak = peak.max(value.abs());
    }
    if count == 0 {
        return Err(AudioError::Probe("в записи нет сэмплов".to_string()));
    }
    let rms = (sum / count as f64).sqrt() as f32;
    let rms_dbfs = 20.0 * rms.max(1e-9).log10();
    let peak_dbfs = 20.0 * peak.max(1e-9).log10();
    Ok(AudioProbe {
        rms_dbfs,
        peak_dbfs,
        silent: rms_dbfs < -50.0,
    })
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

    /// Проверяет разбор `pw-dump` с микрофоном и sink.
    #[test]
    fn parses_devices() {
        let value: Value = serde_json::from_str(
            r#"[
                {"id":1,"info":{"props":{"media.class":"Audio/Source","node.name":"mic","node.description":"Микрофон"}}},
                {"id":2,"info":{"props":{"media.class":"Audio/Sink","node.name":"speakers","node.description":"Колонки"}}}
            ]"#,
        )
        .expect("json");
        let devices = parse_devices(&value);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].kind, AudioDeviceKind::Mic);
        assert_eq!(devices[0].id, "1");
        assert_eq!(devices[1].id, "2");
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
        let started = recorder.start(dir.path(), None, None).expect("start");
        assert!(started.system || started.mic);
        thread::sleep(Duration::from_secs(2));
        recorder.stop().expect("stop");
        assert!(dir.path().join("system.wav").exists() || dir.path().join("mic.wav").exists());
    }

    /// Проверяет реальный preflight микрофона; запускать вручную.
    #[test]
    #[ignore = "требует PipeWire/PulseAudio и микрофон"]
    fn probes_microphone() {
        let probe = LinuxAudioRecorder::probe(None, AudioDeviceKind::Mic).expect("probe");
        assert!(probe.rms_dbfs.is_finite());
    }
}
