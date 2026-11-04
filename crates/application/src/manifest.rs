//! Манифест сессии записи и восстановление незавершённых сессий.
//!
//! Манифест пишется в каталог сессии (`manifest.json`) до старта захвата и
//! обновляется при остановке. Если приложение было убито, при следующем
//! запуске состояние `recording`/`finalizing` превращается в `interrupted`.

use chrono::{DateTime, Utc};
use screenrecorder_domain::{TrackKind, TrackStats, TrackStatus};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Версия схемы манифеста.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Ошибки чтения/записи манифеста.
#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("не удалось прочитать манифест {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("не удалось записать манифест {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("некорректный манифест {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Состояние сессии в манифесте.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    Recording,
    Finalizing,
    Recorded,
    Exported,
    Failed,
    Interrupted,
    Cancelled,
}

/// Прямоугольник области в пикселях снимка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Описание одной дорожки в манифесте.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackManifest {
    pub kind: TrackKind,
    pub path: String,
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub duration_ms: Option<u64>,
    pub start_offset_ms: i64,
    pub status: TrackStatus,
    pub stats: TrackStats,
}

/// Манифест сессии.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub schema_version: u32,
    pub app_version: String,
    pub session_id: String,
    pub state: ManifestState,
    pub mode: String,
    pub backend: String,
    pub fps: u32,
    pub region: Option<Rect>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub audio_offset_ms: i64,
    pub tracks: Vec<TrackManifest>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub export_path: Option<String>,
}

impl SessionManifest {
    /// Создаёт манифест для только что начатой записи.
    pub fn new(
        session_id: impl Into<String>,
        mode: impl Into<String>,
        backend: impl Into<String>,
        fps: u32,
        region: Option<Rect>,
    ) -> Self {
        let now = Utc::now();
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            session_id: session_id.into(),
            state: ManifestState::Recording,
            mode: mode.into(),
            backend: backend.into(),
            fps,
            region,
            started_at: Some(now),
            ended_at: None,
            updated_at: now,
            audio_offset_ms: 0,
            tracks: Vec::new(),
            warnings: Vec::new(),
            export_path: None,
        }
    }

    /// Добавляет дорожку, если такой ещё нет.
    pub fn add_track(&mut self, track: TrackManifest) {
        if self.tracks.iter().any(|item| item.kind == track.kind) {
            return;
        }
        self.tracks.push(track);
    }

    /// Находит дорожку по типу.
    pub fn track_mut(&mut self, kind: TrackKind) -> Option<&mut TrackManifest> {
        self.tracks.iter_mut().find(|track| track.kind == kind)
    }

    /// Добавляет предупреждение без дублей.
    pub fn warn(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.iter().any(|item| item == &warning) {
            self.warnings.push(warning);
        }
    }

    /// Завершает запись и фиксирует время окончания.
    pub fn finish(&mut self, state: ManifestState) {
        self.state = state;
        self.ended_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }

    /// Помечает сессию экспортированной.
    pub fn set_exported(&mut self, path: impl Into<String>) {
        self.state = ManifestState::Exported;
        self.export_path = Some(path.into());
        self.updated_at = Utc::now();
        if self.ended_at.is_none() {
            self.ended_at = Some(Utc::now());
        }
    }

    /// Помечает сессию прерванной.
    pub fn interrupt(&mut self) {
        self.state = ManifestState::Interrupted;
        if self.ended_at.is_none() {
            self.ended_at = Some(Utc::now());
        }
        self.updated_at = Utc::now();
        self.warn("Запись прервана: приложение было закрыто или завершилось с ошибкой.");
    }
}

/// Восстановленная незавершённая сессия.
#[derive(Debug, Clone)]
pub struct RecoveredSession {
    pub session_id: String,
    pub mode: String,
    pub dir: PathBuf,
}

/// Путь к манифесту в каталоге сессии.
pub fn manifest_path(session_dir: &Path) -> PathBuf {
    session_dir.join("manifest.json")
}

/// Сохраняет манифест атомарно.
pub fn save_manifest(session_dir: &Path, manifest: &SessionManifest) -> Result<(), ManifestError> {
    let path = manifest_path(session_dir);
    let temp = session_dir.join("manifest.json.tmp");
    let data = serde_json::to_vec_pretty(manifest).map_err(|source| ManifestError::Json {
        path: path.clone(),
        source,
    })?;
    fs::write(&temp, data).map_err(|source| ManifestError::Write {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, &path).map_err(|source| ManifestError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(())
}

/// Загружает манифест из каталога сессии.
pub fn load_manifest(session_dir: &Path) -> Result<SessionManifest, ManifestError> {
    let path = manifest_path(session_dir);
    let data = fs::read(&path).map_err(|source| ManifestError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&data).map_err(|source| ManifestError::Json { path, source })
}

/// Помечает незавершённые сессии прерванными.
///
/// Возвращает список восстановленных сессий для сообщения пользователю.
pub fn recover_incomplete(recordings_dir: &Path) -> Result<Vec<RecoveredSession>, ManifestError> {
    let mut recovered = Vec::new();
    let Ok(entries) = fs::read_dir(recordings_dir) else {
        return Ok(recovered);
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let Ok(mut manifest) = load_manifest(&dir) else {
            continue;
        };
        if !matches!(
            manifest.state,
            ManifestState::Recording | ManifestState::Finalizing
        ) {
            continue;
        }
        manifest.interrupt();
        if let Err(error) = save_manifest(&dir, &manifest) {
            tracing_warn(&format!(
                "не удалось сохранить восстановленный манифест {}: {error}",
                manifest.session_id
            ));
            continue;
        }
        recovered.push(RecoveredSession {
            session_id: manifest.session_id.clone(),
            mode: manifest.mode.clone(),
            dir,
        });
    }
    Ok(recovered)
}

/// Дописывает строку в журнал сессии.
pub fn append_session_log(session_dir: &Path, message: &str) {
    let path = session_dir.join("capture.log");
    let line = format!(
        "[{}] {message}\n",
        Utc::now().format("%Y-%m-%d %H:%M:%S%.3f")
    );
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}

/// Пишет предупреждение в stderr без зависимости от tracing.
fn tracing_warn(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет сохранение и чтение манифеста.
    #[test]
    fn saves_and_loads_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut manifest = SessionManifest::new("s1", "Весь экран", "x11", 20, None);
        manifest.add_track(TrackManifest {
            kind: TrackKind::Screen,
            path: "screen.mkv".to_string(),
            device: None,
            sample_rate: None,
            channels: None,
            duration_ms: Some(1_000),
            start_offset_ms: 0,
            status: TrackStatus::Ready,
            stats: TrackStats::default(),
        });
        manifest.finish(ManifestState::Recorded);
        save_manifest(dir.path(), &manifest).expect("save");
        let loaded = load_manifest(dir.path()).expect("load");
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.state, ManifestState::Recorded);
        assert_eq!(loaded.tracks.len(), 1);
    }

    /// Проверяет восстановление незавершённой сессии.
    #[test]
    fn recovers_incomplete_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = dir.path().join("session");
        fs::create_dir_all(&session).expect("mkdir");
        let mut manifest = SessionManifest::new("s2", "Область экрана", "wayland", 20, None);
        save_manifest(&session, &manifest).expect("save");
        let recovered = recover_incomplete(dir.path()).expect("recover");
        assert_eq!(recovered.len(), 1);
        manifest = load_manifest(&session).expect("load");
        assert_eq!(manifest.state, ManifestState::Interrupted);
        assert!(manifest.ended_at.is_some());
    }

    /// Проверяет, что завершённая сессия не восстанавливается.
    #[test]
    fn ignores_finished_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = dir.path().join("session");
        fs::create_dir_all(&session).expect("mkdir");
        let mut manifest = SessionManifest::new("s3", "Только аудио", "pw-record", 0, None);
        manifest.finish(ManifestState::Recorded);
        save_manifest(&session, &manifest).expect("save");
        let recovered = recover_incomplete(dir.path()).expect("recover");
        assert!(recovered.is_empty());
    }

    /// Проверяет, что журнал сессии создаётся.
    #[test]
    fn writes_session_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        append_session_log(dir.path(), "старт");
        let text = fs::read_to_string(dir.path().join("capture.log")).expect("log");
        assert!(text.contains("старт"));
    }
}
