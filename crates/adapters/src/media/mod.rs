//! Медиа-операции: склейка видео и аудиодорожек, экспорт.

pub mod ffmpeg;

pub use ffmpeg::{ExportRequest, MediaError, merge_audio, merge_recording};
