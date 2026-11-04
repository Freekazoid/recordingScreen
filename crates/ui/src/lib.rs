//! Нативный интерфейс v4 на egui/eframe.
//!
//! Внешний вид повторяет текущее приложение: тёмная тема, вкладки
//! «Запись / Настройки / О авторе», крупные кнопки режимов, блок экспорта
//! и журнал внизу. UI вызывает только прикладные сервисы и не работает с
//! файлами/процессами напрямую.

use chrono::Local;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    TextStyle, Vec2,
};
use screenrecorder_adapters::{
    AudioDevice, AudioDeviceKind, AudioProbe, DownloadProgress, ExportRequest, LinuxAudioRecorder,
    ModelManager, PORTAL_CANCELLED, Paths, PortalSource, Region, WaylandPortalRecorder, WindowInfo,
    X11VideoRecorder, analyze_track, list_windows, merge_audio, merge_recording, read_levels,
    recover_all_segments, take_screenshot,
};
use screenrecorder_application::{
    DiarizeWorker, HistoryStore, JobRecord, ManifestState, MlWorker, Rect, SessionManifest,
    SessionRecord, Settings, SettingsStore, TrackManifest, Transcript, append_session_log,
    assign_speakers, display_date, import_manifests, load_manifest, recover_incomplete,
    save_manifest,
};
use screenrecorder_domain::{SessionId, TrackKind, TrackStats, TrackStatus};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Результат фоновой загрузки модели.
enum DownloadEvent {
    Progress(DownloadProgress),
    Finished(Result<(), String>),
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
const LOG_TEXT: Color32 = Color32::from_rgb(0xbb, 0xbb, 0x88);
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
    recorder: X11VideoRecorder,
    wayland_recorder: WaylandPortalRecorder,
    using_wayland: bool,
    wayland_pending: bool,
    wayland_started_at: Option<Instant>,
    audio_recorder: LinuxAudioRecorder,
    recording: bool,
    session_dir: Option<PathBuf>,
    model_manager: ModelManager,
    download_rx: Option<Receiver<DownloadEvent>>,
    download_model: Option<&'static str>,
    download_progress: f32,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    folder_target: Option<FolderTarget>,
    settings_dirty: bool,
    audio_devices: Vec<AudioDevice>,
    probe_rx: Option<Receiver<Result<AudioProbe, String>>>,
    probe_note: String,
    export_rx: Option<Receiver<Result<PathBuf, String>>>,
    exporting: bool,
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
    history: HistoryStore,
    history_records: Vec<SessionRecord>,
    export_job: Option<JobRecord>,
    transcription_job: Option<JobRecord>,
    queued_jobs: Vec<JobRecord>,
}

impl ScreenRecorderApp {
    /// Создаёт приложение с уже загруженными настройками и путями.
    pub fn new(paths: Paths, settings: Settings) -> Self {
        let model_manager = ModelManager::new(paths.models_dir());
        let audio_devices = LinuxAudioRecorder::list_devices().unwrap_or_default();
        let recordings = paths.data_dir.join("recordings");
        let recovered = recover_incomplete(&recordings).unwrap_or_default();
        let (recovered_segments, segment_errors) = recover_all_segments(&recordings);
        let history = HistoryStore::new(paths.data_dir.join("history.sqlite3"));
        let mut log = if recovered.is_empty() {
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
        } else if let Ok(imported) = import_manifests(&history, &recordings)
            && imported > 0
        {
            log = format!("{log} В истории записей: {imported}.");
        }
        if let Ok(recovered_jobs) = history.recover_jobs()
            && recovered_jobs > 0
        {
            log = format!("{log} Задач возвращено в очередь: {recovered_jobs}.");
        }
        let queued_jobs = history.queued_jobs(20).unwrap_or_default();
        let history_records = history.list(100).unwrap_or_default();
        Self {
            paths,
            settings,
            screen: Screen::Record,
            status: "Модели не скачаны".to_string(),
            log,
            output_format: "mp4".to_string(),
            video_quality: "Хорошее (CRF 23)".to_string(),
            audio_track_mode: "Как есть (без пережатия)".to_string(),
            recorder: X11VideoRecorder::new(),
            wayland_recorder: WaylandPortalRecorder::new(),
            using_wayland: false,
            wayland_pending: false,
            wayland_started_at: None,
            audio_recorder: LinuxAudioRecorder::new(),
            recording: false,
            session_dir: None,
            model_manager,
            download_rx: None,
            download_model: None,
            download_progress: 0.0,
            folder_rx: None,
            folder_target: None,
            settings_dirty: false,
            audio_devices,
            probe_rx: None,
            probe_note: String::new(),
            export_rx: None,
            exporting: false,
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
            history,
            history_records,
            export_job: None,
            transcription_job: None,
            queued_jobs,
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
            self.log = "Выбор каталога отменён.".to_string();
            return;
        };
        let value = path.to_string_lossy().to_string();
        let title = match target {
            FolderTarget::Models => "Папка моделей",
            FolderTarget::Output => "Папка результатов",
        };
        match target {
            FolderTarget::Models => self.settings.models_dir = value.clone(),
            FolderTarget::Output => self.settings.output_dir = value.clone(),
        }
        self.settings_dirty = true;
        self.log = format!("{title}: {value}");
    }

    /// Сохраняет настройки, если пользователь что-то изменил.
    fn persist_settings(&mut self) {
        if !self.settings_dirty {
            return;
        }
        let store = SettingsStore::new(self.paths.settings_path());
        match store.save(&self.settings) {
            Ok(()) => self.log = "Настройки сохранены автоматически.".to_string(),
            Err(error) => self.status = format!("Ошибка сохранения: {error}"),
        }
        self.settings_dirty = false;
    }

    /// Запускает скачивание модели в фоне.
    fn start_download(&mut self, id: &'static str) {
        if self.download_rx.is_some() {
            self.log = "Другая модель уже скачивается.".to_string();
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
                    self.status = "Модель скачана".to_string();
                    self.log = "Загрузка модели завершена.".to_string();
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
                LinuxAudioRecorder::probe(mic_target.as_deref(), AudioDeviceKind::Mic)
            });
            let system_handle = std::thread::spawn(move || {
                LinuxAudioRecorder::probe(system_target.as_deref(), AudioDeviceKind::System)
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
        if matches!(start, PendingStart::AudioOnly) && !audibly_ok {
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
            self.recording_mode = Some("Только аудио");
            self.status = "Идёт аудиозапись".to_string();
        } else {
            self.fail_manifest("не удалось начать запись звука");
            self.audio_only = false;
            self.recording_mode = None;
            self.status = "Не удалось начать запись звука".to_string();
        }
    }

    /// Запускает запись всего экрана.
    fn start_full_screen(&mut self) {
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
                "ffmpeg-x11grab"
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
                self.recording_mode = Some("Весь экран");
                self.status = "Идёт запись: Весь экран".to_string();
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
                    self.add_manifest_track(
                        TrackKind::Mic,
                        "mic.wav",
                        mic_device,
                        Some(48_000),
                        Some(1),
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
                "ffmpeg-x11grab"
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
                self.recording_mode = Some(mode);
                self.session_dir = Some(session_dir);
                self.status = format!("Идёт запись области {}×{}", region.width, region.height);
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
            self.begin_preflight(PendingStart::ProgramWayland);
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

    /// Останавливает текущую запись.
    fn stop_recording(&mut self) {
        if !self.recording {
            return;
        }
        let was_wayland_pending = self.wayland_pending;
        let was_wayland_active = self.using_wayland && self.wayland_recorder.is_running();
        self.recording = false;
        self.wayland_pending = false;
        self.wayland_started_at = None;
        self.recording_mode = None;
        let audio_only = self.audio_only;
        self.audio_only = false;
        let audio_result = self
            .audio_recorder
            .stop()
            .map_err(|error| error.to_string());
        let video_result: Result<(), String> = if audio_only {
            Ok(())
        } else if self.using_wayland {
            if was_wayland_pending && !was_wayland_active {
                self.wayland_recorder.request_stop();
                Ok(())
            } else {
                self.wayland_recorder.stop()
            }
        } else {
            self.recorder.stop().map_err(|error| error.to_string())
        };
        self.using_wayland = false;
        let dir = self
            .session_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "каталог сессии".to_string());
        if was_wayland_pending && !was_wayland_active {
            // Остановка кнопкой, а не Esc: просто сообщаем, что запись не
            // успела начаться и файла ещё нет.
            self.cancel_manifest("остановлено до получения доступа к экрану");
            self.status = "Запись остановлена".to_string();
            self.log = format!(
                "Остановлено до подтверждения доступа к экрану; файл записи может отсутствовать: {dir}"
            );
            return;
        }
        if let Err(error) = &video_result {
            self.warn_manifest(format!("видео: {error}"));
        }
        if let Err(error) = &audio_result {
            self.warn_manifest(format!("аудио: {error}"));
        }
        self.finalize_manifest(audio_only);
        match (video_result, audio_result) {
            (Ok(()), Ok(())) => {
                self.status = "Запись остановлена".to_string();
                self.log = format!("Запись сохранена: {dir}");
            }
            (video, audio) => {
                self.status = "Запись остановлена с ошибками".to_string();
                self.log = format!("Остановка: видео={video:?}, звук={audio:?}");
            }
        }
        if let Some(session_dir) = self.session_dir.clone() {
            append_session_log(&session_dir, "остановка записи");
            self.start_export(&session_dir, audio_only, None);
        }
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
        let models_dir = self.paths.models_dir();
        let language = self.settings.transcription.language.clone();
        let diarization_enabled = self.settings.diarization.enabled;
        let min_speakers = self.settings.diarization.min_speakers;
        let max_speakers = self.settings.diarization.max_speakers;
        let dir = session_dir.to_path_buf();
        self.log = "ASR: подготовка аудио и запуск воркера…".to_string();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| {
                let mut warning = None;
                let mut worker = MlWorker::spawn(&models_dir)?;
                let transcript = if has_mic && has_system {
                    // Channel-aware: system и mic транскрибируются отдельно,
                    // затем сегменты сливаются по времени.
                    let mut system_transcript =
                        worker.transcribe(&system, Some(&language), true)?;
                    let mut mic_transcript = worker.transcribe(&mic, Some(&language), true)?;
                    for segment in &mut mic_transcript.segments {
                        segment.speaker = Some("Микрофон".to_string());
                    }
                    if diarization_enabled {
                        match DiarizeWorker::spawn(&models_dir).and_then(|mut worker| {
                            worker.diarize(&system, min_speakers, max_speakers)
                        }) {
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
                    let mut transcript = worker.transcribe(&audio, Some(&language), true)?;
                    for segment in &mut transcript.segments {
                        segment.speaker = Some(label.to_string());
                    }
                    transcript
                };
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
        self.transcription_job = self.activate_job(queued, session_dir, "transcribe");
        self.status = "Распознавание речи…".to_string();
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
                    self.status = "Распознавание прервано".to_string();
                    return;
                }
            },
            None => return,
        };
        self.transcript_rx = None;
        self.transcribing = false;
        let job_result = result.as_ref().map(|_| ()).map_err(|error| error.clone());
        let job = self.transcription_job.take();
        self.finish_job(job, job_result);
        match result {
            Ok(output) => {
                self.status = "Транскрипция готова".to_string();
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
            }
            Err(error) => {
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
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = merge_recording(&request).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.export_rx = Some(receiver);
        self.exporting = true;
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
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = merge_audio(system, mic, &target).map_err(|error| error.to_string());
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
                self.status = "Экспорт завершён".to_string();
                self.log = format!("Готово: {}", path.display());
                if let Some(dir) = self.session_dir.clone()
                    && let Ok(mut manifest) = load_manifest(&dir)
                {
                    manifest.set_exported(path.display().to_string());
                    let _ = save_manifest(&dir, &manifest);
                    append_session_log(&dir, &format!("экспорт: {}", path.display()));
                    self.sync_history(&dir);
                }
                if let Some(dir) = self.session_dir.clone() {
                    self.start_transcription(&dir, None);
                }
            }
            Err(error) => {
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
                        self.status = "Идёт запись: Wayland".to_string();
                        self.log = "Портал ScreenCast запущен.".to_string();
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
                    self.start_export(&session_dir, audio_only, None);
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

    /// Рисует строку состояния моделей.
    fn status_row(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(32.0, 24.0), Sense::hover());
            let ready = self.recording || self.model_manager.is_downloaded("whisper");
            let color = if ready { GREEN } else { RED };
            ui.painter().circle_filled(rect.center(), 9.0, color);
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(&self.status).size(14.0).color(TEXT)).wrap());
        });
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
            .width(210.0)
            .selected_text(value.clone())
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
            let result = LinuxAudioRecorder::probe(target, kind).map_err(|error| error.to_string());
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

    /// Рисует строку модели в настройках.
    fn model_row(
        ui: &mut egui::Ui,
        title: &str,
        ready: bool,
        progress: Option<f32>,
        description: &str,
    ) -> bool {
        let mut clicked = false;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, 20.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 5.0, if ready { GREEN } else { RED });
            ui.vertical(|ui| {
                ui.label(RichText::new(title).color(TEXT));
                ui.label(RichText::new(description).size(11.0).color(HINT));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ready {
                    ui.label(RichText::new("Готово").color(GREEN));
                } else if let Some(percent) = progress {
                    ui.add(
                        egui::ProgressBar::new((percent / 100.0).clamp(0.0, 1.0))
                            .desired_width(110.0)
                            .show_percentage(),
                    );
                } else if ui.button("Скачать").clicked() {
                    clicked = true;
                }
            });
        });
        clicked
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
            self.begin_preflight(PendingStart::Region(region, "Программа"));
        }
    }

    /// Рисует вкладку «Запись».
    fn draw_record(&mut self, ui: &mut egui::Ui) {
        self.status_row(ui);
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
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Формат файла:");
                    Self::combo(
                        ui,
                        "output_format",
                        &mut self.output_format,
                        &["mp4", "mkv", "webm", "avi", "mov", "flv"],
                    );
                    ui.end_row();

                    ui.label("Качество видео:");
                    Self::combo(
                        ui,
                        "video_quality",
                        &mut self.video_quality,
                        &[
                            "Отличное (CRF 18)",
                            "Хорошее (CRF 23)",
                            "Среднее (CRF 28)",
                            "Сильное сжатие (CRF 35)",
                        ],
                    );
                    ui.end_row();

                    ui.label("Звуковая дорожка:");
                    Self::combo(
                        ui,
                        "audio_track_mode",
                        &mut self.audio_track_mode,
                        &["Как есть (без пережатия)", "Сжать (AAC 128k)", "Без звука"],
                    );
                    ui.end_row();
                });
        });

        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            if ui.button("Копировать журнал").clicked() {
                ui.ctx().copy_text(self.log.clone());
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(88.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.log)
                                .monospace()
                                .size(12.0)
                                .color(LOG_TEXT),
                        )
                        .selectable(true)
                        .wrap(),
                    );
                });
        });
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
                            if ui.button("Открыть папку").clicked() {
                                open_path(&record.output_dir);
                            }
                            if ui.button("Копировать путь").clicked() {
                                ui.ctx().copy_text(record.output_dir.clone());
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
    }

    /// Рисует вкладку «Настройки».
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        let whisper_ready = self.model_manager.is_downloaded("whisper");
        let diarization_ready = self.model_manager.is_downloaded("diarization");
        let vad_ready = self.model_manager.is_downloaded("vad");
        let whisper_progress =
            (self.download_model == Some("whisper")).then_some(self.download_progress);
        let diarization_progress =
            (self.download_model == Some("diarization")).then_some(self.download_progress);
        let vad_progress = (self.download_model == Some("vad")).then_some(self.download_progress);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut requested: Option<&'static str> = None;
                Self::section(ui, "Модели", |ui| {
                    if Self::model_row(
                        ui,
                        "Whisper",
                        whisper_ready,
                        whisper_progress,
                        "large-v3-turbo · whisper.cpp",
                    ) {
                        requested = Some("whisper");
                    }
                    ui.add_space(4.0);
                    if Self::model_row(
                        ui,
                        "Диаризация",
                        diarization_ready,
                        diarization_progress,
                        "sherpa-onnx · сегментация + эмбеддинги",
                    ) {
                        requested = Some("diarization");
                    }
                    ui.add_space(4.0);
                    if Self::model_row(
                        ui,
                        "Silero VAD",
                        vad_ready,
                        vad_progress,
                        "silero-v5.1.2 · whisper.cpp",
                    ) {
                        requested = Some("vad");
                    }
                });
                if let Some(id) = requested {
                    self.start_download(id);
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
                        .spacing([8.0, 6.0])
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
                            "Включить транскрипцию",
                        )
                        .changed()
                    {
                        self.settings_dirty = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.settings.diarization.enabled,
                            "Включить диаризацию",
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
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Файл: {}", self.paths.settings_path().display()))
                            .size(11.0)
                            .color(HINT),
                    );
                });

                if pick_models {
                    self.pick_folder(FolderTarget::Models);
                }
                if pick_output {
                    self.pick_folder(FolderTarget::Output);
                }

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Изменения сохраняются автоматически")
                        .size(11.0)
                        .color(HINT),
                );
            });
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
}

impl eframe::App for ScreenRecorderApp {
    /// Обновляет интерфейс на каждом кадре.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_recording();
        self.poll_preflight();
        self.poll_transcription();
        self.poll_job_queue();
        self.poll_download();
        self.poll_folder_picker();
        self.poll_probe();
        self.poll_area();
        self.poll_export();
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
        self.persist_settings();
    }
}

/// Открывает файл или каталог системным обработчиком.
fn open_path(path: &str) {
    let _ = Command::new("xdg-open")
        .arg(path)
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

/// Запускает нативное окно приложения.
pub fn run(paths: Paths, settings: Settings) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([380.0, 600.0])
            .with_min_inner_size([380.0, 600.0])
            .with_resizable(false)
            .with_title("Рекордер экрана"),
        ..Default::default()
    };

    eframe::run_native(
        "Рекордер экрана",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            configure_style(&cc.egui_ctx);
            Ok(Box::new(ScreenRecorderApp::new(paths, settings)))
        }),
    )
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
                speaker: Some("SPEAKER_01".to_string()),
            }],
        }
    }

    /// Проверяет формат SRT.
    #[test]
    fn formats_srt() {
        let text = format_srt(&sample());
        assert!(text.contains("00:00:01,500 --> 00:00:03,000"));
        assert!(text.contains("SPEAKER_01: привет"));
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
        assert!(text.contains("**SPEAKER_01** привет"));
    }

    /// Проверяет разбор области.
    #[test]
    fn parses_region() {
        let region = parse_region("10,20,800,600").expect("region");
        assert_eq!(region.x, 10);
        assert_eq!(region.width, 800);
    }
}
