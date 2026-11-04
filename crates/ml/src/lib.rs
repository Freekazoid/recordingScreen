//! ML-слой v4: контракты ASR, VAD и диаризации без привязки к UI.
//!
//! Реализации на whisper-rs и sherpa-onnx добавляются отдельными адаптерами;
//! этот crate задаёт общие типы и ошибки.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

/// Ошибки ML-слоя.
#[derive(Debug, Error)]
pub enum MlError {
    #[error("модель недоступна: {0}")]
    ModelUnavailable(String),

    #[error("ошибка инференса: {0}")]
    Inference(String),

    #[error("запрос отменён")]
    Cancelled,
}

/// Запрос на распознавание речи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrRequest {
    pub audio_path: PathBuf,
    pub model: String,
    pub language: Option<String>,
    pub timestamps: bool,
}

/// Фрагмент распознанного текста.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub speaker: Option<String>,
}

/// Результат распознавания.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub segments: Vec<TranscriptSegment>,
}

/// Запрос на диаризацию.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationRequest {
    pub audio_path: PathBuf,
    pub min_speakers: Option<u32>,
    pub max_speakers: Option<u32>,
}

/// Реплика спикера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
}

/// Статус готовности ML-компонентов.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlStatus {
    pub asr_ready: bool,
    pub diarization_ready: bool,
    pub device: String,
}

/// Порт распознавания речи.
pub trait AsrEngine: Send + Sync {
    /// Распознаёт аудиофайл и возвращает сегменты с таймкодами.
    fn transcribe(&self, request: &AsrRequest) -> Result<Transcript, MlError>;
}

/// Порт диаризации.
pub trait DiarizationEngine: Send + Sync {
    /// Возвращает реплики спикеров для аудиофайла.
    fn diarize(&self, request: &DiarizationRequest) -> Result<Vec<SpeakerTurn>, MlError>;
}
