//! Нативный интерфейс v4 на egui/eframe.
//!
//! Внешний вид повторяет текущее приложение: тёмная тема, вкладки
//! «Запись / Настройки / О авторе», крупные кнопки режимов, блок экспорта
//! и журнал внизу. Окно — тонкий клиент фонового сервиса: запись, очередь
//! и трей живут в отдельном процессе и переживают закрытие окна.

mod daemon;
mod ipc;
#[cfg(target_os = "linux")]
mod portal_hotkeys;
mod protocol;
#[cfg(windows)]
mod win32;
#[cfg(target_os = "linux")]
mod x11_activation;

pub use daemon::run_daemon;

use chrono::Local;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    TextStyle, Vec2,
};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, hotkey::HotKey};
use protocol::{
    HotkeyAction, ProgressDto, RecordMode, RegionDto, Request, Response, Snapshot, StartRequest,
    UiAction,
};
use screenrecorder_adapters::{
    AudioDevice, AudioDeviceKind, AudioProbe, AudioRecorder, DownloadProgress, ExportRequest,
    ModelManager, PORTAL_CANCELLED, Paths, PortalSource, Region, VideoRecorder,
    WaylandPortalRecorder, WindowInfo, analyze_track, list_windows, merge_audio, merge_recording,
    read_levels, recover_all_segments, resolve_models_root, take_screenshot,
};
use screenrecorder_application::{
    DiarizeWorker, HistoryStore, JobRecord, ManifestState, MlWorker, NemoWorker, Rect,
    SessionManifest, SessionRecord, Settings, SettingsStore, TrackManifest, Transcript,
    append_session_log, assign_speakers, display_date, import_manifests, load_manifest,
    recover_incomplete, save_manifest,
};
use screenrecorder_domain::{SessionId, TrackKind, TrackStats, TrackStatus};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(target_os = "linux")]
use std::sync::mpsc::Sender;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuId, MenuItem, accelerator::Accelerator},
};
#[cfg(target_os = "linux")]
use winit::platform::wayland::EventLoopBuilderExtWayland;
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11;

/// Результат фоновой загрузки модели.
enum DownloadEvent {
    Progress(DownloadProgress),
    Finished(Result<(), String>),
}

/// Прогресс ML-воркера.
struct ProgressInfo {
    stage: String,
    percent: f32,
    pid: u32,
    pass: u8,
    device: String,
}

/// Результат фоновой остановки записи.
struct StopOutcome {
    audio_result: Result<(), String>,
    video_result: Result<(), String>,
    audio_only: bool,
    was_wayland_pending: bool,
    was_wayland_active: bool,
}

/// Результат фонового распознавания.
struct TranscriptOutput {
    text_path: PathBuf,
    json_path: PathBuf,
    srt_path: PathBuf,
    vtt_path: PathBuf,
    md_path: PathBuf,
    segments: usize,
    language: String,
    warning: Option<String>,
}

/// Редактируемое поле глобального сочетания.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyField {
    FullScreen,
    Area,
    Program,
    AudioOnly,
    Stop,
}

/// Действие из трея или горячей клавиши.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayCommand {
    FullScreen,
    Area,
    Program,
    AudioOnly,
    Stop,
    Show,
    Quit,
}

/// Действие с моделью в настройках.
#[derive(Debug, Clone, Copy)]
enum ModelAction {
    Download,
    Delete,
}

/// Каталог, который выбирает пользователь.
#[derive(Debug, Clone, Copy)]
enum FolderTarget {
    Models,
    Output,
}

/// Результат автоматической проверки звука перед записью.
struct PreflightResult {
    mic: Result<AudioProbe, String>,
    system: Result<AudioProbe, String>,
}

/// Что запустить после preflight-проверки.
#[derive(Debug, Clone, Copy)]
enum PendingStart {
    AudioOnly,
    FullScreen,
    Region(Region, &'static str),
    ProgramWayland,
}

const BG: Color32 = Color32::from_rgb(0x20, 0x20, 0x20);
const TEXT: Color32 = Color32::from_rgb(0xcc, 0xca, 0xca);
const BTN: Color32 = Color32::from_rgb(0x35, 0x35, 0x35);
const ENTRY: Color32 = Color32::from_rgb(0x28, 0x28, 0x28);
const HINT: Color32 = Color32::from_rgb(0xe0, 0xb0, 0x40);
const LINK: Color32 = Color32::from_rgb(0x8c, 0xc8, 0xff);
const WHITE: Color32 = Color32::WHITE;
const RED: Color32 = Color32::from_rgb(0xee, 0x54, 0x54);
const GREEN: Color32 = Color32::from_rgb(0x46, 0xb8, 0x6f);

/// Вкладки приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Record,
    History,
    Settings,
    About,
}

/// Графический бэкенд окна.
///
/// На Wayland winit может навсегда перестать присылать `RedrawRequested`,
/// если compositor задержал frame callback (окно было закрыто другим окном
/// или потерялось событие драйвера). Тогда процесс жив, сервис отвечает,
/// но окно не перерисовывается и не реагирует на клики. Для панели
/// управления это критичнее нативности, поэтому по умолчанию выбираем X11
/// через XWayland; Wayland можно включить принудительно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiBackend {
    X11,
    Wayland,
}

/// Главное окно приложения.
pub struct ScreenRecorderApp {
    paths: Paths,
    settings: Settings,
    screen: Screen,
    status: String,
    log: String,
    output_format: String,
    video_quality: String,
    audio_track_mode: String,
    recorder: VideoRecorder,
    wayland_recorder: WaylandPortalRecorder,
    using_wayland: bool,
    wayland_pending: bool,
    wayland_started_at: Option<Instant>,
    audio_recorder: AudioRecorder,
    recording: bool,
    recording_started_at: Option<Instant>,
    session_dir: Option<PathBuf>,
    last_export_path: Option<PathBuf>,
    model_manager: ModelManager,
    download_rx: Option<Receiver<DownloadEvent>>,
    download_model: Option<&'static str>,
    pending_model_download: Option<&'static str>,
    download_progress: f32,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    folder_target: Option<FolderTarget>,
    settings_dirty: bool,
    client: Option<ipc::Client>,
    ui_backend: UiBackend,
    daemon_token: Option<String>,
    last_daemon_poll: Instant,
    last_history_refresh: Instant,
    last_state_log: Instant,
    daemon_lost_at: Option<Instant>,
    recording_elapsed: u64,
    preflight_active: bool,
    queued_jobs_count: usize,
    show_requested: bool,
    ui_action: Option<UiAction>,
    ui_clients: usize,
    ui_launching_until: Option<Instant>,
    quit_requested: bool,
    audio_devices: Vec<AudioDevice>,
    probe_rx: Option<Receiver<Result<AudioProbe, String>>>,
    probe_note: String,
    export_rx: Option<Receiver<Result<PathBuf, String>>>,
    exporting: bool,
    stop_rx: Option<Receiver<StopOutcome>>,
    stopping: bool,
    quit_after_stop: bool,
    audio_offset_ms: i64,
    audio_only: bool,
    recording_mode: Option<&'static str>,
    mic_dbfs: f32,
    system_dbfs: f32,
    last_levels: Instant,
    area_rx: Option<Receiver<Result<Region, String>>>,
    window_list: Vec<WindowInfo>,
    show_windows: bool,
    manifest: Option<SessionManifest>,
    preflight_rx: Option<Receiver<PreflightResult>>,
    pending_start: Option<PendingStart>,
    preflight_warnings: Vec<String>,
    transcript_rx: Option<Receiver<Result<TranscriptOutput, String>>>,
    transcribing: bool,
    ml_progress: Option<ProgressInfo>,
    ml_asr_passes: u8,
    ml_total_phases: u8,
    last_progress_poll: Instant,
    cancel_requested: bool,
    history: HistoryStore,
    history_records: Vec<SessionRecord>,
    export_job: Option<JobRecord>,
    transcription_job: Option<JobRecord>,
    queued_jobs: Vec<JobRecord>,
    delete_confirm: Option<SessionRecord>,
    tray: Option<TrayIcon>,
    tray_status_item: Option<MenuItem>,
    tray_stop_item: Option<MenuItem>,
    tray_commands: Vec<(MenuId, TrayCommand)>,
    tray_hotkey_items: Vec<(MenuItem, TrayCommand, &'static str)>,
    tray_recording_state: Option<bool>,
    last_tray_command: Option<(TrayCommand, Instant)>,
    hotkey_manager: Option<GlobalHotKeyManager>,
    hotkey_bindings: Vec<(HotKey, TrayCommand)>,
    hotkey_edit: Option<HotkeyField>,
    #[cfg(target_os = "linux")]
    portal_hotkeys_rx: Option<Receiver<portal_hotkeys::PortalHotkeyEvent>>,
    #[cfg(target_os = "linux")]
    portal_parent_tx: Option<Sender<u64>>,
    #[cfg(target_os = "linux")]
    hotkey_parent_sent: bool,
}

impl ScreenRecorderApp {
    /// Создаёт окно, подключённое к фоновому сервису.
    pub(crate) fn new_ui(
        paths: Paths,
        settings: Settings,
        client: ipc::Client,
        backend: UiBackend,
    ) -> Self {
        Self::build(paths, settings, Some(client), backend)
    }

    /// Создаёт фоновый сервис без окна.
    pub(crate) fn new_daemon(paths: Paths, settings: Settings) -> Self {
        Self::build(paths, settings, None, UiBackend::X11)
    }

    /// Общая инициализация окна и сервиса.
    ///
    /// Сервис владеет записью, очередью, историей и треем. Окно только
    /// читает снимок состояния и отправляет команды.
    fn build(
        paths: Paths,
        settings: Settings,
        client: Option<ipc::Client>,
        backend: UiBackend,
    ) -> Self {
        let daemon = client.is_none();
        let model_manager = ModelManager::new(models_root_for(&paths, &settings.models_dir));
        let audio_devices = AudioRecorder::list_devices().unwrap_or_default();
        let recordings = paths.data_dir.join("recordings");
        let recovered = if daemon {
            recover_incomplete(&recordings).unwrap_or_default()
        } else {
            Vec::new()
        };
        let (recovered_segments, segment_errors) = if daemon {
            recover_all_segments(&recordings)
        } else {
            (0, Vec::new())
        };
        let history = HistoryStore::new(paths.data_dir.join("history.sqlite3"));
        let mut log = if !daemon {
            "Окно подключено к фоновому сервису.".to_string()
        } else if recovered.is_empty() {
            "Готово. Проверьте систему и настройки.".to_string()
        } else {
            format!(
                "Восстановлено незавершённых сессий: {}. Файлы лежат в {}.",
                recovered.len(),
                recordings.display()
            )
        };
        if recovered_segments > 0 {
            log = format!("{log} Склеено сегментов видео: {recovered_segments}.");
        }
        if !segment_errors.is_empty() {
            log = format!("{log} Ошибки склейки: {}", segment_errors.join("; "));
        }
        if let Err(error) = history.init() {
            log = format!("История недоступна: {error}");
        } else if daemon
            && let Ok(imported) = import_manifests(&history, &recordings)
            && imported > 0
        {
            log = format!("{log} В истории записей: {imported}.");
        }
        if daemon
            && let Ok(recovered_jobs) = history.recover_jobs()
            && recovered_jobs > 0
        {
            log = format!("{log} Задач возвращено в очередь: {recovered_jobs}.");
        }
        let queued_jobs = if daemon {
            history.queued_jobs(20).unwrap_or_default()
        } else {
            Vec::new()
        };
        let history_records = history.list(100).unwrap_or_default();
        let status = if !daemon {
            "Подключение к сервису…".to_string()
        } else if model_manager.is_downloaded("whisper") {
            "Готово к записи".to_string()
        } else {
            "Модели не скачаны".to_string()
        };
        let output_format = settings.export.format.clone();
        let video_quality = settings.export.video_quality.clone();
        let audio_track_mode = settings.export.audio_mode.clone();
        let mut app = Self {
            paths,
            settings,
            screen: Screen::Record,
            status,
            log,
            output_format,
            video_quality,
            audio_track_mode,
            recorder: VideoRecorder::new(),
            wayland_recorder: WaylandPortalRecorder::new(),
            using_wayland: false,
            wayland_pending: false,
            wayland_started_at: None,
            audio_recorder: AudioRecorder::new(),
            recording: false,
            recording_started_at: None,
            session_dir: None,
            last_export_path: None,
            model_manager,
            download_rx: None,
            download_model: None,
            pending_model_download: None,
            download_progress: 0.0,
            folder_rx: None,
            folder_target: None,
            settings_dirty: false,
            client,
            ui_backend: backend,
            daemon_token: None,
            last_daemon_poll: Instant::now(),
            last_history_refresh: Instant::now(),
            last_state_log: Instant::now(),
            daemon_lost_at: None,
            recording_elapsed: 0,
            preflight_active: false,
            queued_jobs_count: queued_jobs.len(),
            show_requested: false,
            ui_action: None,
            ui_clients: 0,
            ui_launching_until: None,
            quit_requested: false,
            audio_devices,
            probe_rx: None,
            probe_note: String::new(),
            export_rx: None,
            exporting: false,
            stop_rx: None,
            stopping: false,
            quit_after_stop: false,
            audio_offset_ms: 0,
            audio_only: false,
            recording_mode: None,
            mic_dbfs: -90.0,
            system_dbfs: -90.0,
            last_levels: Instant::now(),
            area_rx: None,
            window_list: Vec::new(),
            show_windows: false,
            manifest: None,
            preflight_rx: None,
            pending_start: None,
            preflight_warnings: Vec::new(),
            transcript_rx: None,
            transcribing: false,
            ml_progress: None,
            ml_asr_passes: 1,
            ml_total_phases: 1,
            last_progress_poll: Instant::now(),
            cancel_requested: false,
            history,
            history_records,
            export_job: None,
            transcription_job: None,
            queued_jobs,
            delete_confirm: None,
            tray: None,
            tray_status_item: None,
            tray_stop_item: None,
            tray_commands: Vec::new(),
            tray_hotkey_items: Vec::new(),
            tray_recording_state: None,
            last_tray_command: None,
            hotkey_manager: None,
            hotkey_bindings: Vec::new(),
            hotkey_edit: None,
            #[cfg(target_os = "linux")]
            portal_hotkeys_rx: None,
            #[cfg(target_os = "linux")]
            portal_parent_tx: None,
            #[cfg(target_os = "linux")]
            hotkey_parent_sent: false,
        };
        if daemon {
            app.init_tray();
            app.init_hotkeys();
        } else {
            #[cfg(target_os = "linux")]
            std::thread::spawn(|| {
                // Окно уже создано eframe. Убираем кнопку maximize через
                // Motif hints; Mutter перечитывает их при повторном map.
                let pid = std::process::id();
                for _ in 0..10 {
                    match x11_activation::disable_maximize(pid) {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(%error, "не удалось убрать кнопку maximize");
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
        }
        app
    }

    /// Создаёт иконку в системном трее.
    fn init_tray(&mut self) {
        let icon = match image::load_from_memory(include_bytes!("../../../assets/icon.png")) {
            Ok(image) => image
                .resize(64, 64, image::imageops::FilterType::Lanczos3)
                .to_rgba8(),
            Err(error) => {
                self.log = format!("Трей: иконка недоступна: {error}");
                return;
            }
        };
        let (width, height) = icon.dimensions();
        let icon = match tray_icon::Icon::from_rgba(icon.into_raw(), width, height) {
            Ok(icon) => icon,
            Err(error) => {
                self.log = format!("Трей: иконка: {error}");
                return;
            }
        };
        let menu = Menu::new();
        let hotkeys = self.settings.hotkeys.clone();
        let accelerator = |combo: &str| {
            parse_hotkey(combo).map(|hotkey| Accelerator::new(hotkey.mods, hotkey.key))
        };
        let status = MenuItem::new("Статус: готово", false, None);
        let full = MenuItem::new(
            tray_label("Весь экран", &hotkeys.fullscreen),
            true,
            accelerator(&hotkeys.fullscreen),
        );
        let area = MenuItem::new(
            tray_label("Область экрана", &hotkeys.area),
            true,
            accelerator(&hotkeys.area),
        );
        let program = MenuItem::new(
            tray_label("Программа", &hotkeys.program),
            true,
            accelerator(&hotkeys.program),
        );
        let audio = MenuItem::new(
            tray_label("Только аудио", &hotkeys.audio_only),
            true,
            accelerator(&hotkeys.audio_only),
        );
        let stop = MenuItem::new(
            tray_label("Остановить запись", &hotkeys.stop),
            false,
            accelerator(&hotkeys.stop),
        );
        let show = MenuItem::new("Показать", true, None);
        let quit = MenuItem::new("Выход", true, None);

        let mut commands = Vec::new();
        let mut hotkey_items = Vec::new();
        for (item, command, name) in [
            (&full, TrayCommand::FullScreen, "Весь экран"),
            (&area, TrayCommand::Area, "Область экрана"),
            (&program, TrayCommand::Program, "Программа"),
            (&audio, TrayCommand::AudioOnly, "Только аудио"),
            (&stop, TrayCommand::Stop, "Остановить запись"),
            (&show, TrayCommand::Show, "Показать"),
            (&quit, TrayCommand::Quit, "Выход"),
        ] {
            if menu.append(item).is_err() {
                self.log = "Трей: не удалось собрать меню".to_string();
                return;
            }
            commands.push((item.id().clone(), command));
            if !matches!(command, TrayCommand::Show | TrayCommand::Quit) {
                hotkey_items.push((item.clone(), command, name));
            }
        }
        if menu.prepend(&status).is_err() {
            self.log = "Трей: не удалось добавить статус".to_string();
            return;
        }
        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("Screen Recorder")
            .build()
        {
            Ok(tray) => {
                self.tray_status_item = Some(status);
                self.tray_stop_item = Some(stop);
                self.tray_commands = commands;
                self.tray_hotkey_items = hotkey_items;
                self.tray = Some(tray);
            }
            Err(error) => {
                self.log = format!("Трей недоступен: {error}");
            }
        }
    }

    /// Регистрирует глобальные горячие клавиши для режимов записи.
    ///
    /// На Wayland сначала пробуем портал: X11-захват не видит нативные
    /// приложения. Если портал недоступен, остаётся X11-fallback.
    fn init_hotkeys(&mut self) {
        // X11-сочетания оставляем как немедленный fallback. Портал на
        // Wayland добавляет глобальные сочетания поверх; дубли гасятся
        // дебаунсом в `execute_tray_command`.
        self.init_x11_hotkeys();
        #[cfg(target_os = "linux")]
        if is_wayland() && std::env::var_os("SCREENRECORDER_PORTAL_HOTKEYS").is_some() {
            let (parent_tx, parent_rx) = mpsc::channel();
            match portal_hotkeys::spawn(parent_rx) {
                Ok(receiver) => {
                    tracing::info!("глобальные клавиши: XDG-портал");
                    self.portal_hotkeys_rx = Some(receiver);
                    self.portal_parent_tx = Some(parent_tx);
                }
                Err(error) => {
                    tracing::warn!(%error, "портал глобальных клавиш не запущен");
                    self.log = format!("Глобальные клавиши: {error}");
                }
            }
        }
    }

    /// Регистрирует X11-горячие клавиши (fallback).
    fn init_x11_hotkeys(&mut self) {
        if self.hotkey_manager.is_some() {
            return;
        }
        let manager = match GlobalHotKeyManager::new() {
            Ok(manager) => manager,
            Err(error) => {
                self.log = format!("Горячие клавиши недоступны: {error}");
                return;
            }
        };
        self.hotkey_manager = Some(manager);
        self.register_x11_hotkeys();
    }

    /// Перерегистрирует X11-сочетания из настроек.
    fn register_x11_hotkeys(&mut self) {
        let mut registered = Vec::new();
        let mut errors = Vec::new();
        if let Some(manager) = self.hotkey_manager.as_ref() {
            let old = self
                .hotkey_bindings
                .iter()
                .map(|(hotkey, _)| *hotkey)
                .collect::<Vec<_>>();
            if !old.is_empty() {
                let _ = manager.unregister_all(&old);
            }
            let bindings = [
                (
                    self.settings.hotkeys.fullscreen.clone(),
                    TrayCommand::FullScreen,
                ),
                (self.settings.hotkeys.area.clone(), TrayCommand::Area),
                (self.settings.hotkeys.program.clone(), TrayCommand::Program),
                (
                    self.settings.hotkeys.audio_only.clone(),
                    TrayCommand::AudioOnly,
                ),
                (self.settings.hotkeys.stop.clone(), TrayCommand::Stop),
            ];
            for (combo, command) in bindings {
                let Some(hotkey) = parse_hotkey(&combo) else {
                    errors.push(format!("неверное сочетание: {combo}"));
                    continue;
                };
                if let Err(error) = manager.register(hotkey) {
                    errors.push(format!("{combo}: {error}"));
                    continue;
                }
                registered.push((hotkey, command));
            }
        }
        self.hotkey_bindings = registered;
        tracing::info!(
            registered = self.hotkey_bindings.len(),
            errors = ?errors,
            "глобальные горячие клавиши"
        );
        if !errors.is_empty() {
            self.log = errors.join("; ");
        }
    }

    /// Применяет изменённые сочетания к X11 и подписям трея.
    fn apply_hotkeys(&mut self) {
        self.register_x11_hotkeys();
        self.refresh_tray_hotkey_labels();
    }

    /// Обновляет подписи сочетаний в меню трея.
    fn refresh_tray_hotkey_labels(&self) {
        for (item, command, name) in &self.tray_hotkey_items {
            let combo = self.hotkey_combo(*command);
            if !combo.is_empty() {
                item.set_text(tray_label(name, combo));
            }
        }
    }

    /// Возвращает сочетание для команды.
    fn hotkey_combo(&self, command: TrayCommand) -> &str {
        match command {
            TrayCommand::FullScreen => &self.settings.hotkeys.fullscreen,
            TrayCommand::Area => &self.settings.hotkeys.area,
            TrayCommand::Program => &self.settings.hotkeys.program,
            TrayCommand::AudioOnly => &self.settings.hotkeys.audio_only,
            TrayCommand::Stop => &self.settings.hotkeys.stop,
            TrayCommand::Show | TrayCommand::Quit => "",
        }
    }

    /// Записывает новое сочетание в настройки и применяет его.
    fn set_hotkey(&mut self, field: HotkeyField, combo: String) {
        match field {
            HotkeyField::FullScreen => self.settings.hotkeys.fullscreen = combo,
            HotkeyField::Area => self.settings.hotkeys.area = combo,
            HotkeyField::Program => self.settings.hotkeys.program = combo,
            HotkeyField::AudioOnly => self.settings.hotkeys.audio_only = combo,
            HotkeyField::Stop => self.settings.hotkeys.stop = combo,
        }
        self.settings_dirty = true;
    }

    /// Ловит следующее сочетание клавиш для выбранного поля.
    fn capture_hotkey(&mut self, ctx: &egui::Context) {
        let Some(field) = self.hotkey_edit else {
            return;
        };
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.hotkey_edit = None;
            return;
        }
        let captured = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => format_hotkey(*key, *modifiers),
                _ => None,
            })
        });
        if let Some(combo) = captured {
            self.set_hotkey(field, combo);
            self.hotkey_edit = None;
        }
    }

    /// Обновляет подписи трея фактическими сочетаниями из портала.
    #[cfg(target_os = "linux")]
    fn apply_portal_shortcuts(&mut self, shortcuts: &[(String, String)]) {
        for (id, description) in shortcuts {
            let Some(command) = portal_command(id) else {
                continue;
            };
            let trigger = clean_trigger(description);
            if trigger.is_empty() {
                continue;
            }
            if let Some((item, _, name)) = self
                .tray_hotkey_items
                .iter()
                .find(|(_, item_command, _)| *item_command == command)
            {
                item.set_text(tray_label(name, &trigger));
            }
        }
        tracing::info!(?shortcuts, "глобальные клавиши привязаны через портал");
    }

    /// Обрабатывает события трея.
    fn poll_tray(&mut self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(command) = self
                .tray_commands
                .iter()
                .find(|(id, _)| id == &event.id)
                .map(|(_, command)| *command)
            {
                self.execute_tray_command(command);
            }
        }
    }

    /// Обрабатывает глобальные горячие клавиши.
    fn poll_hotkeys(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let events = self
                .portal_hotkeys_rx
                .as_ref()
                .map(|receiver| {
                    let mut events = Vec::new();
                    while let Ok(event) = receiver.try_recv() {
                        events.push(event);
                    }
                    events
                })
                .unwrap_or_default();
            for event in events {
                match event {
                    portal_hotkeys::PortalHotkeyEvent::Bound(shortcuts) => {
                        self.apply_portal_shortcuts(&shortcuts);
                    }
                    portal_hotkeys::PortalHotkeyEvent::Activated(command) => {
                        self.execute_tray_command(command);
                    }
                    portal_hotkeys::PortalHotkeyEvent::Failed(error) => {
                        tracing::warn!(%error, "глобальные клавиши через портал недоступны");
                        self.log = format!("Глобальные клавиши: {error}");
                        self.portal_hotkeys_rx = None;
                        self.init_x11_hotkeys();
                    }
                }
            }
        }
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            if let Some((_, command)) = self
                .hotkey_bindings
                .iter()
                .find(|(hotkey, _)| hotkey.id() == event.id)
                .copied()
            {
                tracing::info!(?command, "глобальная горячая клавиша");
                self.execute_tray_command(command);
            }
        }
    }

    /// Выполняет команду трея или горячей клавиши в фоновом сервисе.
    fn execute_tray_command(&mut self, command: TrayCommand) {
        if let Some((last, at)) = self.last_tray_command
            && last == command
            && at.elapsed() < Duration::from_millis(300)
        {
            return;
        }
        self.last_tray_command = Some((command, Instant::now()));
        match command {
            TrayCommand::Show => self.request_show_ui(),
            TrayCommand::Quit => self.request_quit(),
            TrayCommand::Stop => {
                if self.recording {
                    self.stop_recording();
                } else {
                    self.log = "Запись не идёт.".to_string();
                }
            }
            TrayCommand::FullScreen
            | TrayCommand::Area
            | TrayCommand::Program
            | TrayCommand::AudioOnly => {
                if self.recording || self.stopping {
                    self.log = "Сначала дождитесь остановки текущей записи.".to_string();
                    return;
                }
                if self.preflight_rx.is_some() {
                    self.log = "Дождитесь окончания проверки звука.".to_string();
                    return;
                }
                match command {
                    TrayCommand::FullScreen => self.begin_preflight(PendingStart::FullScreen),
                    TrayCommand::Area => self.start_area_selection(),
                    TrayCommand::Program => {
                        if is_wayland() {
                            self.begin_preflight(PendingStart::ProgramWayland);
                        } else {
                            self.request_ui_action(UiAction::Program);
                        }
                    }
                    TrayCommand::AudioOnly => self.begin_preflight(PendingStart::AudioOnly),
                    _ => {}
                }
            }
        }
    }

    /// Показывает окно: фокусирует открытое или запускает новое.
    fn request_show_ui(&mut self) {
        if self.ui_clients > 0 {
            self.show_requested = true;
            return;
        }
        if self
            .ui_launching_until
            .is_some_and(|until| until > Instant::now())
        {
            return;
        }
        self.ui_launching_until = Some(Instant::now() + Duration::from_secs(3));
        self.show_requested = true;
        tracing::info!("запуск окна из трея");
        if let Err(error) = self.spawn_ui() {
            tracing::error!(%error, "не удалось открыть окно из трея");
            self.log = format!("Не удалось открыть окно: {error}");
            self.show_requested = false;
            self.ui_launching_until = None;
        }
    }

    /// Просит окно выполнить действие (например, выбрать программу).
    fn request_ui_action(&mut self, action: UiAction) {
        self.ui_action = Some(action);
        if self.ui_clients > 0 {
            self.show_requested = true;
        } else {
            self.request_show_ui();
        }
    }

    /// Запускает отдельный процесс окна.
    fn spawn_ui(&self) -> Result<(), String> {
        let exe = ui_executable()?;
        let mut child = Command::new(&exe)
            .arg("--ui")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("запуск окна {}: {error}", exe.display()))?;
        tracing::info!(path = %exe.display(), "процесс окна запущен");
        // Окно — отдельный процесс; обязательно пожинаем его, иначе
        // после закрытия остаётся зомби.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// Останавливает запись и завершает сервис.
    fn request_quit(&mut self) {
        if self.stopping {
            self.quit_after_stop = true;
            return;
        }
        if self.recording {
            self.quit_after_stop = true;
            self.stop_recording();
            return;
        }
        self.quit_requested = true;
    }

    /// Обновляет статус в меню трея при изменении записи.
    fn sync_tray_state(&mut self) {
        if self.tray_recording_state == Some(self.recording) {
            return;
        }
        self.tray_recording_state = Some(self.recording);
        if let Some(item) = &self.tray_status_item {
            let text = if self.recording {
                format!(
                    "Статус: идёт запись · {}",
                    self.recording_mode.unwrap_or("запись")
                )
            } else if self.stopping {
                "Статус: остановка".to_string()
            } else {
                "Статус: готово".to_string()
            };
            item.set_text(text);
        }
        if let Some(item) = &self.tray_stop_item {
            item.set_enabled(self.recording);
        }
    }

    /// Активирует существующую задачу из очереди или создаёт новую.
    fn activate_job(
        &mut self,
        queued: Option<JobRecord>,
        session_dir: &Path,
        kind: &str,
    ) -> Option<JobRecord> {
        let Some(mut job) = queued else {
            return self.begin_job(session_dir, kind);
        };
        job.start();
        if let Err(error) = self.history.upsert_job(&job) {
            self.log = format!("История задач: {error}");
        }
        Some(job)
    }

    /// Создаёт задачу обработки и сохраняет её как выполняющуюся.
    fn begin_job(&mut self, session_dir: &Path, kind: &str) -> Option<JobRecord> {
        let session_id = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session")
            .to_string();
        let mut job = JobRecord::new(session_id, kind);
        job.start();
        if let Err(error) = self.history.upsert_job(&job) {
            self.log = format!("История задач: {error}");
        }
        Some(job)
    }

    /// Завершает задачу и сохраняет её статус.
    fn finish_job(&mut self, job: Option<JobRecord>, result: Result<(), String>) {
        let Some(mut job) = job else {
            return;
        };
        match result {
            Ok(()) => job.succeed(),
            Err(error) => job.fail(error),
        }
        if let Err(error) = self.history.upsert_job(&job) {
            self.log = format!("История задач: {error}");
        }
    }

    /// Обновляет запись в истории по манифесту сессии.
    fn sync_history(&mut self, session_dir: &Path) {
        let Ok(manifest) = load_manifest(session_dir) else {
            return;
        };
        let record = SessionRecord::from_manifest(&manifest, session_dir);
        if let Err(error) = self.history.upsert(&record) {
            self.log = format!("История: {error}");
            return;
        }
        self.history_records = self.history.list(100).unwrap_or_default();
    }

    /// Создаёт манифест и журнал сессии перед стартом захвата.
    fn begin_manifest(
        &mut self,
        session_dir: &Path,
        mode: &str,
        backend: &str,
        region: Option<Region>,
    ) {
        if let Err(error) = std::fs::create_dir_all(session_dir) {
            self.log = format!("Не удалось создать каталог сессии: {error}");
            return;
        }
        let session_id = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session")
            .to_string();
        let mut manifest = SessionManifest::new(
            session_id,
            mode,
            backend,
            self.settings.capture.fps,
            region.map(Self::rect_from_region),
        );
        for warning in self.preflight_warnings.drain(..) {
            manifest.warn(warning);
        }
        manifest.audio_offset_ms = self.audio_offset_ms;
        if let Err(error) = save_manifest(session_dir, &manifest) {
            self.log = format!("Не удалось сохранить манифест: {error}");
        }
        append_session_log(
            session_dir,
            &format!(
                "старт: режим={mode}, backend={backend}, fps={}",
                self.settings.capture.fps
            ),
        );
        self.manifest = Some(manifest);
        self.sync_history(session_dir);
    }

    /// Преобразует область в прямоугольник манифеста.
    fn rect_from_region(region: Region) -> Rect {
        Rect {
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
        }
    }

    /// Добавляет дорожку в текущий манифест.
    fn add_manifest_track(
        &mut self,
        kind: TrackKind,
        path: &str,
        device: Option<String>,
        sample_rate: Option<u32>,
        channels: Option<u16>,
        start_offset_ms: i64,
    ) {
        let Some(manifest) = self.manifest.as_mut() else {
            return;
        };
        manifest.add_track(TrackManifest {
            kind,
            path: path.to_string(),
            device,
            sample_rate,
            channels,
            duration_ms: None,
            start_offset_ms,
            status: TrackStatus::Recording,
            stats: TrackStats::default(),
        });
    }

    /// Добавляет предупреждение в текущий манифест.
    fn warn_manifest(&mut self, warning: impl Into<String>) {
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.warn(warning);
        }
    }

    /// Помечает текущую сессию ошибкой и сохраняет манифест.
    fn fail_manifest(&mut self, reason: &str) {
        let Some(mut manifest) = self.manifest.take() else {
            return;
        };
        manifest.warn(reason);
        manifest.finish(ManifestState::Failed);
        if let Some(dir) = self.session_dir.clone() {
            let _ = save_manifest(&dir, &manifest);
            append_session_log(&dir, &format!("ошибка: {reason}"));
            self.sync_history(&dir);
        }
    }

    /// Помечает текущую сессию отменённой и сохраняет манифест.
    fn cancel_manifest(&mut self, reason: &str) {
        let Some(mut manifest) = self.manifest.take() else {
            return;
        };
        manifest.warn(reason);
        manifest.finish(ManifestState::Cancelled);
        if let Some(dir) = self.session_dir.clone() {
            let _ = save_manifest(&dir, &manifest);
            append_session_log(&dir, reason);
            self.sync_history(&dir);
        }
    }

    /// Обновляет смещение аудио в манифесте.
    fn set_audio_offset(&mut self, offset_ms: i64) {
        self.audio_offset_ms = offset_ms;
        let Some(manifest) = self.manifest.as_mut() else {
            return;
        };
        manifest.audio_offset_ms = offset_ms;
        for track in &mut manifest.tracks {
            if matches!(track.kind, TrackKind::System | TrackKind::Mic) {
                track.start_offset_ms = offset_ms;
            }
        }
    }

    /// Считает статистику дорожек, завершает манифест и сохраняет его.
    fn finalize_manifest(&mut self, audio_only: bool) {
        let Some(dir) = self.session_dir.clone() else {
            return;
        };
        let Some(mut manifest) = self.manifest.take() else {
            return;
        };
        manifest.audio_offset_ms = self.audio_offset_ms;
        if !audio_only {
            let video = dir.join("screen.mkv");
            let ready = std::fs::metadata(&video).is_ok_and(|meta| meta.len() > 0);
            manifest.add_track(TrackManifest {
                kind: TrackKind::Screen,
                path: "screen.mkv".to_string(),
                device: None,
                sample_rate: None,
                channels: None,
                duration_ms: None,
                start_offset_ms: 0,
                status: if ready {
                    TrackStatus::Ready
                } else {
                    TrackStatus::Failed
                },
                stats: TrackStats::default(),
            });
            if !ready {
                manifest.warn("Видеофайл не создан или пуст.");
            }
        }

        let mut warnings = Vec::new();
        let mut silent = Vec::new();
        let mut any_ready = false;
        for track in &mut manifest.tracks {
            if !matches!(track.kind, TrackKind::System | TrackKind::Mic) {
                continue;
            }
            let path = dir.join(&track.path);
            match analyze_track(&path) {
                Ok(analysis) => {
                    track.duration_ms = Some(analysis.duration_ms);
                    track.stats = analysis.stats;
                    let is_silent =
                        analysis.stats.silence_ratio >= 0.98 && analysis.stats.rms_dbfs < -60.0;
                    track.status = if is_silent {
                        TrackStatus::Silent
                    } else {
                        TrackStatus::Ready
                    };
                    any_ready = true;
                    if is_silent {
                        silent.push(match track.kind {
                            TrackKind::System => "системный звук",
                            TrackKind::Mic => "микрофон",
                            _ => "аудио",
                        });
                    }
                }
                Err(error) => {
                    track.status = TrackStatus::Failed;
                    warnings.push(format!("{}: {error}", track.path));
                }
            }
        }
        for warning in warnings {
            manifest.warn(warning);
        }
        if manifest
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Screen && track.status == TrackStatus::Ready)
        {
            any_ready = true;
        }
        match silent.as_slice() {
            [one] => manifest.warn(format!(
                "{one} записан тихо; вторая дорожка продолжает работать."
            )),
            [_, _, ..] => {
                manifest.warn("Обе аудиодорожки тихие: проверьте устройства и уровень сигнала.")
            }
            [] => {}
        }
        manifest.finish(if any_ready {
            ManifestState::Recorded
        } else {
            ManifestState::Failed
        });
        if let Err(error) = save_manifest(&dir, &manifest) {
            self.log = format!("Не удалось сохранить манифест: {error}");
        }
        append_session_log(
            &dir,
            &format!(
                "стоп: state={:?}, дорожек={}, предупреждений={}",
                manifest.state,
                manifest.tracks.len(),
                manifest.warnings.len()
            ),
        );
        self.sync_history(&dir);
    }

    /// Открывает нативный диалог выбора каталога.
    fn pick_folder(&mut self, target: FolderTarget) {
        if self.folder_rx.is_some() {
            self.log = "Диалог выбора каталога уже открыт.".to_string();
            return;
        }
        let (title, current) = match target {
            FolderTarget::Models => ("Папка моделей", &self.settings.models_dir),
            FolderTarget::Output => ("Папка результатов", &self.settings.output_dir),
        };
        let start = if current.trim().is_empty() {
            self.paths.data_dir.clone()
        } else {
            PathBuf::from(current)
        };
        let title = title.to_string();
        let (sender, receiver) = mpsc::channel();
        let dialog_title = title.clone();
        std::thread::spawn(move || {
            // Диалог нельзя вызывать в UI-потоке: на Wayland портал блокирует
            // обработку событий приложения.
            let result = rfd::FileDialog::new()
                .set_title(dialog_title)
                .set_directory(start)
                .pick_folder();
            let _ = sender.send(result);
        });
        self.folder_rx = Some(receiver);
        self.folder_target = Some(target);
        self.log = format!("{title}: ожидание выбора…");
    }

    /// Забирает результат фонового выбора каталога.
    fn poll_folder_picker(&mut self) {
        let Some(receiver) = &self.folder_rx else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Some(None),
        };
        self.folder_rx = None;
        let Some(target) = self.folder_target.take() else {
            return;
        };
        let Some(Some(path)) = result else {
            self.pending_model_download = None;
            self.log = "Выбор каталога отменён.".to_string();
            return;
        };
        let value = path.to_string_lossy().to_string();
        let title = match target {
            FolderTarget::Models => "Папка моделей",
            FolderTarget::Output => "Папка результатов",
        };
        match target {
            FolderTarget::Models => {
                self.settings.models_dir = value.clone();
                let root = models_root_for(&self.paths, &value);
                self.model_manager = ModelManager::new(&root);
                self.log = format!("{title}: {value} (модели: {})", root.display());
            }
            FolderTarget::Output => self.settings.output_dir = value.clone(),
        }
        self.settings_dirty = true;
        if let Some(id) = self.pending_model_download.take() {
            self.start_download(id);
        }
    }

    /// Сохраняет настройки, если пользователь что-то изменил.
    fn persist_settings(&mut self) {
        if !self.settings_dirty {
            return;
        }
        self.settings.export.format = self.output_format.clone();
        self.settings.export.video_quality = self.video_quality.clone();
        self.settings.export.audio_mode = self.audio_track_mode.clone();
        let store = SettingsStore::new(self.paths.settings_path());
        match store.save(&self.settings) {
            Ok(()) => self.log = "Настройки сохранены автоматически.".to_string(),
            Err(error) => self.status = format!("Ошибка сохранения: {error}"),
        }
        self.model_manager =
            ModelManager::new(models_root_for(&self.paths, &self.settings.models_dir));
        self.settings_dirty = false;
        if self.client.is_some() {
            let request = Request::ReloadSettings {
                settings: Box::new(self.settings.clone()),
            };
            if let Err(error) = self.send_request(&request) {
                self.log = format!("Сервис не принял настройки: {error}");
            }
        }
    }

    /// Удаляет скачанную модель.
    fn delete_model(&mut self, id: &str) {
        match self.model_manager.delete(id) {
            Ok(()) => {
                self.status = if self.model_manager.is_downloaded("whisper") {
                    "Готово к записи".to_string()
                } else {
                    "Модели не скачаны".to_string()
                };
                self.log = format!("Модель удалена: {id}");
                self.notify_service_settings();
            }
            Err(error) => {
                self.status = format!("Ошибка удаления модели: {error}");
                self.log = format!("Не удалось удалить модель {id}: {error}");
            }
        }
    }

    /// Запускает скачивание модели в фоне.
    fn start_download(&mut self, id: &'static str) {
        if self.download_rx.is_some() {
            self.log = "Другая модель уже скачивается.".to_string();
            return;
        }
        if self.settings.models_dir.trim().is_empty() {
            self.pending_model_download = Some(id);
            self.log = "Выберите папку моделей — после выбора скачивание начнётся.".to_string();
            self.pick_folder(FolderTarget::Models);
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let manager = self.model_manager.clone();
        let model_id = id.to_string();
        std::thread::spawn(move || {
            let mut last_percent = -1.0_f32;
            let result = manager.download(&model_id, |progress| {
                if progress.percent - last_percent >= 0.5 || progress.percent >= 100.0 {
                    last_percent = progress.percent;
                    let _ = sender.send(DownloadEvent::Progress(progress));
                }
            });
            let _ = sender.send(DownloadEvent::Finished(
                result.map_err(|error| error.to_string()),
            ));
        });
        self.download_rx = Some(receiver);
        self.download_model = Some(id);
        self.download_progress = 0.0;
        self.status = format!("Скачивание модели: {id}");
        self.log = format!("Начато скачивание модели {id}.");
    }

    /// Обрабатывает события фоновой загрузки.
    fn poll_download(&mut self) {
        let events: Vec<DownloadEvent> = match &self.download_rx {
            Some(receiver) => std::iter::from_fn(|| receiver.try_recv().ok()).collect(),
            None => return,
        };
        for event in events {
            match event {
                DownloadEvent::Progress(progress) => {
                    self.download_progress = progress.percent;
                    self.status = format!(
                        "Скачивание: {} · {:.1}%",
                        progress.model_id, progress.percent
                    );
                }
                DownloadEvent::Finished(Ok(())) => {
                    self.status = if self.model_manager.is_downloaded("whisper") {
                        "Готово к записи".to_string()
                    } else {
                        "Модель скачана".to_string()
                    };
                    self.log = "Загрузка модели завершена.".to_string();
                    self.notify_service_settings();
                    self.download_rx = None;
                    self.download_model = None;
                    self.download_progress = 0.0;
                }
                DownloadEvent::Finished(Err(error)) => {
                    self.status = format!("Ошибка загрузки: {error}");
                    self.log = format!("Не удалось скачать модель: {error}");
                    self.download_rx = None;
                    self.download_model = None;
                    self.download_progress = 0.0;
                }
            }
        }
    }

    /// Передаёт сервису актуальные настройки после смены моделей.
    fn notify_service_settings(&mut self) {
        if self.client.is_none() {
            return;
        }
        let request = Request::ReloadSettings {
            settings: Box::new(self.settings.clone()),
        };
        let _ = self.send_request(&request);
    }

    /// Запускает фоновую проверку звука перед записью.
    fn begin_preflight(&mut self, start: PendingStart) {
        if self.preflight_rx.is_some() {
            self.log = "Проверка звука уже идёт.".to_string();
            return;
        }
        if self.recording {
            return;
        }
        let mic_setting = self.settings.capture.mic_device.clone();
        let system_setting = self.settings.capture.system_device.clone();
        let mic_target = (!mic_setting.trim().is_empty()).then(|| mic_setting.trim().to_string());
        let system_target =
            (!system_setting.trim().is_empty()).then(|| system_setting.trim().to_string());
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mic_handle = std::thread::spawn(move || {
                AudioRecorder::probe(mic_target.as_deref(), AudioDeviceKind::Mic)
            });
            let system_handle = std::thread::spawn(move || {
                AudioRecorder::probe(system_target.as_deref(), AudioDeviceKind::System)
            });
            let mic = mic_handle
                .join()
                .unwrap_or_else(|_| {
                    Err(screenrecorder_adapters::AudioError::Probe(
                        "поток проверки микрофона завершился".to_string(),
                    ))
                })
                .map_err(|error| error.to_string());
            let system = system_handle
                .join()
                .unwrap_or_else(|_| {
                    Err(screenrecorder_adapters::AudioError::Probe(
                        "поток проверки системного звука завершился".to_string(),
                    ))
                })
                .map_err(|error| error.to_string());
            let _ = sender.send(PreflightResult { mic, system });
        });
        self.preflight_rx = Some(receiver);
        self.pending_start = Some(start);
        self.preflight_warnings.clear();
        self.status = "Проверка звука перед записью…".to_string();
        self.log = "Проверяю микрофон и системный звук (около 1–2 секунд).".to_string();
    }

    /// Забирает результат preflight-проверки и запускает запись.
    fn poll_preflight(&mut self) {
        let result = match &self.preflight_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.preflight_rx = None;
                    self.pending_start = None;
                    self.status = "Проверка звука не удалась".to_string();
                    self.log = "Поток проверки звука завершился без результата.".to_string();
                    return;
                }
            },
            None => return,
        };
        self.preflight_rx = None;
        let Some(start) = self.pending_start.take() else {
            return;
        };
        let mut warnings = Vec::new();
        let mut audibly_ok = false;
        match &result.mic {
            Ok(probe) if probe.silent => {
                warnings.push(format!("микрофон: тишина (RMS {:.1} dBFS)", probe.rms_dbfs))
            }
            Ok(probe) => {
                audibly_ok = true;
                if probe.rms_dbfs > -20.0 {
                    warnings.push(format!(
                        "микрофон: очень громкий сигнал ({:.1} dBFS)",
                        probe.rms_dbfs
                    ));
                }
            }
            Err(error) => warnings.push(format!("микрофон: {error}")),
        }
        match &result.system {
            Ok(probe) if probe.silent => warnings.push(format!(
                "системный звук: тишина (RMS {:.1} dBFS)",
                probe.rms_dbfs
            )),
            Ok(_) => audibly_ok = true,
            Err(error) => warnings.push(format!("системный звук: {error}")),
        }
        tracing::info!(
            mic = ?result.mic,
            system = ?result.system,
            warnings = ?warnings,
            "preflight звука"
        );
        if matches!(start, PendingStart::AudioOnly) && !audibly_ok {
            tracing::warn!("аудиозапись не начата: нет сигнала");
            self.preflight_warnings.clear();
            self.status = "Звук не обнаружен".to_string();
            self.log = format!("Запись не начата: {}", warnings.join("; "));
            return;
        }
        self.preflight_warnings = warnings.clone();
        if warnings.is_empty() {
            self.log = "Проверка звука пройдена.".to_string();
        } else {
            self.log = format!("Предупреждение перед записью: {}", warnings.join("; "));
        }
        self.start_pending(start);
    }

    /// Запускает выбранный режим после preflight-проверки.
    fn start_pending(&mut self, start: PendingStart) {
        match start {
            PendingStart::AudioOnly => self.start_audio_only(),
            PendingStart::FullScreen => self.start_full_screen(),
            PendingStart::Region(region, mode) => self.start_region_recording(region, mode),
            PendingStart::ProgramWayland => self.start_program_wayland(),
        }
    }

    /// Запускает запись только звука.
    fn start_audio_only(&mut self) {
        tracing::info!("старт аудиозаписи");
        let session_id = SessionId::new();
        let session_dir = self.paths.session_dir(session_id.as_str());
        if let Err(error) = std::fs::create_dir_all(&session_dir) {
            self.status = format!("Ошибка каталога сессии: {error}");
            return;
        }
        self.audio_only = true;
        self.using_wayland = false;
        self.wayland_pending = false;
        self.session_dir = Some(session_dir.clone());
        self.begin_manifest(&session_dir, "Только аудио", "pw-record", None);
        self.start_audio(&session_dir);
        if self.audio_recorder.is_running() {
            self.recording = true;
            self.recording_started_at = Some(Instant::now());
            self.recording_mode = Some("Только аудио");
            self.status = "Идёт аудиозапись".to_string();
            notify("Screen Recorder", "Аудиозапись началась");
        } else {
            self.fail_manifest("не удалось начать запись звука");
            self.audio_only = false;
            self.recording_mode = None;
            self.status = "Не удалось начать запись звука".to_string();
        }
    }

    /// Запускает запись всего экрана.
    fn start_full_screen(&mut self) {
        tracing::info!("старт записи всего экрана");
        self.audio_only = false;
        self.recording_mode = None;
        let session_id = SessionId::new();
        let session_dir = self.paths.session_dir(session_id.as_str());
        let output = session_dir.join("screen.mkv");
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        self.begin_manifest(
            &session_dir,
            "Весь экран",
            if wayland {
                "portal-pipewire-gst"
            } else {
                video_backend_label()
            },
            None,
        );
        self.session_dir = Some(session_dir.clone());
        if wayland {
            match self
                .wayland_recorder
                .start(&output, self.settings.capture.fps)
            {
                Ok(()) => {
                    self.using_wayland = true;
                    self.wayland_pending = true;
                    self.wayland_started_at = Some(Instant::now());
                    self.recording = true;
                    self.recording_mode = Some("Весь экран");
                    self.audio_offset_ms = 0;
                    self.status = "Wayland: ожидание доступа к экрану…".to_string();
                    self.log = "Подтвердите доступ к экрану в системном диалоге.".to_string();
                }
                Err(error) => {
                    self.fail_manifest(&error);
                    self.status = format!("Ошибка Wayland-записи: {error}");
                    self.log = format!("Не удалось запустить портал: {error}");
                }
            }
            return;
        }
        let video_started = Instant::now();
        match self.recorder.start(&output, self.settings.capture.fps) {
            Ok(()) => {
                self.using_wayland = false;
                self.wayland_pending = false;
                self.start_audio(&session_dir);
                let offset = video_started.elapsed().as_millis() as i64;
                self.set_audio_offset(offset);
                self.recording = true;
                self.recording_started_at = Some(Instant::now());
                self.recording_mode = Some("Весь экран");
                self.status = "Идёт запись: Весь экран".to_string();
                notify("Screen Recorder", "Запись экрана началась");
            }
            Err(error) => {
                let reason = error.to_string();
                self.fail_manifest(&reason);
                self.status = format!("Ошибка записи: {error}");
                self.log = format!("Не удалось начать запись: {error}");
            }
        }
    }

    /// Запускает выбор окна через портал Wayland.
    fn start_program_wayland(&mut self) {
        let session_id = SessionId::new();
        let session_dir = self.paths.session_dir(session_id.as_str());
        let output = session_dir.join("screen.mkv");
        self.audio_only = false;
        self.begin_manifest(&session_dir, "Программа", "portal-pipewire-gst", None);
        self.session_dir = Some(session_dir.clone());
        match self.wayland_recorder.start_with_source(
            &output,
            self.settings.capture.fps,
            PortalSource::Window,
            None,
        ) {
            Ok(()) => {
                self.using_wayland = true;
                self.wayland_pending = true;
                self.wayland_started_at = Some(Instant::now());
                self.recording = true;
                self.recording_mode = Some("Программа");
                self.audio_offset_ms = 0;
                self.status = "Wayland: выбор окна…".to_string();
            }
            Err(error) => {
                self.fail_manifest(&error);
                self.status = format!("Ошибка выбора окна: {error}");
            }
        }
    }

    /// Запускает аудиодорожки в каталоге сессии.
    fn start_audio(&mut self, session_dir: &Path) -> Option<screenrecorder_adapters::AudioStart> {
        let mic_setting = self.settings.capture.mic_device.clone();
        let system_setting = self.settings.capture.system_device.clone();
        let mic_device = (!mic_setting.trim().is_empty()).then(|| mic_setting.trim().to_string());
        let system_device =
            (!system_setting.trim().is_empty()).then(|| system_setting.trim().to_string());
        match self.audio_recorder.start(
            session_dir,
            system_device.as_deref(),
            mic_device.as_deref(),
        ) {
            Ok(audio) => {
                tracing::info!(
                    system = ?system_device,
                    mic = ?mic_device,
                    system_started = audio.system,
                    mic_started = audio.mic,
                    warnings = ?audio.warnings,
                    "аудиодорожки"
                );
                if audio.system {
                    self.add_manifest_track(
                        TrackKind::System,
                        "system.wav",
                        system_device,
                        Some(48_000),
                        Some(2),
                        self.audio_offset_ms,
                    );
                }
                if audio.mic {
                    // WASAPI пишет микрофон в stereo, pw-record — в mono.
                    let channels = if cfg!(windows) { 2 } else { 1 };
                    self.add_manifest_track(
                        TrackKind::Mic,
                        "mic.wav",
                        mic_device,
                        Some(48_000),
                        Some(channels),
                        self.audio_offset_ms,
                    );
                }
                for warning in &audio.warnings {
                    self.warn_manifest(warning.clone());
                }
                if !audio.warnings.is_empty() {
                    self.log = format!(
                        "Видео пишется, но звук неполный: {}",
                        audio.warnings.join("; ")
                    );
                } else if audio.system && audio.mic {
                    self.log = format!(
                        "Запись: видео + system.wav + mic.wav ({})",
                        session_dir.display()
                    );
                } else {
                    self.log = "Запись: доступна только часть аудиодорожек".to_string();
                }
                Some(audio)
            }
            Err(error) => {
                self.warn_manifest(format!("аудио: {error}"));
                self.log = format!("Видео пишется без звука: {error}");
                None
            }
        }
    }

    /// Запускает выбор области через полноэкранный скриншот.
    fn start_area_selection(&mut self) {
        if self.area_rx.is_some() {
            self.log = "Выбор области уже открыт.".to_string();
            return;
        }
        let Ok(exe) = std::env::current_exe() else {
            self.status = "Не удалось определить путь к приложению".to_string();
            return;
        };
        let stamp = Local::now().format("%s%f");
        let image = std::env::temp_dir().join(format!("screenrecorder-area-{stamp}.png"));
        let out = std::env::temp_dir().join(format!("screenrecorder-area-{stamp}.txt"));
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                // Снимок делается сразу после нажатия «Область экрана»:
                // на нём пользователь и выделяет нужный прямоугольник.
                take_screenshot(&image)?;
                let status = Command::new(exe)
                    .arg("--select-area")
                    .arg(&image)
                    .arg(&out)
                    .status()
                    .map_err(|error| format!("окно выбора: {error}"))?;
                if !status.success() {
                    return Err(format!("выбор области завершился: {status}"));
                }
                let text = match std::fs::read_to_string(&out) {
                    Ok(text) => text,
                    Err(_) => return Err("область не выбрана".to_string()),
                };
                parse_region(&text)
            })();
            let _ = std::fs::remove_file(&image);
            let _ = std::fs::remove_file(image.with_extension("gst.log"));
            let _ = std::fs::remove_file(&out);
            let _ = sender.send(result);
        });
        self.area_rx = Some(receiver);
        self.status = "Снимок экрана для выбора области…".to_string();
        self.log = "Снимок экрана делается сейчас; затем выделите область.".to_string();
    }

    /// Забирает результат выбора области.
    fn poll_area(&mut self) {
        let result = match &self.area_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    Err("поток выбора области завершился".to_string())
                }
            },
            None => return,
        };
        self.area_rx = None;
        match result {
            Ok(region) if self.client.is_some() => {
                self.send_start(RecordMode::Area, Some(region));
            }
            Ok(region) => self.begin_preflight(PendingStart::Region(region, "Область экрана")),
            Err(error) if error == "область не выбрана" => {
                self.status = "Выбор области отменён".to_string();
                self.log = "Область не выбрана.".to_string();
            }
            Err(error) if error == PORTAL_CANCELLED => {
                self.status = "Снимок экрана отменён".to_string();
                self.log = "Диалог доступа к экрану закрыт без подтверждения.".to_string();
            }
            Err(error) => {
                self.status = format!("Область не выбрана: {error}");
                self.log = format!("Выбор области: {error}");
            }
        }
    }

    /// Запускает запись выбранной области.
    fn start_region_recording(&mut self, region: Region, mode: &'static str) {
        tracing::info!(mode, ?region, "старт записи области");
        let session_id = SessionId::new();
        let session_dir = self.paths.session_dir(session_id.as_str());
        let output = session_dir.join("screen.mkv");
        self.audio_only = false;
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        self.begin_manifest(
            &session_dir,
            mode,
            if wayland {
                "portal-pipewire-gst"
            } else {
                video_backend_label()
            },
            Some(region),
        );
        self.session_dir = Some(session_dir.clone());
        if wayland {
            match self.wayland_recorder.start_with_source(
                &output,
                self.settings.capture.fps,
                PortalSource::Monitor,
                Some(region),
            ) {
                Ok(()) => {
                    self.using_wayland = true;
                    self.wayland_pending = true;
                    self.wayland_started_at = Some(Instant::now());
                    self.recording = true;
                    self.recording_mode = Some(mode);
                    self.audio_offset_ms = 0;
                    self.session_dir = Some(session_dir);
                    self.status = "Wayland: ожидание доступа к области…".to_string();
                }
                Err(error) => {
                    self.fail_manifest(&error);
                    self.status = format!("Ошибка области: {error}");
                    self.log = error;
                }
            }
            return;
        }
        let started = Instant::now();
        match self
            .recorder
            .start_region(&output, self.settings.capture.fps, Some(region))
        {
            Ok(()) => {
                self.using_wayland = false;
                self.wayland_pending = false;
                self.start_audio(&session_dir);
                let offset = started.elapsed().as_millis() as i64;
                self.set_audio_offset(offset);
                self.recording = true;
                self.recording_started_at = Some(Instant::now());
                self.recording_mode = Some(mode);
                self.session_dir = Some(session_dir);
                self.status = format!("Идёт запись области {}×{}", region.width, region.height);
                notify("Screen Recorder", "Запись области началась");
            }
            Err(error) => {
                let reason = error.to_string();
                self.fail_manifest(&reason);
                self.status = format!("Ошибка области: {error}");
            }
        }
    }

    /// Запускает выбор программы/окна.
    fn start_program_selection(&mut self) {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if self.client.is_some() {
                self.send_start(RecordMode::Program, None);
            } else {
                self.begin_preflight(PendingStart::ProgramWayland);
            }
            return;
        }

        match list_windows() {
            Ok(windows) if !windows.is_empty() => {
                self.window_list = windows;
                self.show_windows = true;
                self.status = "Выберите программу".to_string();
            }
            Ok(_) => {
                self.status = "Не найдено видимых окон".to_string();
            }
            Err(error) => {
                self.status = format!("Список окон: {error}");
            }
        }
    }

    /// Запускает или останавливает запись экрана.
    fn toggle_recording(&mut self, mode: &'static str) {
        if self.stopping {
            self.log = "Дождитесь завершения остановки записи.".to_string();
            return;
        }
        if self.client.is_some() {
            if self.recording {
                self.send_stop();
                return;
            }
            match mode {
                "Только аудио" => self.send_start(RecordMode::AudioOnly, None),
                "Область экрана" => self.start_area_selection(),
                "Программа" => self.start_program_selection(),
                _ => self.send_start(RecordMode::FullScreen, None),
            }
            return;
        }
        if self.recording {
            self.stop_recording();
            return;
        }
        if self.preflight_rx.is_some() {
            self.log = "Дождитесь окончания проверки звука.".to_string();
            return;
        }
        match mode {
            "Только аудио" => self.begin_preflight(PendingStart::AudioOnly),
            "Область экрана" => self.start_area_selection(),
            "Программа" => self.start_program_selection(),
            _ => self.begin_preflight(PendingStart::FullScreen),
        }
    }

    /// Отправляет запрос сервису.
    fn send_request(&mut self, request: &Request) -> Result<Response, String> {
        self.client
            .as_mut()
            .ok_or_else(|| "фоновый сервис не подключён".to_string())?
            .request(request)
    }

    /// Просит сервис остановить запись.
    fn send_stop(&mut self) {
        tracing::info!("окно просит остановить запись");
        match self.send_request(&Request::Stop) {
            Ok(Response::Ok) => self.status = "Остановка записи…".to_string(),
            Ok(Response::Error { message }) | Err(message) => self.log = message,
            Ok(_) => {}
        }
    }

    /// Просит сервис начать запись.
    fn send_start(&mut self, mode: RecordMode, region: Option<Region>) {
        tracing::info!(
            ?mode,
            has_region = region.is_some(),
            "окно просит начать запись"
        );
        let request = Request::Start(StartRequest {
            mode,
            region: region.map(|region| RegionDto {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            }),
        });
        match self.send_request(&request) {
            Ok(Response::Ok) => self.status = "Запуск записи…".to_string(),
            Ok(Response::Error { message }) | Err(message) => self.log = message,
            Ok(_) => {}
        }
    }

    /// Останавливает текущую запись (локально или через сервис).
    fn stop_recording(&mut self) {
        if self.client.is_some() {
            self.send_stop();
        } else {
            self.stop_recording_local();
        }
    }

    /// Запускает остановку записи в фоне, чтобы окно и горячие клавиши
    /// не зависали на склейке сегментов и финализации GStreamer.
    fn stop_recording_local(&mut self) {
        if !self.recording || self.stopping {
            return;
        }
        tracing::info!(
            mode = ?self.recording_mode,
            mic_dbfs = self.mic_dbfs,
            system_dbfs = self.system_dbfs,
            "остановка записи"
        );
        let was_wayland_pending = self.wayland_pending;
        let was_wayland_active = self.using_wayland && self.wayland_recorder.is_running();
        let using_wayland = self.using_wayland;
        let audio_only = self.audio_only;
        self.recording = false;
        self.stopping = true;
        self.recording_started_at = None;
        self.wayland_pending = false;
        self.wayland_started_at = None;
        self.recording_mode = None;
        self.audio_only = false;
        self.status = "Остановка записи…".to_string();
        let mut audio_recorder = std::mem::replace(&mut self.audio_recorder, AudioRecorder::new());
        let mut recorder = std::mem::replace(&mut self.recorder, VideoRecorder::new());
        let mut wayland_recorder =
            std::mem::replace(&mut self.wayland_recorder, WaylandPortalRecorder::new());
        let (sender, receiver) = mpsc::channel();
        self.stop_rx = Some(receiver);
        std::thread::spawn(move || {
            let audio_started = Instant::now();
            let audio_result = audio_recorder.stop().map_err(|error| error.to_string());
            let audio_ms = audio_started.elapsed().as_millis();
            let video_started = Instant::now();
            let video_result: Result<(), String> = if audio_only {
                Ok(())
            } else if using_wayland {
                if was_wayland_pending && !was_wayland_active {
                    wayland_recorder.request_stop();
                    Ok(())
                } else {
                    wayland_recorder.stop()
                }
            } else {
                recorder.stop().map_err(|error| error.to_string())
            };
            let video_ms = video_started.elapsed().as_millis();
            tracing::info!(
                audio = ?audio_result,
                video = ?video_result,
                audio_ms,
                video_ms,
                "фоновые процессы остановки завершены"
            );
            let _ = sender.send(StopOutcome {
                audio_result,
                video_result,
                audio_only,
                was_wayland_pending,
                was_wayland_active,
            });
        });
    }

    /// Завершает остановку: манифест, уведомление и очередь экспорта.
    fn poll_stop(&mut self) {
        let outcome = match &self.stop_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(outcome) => outcome,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.stop_rx = None;
                    self.stopping = false;
                    self.status = "Остановка прервана".to_string();
                    self.log = "Поток остановки записи завершился.".to_string();
                    return;
                }
            },
            None => return,
        };
        self.stop_rx = None;
        self.using_wayland = false;
        let dir = self
            .session_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "каталог сессии".to_string());
        if outcome.was_wayland_pending && !outcome.was_wayland_active {
            self.cancel_manifest("остановлено до получения доступа к экрану");
            self.status = "Запись остановлена".to_string();
            self.log = format!(
                "Остановлено до подтверждения доступа к экрану; файл записи может отсутствовать: {dir}"
            );
            self.stopping = false;
            self.finish_quit_after_stop();
            return;
        }
        if let Err(error) = &outcome.video_result {
            self.warn_manifest(format!("видео: {error}"));
        }
        if let Err(error) = &outcome.audio_result {
            self.warn_manifest(format!("аудио: {error}"));
        }
        match (&outcome.video_result, &outcome.audio_result) {
            (Ok(()), Ok(())) => {
                self.status = "Запись остановлена".to_string();
                self.log = format!("Запись сохранена: {dir}");
                notify("Screen Recorder", "Запись остановлена");
            }
            (video, audio) => {
                self.status = "Запись остановлена с ошибками".to_string();
                self.log = format!("Остановка: видео={video:?}, звук={audio:?}");
                notify(
                    "Screen Recorder",
                    "Запись остановлена с ошибками; подробности в журнале",
                );
            }
        }
        let finalize_started = Instant::now();
        self.finalize_manifest(outcome.audio_only);
        tracing::info!(
            elapsed_ms = finalize_started.elapsed().as_millis(),
            "манифест финализирован"
        );
        if let Some(session_dir) = self.session_dir.clone() {
            append_session_log(&session_dir, "остановка записи");
            self.enqueue_export(&session_dir);
        }
        self.stopping = false;
        self.finish_quit_after_stop();
    }

    /// Завершает сервис после фоновой остановки записи.
    fn finish_quit_after_stop(&mut self) {
        if self.quit_after_stop {
            self.quit_after_stop = false;
            self.quit_requested = true;
        }
    }

    /// Ставит экспорт сессии в очередь.
    fn enqueue_export(&mut self, session_dir: &Path) {
        if self.exporting {
            return;
        }
        let session_id = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session")
            .to_string();
        if self
            .queued_jobs
            .iter()
            .any(|job| job.session_id == session_id && job.kind == "export")
        {
            return;
        }
        let has_video = session_dir.join("screen.mkv").exists();
        let has_audio =
            session_dir.join("system.wav").exists() || session_dir.join("mic.wav").exists();
        if !has_video && !has_audio {
            self.log = "Экспорт пропущен: нет файлов.".to_string();
            return;
        }
        let job = JobRecord::new(session_id, "export");
        if let Err(error) = self.history.upsert_job(&job) {
            self.log = format!("История задач: {error}");
        }
        self.queued_jobs.push(job);
        self.status = "Экспорт в очереди".to_string();
    }

    /// Ставит распознавание сессии в очередь.
    fn enqueue_transcription(&mut self, session_dir: &Path) {
        if !self.settings.transcription.enabled || self.transcribing {
            return;
        }
        let has_audio =
            session_dir.join("system.wav").exists() || session_dir.join("mic.wav").exists();
        if !has_audio || session_dir.join("transcript.txt").exists() {
            return;
        }
        let session_id = session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session")
            .to_string();
        if self
            .queued_jobs
            .iter()
            .any(|job| job.session_id == session_id && job.kind == "transcribe")
        {
            return;
        }
        let job = JobRecord::new(session_id, "transcribe");
        if let Err(error) = self.history.upsert_job(&job) {
            self.log = format!("История задач: {error}");
        }
        self.queued_jobs.push(job);
    }

    /// Запускает следующую восстановленную задачу из очереди.
    fn poll_job_queue(&mut self) {
        if self.exporting || self.transcribing || self.recording || self.preflight_rx.is_some() {
            return;
        }
        let Some(job) = self.queued_jobs.first().cloned() else {
            return;
        };
        let dir = self.paths.session_dir(&job.session_id);
        if !dir.is_dir() {
            self.queued_jobs.remove(0);
            self.finish_job(Some(job), Err("каталог сессии не найден".to_string()));
            return;
        }
        let kind = job.kind.clone();
        match kind.as_str() {
            "transcribe" => {
                let has_audio = dir.join("mic.wav").exists() || dir.join("system.wav").exists();
                if !self.settings.transcription.enabled {
                    self.queued_jobs.remove(0);
                    self.finish_job(
                        Some(job),
                        Err("транскрипция отключена в настройках".to_string()),
                    );
                    return;
                }
                if !has_audio {
                    self.queued_jobs.remove(0);
                    self.finish_job(
                        Some(job),
                        Err("нет аудиодорожек для транскрипции".to_string()),
                    );
                    return;
                }
                if dir.join("transcript.txt").exists() {
                    self.queued_jobs.remove(0);
                    self.finish_job(Some(job), Ok(()));
                    return;
                }
                self.queued_jobs.remove(0);
                self.start_transcription(&dir, Some(job));
            }
            "export" => {
                let has_video = dir.join("screen.mkv").exists();
                let has_audio = dir.join("mic.wav").exists() || dir.join("system.wav").exists();
                if !has_video && !has_audio {
                    self.queued_jobs.remove(0);
                    self.finish_job(Some(job), Err("нет файлов для экспорта".to_string()));
                    return;
                }
                self.queued_jobs.remove(0);
                self.start_export(&dir, !has_video, Some(job));
            }
            other => {
                self.queued_jobs.remove(0);
                self.finish_job(Some(job), Err(format!("неизвестный тип задачи: {other}")));
            }
        }
    }

    /// Запускает фоновое распознавание речи после экспорта.
    fn start_transcription(&mut self, session_dir: &Path, queued: Option<JobRecord>) {
        if !self.settings.transcription.enabled || self.transcribing {
            return;
        }
        let mic = session_dir.join("mic.wav");
        let system = session_dir.join("system.wav");
        let has_mic = mic.metadata().is_ok_and(|meta| meta.len() > 44);
        let has_system = system.metadata().is_ok_and(|meta| meta.len() > 44);
        if !has_mic && !has_system {
            self.log = "Распознавание пропущено: нет аудиодорожек.".to_string();
            return;
        }
        let models_dir = models_root_for(&self.paths, &self.settings.models_dir);
        let language = self.settings.transcription.language.clone();
        let compute_device = self.settings.transcription.compute_device.clone();
        let diarization_enabled =
            self.settings.diarization.enabled && self.settings.diarization.engine != "none";
        let diarization_engine = self.settings.diarization.engine.clone();
        let min_speakers = self.settings.diarization.min_speakers;
        let max_speakers = self.settings.diarization.max_speakers;
        let asr_passes = if has_mic && has_system { 2 } else { 1 };
        let diarization_phase = diarization_enabled && has_system && diarization_engine != "none";
        let total_phases = asr_passes + u8::from(diarization_phase);
        self.ml_asr_passes = asr_passes;
        self.ml_total_phases = total_phases;
        let dir = session_dir.to_path_buf();
        // Таймкоды WAV не совпадают с итоговым видео: при экспорте аудио
        // задерживается на audio_offset_ms. Сдвигаем субтитры туда же.
        let offset_ms = load_manifest(session_dir)
            .map(|manifest| manifest.audio_offset_ms)
            .unwrap_or(self.audio_offset_ms);
        self.log = "ASR: подготовка аудио и запуск воркера…".to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                let mut warning = None;
                let mut worker = MlWorker::spawn(&models_dir)?;
                let mut transcript = if has_mic && has_system {
                    // Channel-aware: system и mic транскрибируются отдельно,
                    // затем сегменты сливаются по времени.
                    let mut system_transcript = worker.transcribe(
                        &system,
                        Some(&language),
                        true,
                        &compute_device,
                        "asr:system",
                        1,
                    )?;
                    let mut mic_transcript = worker.transcribe(
                        &mic,
                        Some(&language),
                        true,
                        &compute_device,
                        "asr:mic",
                        2,
                    )?;
                    for segment in &mut mic_transcript.segments {
                        segment.speaker = Some("Микрофон".to_string());
                    }
                    if diarization_enabled {
                        let turns = if diarization_engine == "nemo" {
                            NemoWorker::spawn(&models_dir)
                                .and_then(|mut worker| worker.diarize(&system))
                        } else {
                            DiarizeWorker::spawn(&models_dir).and_then(|mut worker| {
                                worker.diarize(&system, min_speakers, max_speakers)
                            })
                        };
                        match turns {
                            Ok(turns) => assign_speakers(&mut system_transcript, &turns),
                            Err(error) => warning = Some(format!("диаризация: {error}")),
                        }
                    }
                    let mut segments = std::mem::take(&mut system_transcript.segments);
                    segments.extend(mic_transcript.segments);
                    segments.sort_by_key(|segment| segment.start_ms);
                    Transcript {
                        language: system_transcript.language,
                        segments,
                    }
                } else {
                    let (audio, label) = if has_mic {
                        (mic, "Микрофон")
                    } else {
                        (system, "Система")
                    };
                    let mut transcript = worker.transcribe(
                        &audio,
                        Some(&language),
                        true,
                        &compute_device,
                        if has_mic { "asr:mic" } else { "asr:system" },
                        1,
                    )?;
                    let mut diarized = false;
                    if !has_mic && diarization_enabled {
                        let turns = if diarization_engine == "nemo" {
                            NemoWorker::spawn(&models_dir)
                                .and_then(|mut worker| worker.diarize(&audio))
                        } else {
                            DiarizeWorker::spawn(&models_dir).and_then(|mut worker| {
                                worker.diarize(&audio, min_speakers, max_speakers)
                            })
                        };
                        match turns {
                            Ok(turns) => {
                                assign_speakers(&mut transcript, &turns);
                                diarized = true;
                            }
                            Err(error) => warning = Some(format!("диаризация: {error}")),
                        }
                    }
                    if !diarized {
                        for segment in &mut transcript.segments {
                            segment.speaker = Some(label.to_string());
                        }
                    }
                    transcript
                };
                shift_transcript(&mut transcript, offset_ms);
                drop(worker);
                let text_path = dir.join("transcript.txt");
                let json_path = dir.join("transcript.json");
                let srt_path = dir.join("transcript.srt");
                let vtt_path = dir.join("transcript.vtt");
                let md_path = dir.join("transcript.md");
                std::fs::write(&text_path, format_transcript(&transcript))
                    .map_err(|error| format!("запись {}: {error}", text_path.display()))?;
                let json = serde_json::to_string_pretty(&transcript)
                    .map_err(|error| format!("JSON транскрипта: {error}"))?;
                std::fs::write(&json_path, json)
                    .map_err(|error| format!("запись {}: {error}", json_path.display()))?;
                std::fs::write(&srt_path, format_srt(&transcript))
                    .map_err(|error| format!("запись {}: {error}", srt_path.display()))?;
                std::fs::write(&vtt_path, format_vtt(&transcript))
                    .map_err(|error| format!("запись {}: {error}", vtt_path.display()))?;
                std::fs::write(&md_path, format_markdown(&transcript))
                    .map_err(|error| format!("запись {}: {error}", md_path.display()))?;
                Ok(TranscriptOutput {
                    text_path,
                    json_path,
                    srt_path,
                    vtt_path,
                    md_path,
                    segments: transcript.segments.len(),
                    language: transcript.language,
                    warning,
                })
            })();
            let _ = sender.send(result);
        });
        self.transcript_rx = Some(receiver);
        self.transcribing = true;
        self.ml_progress = None;
        self.last_progress_poll = Instant::now();
        self.cancel_requested = false;
        let _ = std::fs::remove_file(session_dir.join("progress.json"));
        self.transcription_job = self.activate_job(queued, session_dir, "transcribe");
        tracing::info!(dir = %session_dir.display(), "старт распознавания");
        self.status = "Распознавание речи…".to_string();
    }

    /// Читает прогресс ML-воркера из файла сессии.
    fn poll_ml_progress(&mut self) {
        if !self.transcribing {
            self.ml_progress = None;
            return;
        }
        if self.last_progress_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_progress_poll = Instant::now();
        if let Some(dir) = &self.session_dir
            && let Some(mut progress) = read_progress(&dir.join("progress.json"))
        {
            progress.percent =
                overall_progress(&progress, self.ml_asr_passes, self.ml_total_phases);
            self.ml_progress = Some(progress);
        }
    }

    /// Отменяет распознавание: локально или командой сервису.
    fn cancel_ml(&mut self) {
        if self.client.is_some() {
            match self.send_request(&Request::CancelMl) {
                Ok(Response::Ok) => self.status = "Отмена распознавания…".to_string(),
                Ok(Response::Error { message }) | Err(message) => self.log = message,
                Ok(_) => {}
            }
            return;
        }
        self.cancel_ml_local();
    }

    /// Завершает ML-воркер в текущем процессе.
    fn cancel_ml_local(&mut self) {
        let pid = self
            .ml_progress
            .as_ref()
            .map(|progress| progress.pid)
            .or_else(|| {
                self.session_dir
                    .as_ref()
                    .and_then(|dir| read_progress(&dir.join("progress.json")))
                    .map(|progress| progress.pid)
            });
        #[cfg(unix)]
        if let Some(pid) = pid.filter(|pid| *pid > 0) {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        let _ = pid;
        self.cancel_requested = true;
        self.status = "Отмена распознавания…".to_string();
    }

    /// Забирает результат распознавания.
    fn poll_transcription(&mut self) {
        let result = match &self.transcript_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.transcript_rx = None;
                    self.transcribing = false;
                    let was_cancelled = self.cancel_requested;
                    self.cancel_requested = false;
                    self.ml_progress = None;
                    if let Some(dir) = &self.session_dir {
                        let _ = std::fs::remove_file(dir.join("progress.json"));
                    }
                    let job = self.transcription_job.take();
                    self.finish_job(
                        job,
                        Err(if was_cancelled {
                            "отменено пользователем".to_string()
                        } else {
                            "поток распознавания завершился".to_string()
                        }),
                    );
                    if was_cancelled {
                        self.status = "Распознавание отменено".to_string();
                        self.log = "Распознавание остановлено пользователем.".to_string();
                    } else {
                        self.status = "Распознавание прервано".to_string();
                    }
                    return;
                }
            },
            None => return,
        };
        self.transcript_rx = None;
        let was_cancelled = self.cancel_requested;
        self.cancel_requested = false;
        self.transcribing = false;
        self.ml_progress = None;
        if let Some(dir) = &self.session_dir {
            let _ = std::fs::remove_file(dir.join("progress.json"));
        }
        let job_result = if was_cancelled {
            Err("отменено пользователем".to_string())
        } else {
            result.as_ref().map(|_| ()).map_err(|error| error.clone())
        };
        let job = self.transcription_job.take();
        self.finish_job(job, job_result);
        if was_cancelled {
            tracing::info!("распознавание отменено");
            self.status = "Распознавание отменено".to_string();
            self.log = "Распознавание остановлено пользователем.".to_string();
            notify("Screen Recorder", "Распознавание отменено");
            return;
        }
        match result {
            Ok(output) => {
                tracing::info!(segments = output.segments, language = %output.language, "распознавание завершено");
                self.status = "Транскрипция готова".to_string();
                notify("Screen Recorder", "Транскрипция готова");
                self.log = format!(
                    "Текст: {} · JSON: {} · SRT: {} · VTT: {} · MD: {} ({} сегментов, язык {}){}",
                    output.text_path.display(),
                    output.json_path.display(),
                    output.srt_path.display(),
                    output.vtt_path.display(),
                    output.md_path.display(),
                    output.segments,
                    output.language,
                    output
                        .warning
                        .as_deref()
                        .map(|warning| format!(" · {warning}"))
                        .unwrap_or_default()
                );
                if let Some(dir) = self.session_dir.clone()
                    && let Ok(mut manifest) = load_manifest(&dir)
                {
                    manifest.set_transcript(output.text_path.display().to_string());
                    let _ = save_manifest(&dir, &manifest);
                    append_session_log(
                        &dir,
                        &format!("транскрипт: {}", output.text_path.display()),
                    );
                    self.sync_history(&dir);
                }
                let video = self.last_export_path.clone().or_else(|| {
                    self.session_dir
                        .as_ref()
                        .and_then(|dir| load_manifest(dir).ok())
                        .and_then(|manifest| manifest.export_path.map(PathBuf::from))
                });
                if let Some(video) = video {
                    match copy_transcripts_next_to_video(&video, &output) {
                        Ok(paths) => {
                            self.log = format!(
                                "{} · рядом с видео: {}",
                                self.log,
                                paths
                                    .iter()
                                    .map(|path| path.display().to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                        }
                        Err(error) => {
                            self.log = format!("{} · копирование к видео: {error}", self.log);
                        }
                    }
                }
            }
            Err(error) => {
                tracing::error!(%error, "распознавание не удалось");
                self.status = format!("Ошибка распознавания: {error}");
                self.log = format!("ASR: {error}");
            }
        }
    }

    /// Запускает экспорт записи в выбранный формат в фоне.
    fn start_export(&mut self, session_dir: &Path, audio_only: bool, queued: Option<JobRecord>) {
        if self.exporting {
            return;
        }
        let system = session_dir.join("system.wav");
        let mic = session_dir.join("mic.wav");
        if audio_only {
            if !system.exists() && !mic.exists() {
                self.log = "Экспорт аудио пропущен: нет дорожек.".to_string();
                return;
            }
            self.export_job = self.activate_job(queued, session_dir, "export");
            self.start_audio_export(session_dir);
            return;
        }
        let video = session_dir.join("screen.mkv");
        if !video.exists() {
            self.log = "Экспорт пропущен: нет видеофайла.".to_string();
            return;
        }
        self.export_job = self.activate_job(queued, session_dir, "export");
        let output_dir = if self.settings.output_dir.trim().is_empty() {
            self.paths.default_output_dir.clone()
        } else {
            PathBuf::from(&self.settings.output_dir)
        };
        if let Err(error) = self.paths.ensure_writable(&output_dir) {
            self.status = format!("Ошибка папки результатов: {error}");
            return;
        }
        let format = self.output_format.to_lowercase();
        let stamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let output = output_dir.join(format!("{stamp}.{format}"));
        let request = ExportRequest {
            video,
            system_audio: system.exists().then_some(system),
            mic_audio: mic.exists().then_some(mic),
            output: output.clone(),
            format,
            crf: crf_from_label(&self.video_quality),
            audio_mode: audio_mode_from_label(&self.audio_track_mode),
            audio_offset_ms: self.audio_offset_ms,
            mic_gain_db: microphone_gain_db(session_dir),
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = merge_recording(&request).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.export_rx = Some(receiver);
        self.exporting = true;
        tracing::info!(output = %output.display(), "старт экспорта");
        self.status = "Экспорт записи...".to_string();
        self.log = format!("Экспорт: {}", output.display());
    }

    /// Экспортирует только аудиодорожки в WAV.
    fn start_audio_export(&mut self, session_dir: &Path) {
        if self.exporting {
            return;
        }
        let system = session_dir.join("system.wav");
        let mic = session_dir.join("mic.wav");
        if !system.exists() && !mic.exists() {
            self.log = "Экспорт аудио пропущен: нет дорожек.".to_string();
            return;
        }
        let output_dir = if self.settings.output_dir.trim().is_empty() {
            self.paths.default_output_dir.clone()
        } else {
            PathBuf::from(&self.settings.output_dir)
        };
        if let Err(error) = self.paths.ensure_writable(&output_dir) {
            self.status = format!("Ошибка папки результатов: {error}");
            return;
        }
        let stamp = Local::now().format("%Y-%m-%d_%H-%M-%S");
        let output = output_dir.join(format!("{stamp}.wav"));
        let system = system.exists().then_some(system);
        let mic = mic.exists().then_some(mic);
        let target = output.clone();
        let mic_gain_db = microphone_gain_db(session_dir);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result =
                merge_audio(system, mic, &target, mic_gain_db).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.export_rx = Some(receiver);
        self.exporting = true;
        self.status = "Экспорт аудио...".to_string();
        self.log = format!("Экспорт аудио: {}", output.display());
    }

    /// Забирает результат фонового экспорта.
    fn poll_export(&mut self) {
        let result = match &self.export_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("поток экспорта завершился".to_string()),
            },
            None => return,
        };
        self.export_rx = None;
        self.exporting = false;
        let job_result = result.as_ref().map(|_| ()).map_err(|error| error.clone());
        let job = self.export_job.take();
        self.finish_job(job, job_result);
        match result {
            Ok(path) => {
                tracing::info!(output = %path.display(), "экспорт завершён");
                self.status = "Экспорт завершён".to_string();
                self.log = format!("Готово: {}", path.display());
                notify(
                    "Screen Recorder",
                    &format!("Экспорт готов: {}", path.display()),
                );
                self.last_export_path = Some(path.clone());
                if let Some(dir) = self.session_dir.clone()
                    && let Ok(mut manifest) = load_manifest(&dir)
                {
                    manifest.set_exported(path.display().to_string());
                    let _ = save_manifest(&dir, &manifest);
                    append_session_log(&dir, &format!("экспорт: {}", path.display()));
                    self.sync_history(&dir);
                }
                if let Some(dir) = self.session_dir.clone() {
                    self.enqueue_transcription(&dir);
                }
            }
            Err(error) => {
                tracing::error!(%error, "экспорт не удался");
                self.status = format!("Ошибка экспорта: {error}");
                self.log = format!("Не удалось экспортировать запись: {error}");
            }
        }
    }

    /// Обновляет уровни микрофона/системы во время записи.
    fn update_levels(&mut self) {
        if !self.recording || self.wayland_pending {
            return;
        }
        if self.last_levels.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_levels = Instant::now();
        let Some(dir) = &self.session_dir else {
            return;
        };
        if let Ok(probe) = read_levels(&dir.join("mic.wav")) {
            self.mic_dbfs = probe.rms_dbfs;
        }
        if let Ok(probe) = read_levels(&dir.join("system.wav")) {
            self.system_dbfs = probe.rms_dbfs;
        }
    }

    /// Проверяет, не завершились ли процессы записи сами.
    fn poll_recording(&mut self) {
        self.update_levels();
        if self.wayland_pending {
            if let Some(result) = self.wayland_recorder.take_start_result() {
                self.wayland_pending = false;
                match result {
                    Ok(()) => {
                        if let Some(started) = self.wayland_started_at.take() {
                            let offset = started.elapsed().as_millis() as i64;
                            self.set_audio_offset(offset);
                        }
                        if let Some(session_dir) = self.session_dir.clone() {
                            self.start_audio(&session_dir);
                            append_session_log(&session_dir, "портал ScreenCast запущен");
                        }
                        self.recording_started_at = Some(Instant::now());
                        self.status = "Идёт запись: Wayland".to_string();
                        self.log = "Портал ScreenCast запущен.".to_string();
                        notify("Screen Recorder", "Запись экрана началась");
                    }
                    Err(error) => {
                        self.recording = false;
                        self.using_wayland = false;
                        self.wayland_started_at = None;
                        self.recording_mode = None;
                        let _ = self.wayland_recorder.stop();
                        self.fail_manifest(&error);
                        if error == PORTAL_CANCELLED {
                            self.status = "Доступ к экрану не предоставлен".to_string();
                            self.log = "Диалог портала закрыт без подтверждения.".to_string();
                        } else {
                            self.status = format!("Ошибка Wayland: {error}");
                            self.log = format!("Портал не запустил запись: {error}");
                        }
                    }
                }
            }
            return;
        }

        if self.recording {
            let video_running = if self.using_wayland {
                self.wayland_recorder.is_running()
            } else {
                self.recorder.is_running()
            };
            if !video_running && !self.audio_recorder.is_running() {
                self.recording = false;
                self.recording_mode = None;
                let audio_only = self.audio_only;
                self.audio_only = false;
                if let Some(error) = self.wayland_recorder.take_error() {
                    self.warn_manifest(format!("портал/GStreamer: {error}"));
                    self.status = format!("Ошибка Wayland-записи: {error}");
                    self.log = format!("Портал/GStreamer: {error}");
                } else {
                    self.status = "Запись завершена".to_string();
                    self.log = "Запись завершилась. Проверьте файлы сессии.".to_string();
                }
                self.finalize_manifest(audio_only);
                if let Some(session_dir) = self.session_dir.clone() {
                    self.enqueue_export(&session_dir);
                }
                if self.using_wayland {
                    let _ = self.wayland_recorder.stop();
                    self.using_wayland = false;
                }
            }
        }
    }

    /// Рисует индикатор уровня сигнала.
    fn level_bar(ui: &mut egui::Ui, label: &str, dbfs: f32) {
        let fraction = ((dbfs + 60.0) / 60.0).clamp(0.0, 1.0);
        ui.label(RichText::new(label).size(12.0).color(TEXT));
        ui.add(
            egui::ProgressBar::new(fraction)
                .desired_width(120.0)
                .text(format!("{dbfs:.0} dB")),
        );
    }

    /// Рисует вкладки в стиле текущего приложения.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width() / 4.0;
        let previous_spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            for (screen, label) in [
                (Screen::Record, "Запись"),
                (Screen::History, "История"),
                (Screen::Settings, "Настройки"),
                (Screen::About, "О авторе"),
            ] {
                let selected = self.screen == screen;
                let fill = if selected { BTN } else { ENTRY };
                let color = if selected { WHITE } else { TEXT };
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
                ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(14.0),
                    color,
                );
                if response.clicked() {
                    self.screen = screen;
                }
            }
        });
        ui.spacing_mut().item_spacing.x = previous_spacing;
    }

    /// Рисует кнопку режима записи с иконкой.
    fn record_button(
        ui: &mut egui::Ui,
        image: egui::Image<'_>,
        label: &str,
        enabled: bool,
    ) -> egui::Response {
        ui.add_enabled(
            enabled,
            egui::Button::image_and_text(image, RichText::new(label).size(16.0))
                .fill(BTN)
                .corner_radius(CornerRadius::ZERO)
                .min_size(Vec2::new(200.0, 40.0)),
        )
    }

    /// Рисует блок с рамкой и заголовком (аналог LabelFrame).
    fn section<R>(
        ui: &mut egui::Ui,
        title: &str,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let width = ui.available_width();
        egui::Frame::NONE
            .stroke(Stroke::new(1.0, BTN))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.set_min_width((width - 16.0).max(0.0));
                ui.label(RichText::new(title).strong().color(TEXT));
                add_contents(ui)
            })
            .inner
    }

    /// Рисует выпадающий список.
    fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[&str]) -> bool {
        let mut changed = false;
        egui::ComboBox::from_id_salt(id)
            .width(200.0)
            .selected_text(value.clone())
            .truncate()
            .show_ui(ui, |ui| {
                for option in options {
                    if ui
                        .selectable_value(value, option.to_string(), *option)
                        .clicked()
                    {
                        changed = true;
                    }
                }
            });
        changed
    }

    /// Рисует выбор движка диаризации с понятными подписями.
    fn diarization_combo(ui: &mut egui::Ui, value: &mut String) -> bool {
        let mut changed = false;
        egui::ComboBox::from_id_salt("diarization_engine_models")
            .width(220.0)
            .selected_text(if value == "nemo" {
                "NeMo Sortformer"
            } else {
                "sherpa-onnx"
            })
            .truncate()
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(value, "sherpa-onnx".to_string(), "sherpa-onnx · быстрый")
                    .clicked()
                {
                    changed = true;
                }
                if ui
                    .selectable_value(value, "nemo".to_string(), "NeMo Sortformer · до 8 спикеров")
                    .clicked()
                {
                    changed = true;
                }
            });
        changed
    }

    /// Рисует выпадающий список аудиоустройств.
    fn device_combo(
        ui: &mut egui::Ui,
        id: &str,
        value: &mut String,
        devices: &[AudioDevice],
        kind: AudioDeviceKind,
    ) -> bool {
        let selected = devices
            .iter()
            .find(|device| device.id == *value)
            .map(|device| device.label.clone())
            .unwrap_or_else(|| "По умолчанию".to_string());
        let mut changed = false;
        egui::ComboBox::from_id_salt(id)
            .width(210.0)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(value, String::new(), "По умолчанию")
                    .clicked()
                {
                    changed = true;
                }
                for device in devices.iter().filter(|device| device.kind == kind) {
                    if ui
                        .selectable_value(value, device.id.clone(), device.label.as_str())
                        .clicked()
                    {
                        changed = true;
                    }
                }
            });
        changed
    }

    /// Запускает фоновую проверку уровня сигнала.
    fn start_probe(&mut self, kind: AudioDeviceKind) {
        if self.probe_rx.is_some() {
            self.log = "Проверка звука уже идёт.".to_string();
            return;
        }
        let target = match kind {
            AudioDeviceKind::Mic => self.settings.capture.mic_device.clone(),
            AudioDeviceKind::System => self.settings.capture.system_device.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let target = if target.trim().is_empty() {
                None
            } else {
                Some(target.as_str())
            };
            let result = AudioRecorder::probe(target, kind).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.probe_rx = Some(receiver);
        self.probe_note = "Проверка звука: идёт запись 1 с…".to_string();
    }

    /// Забирает результат проверки звука.
    fn poll_probe(&mut self) {
        let result = match &self.probe_rx {
            Some(receiver) => match receiver.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Err("поток проверки завершился".to_string()),
            },
            None => return,
        };
        self.probe_rx = None;
        match result {
            Ok(probe) => {
                self.probe_note = if probe.silent {
                    format!(
                        "Тихо: RMS {:.1} dBFS, peak {:.1} dBFS",
                        probe.rms_dbfs, probe.peak_dbfs
                    )
                } else {
                    format!(
                        "Сигнал есть: RMS {:.1} dBFS, peak {:.1} dBFS",
                        probe.rms_dbfs, probe.peak_dbfs
                    )
                };
            }
            Err(error) => {
                self.probe_note = format!("Ошибка проверки: {error}");
            }
        }
    }

    /// Рисует строку модели: слева текст с переносом, справа фиксированная
    /// колонка статуса и действия, чтобы все строки были выровнены.
    fn model_row(
        ui: &mut egui::Ui,
        title: &str,
        ready: bool,
        partial: bool,
        progress: Option<f32>,
        description: &str,
    ) -> Option<ModelAction> {
        /// Ширина правой колонки со статусом и кнопкой.
        const ACTIONS_WIDTH: f32 = 150.0;
        let mut action = None;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, 20.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 5.0, if ready { GREEN } else { RED });
            let text_width = (ui.available_width() - ACTIONS_WIDTH - 8.0).max(120.0);
            ui.allocate_ui_with_layout(
                Vec2::new(text_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ui.set_width(text_width);
                    ui.label(RichText::new(title).color(TEXT));
                    // Описание переносится по словам и не обрезается справа.
                    ui.add(
                        egui::Label::new(RichText::new(description).size(11.0).color(HINT)).wrap(),
                    );
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.set_min_width(ACTIONS_WIDTH);
                if ready {
                    ui.label(RichText::new("Готово").color(GREEN));
                    if ui.button("Удалить").clicked() {
                        action = Some(ModelAction::Delete);
                    }
                } else if let Some(percent) = progress {
                    ui.add(
                        egui::ProgressBar::new((percent / 100.0).clamp(0.0, 1.0))
                            .desired_width(110.0)
                            .show_percentage(),
                    );
                } else {
                    let label = if partial {
                        "Докачать"
                    } else {
                        "Скачать"
                    };
                    if ui.button(label).clicked() {
                        action = Some(ModelAction::Download);
                    }
                }
            });
        });
        action
    }

    /// Показывает список X11-окон для выбора «Программа».
    fn window_picker(&mut self, ctx: &egui::Context) {
        if !self.show_windows {
            return;
        }
        let mut selected = None;
        let mut close = false;
        egui::Window::new("Выбор программы")
            .collapsible(false)
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                if self.window_list.is_empty() {
                    ui.label("Нет доступных окон.");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for window in &self.window_list {
                                if ui.selectable_label(false, &window.title).clicked() {
                                    selected = Some(window.region);
                                    close = true;
                                }
                            }
                        });
                }
                ui.add_space(6.0);
                if ui.button("Отмена").clicked() {
                    close = true;
                }
            });
        if close {
            self.show_windows = false;
        }
        if let Some(region) = selected {
            if self.client.is_some() {
                self.send_start(RecordMode::Program, Some(region));
            } else {
                self.begin_preflight(PendingStart::Region(region, "Программа"));
            }
        }
    }

    /// Рисует вкладку «Запись».
    fn draw_record(&mut self, ui: &mut egui::Ui) {
        // Прокрутка нужна только для маленьких окон: при обычном размере
        // все элементы, включая кнопку отмены, помещаются целиком.
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| self.draw_record_content(ui));
    }

    /// Рисует содержимое вкладки «Запись».
    fn draw_record_content(&mut self, ui: &mut egui::Ui) {
        if self.recording {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                Self::level_bar(ui, "Mic", self.mic_dbfs);
                ui.add_space(8.0);
                Self::level_bar(ui, "System", self.system_dbfs);
            });
        }
        ui.add_space(6.0);

        ui.vertical_centered(|ui| {
            let recording = self.recording;
            let active = self.recording_mode;
            let state = |mode: &'static str| {
                (
                    if active == Some(mode) {
                        "Остановить запись"
                    } else {
                        mode
                    },
                    !recording || active == Some(mode),
                )
            };

            let (label, enabled) = state("Весь экран");
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/fullscreen.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                label,
                enabled,
            )
            .clicked()
            {
                self.toggle_recording("Весь экран");
            }
            ui.add_space(8.0);
            let (label, enabled) = state("Область экрана");
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/area.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                label,
                enabled,
            )
            .clicked()
            {
                self.toggle_recording("Область экрана");
            }
            ui.add_space(8.0);
            let (label, enabled) = state("Программа");
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/program.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                label,
                enabled,
            )
            .clicked()
            {
                self.toggle_recording("Программа");
            }
            ui.add_space(8.0);
            let (label, enabled) = state("Только аудио");
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/empty.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                label,
                enabled,
            )
            .clicked()
            {
                self.toggle_recording("Только аудио");
            }
        });

        ui.add_space(4.0);
        Self::section(ui, "Формат и сжатие", |ui| {
            egui::Grid::new("export_grid")
                .num_columns(2)
                .min_col_width(150.0)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Формат файла:");
                    if Self::combo(
                        ui,
                        "output_format",
                        &mut self.output_format,
                        &["mp4", "mkv", "webm", "avi", "mov", "flv"],
                    ) {
                        self.settings_dirty = true;
                    }
                    ui.end_row();

                    ui.label("Качество видео:");
                    if Self::combo(
                        ui,
                        "video_quality",
                        &mut self.video_quality,
                        &[
                            "Отличное (CRF 18)",
                            "Хорошее (CRF 23)",
                            "Среднее (CRF 28)",
                            "Сильное сжатие (CRF 35)",
                        ],
                    ) {
                        self.settings_dirty = true;
                    }
                    ui.end_row();

                    ui.label("Звуковая дорожка:");
                    if Self::combo(
                        ui,
                        "audio_track_mode",
                        &mut self.audio_track_mode,
                        &["Без пережатия", "Сжать (AAC 128k)", "Без звука"],
                    ) {
                        self.settings_dirty = true;
                    }
                    ui.end_row();
                });
        });

        ui.add_space(6.0);
        Self::section(ui, "Состояние", |ui| {
            self.progress_panel(ui);
        });
    }

    /// Показывает этапы записи и обработки вместо текстового журнала.
    fn progress_panel(&mut self, ui: &mut egui::Ui) {
        let (title, detail, active, failed) = self.state_description();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
            let color = if failed {
                RED
            } else if active {
                LINK
            } else {
                GREEN
            };
            ui.painter().circle_filled(rect.center(), 4.0, color);
            ui.label(RichText::new(title).strong().color(TEXT));
        });
        ui.label(RichText::new(detail).size(11.0).color(HINT));
        if active {
            ui.add_space(2.0);
            if let Some(progress) = self.ml_progress.as_ref().filter(|_| self.transcribing) {
                let stage = match progress.stage.as_str() {
                    "diarization" => "Голоса",
                    _ => "Распознавание",
                };
                ui.add(
                    egui::ProgressBar::new((progress.percent / 100.0).clamp(0.0, 1.0))
                        .desired_width(ui.available_width())
                        .text(format!("{stage} · {:.0}%", progress.percent)),
                );
            } else {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .animate(true)
                        .desired_width(ui.available_width())
                        .text(title),
                );
            }
        }
        if self.transcribing && ui.button("Отменить распознавание").clicked() {
            self.cancel_ml();
        }
        let queued = self.queued_total();
        if queued > 0 {
            ui.label(
                RichText::new(format!("В очереди задач: {queued}"))
                    .size(11.0)
                    .color(HINT),
            );
        }
    }

    /// Количество задач в очереди (локальное или из снимка сервиса).
    fn queued_total(&self) -> usize {
        self.queued_jobs.len().max(self.queued_jobs_count)
    }

    /// Возвращает заголовок, подробное описание, активность и признак ошибки.
    fn state_description(&self) -> (&'static str, String, bool, bool) {
        if self.status.starts_with("Ошибка") || self.status.contains("с ошибками") {
            return ("Ошибка", self.status.clone(), false, true);
        }
        if self.preflight_active || self.preflight_rx.is_some() {
            return (
                "Проверка звука",
                "Проверяю микрофон и системный звук. Это занимает 1–2 секунды.".to_string(),
                true,
                false,
            );
        }
        if self.recording && self.wayland_pending {
            return (
                "Ожидание доступа",
                "Подтвердите доступ к экрану в системном окне.".to_string(),
                true,
                false,
            );
        }
        if self.recording {
            let elapsed = if self.recording_elapsed > 0 {
                self.recording_elapsed
            } else {
                self.recording_started_at
                    .map(|started| started.elapsed().as_secs())
                    .unwrap_or(0)
            };
            let mode = self.recording_mode.unwrap_or("Запись");
            return (
                "Идёт запись",
                format!(
                    "{mode} · {:02}:{:02} · идёт запись видео и звука",
                    elapsed / 60,
                    elapsed % 60
                ),
                true,
                false,
            );
        }
        if self.stopping {
            return (
                "Остановка",
                "Завершаю запись и готовлю файл.".to_string(),
                true,
                false,
            );
        }
        if self.exporting {
            return (
                "Экспорт",
                "Собираю видео и звук в готовый файл.".to_string(),
                true,
                false,
            );
        }
        if self.transcribing {
            return (
                "Распознавание",
                "Распознаю речь и определяю, кто говорит.".to_string(),
                true,
                false,
            );
        }
        let queued = self.queued_total();
        if queued > 0 {
            return (
                "В очереди",
                format!("Ещё задач в очереди: {queued}."),
                true,
                false,
            );
        }
        let transcription_ready = self.status == "Транскрипция готова";
        let export_ready = self.status == "Экспорт завершён" || transcription_ready;
        if transcription_ready {
            return (
                "Готово",
                "Видео и транскрипт сохранены. Результаты доступны во вкладке «История»."
                    .to_string(),
                false,
                false,
            );
        }
        if export_ready {
            return (
                "Готово",
                "Видео экспортировано. Распознавание отключено в настройках.".to_string(),
                false,
                false,
            );
        }
        if !self.model_manager.is_downloaded("whisper") {
            return (
                "Модели не скачаны",
                "Скачайте модели в настройках.".to_string(),
                false,
                false,
            );
        }
        if self.settings.diarization.enabled {
            let ready = if self.settings.diarization.engine == "nemo" {
                self.model_manager.is_downloaded("nemo")
            } else {
                self.model_manager.is_downloaded("diarization")
            };
            if !ready {
                return (
                    "Не хватает модели для разделения голосов",
                    "Скачайте её в настройках или отключите разделение голосов.".to_string(),
                    false,
                    false,
                );
            }
        }
        (
            "Готово к записи",
            "Выберите режим и нажмите кнопку. Перед записью будет проверка звука.".to_string(),
            false,
            false,
        )
    }

    /// Удаляет запись: экспортированное видео, дорожки и транскрипты.
    fn delete_record(&mut self, record: SessionRecord) {
        if self.client.is_some() {
            match self.send_request(&Request::DeleteSession {
                session_id: record.id.clone(),
            }) {
                Ok(Response::Ok) => self.log = format!("Удалено: {}", record.title),
                Ok(Response::Error { message }) | Err(message) => {
                    self.log = format!("Удаление: {message}");
                }
                Ok(_) => {}
            }
            self.delete_confirm = None;
            self.refresh_history();
            return;
        }
        self.delete_session_record(&record);
        self.delete_confirm = None;
    }

    /// Удаляет файлы сессии и запись истории в текущем процессе.
    fn delete_session_record(&mut self, record: &SessionRecord) {
        let session_dir = PathBuf::from(&record.output_dir);
        if self.recording && self.session_dir.as_deref() == Some(session_dir.as_path()) {
            self.log = "Нельзя удалить запись, которая ещё идёт.".to_string();
            return;
        }
        let mut errors = Vec::new();
        if let Some(export) = &record.export_path {
            let video = PathBuf::from(export);
            if video.exists()
                && let Err(error) = std::fs::remove_file(&video)
            {
                errors.push(format!("видео {}: {error}", video.display()));
            }
            if let (Some(parent), Some(stem)) = (
                video.parent(),
                video.file_stem().and_then(|stem| stem.to_str()),
            ) {
                for extension in ["txt", "json", "srt", "vtt", "md"] {
                    let sidecar = parent.join(format!("{stem}.{extension}"));
                    if sidecar.exists()
                        && let Err(error) = std::fs::remove_file(&sidecar)
                    {
                        errors.push(format!("{}: {error}", sidecar.display()));
                    }
                }
            }
        }
        for name in [
            "transcript.txt",
            "transcript.json",
            "transcript.srt",
            "transcript.vtt",
            "transcript.md",
        ] {
            let path = session_dir.join(name);
            if path.exists()
                && let Err(error) = std::fs::remove_file(&path)
            {
                errors.push(format!("{}: {error}", path.display()));
            }
        }
        let recordings_root = self.paths.data_dir.join("recordings");
        if session_dir.starts_with(&recordings_root)
            && session_dir != recordings_root
            && session_dir.exists()
            && let Err(error) = std::fs::remove_dir_all(&session_dir)
        {
            errors.push(format!("сессия {}: {error}", session_dir.display()));
        }
        if let Err(error) = self.history.delete_session(&record.id) {
            errors.push(format!("история: {error}"));
        }
        self.history_records.retain(|item| item.id != record.id);
        self.queued_jobs.retain(|job| job.session_id != record.id);
        if errors.is_empty() {
            self.log = format!("Удалено: {}", record.title);
        } else {
            self.log = format!("Удаление с ошибками: {}", errors.join("; "));
        }
    }

    /// Удаляет сессию по id из команды окна.
    fn delete_session_by_id(&mut self, session_id: &str) -> Result<(), String> {
        let record =
            self.history_records
                .iter()
                .find(|record| record.id == session_id)
                .cloned()
                .or_else(|| {
                    self.history.list(100).ok().and_then(|records| {
                        records.into_iter().find(|record| record.id == session_id)
                    })
                })
                .ok_or_else(|| "запись не найдена".to_string())?;
        self.delete_session_record(&record);
        Ok(())
    }

    /// Показывает подтверждение удаления записи.
    fn delete_window(&mut self, ctx: &egui::Context) {
        let Some(record) = self.delete_confirm.clone() else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Удалить запись")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(RichText::new(&record.title).strong().color(TEXT));
                ui.label("Видео и транскрипт этой записи будут удалены.");
                ui.label(
                    RichText::new("Рабочие файлы сессии тоже удаляются.")
                        .size(11.0)
                        .color(HINT),
                );
                ui.label(
                    RichText::new("Действие нельзя отменить.")
                        .size(11.0)
                        .color(RED),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Удалить").clicked() {
                        confirm = true;
                    }
                    if ui.button("Отмена").clicked() {
                        cancel = true;
                    }
                });
            });
        if confirm {
            self.delete_record(record);
        }
        if cancel {
            self.delete_confirm = None;
        }
    }

    /// Рисует вкладку «История».
    fn draw_history(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("История записей")
                .size(16.0)
                .strong()
                .color(TEXT),
        );
        ui.add_space(6.0);
        let records = self.history_records.clone();
        if records.is_empty() {
            ui.label(RichText::new("Записей пока нет.").color(HINT));
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for record in records {
                    let title = format!("{} · {}", record.title, display_date(&record.created_at));
                    Self::section(ui, &title, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                state_label(&record.state),
                                record.mode
                            ))
                            .size(12.0)
                            .color(TEXT),
                        );
                        if let Some(export) = &record.export_path {
                            ui.label(
                                RichText::new(format!("Файл: {export}"))
                                    .size(11.0)
                                    .color(HINT),
                            );
                        }
                        if let Some(transcript) = &record.transcript_path {
                            ui.label(
                                RichText::new(format!("Текст: {transcript}"))
                                    .size(11.0)
                                    .color(HINT),
                            );
                        }
                        if let Some(error) = &record.error {
                            ui.label(RichText::new(error).size(11.0).color(RED));
                        }
                        ui.horizontal(|ui| {
                            if let Some(export) = &record.export_path
                                && ui.button("Открыть видео").clicked()
                            {
                                open_path(export);
                            }
                            if let Some(transcript) = &record.transcript_path
                                && ui.button("Открыть текст").clicked()
                            {
                                open_path(transcript);
                            }
                            if ui.button("Удалить").clicked() {
                                self.delete_confirm = Some(record.clone());
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
    }

    /// Рисует вкладку «Настройки».
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        self.capture_hotkey(ui.ctx());
        let whisper_ready = self.model_manager.is_downloaded("whisper");
        let diarization_ready = self.model_manager.is_downloaded("diarization");
        let vad_ready = self.model_manager.is_downloaded("vad");
        let nemo_ready = self.model_manager.is_downloaded("nemo");
        let nemo_progress = (self.download_model == Some("nemo")).then_some(self.download_progress);
        let whisper_partial = self.model_manager.is_partial("whisper");
        let diarization_partial = self.model_manager.is_partial("diarization");
        let vad_partial = self.model_manager.is_partial("vad");
        let nemo_partial = self.model_manager.is_partial("nemo");
        let whisper_progress =
            (self.download_model == Some("whisper")).then_some(self.download_progress);
        let diarization_progress =
            (self.download_model == Some("diarization")).then_some(self.download_progress);
        let vad_progress = (self.download_model == Some("vad")).then_some(self.download_progress);
        let whisper_size = format_size(self.model_manager.size("whisper"));
        let diarization_size = format_size(self.model_manager.size("diarization"));
        let vad_size = format_size(self.model_manager.size("vad"));
        let nemo_size = format_size(self.model_manager.size("nemo"));

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut requested: Option<&'static str> = None;
                let mut delete_requested: Option<&'static str> = None;
                Self::section(ui, "Модели", |ui| {
                    match Self::model_row(
                        ui,
                        "Whisper large-v3-turbo",
                        whisper_ready,
                        whisper_partial,
                        whisper_progress,
                        &format!("распознавание речи · {whisper_size}"),
                    ) {
                        Some(ModelAction::Download) => requested = Some("whisper"),
                        Some(ModelAction::Delete) => delete_requested = Some("whisper"),
                        None => {}
                    }
                    ui.add_space(4.0);
                    match Self::model_row(
                        ui,
                        "sherpa-onnx",
                        diarization_ready,
                        diarization_partial,
                        diarization_progress,
                        &format!("кто говорит · {diarization_size}"),
                    ) {
                        Some(ModelAction::Download) => requested = Some("diarization"),
                        Some(ModelAction::Delete) => delete_requested = Some("diarization"),
                        None => {}
                    }
                    ui.add_space(4.0);
                    match Self::model_row(
                        ui,
                        "Silero VAD",
                        vad_ready,
                        vad_partial,
                        vad_progress,
                        &format!("речь и тишина · {vad_size}"),
                    ) {
                        Some(ModelAction::Download) => requested = Some("vad"),
                        Some(ModelAction::Delete) => delete_requested = Some("vad"),
                        None => {}
                    }
                    ui.add_space(4.0);
                    match Self::model_row(
                        ui,
                        "NeMo Sortformer",
                        nemo_ready,
                        nemo_partial,
                        nemo_progress,
                        &format!("до 8 говорящих · {nemo_size}"),
                    ) {
                        Some(ModelAction::Download) => requested = Some("nemo"),
                        Some(ModelAction::Delete) => delete_requested = Some("nemo"),
                        None => {}
                    }

                    ui.add_space(6.0);
                    ui.label("Разделение говорящих:");
                    if Self::diarization_combo(ui, &mut self.settings.diarization.engine) {
                        self.settings_dirty = true;
                    }
                });
                if let Some(id) = requested {
                    self.start_download(id);
                }
                if let Some(id) = delete_requested {
                    self.delete_model(id);
                }

                ui.add_space(6.0);
                let devices = self.audio_devices.clone();
                let probe_note = self.probe_note.clone();
                let mut probe_mic = false;
                let mut probe_system = false;
                Self::section(ui, "Аудио", |ui| {
                    ui.label("Микрофон:");
                    ui.horizontal(|ui| {
                        if Self::device_combo(
                            ui,
                            "mic_device",
                            &mut self.settings.capture.mic_device,
                            &devices,
                            AudioDeviceKind::Mic,
                        ) {
                            self.settings_dirty = true;
                        }
                        if ui.button("Проверить").clicked() {
                            probe_mic = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label("Системный звук:");
                    ui.horizontal(|ui| {
                        if Self::device_combo(
                            ui,
                            "system_device",
                            &mut self.settings.capture.system_device,
                            &devices,
                            AudioDeviceKind::System,
                        ) {
                            self.settings_dirty = true;
                        }
                        if ui.button("Проверить").clicked() {
                            probe_system = true;
                        }
                    });
                    if !probe_note.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(probe_note).size(11.0).color(HINT));
                    }
                });
                if probe_mic {
                    self.start_probe(AudioDeviceKind::Mic);
                }
                if probe_system {
                    self.start_probe(AudioDeviceKind::System);
                }

                ui.add_space(6.0);
                let mut pick_models = false;
                let mut pick_output = false;

                Self::section(ui, "Параметры", |ui| {
                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .min_col_width(120.0)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Устройство:");
                            if Self::combo(
                                ui,
                                "compute_device",
                                &mut self.settings.transcription.compute_device,
                                &["auto", "cpu", "gpu"],
                            ) {
                                self.settings_dirty = true;
                            }
                            ui.end_row();

                            ui.label("Язык:");
                            if Self::combo(
                                ui,
                                "language",
                                &mut self.settings.transcription.language,
                                &["auto", "ru", "en"],
                            ) {
                                self.settings_dirty = true;
                            }
                            ui.end_row();
                        });

                    ui.add_space(4.0);
                    if ui
                        .checkbox(
                            &mut self.settings.transcription.enabled,
                            "Распознавать речь",
                        )
                        .changed()
                    {
                        self.settings_dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.settings.diarization.enabled,
                            "Разделять по говорящим",
                        )
                        .changed()
                    {
                        self.settings_dirty = true;
                    }
                });

                ui.add_space(6.0);
                Self::section(ui, "Папки", |ui| {
                    ui.label("Папка моделей:");
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 96.0).max(120.0);
                        if ui
                            .add_sized(
                                [width, 26.0],
                                egui::TextEdit::singleline(&mut self.settings.models_dir)
                                    .hint_text("по умолчанию"),
                            )
                            .changed()
                        {
                            self.settings_dirty = true;
                        }
                        if ui.button("Обзор...").clicked() {
                            pick_models = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label("Папка результатов:");
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 96.0).max(120.0);
                        if ui
                            .add_sized(
                                [width, 26.0],
                                egui::TextEdit::singleline(&mut self.settings.output_dir)
                                    .hint_text("по умолчанию"),
                            )
                            .changed()
                        {
                            self.settings_dirty = true;
                        }
                        if ui.button("Обзор...").clicked() {
                            pick_output = true;
                        }
                    });
                });

                if pick_models {
                    self.pick_folder(FolderTarget::Models);
                }
                if pick_output {
                    self.pick_folder(FolderTarget::Output);
                }
                let models_hint = if self.settings.models_dir.trim().is_empty() {
                    "Папка не выбрана — при скачивании спросим и покажем путь здесь.".to_string()
                } else {
                    format!(
                        "Модели: {}",
                        models_root_for(&self.paths, &self.settings.models_dir).display()
                    )
                };
                ui.label(RichText::new(models_hint).size(10.0).color(HINT));

                ui.add_space(6.0);
                let hotkeys = [
                    (
                        self.settings.hotkeys.fullscreen.clone(),
                        "Весь экран",
                        HotkeyField::FullScreen,
                    ),
                    (
                        self.settings.hotkeys.area.clone(),
                        "Область экрана",
                        HotkeyField::Area,
                    ),
                    (
                        self.settings.hotkeys.program.clone(),
                        "Программа",
                        HotkeyField::Program,
                    ),
                    (
                        self.settings.hotkeys.audio_only.clone(),
                        "Только аудио",
                        HotkeyField::AudioOnly,
                    ),
                    (
                        self.settings.hotkeys.stop.clone(),
                        "Остановить запись",
                        HotkeyField::Stop,
                    ),
                ];
                let mut edit_hotkey = None;
                Self::section(
                    ui,
                    "Глобальные горячие клавиши",
                    |ui| {
                        for (combo, name, field) in hotkeys {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(combo).monospace().color(TEXT));
                                ui.label(RichText::new(name).size(11.0).color(TEXT));
                                if ui.small_button("Изменить").clicked() {
                                    edit_hotkey = Some(field);
                                }
                            });
                            ui.add_space(3.0);
                        }
                    },
                );
                if let Some(field) = edit_hotkey {
                    self.hotkey_edit = Some(field);
                }

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Изменения сохраняются автоматически")
                        .size(11.0)
                        .color(HINT),
                );
            });

        if self.hotkey_edit.is_some() {
            let mut cancel = false;
            egui::Window::new("Новое сочетание")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label("Нажмите сочетание клавиш.");
                    if ui.button("Отмена").clicked() {
                        cancel = true;
                    }
                });
            if cancel {
                self.hotkey_edit = None;
            }
        }
    }

    /// Рисует вкладку «О авторе».
    fn draw_about(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("О авторе").size(16.0).strong().color(TEXT));
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.add(
                egui::Image::new(egui::include_image!("../../../assets/ava100x100.png"))
                    .fit_to_exact_size(Vec2::new(100.0, 100.0)),
            );
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.hyperlink_to("Автор (Telegram)", "https://t.me/Ifreekazoid");
                ui.hyperlink_to(
                    "Проект (github.com)",
                    "https://github.com/Freekazoid/recordingScreen",
                );
            });
        });

        ui.add_space(10.0);
        ui.label(
            "Утилита для автоматизации записи и анализа рабочих встреч и дейликов. \
             Позволяет захватывать экран и аудио, а затем с помощью ИИ-моделей \
             автоматически транскрибировать речь, разделять реплики по спикерам \
             и формировать готовые конспекты.\n\n\
             Вся обработка речи выполняется на стороне клиента с использованием \
             открытых нейросетевых моделей.",
        );

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.label(
                RichText::new(format!("Версия сборки: {}", env!("CARGO_PKG_VERSION")))
                    .italics()
                    .color(TEXT),
            );
        });
    }

    /// Обрабатывает фоновые события и команды.
    ///
    /// Собирает снимок состояния для окна.
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            status: self.status.clone(),
            log: self.log.clone(),
            recording: self.recording,
            stopping: self.stopping,
            recording_mode: self.recording_mode.map(str::to_string),
            audio_only: self.audio_only,
            using_wayland: self.using_wayland,
            wayland_pending: self.wayland_pending,
            recording_elapsed: self
                .recording_started_at
                .map(|started| started.elapsed().as_secs())
                .unwrap_or(0),
            mic_dbfs: self.mic_dbfs,
            system_dbfs: self.system_dbfs,
            transcribing: self.transcribing,
            exporting: self.exporting,
            preflight_active: self.preflight_rx.is_some(),
            preflight_warnings: self.preflight_warnings.clone(),
            ml_progress: self.ml_progress.as_ref().map(|progress| ProgressDto {
                stage: progress.stage.clone(),
                percent: progress.percent,
                device: (!progress.device.is_empty()).then(|| progress.device.clone()),
            }),
            queued_jobs: self.queued_jobs.len(),
            show_requested: self.show_requested,
            ui_action: self.ui_action,
        }
    }

    /// Применяет новые настройки, пришедшие из окна.
    fn apply_settings(&mut self, settings: Settings) {
        if let Err(error) = settings.validate() {
            self.log = format!("Настройки не применены: {error}");
            return;
        }
        self.settings = settings;
        self.output_format = self.settings.export.format.clone();
        self.video_quality = self.settings.export.video_quality.clone();
        self.audio_track_mode = self.settings.export.audio_mode.clone();
        self.model_manager =
            ModelManager::new(models_root_for(&self.paths, &self.settings.models_dir));
        self.audio_devices = AudioRecorder::list_devices().unwrap_or_default();
        if self.hotkey_manager.is_some() {
            self.apply_hotkeys();
        }
        if !self.recording && !self.transcribing && !self.exporting {
            self.status = if self.model_manager.is_downloaded("whisper") {
                "Готово к записи".to_string()
            } else {
                "Модели не скачаны".to_string()
            };
        }
    }

    /// Ставит в очередь старт записи по команде окна.
    fn request_start(&mut self, start: StartRequest) {
        if self.recording || self.stopping || self.preflight_rx.is_some() {
            self.log = "Запись уже идёт, останавливается или проверяется звук.".to_string();
            return;
        }
        match start.mode {
            RecordMode::FullScreen => self.begin_preflight(PendingStart::FullScreen),
            RecordMode::AudioOnly => self.begin_preflight(PendingStart::AudioOnly),
            RecordMode::Area => match start.region {
                Some(region) => self.begin_preflight(PendingStart::Region(
                    Region {
                        x: region.x,
                        y: region.y,
                        width: region.width,
                        height: region.height,
                    },
                    "Область экрана",
                )),
                None => self.log = "Не выбрана область записи.".to_string(),
            },
            RecordMode::Program => match start.region {
                Some(region) => self.begin_preflight(PendingStart::Region(
                    Region {
                        x: region.x,
                        y: region.y,
                        width: region.width,
                        height: region.height,
                    },
                    "Программа",
                )),
                None if is_wayland() => self.begin_preflight(PendingStart::ProgramWayland),
                None => self.log = "Для программы нужно выбрать окно.".to_string(),
            },
        }
    }

    /// Передаёт сервису X11-окно для родителя диалога портала.
    #[cfg(target_os = "linux")]
    fn send_hotkey_parent(&mut self) {
        if self.hotkey_parent_sent || self.client.is_none() {
            return;
        }
        if self.ui_backend != UiBackend::X11 {
            self.hotkey_parent_sent = true;
            return;
        }
        let Ok(Some(xid)) = x11_activation::window_id(std::process::id()) else {
            return;
        };
        let request = Request::HotkeyParent { xid };
        if let Some(client) = self.client.as_mut()
            && client.request(&request).is_ok()
        {
            self.hotkey_parent_sent = true;
        }
    }

    /// Обновляет окно: снимок сервиса, загрузки, выбор области и настройки.
    fn tick_ui(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "linux")]
        self.send_hotkey_parent();
        self.poll_daemon(ctx);
        self.poll_stop();
        self.refresh_history();
        self.poll_download();
        self.poll_folder_picker();
        self.poll_probe();
        self.poll_area();
        self.persist_settings();
        if let Some(lost) = self.daemon_lost_at
            && lost.elapsed() > Duration::from_secs(3)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    /// Обслуживает запись, очередь и трей в фоновом сервисе.
    fn tick_daemon(&mut self) {
        self.poll_stop();
        if self.last_state_log.elapsed() >= Duration::from_secs(60) {
            self.last_state_log = Instant::now();
            if self.recording || self.exporting || self.transcribing || !self.queued_jobs.is_empty()
            {
                tracing::info!(
                    recording = self.recording,
                    elapsed = self
                        .recording_started_at
                        .map(|started| started.elapsed().as_secs())
                        .unwrap_or(0),
                    transcribing = self.transcribing,
                    exporting = self.exporting,
                    queued = self.queued_jobs.len(),
                    rss_mb = process_rss_mb().unwrap_or(0),
                    "состояние сервиса"
                );
            }
        }
        self.poll_recording();
        self.poll_preflight();
        self.poll_transcription();
        self.poll_ml_progress();
        self.poll_job_queue();
        self.poll_tray();
        self.poll_hotkeys();
        self.sync_tray_state();
        self.poll_area();
        self.poll_export();
    }

    /// Запрашивает снимок состояния у фонового сервиса.
    fn poll_daemon(&mut self, ctx: &egui::Context) {
        if self.last_daemon_poll.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_daemon_poll = Instant::now();
        let reply = self
            .client
            .as_mut()
            .map(|client| client.request(&Request::Snapshot));
        let Some(reply) = reply else {
            self.mark_daemon_lost("Сервис не подключён");
            return;
        };
        match reply {
            Ok(Response::Snapshot(snapshot)) => {
                self.daemon_lost_at = None;
                if snapshot.show_requested {
                    if self.ui_backend == UiBackend::Wayland {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(
                            egui::UserAttentionType::Critical,
                        ));
                    } else {
                        // На X11 не просим фокус: GNOME может ответить
                        // уведомлением «окно ожидает». Окно само переедет
                        // на текущий стол и поднимется.
                        request_x11_activation();
                    }
                    if let Some(client) = self.client.as_mut() {
                        let _ = client.request(&Request::AckShow);
                    }
                }
                if snapshot.ui_action == Some(UiAction::Program) && !self.show_windows {
                    self.screen = Screen::Record;
                    self.start_program_selection();
                    if let Some(client) = self.client.as_mut() {
                        let _ = client.request(&Request::AckAction);
                    }
                }
                self.apply_snapshot(snapshot);
            }
            Ok(Response::Error { message }) => {
                self.status = message.clone();
                self.log = message;
            }
            Ok(_) => {}
            Err(error) => {
                self.mark_daemon_lost(&error);
            }
        }
    }

    /// Помечает потерю сервиса и сбрасывает активные индикаторы.
    fn mark_daemon_lost(&mut self, reason: &str) {
        if self.daemon_lost_at.is_none() {
            tracing::warn!(%reason, "потеряна связь с сервисом");
            self.daemon_lost_at = Some(Instant::now());
            self.status = "Фоновый сервис остановлен".to_string();
            self.log = reason.to_string();
            self.recording = false;
            self.recording_mode = None;
            self.transcribing = false;
            self.exporting = false;
            self.ml_progress = None;
            self.preflight_active = false;
        }
        self.client = None;
    }

    /// Переносит снимок сервиса в поля интерфейса.
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.status = snapshot.status;
        self.log = snapshot.log;
        self.recording = snapshot.recording;
        self.stopping = snapshot.stopping;
        self.recording_mode = snapshot
            .recording_mode
            .as_deref()
            .and_then(recording_mode_static);
        self.audio_only = snapshot.audio_only;
        self.using_wayland = snapshot.using_wayland;
        self.wayland_pending = snapshot.wayland_pending;
        self.recording_elapsed = snapshot.recording_elapsed;
        self.mic_dbfs = snapshot.mic_dbfs;
        self.system_dbfs = snapshot.system_dbfs;
        self.transcribing = snapshot.transcribing;
        self.exporting = snapshot.exporting;
        self.preflight_active = snapshot.preflight_active;
        self.preflight_warnings = snapshot.preflight_warnings;
        self.ml_progress = snapshot.ml_progress.map(|progress| ProgressInfo {
            stage: progress.stage,
            percent: progress.percent,
            pid: 0,
            pass: 0,
            device: progress.device.unwrap_or_default(),
        });
        self.queued_jobs_count = snapshot.queued_jobs;
        self.show_requested = snapshot.show_requested;
        self.ui_action = snapshot.ui_action;
    }

    /// Обновляет историю из общей SQLite-базы.
    fn refresh_history(&mut self) {
        if self.last_history_refresh.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.last_history_refresh = Instant::now();
        self.history_records = self.history.list(100).unwrap_or_default();
    }
}

impl eframe::App for ScreenRecorderApp {
    /// Обрабатывает события окна до отрисовки.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick_ui(ctx);
    }

    /// Обновляет интерфейс на каждом кадре.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::same(5)))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.add_space(4.0);
                match self.screen {
                    Screen::Record => self.draw_record(ui),
                    Screen::History => self.draw_history(ui),
                    Screen::Settings => self.draw_settings(ui),
                    Screen::About => self.draw_about(ui),
                }
            });
        self.window_picker(ui.ctx());
        self.delete_window(ui.ctx());
    }
}

/// Возвращает RSS текущего процесса в мегабайтах (Linux).
fn process_rss_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            let kb = value.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

/// Имя видеобэкенда для манифеста и журнала.
fn video_backend_label() -> &'static str {
    #[cfg(windows)]
    {
        "ffmpeg-gdigrab"
    }
    #[cfg(not(windows))]
    {
        "ffmpeg-x11grab"
    }
}

/// Проверяет, что приложение работает через Wayland.
///
/// Нужно для возврата окна из трея через xdg-activation: на Wayland
/// обычный `Focus`/`Minimized(false)` не работают.
fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Убирает суффикс ` (deleted)` из пути к исполняемому файлу.
///
/// Linux добавляет его в `/proc/self/exe`, если бинарник пересобрали или
/// заменили при работающем процессе. Запускать такой путь нельзя, поэтому
/// сервис должен использовать актуальный файл без суффикса.
fn clean_executable_path(raw: &str) -> &str {
    raw.strip_suffix(" (deleted)").unwrap_or(raw)
}

/// Возвращает существующий путь к исполняемому файлу приложения.
fn ui_executable() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("исполняемый файл: {error}"))?;
    let raw = exe.to_string_lossy();
    let path = PathBuf::from(clean_executable_path(&raw));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("исполняемый файл не найден: {}", path.display()))
    }
}

/// Запускает в фоне перенос и активацию X11-окна.
///
/// Окно могло остаться на другом рабочем столе или быть свёрнутым.
/// Повторяем попытки, пока окно не появится в списке клиентов X11.
#[cfg(target_os = "linux")]
fn request_x11_activation() {
    let pid = std::process::id();
    std::thread::spawn(move || {
        for _ in 0..15 {
            match x11_activation::activate(pid) {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "активация X11-окна");
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        tracing::warn!("X11-окно для активации не найдено");
    });
}

/// Заглушка для не-Linux платформ.
#[cfg(not(target_os = "linux"))]
fn request_x11_activation() {}

/// Переводит название режима из снимка в статическую строку.
fn recording_mode_static(mode: &str) -> Option<&'static str> {
    match mode {
        "Весь экран" => Some("Весь экран"),
        "Область экрана" => Some("Область экрана"),
        "Программа" => Some("Программа"),
        "Только аудио" => Some("Только аудио"),
        _ => None,
    }
}

/// Разбирает строку сочетания в глобальную горячую клавишу.
///
/// Одиночные клавиши без модификаторов не принимаем: они мешают вводу.
fn parse_hotkey(combo: &str) -> Option<HotKey> {
    let hotkey = combo.parse::<HotKey>().ok()?;
    if hotkey.mods.is_empty() {
        return None;
    }
    Some(hotkey)
}

/// Формирует строку сочетания из нажатой клавиши и модификаторов.
fn format_hotkey(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let key_name = match key {
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::F1 => "F1",
        egui::Key::F2 => "F2",
        egui::Key::F3 => "F3",
        egui::Key::F4 => "F4",
        egui::Key::F5 => "F5",
        egui::Key::F6 => "F6",
        egui::Key::F7 => "F7",
        egui::Key::F8 => "F8",
        egui::Key::F9 => "F9",
        egui::Key::F10 => "F10",
        egui::Key::F11 => "F11",
        egui::Key::F12 => "F12",
        egui::Key::Space => "Space",
        egui::Key::Minus => "Minus",
        egui::Key::Equals => "Equal",
        egui::Key::Comma => "Comma",
        egui::Key::Period => "Period",
        egui::Key::Slash => "Slash",
        egui::Key::Semicolon => "Semicolon",
        egui::Key::Quote => "Quote",
        egui::Key::Backslash => "Backslash",
        egui::Key::OpenBracket => "BracketLeft",
        egui::Key::CloseBracket => "BracketRight",
        egui::Key::Backtick => "Backquote",
        _ => return None,
    };
    if !(modifiers.ctrl || modifiers.alt || modifiers.shift || modifiers.mac_cmd) {
        return None;
    }
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.mac_cmd {
        parts.push("Super");
    }
    parts.push(key_name);
    Some(parts.join("+"))
}

/// Формирует подпись пункта меню трея.
///
/// На Linux StatusNotifier не показывает отдельный столбец сочетаний,
/// поэтому клавиши добавляются в текст с выравниванием по ширине.
/// На Windows/macOS сочетание рисует сама система через `Accelerator`.
fn tray_label(text: &str, keys: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        // Отступы подобраны по ширине Ubuntu Sans 11 (тема Yaru): все
        // подписи получают одинаковую ширину 135–136 px, поэтому
        // сочетания начинаются на одной вертикали и прижаты вправо.
        let padding = match text {
            "Весь экран" => 20,
            "Область экрана" => 8,
            "Программа" => 18,
            "Только аудио" => 14,
            "Остановить запись" => 1,
            _ => 8,
        };
        format!("{text}{}{keys}", " ".repeat(padding))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = keys;
        text.to_string()
    }
}

/// Сопоставляет id сочетания из портала команде трея.
#[cfg(target_os = "linux")]
fn portal_command(id: &str) -> Option<TrayCommand> {
    match id {
        "fullscreen" => Some(TrayCommand::FullScreen),
        "area" => Some(TrayCommand::Area),
        "program" => Some(TrayCommand::Program),
        "audio_only" => Some(TrayCommand::AudioOnly),
        "stop" => Some(TrayCommand::Stop),
        _ => None,
    }
}

/// Приводит человекочитаемое описание портала к короткому сочетанию.
///
/// GNOME может вернуть «Нажмите <Control><Alt>s»; в трее показываем
/// «Ctrl+Alt+s».
#[cfg(target_os = "linux")]
fn clean_trigger(description: &str) -> String {
    let text = description.trim();
    let text = text
        .strip_prefix("Нажмите ")
        .or_else(|| text.strip_prefix("Press "))
        .unwrap_or(text)
        .trim();
    text.replace("<Control>", "Ctrl+")
        .replace("<Ctrl>", "Ctrl+")
        .replace("<Alt>", "Alt+")
        .replace("<Shift>", "Shift+")
        .replace("<Super>", "Super+")
        .replace("<Logo>", "Super+")
        .replace("<Meta>", "Meta+")
        .trim()
        .to_string()
}

/// Переводит прогресс отдельного прохода в общий прогресс обработки.
fn overall_progress(progress: &ProgressInfo, asr_passes: u8, total_phases: u8) -> f32 {
    let total = total_phases.max(1) as f32;
    let phase = if progress.stage == "diarization" {
        asr_passes as f32
    } else {
        progress.pass.saturating_sub(1) as f32
    };
    ((phase + progress.percent / 100.0) / total * 100.0).clamp(0.0, 100.0)
}

/// Читает progress.json ML-воркера.
fn read_progress(path: &Path) -> Option<ProgressInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(ProgressInfo {
        stage: value.get("stage")?.as_str()?.to_string(),
        percent: value.get("percent")?.as_f64()? as f32,
        pid: value.get("pid")?.as_u64()? as u32,
        pass: value
            .get("pass")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as u8,
        device: value
            .get("device")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

/// Подбирает усиление микрофона по статистике записи.
fn microphone_gain_db(session_dir: &Path) -> f32 {
    let Ok(manifest) = load_manifest(session_dir) else {
        return 0.0;
    };
    let Some(track) = manifest
        .tracks
        .iter()
        .find(|track| matches!(track.kind, TrackKind::Mic))
    else {
        return 0.0;
    };
    let rms = track.stats.rms_dbfs;
    if rms.is_finite() && rms < -24.0 {
        (-26.0 - rms).clamp(0.0, 18.0)
    } else {
        0.0
    }
}

/// Фактический корень моделей с учётом выбранной папки.
fn models_root_for(paths: &Paths, setting: &str) -> PathBuf {
    resolve_models_root(setting, &paths.models_dir())
}

/// Форматирует размер файла в мегабайтах.
fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.0} МБ", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} КБ", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} Б")
    }
}

/// Показывает системное уведомление.
fn notify(title: &str, body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = Command::new("notify-send")
            .args([title, body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (title, body);
    }
}

/// Копирует транскрипты рядом с экспортированным видео.
fn copy_transcripts_next_to_video(
    video: &Path,
    output: &TranscriptOutput,
) -> Result<Vec<PathBuf>, String> {
    let parent = video
        .parent()
        .ok_or_else(|| "у видео нет каталога".to_string())?;
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("transcript");
    let mut written = Vec::new();
    for (source, extension) in [
        (&output.text_path, "txt"),
        (&output.json_path, "json"),
        (&output.srt_path, "srt"),
        (&output.vtt_path, "vtt"),
        (&output.md_path, "md"),
    ] {
        let target = parent.join(format!("{stem}.{extension}"));
        std::fs::copy(source, &target)
            .map_err(|error| format!("копирование {}: {error}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}

/// Открывает файл или каталог системным обработчиком.
fn open_path(path: &str) {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    #[cfg(windows)]
    let mut command = {
        // Пустой заголовок обязателен: иначе `start` примет путь за него.
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", path]);
        command
    };
    #[cfg(not(any(target_os = "linux", windows)))]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Переводит состояние истории в русскую подпись.
fn state_label(state: &str) -> &'static str {
    match state {
        "recording" => "Идёт запись",
        "finalizing" => "Завершение",
        "recorded" => "Записано",
        "exported" => "Экспортировано",
        "failed" => "Ошибка",
        "interrupted" => "Прервано",
        "cancelled" => "Отменено",
        _ => "Неизвестно",
    }
}

/// Форматирует транскрипт в Markdown.
fn format_markdown(transcript: &Transcript) -> String {
    let mut output = format!("# Транскрипт\n\nЯзык: {}\n\n", transcript.language);
    for segment in &transcript.segments {
        let speaker = segment
            .speaker
            .as_deref()
            .map(|speaker| format!("**{speaker}** "))
            .unwrap_or_default();
        output.push_str(&format!(
            "- [{} - {}] {speaker}{}\n",
            format_ms(segment.start_ms),
            format_ms(segment.end_ms),
            segment.text.trim()
        ));
    }
    output
}

/// Форматирует транскрипт в SRT.
fn format_srt(transcript: &Transcript) -> String {
    let mut output = String::new();
    for (index, segment) in transcript.segments.iter().enumerate() {
        let text = match &segment.speaker {
            Some(speaker) => format!("{speaker}: {}", segment.text.trim()),
            None => segment.text.trim().to_string(),
        };
        output.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            format_srt_time(segment.start_ms),
            format_srt_time(segment.end_ms),
            text
        ));
    }
    output
}

/// Форматирует транскрипт в WebVTT.
fn format_vtt(transcript: &Transcript) -> String {
    let mut output = String::from("WEBVTT\n\n");
    for (index, segment) in transcript.segments.iter().enumerate() {
        let text = match &segment.speaker {
            Some(speaker) => format!("{speaker}: {}", segment.text.trim()),
            None => segment.text.trim().to_string(),
        };
        output.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            format_vtt_time(segment.start_ms),
            format_vtt_time(segment.end_ms),
            text
        ));
    }
    output
}

/// Форматирует время SRT: `ЧЧ:ММ:СС,ммм`.
fn format_srt_time(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

/// Форматирует время WebVTT: `ЧЧ:ММ:СС.ммм`.
fn format_vtt_time(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Сдвигает таймкоды транскрипта на смещение аудио.
///
/// При экспорте ffmpeg задерживает аудиодорожки на `audio_offset_ms`,
/// поэтому субтитры из WAV нужно сдвинуть на ту же величину.
fn shift_transcript(transcript: &mut Transcript, offset_ms: i64) {
    if offset_ms == 0 {
        return;
    }
    for segment in &mut transcript.segments {
        segment.start_ms = segment.start_ms.saturating_add_signed(offset_ms);
        segment.end_ms = segment.end_ms.saturating_add_signed(offset_ms);
    }
}

/// Форматирует транскрипт в текстовый файл с таймкодами.
fn format_transcript(transcript: &Transcript) -> String {
    let mut output = String::new();
    for segment in &transcript.segments {
        let text = match &segment.speaker {
            Some(speaker) => format!("{speaker}: {}", segment.text.trim()),
            None => segment.text.trim().to_string(),
        };
        output.push_str(&format!(
            "[{} - {}] {text}\n",
            format_ms(segment.start_ms),
            format_ms(segment.end_ms)
        ));
    }
    output
}

/// Форматирует миллисекунды как `ЧЧ:ММ:СС.ммм`.
fn format_ms(ms: u64) -> String {
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// Разбирает `x,y,w,h` из файла выбора области.
fn parse_region(text: &str) -> Result<Region, String> {
    let parts: Vec<&str> = text.trim().split(',').collect();
    if parts.len() != 4 {
        return Err("область не выбрана".to_string());
    }
    let numbers: Result<Vec<u32>, _> = parts.iter().map(|part| part.parse::<u32>()).collect();
    let numbers = numbers.map_err(|error| format!("координаты области: {error}"))?;
    if numbers[2] < 2 || numbers[3] < 2 {
        return Err("слишком маленькая область".to_string());
    }
    Ok(Region {
        x: numbers[0],
        y: numbers[1],
        width: numbers[2],
        height: numbers[3],
    })
}

/// Возвращает CRF по подписи из интерфейса.
fn crf_from_label(label: &str) -> u32 {
    match label {
        "Отличное (CRF 18)" => 18,
        "Среднее (CRF 28)" => 28,
        "Сильное сжатие (CRF 35)" => 35,
        _ => 23,
    }
}

/// Возвращает режим аудио для ffmpeg по подписи из интерфейса.
fn audio_mode_from_label(label: &str) -> String {
    match label {
        "Без звука" => "none",
        "Сжать (AAC 128k)" => "aac",
        _ => "copy",
    }
    .to_string()
}

/// Настраивает тёмную палитру под текущий дизайн приложения.
fn configure_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = BG;
        style.visuals.extreme_bg_color = ENTRY;
        style.visuals.faint_bg_color = BTN;
        style.visuals.code_bg_color = ENTRY;
        style.visuals.hyperlink_color = LINK;
        style.visuals.warn_fg_color = HINT;
        style.visuals.selection.bg_fill = BTN;
        style.visuals.selection.stroke.color = WHITE;
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        style.visuals.widgets.noninteractive.bg_fill = BG;
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        style.visuals.widgets.inactive.bg_fill = BTN;
        style.visuals.widgets.inactive.weak_bg_fill = BTN;
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, WHITE);
        style.visuals.widgets.hovered.bg_fill = ENTRY;
        style.visuals.widgets.hovered.weak_bg_fill = ENTRY;
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, WHITE);
        style.visuals.widgets.active.bg_fill = ENTRY;
        style.visuals.widgets.active.weak_bg_fill = ENTRY;
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.text_styles.insert(
            TextStyle::Body,
            FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(18.0, egui::FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(12.0, egui::FontFamily::Monospace),
        );
    });
}

/// Выбирает бэкенд окна с учётом доступных графических серверов.
///
/// По умолчанию на Linux предпочитается X11, если задан `DISPLAY`:
/// XWayland не зависит от frame callback'ов Wayland и окно не «залипает».
/// Принудительный выбор: `SCREENRECORDER_UI_BACKEND=wayland|x11`.
fn preferred_ui_backend() -> UiBackend {
    let requested = std::env::var("SCREENRECORDER_UI_BACKEND").ok();
    select_ui_backend(requested.as_deref(), x11_socket_available())
}

/// Применяет правило выбора бэкенда к уже прочитанным значениям.
///
/// Вынесено отдельно, чтобы проверить приоритеты без изменения окружения.
fn select_ui_backend(requested: Option<&str>, x11_available: bool) -> UiBackend {
    match requested.unwrap_or_default().to_lowercase().as_str() {
        "wayland" => UiBackend::Wayland,
        "x11" => UiBackend::X11,
        _ if x11_available => UiBackend::X11,
        _ => UiBackend::Wayland,
    }
}

/// Проверяет, что локальный X-сервер (или XWayland) доступен.
///
/// Одного `DISPLAY` недостаточно: переменная может остаться от старой
/// сессии, а сокета уже нет. Удалённые дисплеи не проверяем — пусть
/// подключение выполняет winit.
fn x11_socket_available() -> bool {
    let Some(display) = std::env::var_os("DISPLAY") else {
        return false;
    };
    let display = display.to_string_lossy();
    let Some((host, rest)) = display.split_once(':') else {
        return false;
    };
    if !host.is_empty() {
        return true;
    }
    let number = rest.split('.').next().unwrap_or_default();
    if number.is_empty() {
        return false;
    }
    Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
}

/// Отправляет глобальную команду фоновому сервису.
///
/// Используется системными сочетаниями клавиш:
/// `screenrecorder --hotkey stop` и т.п. Окно не требуется.
pub fn send_hotkey(paths: Paths, action: &str) -> Result<(), String> {
    let action = parse_hotkey_action(action)
        .ok_or_else(|| format!("неизвестное действие горячей клавиши: {action}"))?;
    let spawn = !matches!(action, HotkeyAction::Stop | HotkeyAction::Show);
    let mut client = ipc::connect_for_command(&paths, spawn)?;
    match client.request(&Request::Hotkey { action })? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        _ => Err("неожиданный ответ сервиса".to_string()),
    }
}

/// Разбирает действие из аргумента `--hotkey`.
fn parse_hotkey_action(value: &str) -> Option<HotkeyAction> {
    match value {
        "fullscreen" | "full_screen" | "screen" => Some(HotkeyAction::FullScreen),
        "area" => Some(HotkeyAction::Area),
        "program" | "window" => Some(HotkeyAction::Program),
        "audio_only" | "audio-only" | "audio" => Some(HotkeyAction::AudioOnly),
        "stop" => Some(HotkeyAction::Stop),
        "show" => Some(HotkeyAction::Show),
        _ => None,
    }
}

/// Запускает окно интерфейса, подключённое к фоновому сервису.
pub fn run(paths: Paths, settings: Settings) -> Result<(), String> {
    tracing::info!("подключение окна к сервису");
    let backend = preferred_ui_backend();
    tracing::info!(?backend, "бэкенд окна");
    let client = ipc::ensure_daemon(&paths)?;
    #[cfg_attr(windows, allow(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 660.0])
            .with_min_inner_size([380.0, 600.0])
            .with_resizable(false)
            .with_taskbar(false)
            .with_minimize_button(true)
            .with_maximize_button(false)
            .with_close_button(true)
            .with_app_id("screenrecorder")
            .with_title("Рекордер экрана"),
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    {
        type Builder = winit::event_loop::EventLoopBuilder<eframe::UserEvent>;
        options.event_loop_builder = Some(match backend {
            UiBackend::X11 => Box::new(|builder: &mut Builder| {
                builder.with_x11();
            }),
            UiBackend::Wayland => Box::new(|builder: &mut Builder| {
                builder.with_wayland();
            }),
        });
    }

    eframe::run_native(
        // WM_CLASS/application id оставляем ASCII: GNOME показывает его в
        // диалоге «приложение не отвечает»; кириллица там ломается.
        "Screen Recorder",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            configure_style(&cc.egui_ctx);
            Ok(Box::new(ScreenRecorderApp::new_ui(
                paths, settings, client, backend,
            )))
        }),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use screenrecorder_application::TranscriptSegment;

    /// Возвращает тестовый транскрипт.
    fn sample() -> Transcript {
        Transcript {
            language: "ru".to_string(),
            segments: vec![TranscriptSegment {
                start_ms: 1_500,
                end_ms: 3_000,
                text: "привет".to_string(),
                speaker: Some("спикер_1".to_string()),
            }],
        }
    }

    /// Проверяет формат SRT.
    #[test]
    fn formats_srt() {
        let text = format_srt(&sample());
        assert!(text.contains("00:00:01,500 --> 00:00:03,000"));
        assert!(text.contains("спикер_1: привет"));
    }

    /// Проверяет формат WebVTT.
    #[test]
    fn formats_vtt() {
        let text = format_vtt(&sample());
        assert!(text.starts_with("WEBVTT"));
        assert!(text.contains("00:00:01.500 --> 00:00:03.000"));
    }

    /// Проверяет Markdown-экспорт.
    #[test]
    fn formats_markdown() {
        let text = format_markdown(&sample());
        assert!(text.starts_with("# Транскрипт"));
        assert!(text.contains("**спикер_1** привет"));
    }

    /// Проверяет сдвиг субтитров на смещение аудио.
    #[test]
    fn shifts_transcript_by_audio_offset() {
        let mut transcript = sample();
        shift_transcript(&mut transcript, 3_312);
        assert_eq!(transcript.segments[0].start_ms, 4_812);
        assert_eq!(transcript.segments[0].end_ms, 6_312);
        shift_transcript(&mut transcript, -10_000);
        assert_eq!(transcript.segments[0].start_ms, 0);
        assert_eq!(transcript.segments[0].end_ms, 0);
    }

    /// Проверяет разбор команд глобальных горячих клавиш.
    #[test]
    fn parses_hotkey_actions() {
        assert_eq!(
            parse_hotkey_action("fullscreen"),
            Some(HotkeyAction::FullScreen)
        );
        assert_eq!(parse_hotkey_action("area"), Some(HotkeyAction::Area));
        assert_eq!(parse_hotkey_action("program"), Some(HotkeyAction::Program));
        assert_eq!(
            parse_hotkey_action("audio_only"),
            Some(HotkeyAction::AudioOnly)
        );
        assert_eq!(parse_hotkey_action("stop"), Some(HotkeyAction::Stop));
        assert_eq!(parse_hotkey_action("unknown"), None);
    }

    /// Проверяет ввод и разбор пользовательского сочетания.
    #[test]
    fn formats_and_parses_hotkeys() {
        let combo = format_hotkey(
            egui::Key::Num1,
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            },
        )
        .expect("combo");
        assert_eq!(combo, "Ctrl+Shift+1");
        assert!(parse_hotkey(&combo).is_some());
        assert!(parse_hotkey("1").is_none());
    }

    /// Проверяет приоритет X11 и запасной Wayland.
    #[test]
    fn chooses_ui_backend() {
        assert_eq!(select_ui_backend(None, true), UiBackend::X11);
        assert_eq!(select_ui_backend(None, false), UiBackend::Wayland);
        assert_eq!(select_ui_backend(Some("wayland"), true), UiBackend::Wayland);
        assert_eq!(select_ui_backend(Some("x11"), false), UiBackend::X11);
    }

    /// Проверяет очистку пути после пересборки работающего бинарника.
    #[test]
    fn strips_deleted_executable_suffix() {
        assert_eq!(
            clean_executable_path("/var/www/recordingscreen/target/debug/screenrecorder (deleted)"),
            "/var/www/recordingscreen/target/debug/screenrecorder"
        );
        assert_eq!(
            clean_executable_path("/var/www/recordingscreen/target/debug/screenrecorder"),
            "/var/www/recordingscreen/target/debug/screenrecorder"
        );
    }

    /// Проверяет общий прогресс двух проходов ASR и диаризации.
    #[test]
    fn calculates_overall_progress() {
        let info = |stage: &str, percent: f32, pass: u8| ProgressInfo {
            stage: stage.to_string(),
            percent,
            pid: 1,
            pass,
            device: "cpu".to_string(),
        };
        assert!((overall_progress(&info("asr:system", 50.0, 1), 2, 3) - 16.67).abs() < 0.1);
        assert!((overall_progress(&info("asr:mic", 50.0, 2), 2, 3) - 50.0).abs() < 0.1);
        assert!((overall_progress(&info("diarization", 50.0, 0), 2, 3) - 83.33).abs() < 0.1);
    }

    /// Проверяет разбор области.
    #[test]
    fn parses_region() {
        let region = parse_region("10,20,800,600").expect("region");
        assert_eq!(region.x, 10);
        assert_eq!(region.width, 800);
    }

    /// Проверяет копирование транскриптов рядом с видео.
    #[test]
    fn copies_transcripts_next_to_video() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = dir.path().join("session");
        std::fs::create_dir_all(&session).expect("mkdir");
        let names = ["txt", "json", "srt", "vtt", "md"];
        for name in names {
            std::fs::write(session.join(format!("transcript.{name}")), name).expect("file");
        }
        let video = dir.path().join("video.mp4");
        std::fs::write(&video, b"video").expect("video");
        let output = TranscriptOutput {
            text_path: session.join("transcript.txt"),
            json_path: session.join("transcript.json"),
            srt_path: session.join("transcript.srt"),
            vtt_path: session.join("transcript.vtt"),
            md_path: session.join("transcript.md"),
            segments: 0,
            language: "ru".to_string(),
            warning: None,
        };
        let copied = copy_transcripts_next_to_video(&video, &output).expect("copy");
        assert_eq!(copied.len(), 5);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("video.txt")).expect("txt"),
            "txt"
        );
    }
}
