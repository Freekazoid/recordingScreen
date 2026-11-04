//! Прикладной слой: настройки и сценарии использования.

pub mod settings;

pub use settings::{DEFAULT_SETTINGS_JSON, SCHEMA_VERSION, Settings, SettingsError, SettingsStore};
