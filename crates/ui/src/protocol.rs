//! Протокол обмена между фоновым сервисом и окном интерфейса.
//!
//! Сообщения передаются построчно в JSON через Unix-сокет. Окно опрашивает
//! снимок состояния, а команды отправляет отдельными запросами.

use screenrecorder_application::Settings;
use serde::{Deserialize, Serialize};

/// Версия протокола. Несовпадение версий требует перезапуска сервиса.
pub const PROTOCOL_VERSION: u32 = 2;

/// Прямоугольник области в физических пикселях.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionDto {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Режим запуска записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordMode {
    FullScreen,
    Area,
    Program,
    AudioOnly,
}

/// Запрос на старт записи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartRequest {
    pub mode: RecordMode,
    /// Область для режимов «область» и «программа» на X11.
    pub region: Option<RegionDto>,
}

/// Действие, которое должно выполнить окно по просьбе сервиса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    Area,
    Program,
}

/// Прогресс ML-задачи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressDto {
    pub stage: String,
    pub percent: f32,
    #[serde(default)]
    pub device: Option<String>,
}

/// Снимок состояния для окна.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub status: String,
    pub log: String,
    pub recording: bool,
    pub stopping: bool,
    pub recording_mode: Option<String>,
    pub audio_only: bool,
    pub using_wayland: bool,
    pub wayland_pending: bool,
    pub recording_elapsed: u64,
    pub mic_dbfs: f32,
    pub system_dbfs: f32,
    pub transcribing: bool,
    pub exporting: bool,
    pub preflight_active: bool,
    pub preflight_warnings: Vec<String>,
    pub ml_progress: Option<ProgressDto>,
    pub queued_jobs: usize,
    pub show_requested: bool,
    pub ui_action: Option<UiAction>,
}

/// Запрос окна к сервису.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Регистрация окна и проверка версии протокола.
    Hello { version: u32 },
    /// Получить текущий снимок.
    Snapshot,
    /// Начать запись.
    Start(StartRequest),
    /// Остановить запись.
    Stop,
    /// Отменить текущее распознавание.
    CancelMl,
    /// Удалить сессию и связанные файлы.
    DeleteSession { session_id: String },
    /// Перечитать настройки из переданного объекта.
    ReloadSettings { settings: Box<Settings> },
    /// Подтвердить, что окно обработало запрос «Показать».
    AckShow,
    /// Подтвердить, что окно обработало действие (область/программа).
    AckAction,
    /// Передать X11-идентификатор окна для родителя диалога портала.
    HotkeyParent { xid: u64 },
    /// Показать окно (если оно уже открыто).
    ShowUi,
    /// Завершить сервис.
    Quit,
}

/// Ответ сервиса.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Snapshot(Snapshot),
    Hello { accepted: bool, version: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет сериализацию команды старта с областью.
    #[test]
    fn roundtrips_start_request() {
        let request = Request::Start(StartRequest {
            mode: RecordMode::Area,
            region: Some(RegionDto {
                x: 10,
                y: 20,
                width: 300,
                height: 200,
            }),
        });
        let json = serde_json::to_string(&request).expect("json");
        assert!(json.contains("\"request\":\"start\""));
        let decoded: Request = serde_json::from_str(&json).expect("decode");
        match decoded {
            Request::Start(start) => {
                assert_eq!(start.mode, RecordMode::Area);
                assert_eq!(start.region.map(|region| region.width), Some(300));
            }
            _ => panic!("ожидался start"),
        }
    }
}
