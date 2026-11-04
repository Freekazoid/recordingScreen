//! История записей в SQLite.
//!
//! Таблица `sessions` — витрина для UI; источник истины остаётся в
//! `manifest.json` каждой сессии. При старте история импортирует манифесты,
//! поэтому уже существующие записи не теряются.

use crate::manifest::{ManifestState, SessionManifest, load_manifest};
use chrono::DateTime;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Ошибки истории.
#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("SQLite {path}: {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    #[error("не удалось прочитать каталог {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("манифест {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: crate::manifest::ManifestError,
    },
}

/// Запись истории сессии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub title: String,
    pub mode: String,
    pub state: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub output_dir: String,
    pub manifest_path: String,
    pub transcript_path: Option<String>,
    pub export_path: Option<String>,
    pub error: Option<String>,
}

impl SessionRecord {
    /// Создаёт запись из манифеста сессии.
    pub fn from_manifest(manifest: &SessionManifest, session_dir: &Path) -> Self {
        let created_at = manifest
            .started_at
            .unwrap_or(manifest.updated_at)
            .to_rfc3339();
        let error = manifest.warnings.first().cloned();
        Self {
            id: manifest.session_id.clone(),
            title: manifest.mode.clone(),
            mode: manifest.mode.clone(),
            state: state_name(manifest.state).to_string(),
            created_at,
            started_at: manifest.started_at.map(|value| value.to_rfc3339()),
            ended_at: manifest.ended_at.map(|value| value.to_rfc3339()),
            output_dir: session_dir.display().to_string(),
            manifest_path: session_dir.join("manifest.json").display().to_string(),
            transcript_path: manifest.transcript_path.clone(),
            export_path: manifest.export_path.clone(),
            error,
        }
    }
}

/// Хранилище истории.
#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    /// Создаёт хранилище по пути к файлу SQLite.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Инициализирует схему.
    pub fn init(&self) -> Result<(), HistoryError> {
        let connection = self.connection()?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    mode TEXT NOT NULL,
                    state TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    started_at TEXT,
                    ended_at TEXT,
                    output_dir TEXT NOT NULL,
                    manifest_path TEXT NOT NULL,
                    transcript_path TEXT,
                    export_path TEXT,
                    error TEXT
                );
                CREATE INDEX IF NOT EXISTS sessions_created_idx
                    ON sessions(created_at DESC);",
            )
            .map_err(|source| self.sqlite(source))?;
        Ok(())
    }

    /// Добавляет или обновляет запись.
    pub fn upsert(&self, record: &SessionRecord) -> Result<(), HistoryError> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO sessions (
                    id, title, mode, state, created_at, started_at, ended_at,
                    output_dir, manifest_path, transcript_path, export_path, error
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    mode = excluded.mode,
                    state = excluded.state,
                    ended_at = excluded.ended_at,
                    transcript_path = excluded.transcript_path,
                    export_path = excluded.export_path,
                    error = excluded.error",
                params![
                    record.id,
                    record.title,
                    record.mode,
                    record.state,
                    record.created_at,
                    record.started_at,
                    record.ended_at,
                    record.output_dir,
                    record.manifest_path,
                    record.transcript_path,
                    record.export_path,
                    record.error,
                ],
            )
            .map_err(|source| self.sqlite(source))?;
        Ok(())
    }

    /// Возвращает последние записи.
    pub fn list(&self, limit: usize) -> Result<Vec<SessionRecord>, HistoryError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, title, mode, state, created_at, started_at, ended_at,
                        output_dir, manifest_path, transcript_path, export_path, error
                 FROM sessions ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|source| self.sqlite(source))?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    mode: row.get(2)?,
                    state: row.get(3)?,
                    created_at: row.get(4)?,
                    started_at: row.get(5)?,
                    ended_at: row.get(6)?,
                    output_dir: row.get(7)?,
                    manifest_path: row.get(8)?,
                    transcript_path: row.get(9)?,
                    export_path: row.get(10)?,
                    error: row.get(11)?,
                })
            })
            .map_err(|source| self.sqlite(source))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|source| self.sqlite(source))?);
        }
        Ok(records)
    }

    /// Открывает соединение и создаёт каталог при необходимости.
    fn connection(&self) -> Result<Connection, HistoryError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| HistoryError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Connection::open(&self.path).map_err(|source| self.sqlite(source))
    }

    /// Оборачивает ошибку SQLite с путём.
    fn sqlite(&self, source: rusqlite::Error) -> HistoryError {
        HistoryError::Sqlite {
            path: self.path.clone(),
            source,
        }
    }
}

/// Импортирует все существующие манифесты в историю.
pub fn import_manifests(
    history: &HistoryStore,
    recordings_dir: &Path,
) -> Result<usize, HistoryError> {
    let entries = match std::fs::read_dir(recordings_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => {
            return Err(HistoryError::Io {
                path: recordings_dir.to_path_buf(),
                source,
            });
        }
    };
    let mut imported = 0;
    for entry in entries.flatten() {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let Ok(manifest) = load_manifest(&dir) else {
            continue;
        };
        let record = SessionRecord::from_manifest(&manifest, &dir);
        history.upsert(&record)?;
        imported += 1;
    }
    Ok(imported)
}

/// Строковое имя состояния для БД.
fn state_name(state: ManifestState) -> &'static str {
    match state {
        ManifestState::Recording => "recording",
        ManifestState::Finalizing => "finalizing",
        ManifestState::Recorded => "recorded",
        ManifestState::Exported => "exported",
        ManifestState::Failed => "failed",
        ManifestState::Interrupted => "interrupted",
        ManifestState::Cancelled => "cancelled",
    }
}

/// Форматирует дату для отображения в UI.
pub fn display_date(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%d.%m.%Y %H:%M").to_string())
        .unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{SessionManifest, save_manifest};
    use chrono::Utc;

    /// Проверяет вставку и чтение истории.
    #[test]
    fn stores_and_lists_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = HistoryStore::new(dir.path().join("history.sqlite3"));
        store.init().expect("init");
        let mut record = SessionRecord {
            id: "s1".to_string(),
            title: "Встреча".to_string(),
            mode: "Весь экран".to_string(),
            state: "recorded".to_string(),
            created_at: Utc::now().to_rfc3339(),
            started_at: None,
            ended_at: None,
            output_dir: "/tmp/s1".to_string(),
            manifest_path: "/tmp/s1/manifest.json".to_string(),
            transcript_path: None,
            export_path: None,
            error: None,
        };
        store.upsert(&record).expect("insert");
        record.state = "exported".to_string();
        store.upsert(&record).expect("update");
        let records = store.list(10).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].state, "exported");
    }

    /// Проверяет импорт манифестов в историю.
    #[test]
    fn imports_manifests() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = dir.path().join("session");
        std::fs::create_dir_all(&session).expect("mkdir");
        let mut manifest = SessionManifest::new("s2", "Область экрана", "wayland", 20, None);
        manifest.finish(ManifestState::Recorded);
        save_manifest(&session, &manifest).expect("save");
        let store = HistoryStore::new(dir.path().join("history.sqlite3"));
        store.init().expect("init");
        let imported = import_manifests(&store, dir.path()).expect("import");
        assert_eq!(imported, 1);
        assert_eq!(store.list(10).expect("list")[0].id, "s2");
    }
}
