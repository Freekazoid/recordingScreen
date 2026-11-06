//! Доменный слой: сущности и правила без зависимости от ОС, UI и ML.

pub mod error;
pub mod job;
pub mod session;
pub mod track;

pub use error::DomainError;
pub use job::{Job, JobId, JobState};
pub use session::{Session, SessionId, SessionState};
pub use track::{Track, TrackId, TrackKind, TrackStats, TrackStatus};
