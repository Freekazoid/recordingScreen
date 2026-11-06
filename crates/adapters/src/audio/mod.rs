//! Запись аудио: системный звук и микрофон раздельными дорожками.
//!
//! Общие типы и анализ WAV живут в `linux`-модуле и не зависят от ОС
//! (там только `hound` и std). Платформенный рекордер выбирается по
//! `cfg`: `pw-record` на Linux, WASAPI на Windows, AVFoundation на macOS.

pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

pub use linux::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, AudioTrackAnalysis,
    LinuxAudioRecorder, analyze_track, read_levels,
};

/// Платформенный аудиорекордер.
#[cfg(target_os = "linux")]
pub type AudioRecorder = LinuxAudioRecorder;
/// Платформенный аудиорекордер.
#[cfg(windows)]
pub type AudioRecorder = windows::WindowsAudioRecorder;
/// Платформенный аудиорекордер.
#[cfg(target_os = "macos")]
pub type AudioRecorder = macos::MacAudioRecorder;
