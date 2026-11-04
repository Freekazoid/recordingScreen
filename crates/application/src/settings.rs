//! Типизированные настройки приложения и их атомарное хранение.
//!
//! Заводские значения лежат в `config/default_settings.json` и встраиваются
//! в бинарник на этапе компиляции. Пользовательский файл создаётся в
//! каталоге конфигурации при первом запуске.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Текущая версия схемы настроек.
pub const SCHEMA_VERSION: u32 = 1;

/// Заводские настройки в виде JSON (встроены в бинарник).
pub const DEFAULT_SETTINGS_JSON: &str = include_str!("../../../config/default_settings.json");

/// Ошибки загрузки, проверки и сохранения настроек.
#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("не удалось прочитать настройки {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось записать настройки {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("некорректный JSON настроек: {0}")]
    Json(#[from] serde_json::Error),

    #[error("неподдерживаемая версия схемы настроек: {0} (ожидается 1)")]
    UnsupportedSchema(u32),

    #[error("некорректное поле {field}: {reason}")]
    InvalidField { field: String, reason: String },
}

/// Параметры захвата.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CaptureSettings {
    pub fps: u32,
    pub video_codec: String,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub mic_device: String,
    pub system_device: String,
}

impl Default for CaptureSettings {
    /// Возвращает параметры захвата по умолчанию.
    fn default() -> Self {
        Self {
            fps: 20,
            video_codec: "h264".to_string(),
            audio_sample_rate: 48_000,
            audio_channels: 2,
            mic_device: String::new(),
            system_device: String::new(),
        }
    }
}

/// Параметры распознавания речи.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TranscriptionSettings {
    pub enabled: bool,
    pub engine: String,
    pub model: String,
    pub language: String,
    pub vad: String,
    pub compute_device: String,
}

impl Default for TranscriptionSettings {
    /// Возвращает параметры ASR по умолчанию.
    fn default() -> Self {
        Self {
            enabled: true,
            engine: "whisper-rs".to_string(),
            model: "large-v3-turbo".to_string(),
            language: "auto".to_string(),
            vad: "sherpa-onnx".to_string(),
            compute_device: "auto".to_string(),
        }
    }
}

/// Параметры диаризации.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DiarizationSettings {
    pub engine: String,
    pub enabled: bool,
    pub min_speakers: Option<u32>,
    pub max_speakers: Option<u32>,
}

impl Default for DiarizationSettings {
    /// Возвращает параметры диаризации по умолчанию.
    fn default() -> Self {
        Self {
            engine: "sherpa-onnx".to_string(),
            enabled: true,
            min_speakers: None,
            max_speakers: None,
        }
    }
}

/// Параметры приватности.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PrivacySettings {
    pub cloud_enabled: bool,
}

impl Default for PrivacySettings {
    /// По умолчанию облако отключено.
    fn default() -> Self {
        Self {
            cloud_enabled: false,
        }
    }
}

/// Параметры интерфейса.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiSettings {
    pub theme: String,
}

impl Default for UiSettings {
    /// Возвращает тему по умолчанию.
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
        }
    }
}

/// Полные настройки приложения.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub output_dir: String,
    #[serde(default)]
    pub models_dir: String,
    #[serde(default)]
    pub capture: CaptureSettings,
    #[serde(default)]
    pub transcription: TranscriptionSettings,
    #[serde(default)]
    pub diarization: DiarizationSettings,
    #[serde(default)]
    pub privacy: PrivacySettings,
    #[serde(default)]
    pub ui: UiSettings,
}

/// Возвращает текущую версию схемы для serde-дефолта.
fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// Возвращает язык по умолчанию для serde-дефолта.
fn default_language() -> String {
    "ru".to_string()
}

impl Default for Settings {
    /// Загружает заводские значения из встроенного JSON.
    fn default() -> Self {
        serde_json::from_str(DEFAULT_SETTINGS_JSON).unwrap_or_else(|_| Self::fallback())
    }
}

impl Settings {
    /// Возвращает аварийные значения, если встроенный JSON повреждён.
    fn fallback() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            language: "ru".to_string(),
            output_dir: String::new(),
            models_dir: String::new(),
            capture: CaptureSettings::default(),
            transcription: TranscriptionSettings::default(),
            diarization: DiarizationSettings::default(),
            privacy: PrivacySettings::default(),
            ui: UiSettings::default(),
        }
    }

    /// Проверяет настройки на согласованность и допустимые диапазоны.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema(self.schema_version));
        }
        if self.language.trim().is_empty() {
            return Err(SettingsError::InvalidField {
                field: "language".to_string(),
                reason: "язык не может быть пустым".to_string(),
            });
        }
        if !(1..=60).contains(&self.capture.fps) {
            return Err(SettingsError::InvalidField {
                field: "capture.fps".to_string(),
                reason: "допустимый диапазон: 1..=60".to_string(),
            });
        }
        if !(8_000..=192_000).contains(&self.capture.audio_sample_rate) {
            return Err(SettingsError::InvalidField {
                field: "capture.audio_sample_rate".to_string(),
                reason: "допустимый диапазон: 8000..=192000".to_string(),
            });
        }
        if !(1..=2).contains(&self.capture.audio_channels) {
            return Err(SettingsError::InvalidField {
                field: "capture.audio_channels".to_string(),
                reason: "допустимо 1 или 2 канала".to_string(),
            });
        }
        if self.transcription.engine != "whisper-rs" {
            return Err(SettingsError::InvalidField {
                field: "transcription.engine".to_string(),
                reason: "в v4 поддерживается whisper-rs".to_string(),
            });
        }
        if self.transcription.model.trim().is_empty() {
            return Err(SettingsError::InvalidField {
                field: "transcription.model".to_string(),
                reason: "модель не может быть пустой".to_string(),
            });
        }
        if self.transcription.language.trim().is_empty() {
            return Err(SettingsError::InvalidField {
                field: "transcription.language".to_string(),
                reason: "язык не может быть пустым".to_string(),
            });
        }
        if !matches!(
            self.transcription.compute_device.as_str(),
            "auto" | "cpu" | "gpu"
        ) {
            return Err(SettingsError::InvalidField {
                field: "transcription.compute_device".to_string(),
                reason: "допустимо auto, cpu или gpu".to_string(),
            });
        }
        if !matches!(
            self.diarization.engine.as_str(),
            "sherpa-onnx" | "nemo" | "none"
        ) {
            return Err(SettingsError::InvalidField {
                field: "diarization.engine".to_string(),
                reason: "допустимо sherpa-onnx, nemo или none".to_string(),
            });
        }
        if let (Some(min), Some(max)) =
            (self.diarization.min_speakers, self.diarization.max_speakers)
            && (min == 0 || max == 0 || min > max)
        {
            return Err(SettingsError::InvalidField {
                field: "diarization.min_speakers/max_speakers".to_string(),
                reason: "ожидается 0 < min <= max".to_string(),
            });
        }
        if !matches!(self.ui.theme.as_str(), "system" | "light" | "dark") {
            return Err(SettingsError::InvalidField {
                field: "ui.theme".to_string(),
                reason: "допустимо system, light или dark".to_string(),
            });
        }
        Ok(())
    }
}

/// Хранилище пользовательских настроек.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Создаёт хранилище по пути к пользовательскому JSON.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Возвращает путь к пользовательскому файлу настроек.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Загружает настройки; если файла нет — возвращает заводские значения.
    pub fn load(&self) -> Result<Settings, SettingsError> {
        if !self.path.exists() {
            return Ok(Settings::default());
        }
        let raw = fs::read_to_string(&self.path).map_err(|source| SettingsError::Read {
            path: self.path.clone(),
            source,
        })?;
        let settings: Settings = serde_json::from_str(&raw)?;
        settings.validate()?;
        Ok(settings)
    }

    /// Загружает настройки и создаёт пользовательский файл при первом запуске.
    pub fn load_or_create(&self) -> Result<Settings, SettingsError> {
        let exists = self.path.exists();
        let settings = self.load()?;
        if !exists {
            self.save(&settings)?;
        }
        Ok(settings)
    }

    /// Атомарно сохраняет настройки, заменяя файл только целиком.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        settings.validate()?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| SettingsError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(settings)?;
        {
            let mut file = fs::File::create(&tmp).map_err(|source| SettingsError::Write {
                path: tmp.clone(),
                source,
            })?;
            file.write_all(&data)
                .map_err(|source| SettingsError::Write {
                    path: tmp.clone(),
                    source,
                })?;
            file.sync_all().map_err(|source| SettingsError::Write {
                path: tmp.clone(),
                source,
            })?;
        }
        fs::rename(&tmp, &self.path).map_err(|source| SettingsError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что встроенные заводские настройки валидны.
    #[test]
    fn embedded_defaults_are_valid() {
        Settings::default()
            .validate()
            .expect("дефолты должны быть валидны");
    }

    /// Проверяет цикл сохранения и загрузки.
    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let settings = Settings::default();
        store.save(&settings).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(settings, loaded);
    }

    /// Проверяет отказ на недопустимом FPS.
    #[test]
    fn rejects_invalid_fps() {
        let mut settings = Settings::default();
        settings.capture.fps = 0;
        assert!(settings.validate().is_err());
    }

    /// Проверяет сохранение пользовательских путей и устройства.
    #[test]
    fn persists_custom_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SettingsStore::new(dir.path().join("settings.json"));
        let settings = Settings {
            models_dir: "/tmp/models".to_string(),
            output_dir: "/tmp/output".to_string(),
            transcription: TranscriptionSettings {
                compute_device: "gpu".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        store.save(&settings).expect("save");
        let loaded = store.load().expect("load");
        assert_eq!(loaded.models_dir, "/tmp/models");
        assert_eq!(loaded.output_dir, "/tmp/output");
        assert_eq!(loaded.transcription.compute_device, "gpu");
    }
}
