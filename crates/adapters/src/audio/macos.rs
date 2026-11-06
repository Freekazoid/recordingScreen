//! Запись системного звука и микрофона на macOS через ffmpeg AVFoundation.
//!
//! Системный звук macOS не отдаёт приложениям напрямую: нужен виртуальный
//! loopback-драйвер (BlackHole, Soundflower, Loopback). Если его нет,
//! приложение пишет только микрофон и предупреждает пользователя — так же,
//! как это делала версия 3.x. Дорожки раздельные: `system.wav`, `mic.wav`.

use super::linux::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, AudioTrackAnalysis,
    analyze_track,
};
use crate::media::ffmpeg_binary;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Одна дорожка, записываемая ffmpeg.
#[derive(Debug)]
struct Track {
    child: Child,
}

/// Рекордер системного звука и микрофона macOS.
#[derive(Debug, Default)]
pub struct MacAudioRecorder {
    system: Option<Track>,
    mic: Option<Track>,
}

impl MacAudioRecorder {
    /// Создаёт рекордер без запущенных процессов.
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает аудиоустройства AVFoundation.
    ///
    /// `id` — `audio:<индекс>`; по нему ffmpeg выбирает вход. Устройства с
    /// loopback/BlackHole в имени помечаются как системные.
    pub fn list_devices() -> Result<Vec<AudioDevice>, AudioError> {
        let (_, audio) = avfoundation_devices()?;
        let mut devices = Vec::new();
        for (index, name) in audio {
            let kind = if is_loopback_name(&name) {
                AudioDeviceKind::System
            } else {
                AudioDeviceKind::Mic
            };
            devices.push(AudioDevice {
                id: format!("audio:{index}"),
                label: name,
                kind,
            });
        }
        Ok(devices)
    }

    /// Запускает системную дорожку (loopback) и микрофон.
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
        let (_, audio) = avfoundation_devices()?;
        let mut warnings = Vec::new();

        let system_index = select_device(&audio, system_target, true).or_else(|| {
            audio
                .iter()
                .find(|(_, name)| is_loopback_name(name))
                .map(|(i, _)| *i)
        });
        self.system = match system_index {
            Some(index) => spawn_track(&audio, index, 2, &session_dir.join("system.wav"))
                .inspect_err(|error| warnings.push(format!("системный звук: {error}")))
                .ok(),
            None => {
                warnings.push(
                    "системный звук: не найден loopback (BlackHole/Soundflower); \
                     установите виртуальное аудиоустройство"
                        .to_string(),
                );
                None
            }
        };

        let mic_index = select_device(&audio, mic_target, false).or_else(|| {
            audio
                .iter()
                .find(|(_, name)| !is_loopback_name(name))
                .map(|(i, _)| *i)
        });
        self.mic = match mic_index {
            Some(index) => spawn_track(&audio, index, 1, &session_dir.join("mic.wav"))
                .inspect_err(|error| warnings.push(format!("микрофон: {error}")))
                .ok(),
            None => {
                warnings.push("микрофон: AVFoundation не нашёл устройство ввода".to_string());
                None
            }
        };

        if self.system.is_none() && self.mic.is_none() {
            return Err(AudioError::NoDevices);
        }
        Ok(AudioStart {
            system: self.system.is_some(),
            mic: self.mic.is_some(),
            warnings,
        })
    }

    /// Останавливает дорожки через SIGINT, чтобы ffmpeg дописал WAV-заголовок.
    pub fn stop(&mut self) -> Result<(), AudioError> {
        stop_track(&mut self.system, "system")?;
        stop_track(&mut self.mic, "mic")
    }

    /// Проверяет, живы ли процессы записи.
    pub fn is_running(&mut self) -> bool {
        track_running(&mut self.system) || track_running(&mut self.mic)
    }

    /// Записывает 1,2 секунды с выбранного устройства и считает RMS/peak.
    pub fn probe(target: Option<&str>, kind: AudioDeviceKind) -> Result<AudioProbe, AudioError> {
        let (_, audio) = avfoundation_devices()?;
        let index = select_device(&audio, target, kind == AudioDeviceKind::System)
            .or_else(|| {
                audio
                    .iter()
                    .find(|(_, name)| (kind == AudioDeviceKind::System) == is_loopback_name(name))
                    .map(|(index, _)| *index)
            })
            .ok_or(AudioError::NoDevices)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "screenrecorder-probe-{}-{stamp}.wav",
            std::process::id()
        ));
        let channels = if kind == AudioDeviceKind::System {
            2
        } else {
            1
        };
        let mut track =
            Some(spawn_track(&audio, index, channels, &path).map_err(AudioError::Probe)?);
        thread::sleep(Duration::from_millis(1200));
        let _ = stop_track(&mut track, "probe");
        let analysis: Result<AudioTrackAnalysis, AudioError> = analyze_track(&path);
        let _ = std::fs::remove_file(&path);
        let analysis = analysis?;
        Ok(AudioProbe {
            rms_dbfs: analysis.stats.rms_dbfs,
            peak_dbfs: analysis.stats.peak_dbfs,
            silent: analysis.stats.rms_dbfs < -50.0,
        })
    }
}

/// Устройства AVFoundation: индекс и имя.
type DeviceList = Vec<(usize, String)>;

/// Список видео- и аудиоустройств AVFoundation.
fn avfoundation_devices() -> Result<(DeviceList, DeviceList), AudioError> {
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
        .map_err(|error| AudioError::Probe(format!("ffmpeg avfoundation: {error}")))?;
    let text = String::from_utf8_lossy(&output.stderr);
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut section = "";
    for line in text.lines() {
        if line.contains("AVFoundation video devices:") {
            section = "video";
            continue;
        }
        if line.contains("AVFoundation audio devices:") {
            section = "audio";
            continue;
        }
        let Some(start) = line.rfind('[') else {
            continue;
        };
        let Some(end) = line[start..].find(']') else {
            continue;
        };
        let index = line[start + 1..start + end].trim().parse::<usize>();
        let Ok(index) = index else {
            continue;
        };
        let name = line[start + end + 1..].trim().to_string();
        if name.is_empty() {
            continue;
        }
        if section == "video" {
            video.push((index, name));
        } else if section == "audio" {
            audio.push((index, name));
        }
    }
    if video.is_empty() && audio.is_empty() {
        return Err(AudioError::Probe(
            "AVFoundation не вернул устройства".to_string(),
        ));
    }
    Ok((video, audio))
}

/// Ищет устройство по id/имени или возвращает None.
fn select_device(devices: &[(usize, String)], target: Option<&str>, system: bool) -> Option<usize> {
    if let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) {
        for (index, name) in devices {
            if name == target || target == format!("audio:{index}") {
                return Some(*index);
            }
        }
    }
    devices
        .iter()
        .find(|(_, name)| is_loopback_name(name) == system)
        .map(|(index, _)| *index)
}

/// Запускает ffmpeg на одном аудиоустройстве.
fn spawn_track(
    _devices: &[(usize, String)],
    index: usize,
    channels: u32,
    output: &Path,
) -> Result<Track, String> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("каталог {}: {error}", parent.display()))?;
    }
    let target = format!(":{index}");
    let channels_arg = channels.to_string();
    let output_arg = output.to_string_lossy().to_string();
    let mut child = Command::new(ffmpeg_binary())
        .args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostdin",
            "-y",
            "-f",
            "avfoundation",
            "-i",
            target.as_str(),
            "-vn",
            "-ac",
            channels_arg.as_str(),
            "-ar",
            "48000",
            "-c:a",
            "pcm_s16le",
            output_arg.as_str(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("ffmpeg: {error}"))?;
    // Устройство может быть недоступно (нет разрешения или занято).
    thread::sleep(Duration::from_millis(300));
    if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
        return Err(format!("ffmpeg завершился ({status})"));
    }
    Ok(Track { child })
}

/// Останавливает дорожку и ждёт завершения ffmpeg.
fn stop_track(track: &mut Option<Track>, name: &'static str) -> Result<(), AudioError> {
    let Some(mut track) = track.take() else {
        return Ok(());
    };
    // SIGINT заставляет ffmpeg корректно закрыть WAV.
    unsafe {
        libc::kill(track.child.id() as i32, libc::SIGINT);
    }
    for _ in 0..50 {
        match track.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(source) => {
                return Err(AudioError::Stop {
                    track: name,
                    source,
                });
            }
        }
    }
    track.child.kill().map_err(|source| AudioError::Stop {
        track: name,
        source,
    })?;
    track.child.wait().map_err(|source| AudioError::Stop {
        track: name,
        source,
    })?;
    Ok(())
}

/// Проверяет, жив ли процесс дорожки.
fn track_running(track: &mut Option<Track>) -> bool {
    let Some(current) = track.as_mut() else {
        return false;
    };
    match current.child.try_wait() {
        Ok(Some(_)) => {
            *track = None;
            false
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Похоже ли имя устройства на виртуальный loopback системного звука.
fn is_loopback_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        "blackhole",
        "soundflower",
        "loopback",
        "background music",
        "virtual",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}
