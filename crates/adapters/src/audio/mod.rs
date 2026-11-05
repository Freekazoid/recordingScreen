//! Запись аудио: системный звук и микрофон раздельными дорожками.
//!
//! Общие типы и анализ WAV живут в `linux`-модуле и не зависят от ОС
//! (там только `hound` и std). Платформенный рекордер выбирается по
//! `cfg`: `pw-record` на Linux, WASAPI на Windows.

pub mod linux;
#[cfg(windows)]
pub mod windows;

pub use linux::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, AudioTrackAnalysis,
    LinuxAudioRecorder, analyze_track, read_levels,
};

/// Платформенный аудиорекордер.
#[cfg(unix)]
pub type AudioRecorder = LinuxAudioRecorder;
/// Платформенный аудиорекордер.
#[cfg(windows)]
pub type AudioRecorder = windows::WindowsAudioRecorder;
