//! Сессия записи и её состояния.

use crate::error::DomainError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентификатор сессии.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(String);

impl SessionId {
    /// Создаёт новый случайный идентификатор.
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Возвращает строковое представление идентификатора.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Состояние жизненного цикла сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Created,
    Preparing,
    Recording,
    Finalizing,
    Recorded,
    Processing,
    Transcribed,
    Exported,
    Failed,
    Cancelled,
}

impl SessionState {
    /// Проверяет, допустим ли переход в новое состояние.
    pub fn can_transition_to(self, next: Self) -> bool {
        use SessionState::*;

        matches!(
            (self, next),
            (Created, Preparing)
                | (Created, Cancelled)
                | (Preparing, Recording)
                | (Preparing, Failed)
                | (Preparing, Cancelled)
                | (Recording, Finalizing)
                | (Recording, Failed)
                | (Finalizing, Recorded)
                | (Finalizing, Failed)
                | (Recorded, Processing)
                | (Recorded, Cancelled)
                | (Processing, Transcribed)
                | (Processing, Failed)
                | (Processing, Cancelled)
                | (Transcribed, Exported)
                | (Transcribed, Failed)
                | (Exported, Processing)
                | (Failed, Preparing)
                | (Failed, Cancelled)
        )
    }
}

/// Сессия записи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub title: String,
    pub state: SessionState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub output_dir: String,
}

impl Session {
    /// Создаёт новую сессию в состоянии `Created`.
    pub fn new(title: impl Into<String>, output_dir: impl Into<String>) -> Self {
        Self {
            id: SessionId::new(),
            title: title.into(),
            state: SessionState::Created,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            output_dir: output_dir.into(),
        }
    }

    /// Переводит сессию в новое состояние с проверкой допустимости.
    pub fn transition_to(&mut self, next: SessionState) -> Result<(), DomainError> {
        if !self.state.can_transition_to(next) {
            return Err(DomainError::InvalidSessionTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        match next {
            SessionState::Recording => self.started_at = Some(Utc::now()),
            SessionState::Exported | SessionState::Cancelled | SessionState::Failed => {
                self.ended_at = Some(Utc::now());
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет допустимый путь сессии от создания до экспорта.
    #[test]
    fn session_happy_path() {
        let mut session = Session::new("Встреча", "/tmp");
        for state in [
            SessionState::Preparing,
            SessionState::Recording,
            SessionState::Finalizing,
            SessionState::Recorded,
            SessionState::Processing,
            SessionState::Transcribed,
            SessionState::Exported,
        ] {
            session
                .transition_to(state)
                .expect("переход должен быть допустим");
        }
        assert_eq!(session.state, SessionState::Exported);
        assert!(session.ended_at.is_some());
    }

    /// Проверяет отказ на недопустимом переходе.
    #[test]
    fn rejects_invalid_transition() {
        let mut session = Session::new("Встреча", "/tmp");
        let result = session.transition_to(SessionState::Exported);
        assert!(result.is_err());
        assert_eq!(session.state, SessionState::Created);
    }
}
