//! Прикладной слой: настройки и сценарии использования.

pub mod manifest;
pub mod settings;

pub use manifest::{
    MANIFEST_SCHEMA_VERSION, ManifestError, ManifestState, RecoveredSession, Rect, SessionManifest,
    TrackManifest, append_session_log, load_manifest, manifest_path, recover_incomplete,
    save_manifest,
};
pub use settings::{DEFAULT_SETTINGS_JSON, SCHEMA_VERSION, Settings, SettingsError, SettingsStore};
