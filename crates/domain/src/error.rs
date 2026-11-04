//! Доменные ошибки.

use crate::session::SessionState;
use thiserror::Error;

/// Ошибки, которые может вернуть доменный слой.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DomainError {
    /// Запрошен недопустимый переход состояния сессии.
    #[error("недопустимый переход состояния: {from:?} -> {to:?}")]
    InvalidSessionTransition {
        from: SessionState,
        to: SessionState,
    },

    /// Запрошено пустое или некорректное значение.
    #[error("некорректное значение: {0}")]
    InvalidValue(String),
}
