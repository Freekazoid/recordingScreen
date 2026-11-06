//! Задачи обработки записи.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентификатор задачи.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(String);

impl JobId {
    /// Создаёт новый случайный идентификатор.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Возвращает строковое представление идентификатора.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

/// Состояние задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Задача обработки (например, транскрипция или экспорт).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub session_id: String,
    pub kind: String,
    pub state: JobState,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Job {
    /// Создаёт новую задачу в очереди.
    pub fn new(session_id: impl Into<String>, kind: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: JobId::new(),
            session_id: session_id.into(),
            kind: kind.into(),
            state: JobState::Queued,
            attempts: 0,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Переводит задачу в выполнение.
    pub fn start(&mut self) {
        self.state = JobState::Running;
        self.attempts += 1;
        self.updated_at = Utc::now();
    }

    /// Завершает задачу успешно.
    pub fn succeed(&mut self) {
        self.state = JobState::Succeeded;
        self.updated_at = Utc::now();
    }

    /// Завершает задачу ошибкой.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.state = JobState::Failed;
        self.last_error = Some(error.into());
        self.updated_at = Utc::now();
    }
}
