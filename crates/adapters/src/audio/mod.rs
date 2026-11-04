//! Запись аудио: системный звук и микрофон раздельными дорожками.

pub mod linux;

pub use linux::{AudioError, AudioStart, LinuxAudioRecorder};
