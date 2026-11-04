//! Прикладной слой: настройки и сценарии использования.

pub mod history;
pub mod manifest;
pub mod ml_client;
pub mod settings;

pub use history::{
    HistoryError, HistoryStore, JobRecord, SessionRecord, display_date, import_manifests,
};

pub use manifest::{
    MANIFEST_SCHEMA_VERSION, ManifestError, ManifestState, RecoveredSession, Rect, SessionManifest,
    TrackManifest, append_session_log, load_manifest, manifest_path, recover_incomplete,
    save_manifest,
};
pub use ml_client::{
    DiarizeWorker, MlWorker, SpeakerTurn, Transcript, TranscriptSegment, assign_speakers,
};
pub use settings::{DEFAULT_SETTINGS_JSON, SCHEMA_VERSION, Settings, SettingsError, SettingsStore};
