//! Аудио- и видеодорожки сессии.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентификатор дорожки.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackId(String);

impl TrackId {
    /// Создаёт новый случайный идентификатор.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Возвращает строковое представление идентификатора.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TrackId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TrackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Назначение дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Screen,
    System,
    Mic,
    Mix,
}

/// Состояние готовности дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStatus {
    Pending,
    Recording,
    Ready,
    Silent,
    Failed,
}

/// Измеренные характеристики сигнала.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TrackStats {
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub silence_ratio: f32,
    pub clipping_ratio: f32,
}

impl Default for TrackStats {
    /// Возвращает нейтральные значения до первой проверки сигнала.
    fn default() -> Self {
        Self {
            rms_dbfs: -120.0,
            peak_dbfs: -120.0,
            silence_ratio: 1.0,
            clipping_ratio: 0.0,
        }
    }
}

/// Дорожка записи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub session_id: String,
    pub kind: TrackKind,
    pub device_id: Option<String>,
    pub path: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub duration_ms: Option<u64>,
    pub start_offset_ms: i64,
    pub stats: TrackStats,
    pub status: TrackStatus,
}

impl Track {
    /// Создаёт дорожку в ожидающем состоянии.
    pub fn new(session_id: impl Into<String>, kind: TrackKind, path: impl Into<String>) -> Self {
        Self {
            id: TrackId::new(),
            session_id: session_id.into(),
            kind,
            device_id: None,
            path: path.into(),
            sample_rate: None,
            channels: None,
            duration_ms: None,
            start_offset_ms: 0,
            stats: TrackStats::default(),
            status: TrackStatus::Pending,
        }
    }

    /// Помечает дорожку как готовую и сохраняет измерения.
    pub fn mark_ready(&mut self, stats: TrackStats) {
        self.stats = stats;
        self.status = if stats.silence_ratio >= 0.98 && stats.rms_dbfs < -60.0 {
            TrackStatus::Silent
        } else {
            TrackStatus::Ready
        };
    }
}
