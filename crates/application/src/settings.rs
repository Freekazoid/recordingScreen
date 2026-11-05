//! Типизированные настройки приложения и их атомарное хранение.
//!
//! Заводские значения лежат в `config/default_settings.json` и встраиваются
//! в бинарник на этапе компиляции. Пользовательский файл создаётся в
//! каталоге конфигурации при первом запуске.

use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// Параметры экспорта.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ExportSettings {
    pub format: String,
    pub video_quality: String,
    pub audio_mode: String,
}

impl Default for ExportSettings {
    /// Возвращает параметры экспорта по умолчанию.
    fn default() -> Self {
        Self {
            format: "mp4".to_string(),
            video_quality: "Хорошее (CRF 23)".to_string(),
            audio_mode: "Без пережатия".to_string(),
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
    pub export: ExportSettings,
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
            export: ExportSettings::default(),
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

    /// Загружает v4-настройки; если файла нет, пробует мигрировать 3.x.
    pub fn load_or_create_migrating(&self, legacy: &[PathBuf]) -> Result<Settings, SettingsError> {
        if self.path.exists() {
            return self.load();
        }
        for path in legacy {
            if !path.is_file() {
                continue;
            }
            let raw = match fs::read_to_string(path) {
                Ok(raw) => raw,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "legacy-настройки не прочитаны");
                    continue;
                }
            };
            match migrate_legacy_settings(&raw) {
                Ok(Some(settings)) => {
                    self.save(&settings)?;
                    tracing::info!(
                        source = %path.display(),
                        "настройки 3.x мигрированы в v4"
                    );
                    return Ok(settings);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "legacy-настройки пропущены"
                    );
                }
            }
        }
        self.load_or_create()
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

/// Возвращает кандидатов на legacy-файл настроек 3.x.
///
/// v4 хранит настройки в `config_dir`, а 3.x — рядом с программой или в
/// XDG-каталоге данных. Порядок: exe, текущий каталог, XDG data, macOS,
/// Windows. Отсутствующие и совпадающие с v4 файлы отбрасываются.
pub fn legacy_settings_candidates(settings_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("settings.json"));
    }
    if let Ok(cwd) = std::env::current_dir()
        && cwd.join("src/default_settings.json").is_file()
    {
        // Dev-режим 3.x: settings.json лежит в корне проекта, а бинарник —
        // в target/debug. Не читаем произвольный settings.json из CWD.
        candidates.push(cwd.join("settings.json"));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        let base = PathBuf::from(data);
        candidates.push(base.join("ScreenRecorder").join("settings.json"));
        candidates.push(base.join("screenrecorder").join("settings.json"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/share/ScreenRecorder/settings.json"));
        candidates.push(home.join("Library/Application Support/ScreenRecorder/settings.json"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(
            PathBuf::from(appdata)
                .join("ScreenRecorder")
                .join("settings.json"),
        );
    }
    candidates.retain(|path| path != settings_path && path.is_file());
    candidates.sort();
    candidates.dedup();
    candidates
}

/// Проверяет, что JSON похож на настройки 3.x, а не на v4.
fn is_legacy_settings(value: &Value) -> bool {
    value.is_object()
        && value.get("schema_version").is_none()
        && value.get("capture").is_none()
        && (value.get("transcription_engine").is_some()
            || value.get("enable_transcription").is_some()
            || value.get("audio_track_mode").is_some())
}

/// Читает настройки 3.x и переводит их в схему v4.
///
/// Возвращает `None`, если JSON уже в формате v4 или не похож на 3.x.
pub fn migrate_legacy_settings(raw: &str) -> Result<Option<Settings>, SettingsError> {
    let value: Value = serde_json::from_str(raw)?;
    if !is_legacy_settings(&value) {
        return Ok(None);
    }
    let mut settings = Settings {
        schema_version: SCHEMA_VERSION,
        language: text_value(&value, "language").unwrap_or_else(|| "ru".to_string()),
        ..Settings::default()
    };
    settings.transcription.enabled = bool_value(&value, "enable_transcription").unwrap_or(true);
    settings.transcription.compute_device = normalize_device(text_value(&value, "compute_device"));
    settings.diarization.enabled = bool_value(&value, "enable_diarization").unwrap_or(true);
    settings.diarization.engine = match text_value(&value, "diarization_method").as_deref() {
        Some("nemo") => "nemo".to_string(),
        Some("none") => "none".to_string(),
        _ => "sherpa-onnx".to_string(),
    };
    settings.diarization.min_speakers = speaker_value(&value, "min_speakers");
    settings.diarization.max_speakers = speaker_value(&value, "max_speakers");
    if settings.diarization.min_speakers.is_none()
        && settings.diarization.max_speakers.is_none()
        && let Some(expected) = text_value(&value, "expected_speakers")
    {
        apply_expected_speakers(&mut settings, &expected);
    }
    settings.export.format = normalize_format(text_value(&value, "output_format"));
    settings.export.video_quality = normalize_quality(text_value(&value, "video_quality"));
    settings.export.audio_mode = normalize_audio_mode(text_value(&value, "audio_track_mode"));
    settings.models_dir = legacy_models_dir(text_value(&value, "models_dir"));
    settings.output_dir = text_value(&value, "output_dir").unwrap_or_default();
    settings.validate()?;
    Ok(Some(settings))
}

/// Извлекает непустую строку из JSON.
fn text_value(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Приводит значение к bool, включая строковые формы.
fn bool_value(value: &Value, key: &str) -> Option<bool> {
    match value.get(key)? {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => Some(value.as_i64().unwrap_or_default() != 0),
        Value::String(value) => match value.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" | "да" | "истина" | "вкл" => Some(true),
            "0" | "false" | "no" | "off" | "нет" | "ложь" | "выкл" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Читает число спикеров из числа или строки.
fn speaker_value(value: &Value, key: &str) -> Option<u32> {
    match value.get(key)? {
        Value::Number(value) => value.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(value) => value.trim().parse::<u32>().ok(),
        _ => None,
    }
    .filter(|value| *value > 0)
}

/// Применяет `expected_speakers` вида `2` или `2-4`.
fn apply_expected_speakers(settings: &mut Settings, expected: &str) {
    let parts: Vec<&str> = expected.split('-').map(str::trim).collect();
    match parts.as_slice() {
        [single] => {
            if let Ok(count) = single.parse::<u32>()
                && count > 0
            {
                settings.diarization.min_speakers = Some(count);
                settings.diarization.max_speakers = Some(count);
            }
        }
        [min, max] => {
            let min = min.parse::<u32>().ok();
            let max = max.parse::<u32>().ok();
            if let (Some(min), Some(max)) = (min, max)
                && min > 0
                && max >= min
            {
                settings.diarization.min_speakers = Some(min);
                settings.diarization.max_speakers = Some(max);
            }
        }
        _ => {}
    }
}

/// Ограничивает устройство вычислений известными значениями.
fn normalize_device(value: Option<String>) -> String {
    match value.as_deref() {
        Some("cpu") => "cpu".to_string(),
        Some("gpu") => "gpu".to_string(),
        _ => "auto".to_string(),
    }
}

/// Приводит формат экспорта к поддерживаемому списку.
fn normalize_format(value: Option<String>) -> String {
    match value.as_deref().map(str::to_lowercase).as_deref() {
        Some("mkv") => "mkv".to_string(),
        Some("webm") => "webm".to_string(),
        Some("avi") => "avi".to_string(),
        Some("mov") => "mov".to_string(),
        Some("flv") => "flv".to_string(),
        _ => "mp4".to_string(),
    }
}

/// Переводит старые подписи качества в актуальные.
fn normalize_quality(value: Option<String>) -> String {
    let value = value.unwrap_or_default();
    if value.contains("CRF 18") {
        "Отличное (CRF 18)".to_string()
    } else if value.contains("CRF 28") {
        "Среднее (CRF 28)".to_string()
    } else if value.contains("CRF 35") {
        "Сильное сжатие (CRF 35)".to_string()
    } else {
        "Хорошее (CRF 23)".to_string()
    }
}

/// Переводит старые подписи аудиорежима в актуальные.
fn normalize_audio_mode(value: Option<String>) -> String {
    let value = value.unwrap_or_default().to_lowercase();
    if value.contains("aac") || value.contains("сжать") {
        "Сжать (AAC 128k)".to_string()
    } else if value.contains("без звука") || value.contains("удалить") {
        "Без звука".to_string()
    } else {
        "Без пережатия".to_string()
    }
}

/// Переводит legacy-путь к моделям в фактический корень v4.
///
/// В 3.x `models_dir` — базовая папка, а модели лежат в `<base>/models`.
/// Если пользователь уже указал саму папку `models`, оставляем её.
fn legacy_models_dir(value: Option<String>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let path = PathBuf::from(&value);
    if path.file_name().and_then(|name| name.to_str()) == Some("models") {
        return value;
    }
    path.join("models").display().to_string()
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

    /// Проверяет миграцию настроек 3.x.
    #[test]
    fn migrates_legacy_settings() {
        let raw = r#"{
            "transcription_engine": "whisperx",
            "diarization_method": "nemo",
            "compute_device": "gpu",
            "enable_transcription": true,
            "enable_diarization": false,
            "expected_speakers": "2-4",
            "output_format": "MKV",
            "video_quality": "Отличное (CRF 18) - почти без потерь",
            "audio_track_mode": "Оставить как есть (без пережатия)",
            "models_dir": "/tmp/legacy",
            "output_dir": "/tmp/output"
        }"#;
        let settings = migrate_legacy_settings(raw)
            .expect("migration")
            .expect("legacy settings");
        assert_eq!(settings.transcription.compute_device, "gpu");
        assert!(settings.transcription.enabled);
        assert!(!settings.diarization.enabled);
        assert_eq!(settings.diarization.engine, "nemo");
        assert_eq!(settings.diarization.min_speakers, Some(2));
        assert_eq!(settings.diarization.max_speakers, Some(4));
        assert_eq!(settings.export.format, "mkv");
        assert_eq!(settings.export.video_quality, "Отличное (CRF 18)");
        assert_eq!(settings.export.audio_mode, "Без пережатия");
        assert_eq!(settings.models_dir, "/tmp/legacy/models");
        assert_eq!(settings.output_dir, "/tmp/output");
    }

    /// Проверяет, что v4-настройки не считаются legacy.
    #[test]
    fn ignores_v4_settings() {
        let raw = serde_json::to_string(&Settings::default()).expect("json");
        assert!(migrate_legacy_settings(&raw).expect("migration").is_none());
    }

    /// Проверяет миграцию старых подписей качества и звука.
    #[test]
    fn migrates_legacy_labels() {
        let raw = r#"{
            "transcription_engine": "vosk",
            "diarization_method": "diarize",
            "video_quality": "Сильное сжатие (CRF 35) - маленький файл",
            "audio_track_mode": "Удалить звук (только видео)"
        }"#;
        let settings = migrate_legacy_settings(raw)
            .expect("migration")
            .expect("legacy settings");
        assert_eq!(settings.diarization.engine, "sherpa-onnx");
        assert_eq!(settings.export.video_quality, "Сильное сжатие (CRF 35)");
        assert_eq!(settings.export.audio_mode, "Без звука");
        assert_eq!(settings.export.format, "mp4");
    }

    /// Проверяет миграцию файла через хранилище.
    #[test]
    fn store_migrates_from_legacy_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = dir.path().join("legacy.json");
        std::fs::write(
            &legacy,
            r#"{"transcription_engine":"whisper","output_format":"mkv"}"#,
        )
        .expect("legacy");
        let store = SettingsStore::new(dir.path().join("v4/settings.json"));
        let settings = store
            .load_or_create_migrating(&[legacy])
            .expect("migration");
        assert_eq!(settings.export.format, "mkv");
        assert!(store.path().is_file());
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
