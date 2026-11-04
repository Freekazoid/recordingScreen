//! История записей в SQLite.
//!
//! Таблица `sessions` — витрина для UI; источник истины остаётся в
//! `manifest.json` каждой сессии. При старте история импортирует манифесты,
//! поэтому уже существующие записи не теряются.

use crate::manifest::{ManifestState, SessionManifest, load_manifest};
use chrono::{DateTime, Utc};
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

/// Запись задачи обработки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub state: String,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl JobRecord {
    /// Создаёт задачу в очереди.
    pub fn new(session_id: impl Into<String>, kind: impl Into<String>) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            kind: kind.into(),
            state: "queued".to_string(),
            attempts: 0,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// Переводит задачу в выполнение.
    pub fn start(&mut self) {
        self.state = "running".to_string();
        self.attempts += 1;
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Завершает задачу успешно.
    pub fn succeed(&mut self) {
        self.state = "succeeded".to_string();
        self.updated_at = Utc::now().to_rfc3339();
    }

    /// Завершает задачу ошибкой.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.state = "failed".to_string();
        self.last_error = Some(error.into());
        self.updated_at = Utc::now().to_rfc3339();
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
                    ON sessions(created_at DESC);
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    state TEXT NOT NULL,
                    attempts INTEGER NOT NULL,
                    last_error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS jobs_session_idx
                    ON jobs(session_id, created_at DESC);",
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

    /// Добавляет или обновляет задачу.
    pub fn upsert_job(&self, job: &JobRecord) -> Result<(), HistoryError> {
        let connection = self.connection()?;
        connection
            .execute(
                "INSERT INTO jobs (
                    id, session_id, kind, state, attempts, last_error, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(id) DO UPDATE SET
                    state = excluded.state,
                    attempts = excluded.attempts,
                    last_error = excluded.last_error,
                    updated_at = excluded.updated_at",
                params![
                    job.id,
                    job.session_id,
                    job.kind,
                    job.state,
                    job.attempts,
                    job.last_error,
                    job.created_at,
                    job.updated_at,
                ],
            )
            .map_err(|source| self.sqlite(source))?;
        Ok(())
    }

    /// Возвращает задачи сессии.
    pub fn list_jobs(&self, session_id: &str) -> Result<Vec<JobRecord>, HistoryError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, session_id, kind, state, attempts, last_error, created_at, updated_at
                 FROM jobs WHERE session_id = ?1 ORDER BY created_at",
            )
            .map_err(|source| self.sqlite(source))?;
        let rows = statement
            .query_map([session_id], |row| {
                Ok(JobRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    state: row.get(3)?,
                    attempts: row.get(4)?,
                    last_error: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .map_err(|source| self.sqlite(source))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|source| self.sqlite(source))?);
        }
        Ok(records)
    }

    /// Возвращает задачи в очереди.
    pub fn queued_jobs(&self, limit: usize) -> Result<Vec<JobRecord>, HistoryError> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, session_id, kind, state, attempts, last_error, created_at, updated_at
                 FROM jobs WHERE state = 'queued' ORDER BY created_at LIMIT ?1",
            )
            .map_err(|source| self.sqlite(source))?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(JobRecord {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: row.get(2)?,
                    state: row.get(3)?,
                    attempts: row.get(4)?,
                    last_error: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                })
            })
            .map_err(|source| self.sqlite(source))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|source| self.sqlite(source))?);
        }
        Ok(records)
    }

    /// Возвращает выполнявшиеся задачи в очередь после перезапуска.
    pub fn recover_jobs(&self) -> Result<usize, HistoryError> {
        let connection = self.connection()?;
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "UPDATE jobs SET state = 'queued', updated_at = ?1 WHERE state = 'running'",
                [now],
            )
            .map_err(|source| self.sqlite(source))
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

    /// Проверяет задачи и их восстановление после перезапуска.
    #[test]
    fn stores_and_recovers_jobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = HistoryStore::new(dir.path().join("history.sqlite3"));
        store.init().expect("init");
        let mut job = JobRecord::new("s1", "export");
        store.upsert_job(&job).expect("queued");
        job.start();
        store.upsert_job(&job).expect("running");
        assert_eq!(store.recover_jobs().expect("recover"), 1);
        let jobs = store.list_jobs("s1").expect("jobs");
        assert_eq!(jobs[0].state, "queued");
        job.succeed();
        store.upsert_job(&job).expect("succeeded");
        assert_eq!(store.list_jobs("s1").expect("jobs")[0].state, "succeeded");
    }
}
