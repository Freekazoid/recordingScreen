//! Запись аудио: системный звук и микрофон раздельными дорожками.

pub mod linux;

pub use linux::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, LinuxAudioRecorder,
};
