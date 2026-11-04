//! Wayland-запись через xdg-desktop-portal ScreenCast и PipeWire.
//!
//! Портал показывает системный диалог выбора экрана/окна. После получения
//! PipeWire FD запускается `gst-launch-1.0` с `pipewiresrc`; запись идёт
//! в MKV. Остановка — SIGINT, чтобы GStreamer корректно закрыл файл.

use super::Region;
use super::portal_token;
use super::segments::concat_segments;
use crate::portal;
use ashpd::desktop::screencast::{
    CursorMode, Screencast, SelectSourcesOptions, SourceType, Stream,
};
use ashpd::desktop::{PersistMode, Session};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Что выбирает пользователь в портале.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSource {
    Monitor,
    Window,
}

/// Сообщение, когда пользователь закрыл диалог портала без подтверждения.
pub const PORTAL_CANCELLED: &str = "доступ к экрану не предоставлен";

/// Рекордер Wayland-экрана через портал.
#[derive(Debug, Default)]
pub struct WaylandPortalRecorder {
    start_rx: Option<Receiver<Result<(), String>>>,
    stop_tx: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
    active: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

impl WaylandPortalRecorder {
    /// Создаёт рекордер без запущенного портала.
    pub fn new() -> Self {
        Self::default()
    }

    /// Запускает портал в фоне; результат доступен через `take_start_result`.
    pub fn start(&mut self, output: &Path, fps: u32) -> Result<(), String> {
        self.start_with_source(output, fps, PortalSource::Monitor, None)
    }

    /// Запускает портал с выбором монитора/окна и необязательной обрезкой.
    pub fn start_with_source(
        &mut self,
        output: &Path,
        fps: u32,
        source: PortalSource,
        crop: Option<Region>,
    ) -> Result<(), String> {
        self.clear_finished();
        if self.worker.is_some() {
            if self.active.load(Ordering::SeqCst) {
                return Err("Wayland-запись уже запущена".to_string());
            }
            // Старый worker застрял в диалоге/ошибке — сбрасываем его.
            self.force_reset();
        }
        let output = output.to_path_buf();
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("каталог сессии: {error}"))?;
        }
        let (start_tx, start_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let active = Arc::new(AtomicBool::new(false));
        let worker_active = Arc::clone(&active);
        let error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&error);

        let worker = thread::spawn(move || {
            let runtime = match portal::runtime() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = start_tx.send(Err(format!("Tokio: {error}")));
                    return;
                }
            };

            let portal = runtime.block_on(open_portal(source, &stop_rx));
            let (proxy, session, fd, node_id, stream_size) = match portal {
                Ok(portal) => portal,
                Err(error) => {
                    let _ = start_tx.send(Err(error));
                    return;
                }
            };
            // Пользователь мог нажать «Остановить», пока висел диалог портала.
            if stop_requested(&stop_rx) {
                let _ = start_tx.send(Err("остановлено пользователем".to_string()));
                close_portal_session(runtime, &session);
                return;
            }

            let raw_fd = fd.as_raw_fd();
            // GStreamer должен унаследовать fd, поэтому снимаем CLOEXEC.
            if unsafe { libc::fcntl(raw_fd, libc::F_SETFD, 0) } == -1 {
                let _ = start_tx.send(Err("не удалось подготовить PipeWire fd".to_string()));
                close_portal_session(runtime, &session);
                return;
            }

            let child = match spawn_gst(raw_fd, node_id, &output, fps, crop, stream_size) {
                Ok(child) => child,
                Err(error) => {
                    let _ = start_tx.send(Err(error));
                    close_portal_session(runtime, &session);
                    return;
                }
            };
            let mut child = child;
            // Даём GStreamer время создать файл; если процесс сразу упал,
            // показываем причину из stderr вместо «запись завершилась».
            thread::sleep(Duration::from_millis(500));
            if let Ok(Some(status)) = child.try_wait() {
                let detail = read_tail(&output.with_extension("gst.log"));
                let message = format!("gst-launch завершился ({status}): {detail}");
                if let Ok(mut slot) = worker_error.lock() {
                    *slot = Some(message.clone());
                }
                let _ = start_tx.send(Err(message));
                close_portal_session(runtime, &session);
                return;
            }
            worker_active.store(true, Ordering::SeqCst);
            let _ = start_tx.send(Ok(()));

            loop {
                if stop_requested(&stop_rx) {
                    break;
                }
                if let Ok(Some(status)) = child.try_wait() {
                    let detail = read_tail(&output.with_extension("gst.log"));
                    if let Ok(mut slot) = worker_error.lock() {
                        *slot = Some(format!("gst-launch завершился ({status}): {detail}"));
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = stop_process(&mut child);
            if let Err(error) = concat_segments(&output)
                && let Ok(mut slot) = worker_error.lock()
            {
                *slot = Some(format!("склейка сегментов: {error}"));
            }
            worker_active.store(false, Ordering::SeqCst);
            close_portal_session(runtime, &session);
            drop(fd);
            drop(proxy);
        });

        self.start_rx = Some(start_rx);
        self.stop_tx = Some(stop_tx);
        self.worker = Some(worker);
        self.active = active;
        self.error = error;
        Ok(())
    }

    /// Возвращает последнюю ошибку GStreamer/портала, если была.
    pub fn take_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Возвращает результат запуска портала, если он уже готов.
    pub fn take_start_result(&mut self) -> Option<Result<(), String>> {
        let receiver = self.start_rx.as_ref()?;
        match receiver.try_recv() {
            Ok(result) => {
                self.start_rx = None;
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.start_rx = None;
                Some(Err("поток портала завершился".to_string()))
            }
        }
    }

    /// Проверяет, идёт ли запись.
    pub fn is_running(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Убирает завершившийся worker, чтобы можно было начать заново.
    pub fn clear_finished(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            self.start_rx = None;
            self.stop_tx = None;
            self.active.store(false, Ordering::SeqCst);
        }
    }

    /// Принудительно сбрасывает застрявший worker, не блокируя UI.
    pub fn force_reset(&mut self) {
        if let Some(sender) = self.stop_tx.take() {
            let _ = sender.send(());
        }
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
        // Worker в диалоге портала завершится сам; join здесь заморозил бы UI.
        self.start_rx = None;
        self.active.store(false, Ordering::SeqCst);
    }

    /// Останавливает запись и закрывает портальную сессию.
    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(sender) = self.stop_tx.take() {
            let _ = sender.send(());
        }
        if let Some(worker) = self.worker.take() {
            let result = worker
                .join()
                .map_err(|_| "поток Wayland-записи завершился с ошибкой".to_string());
            self.start_rx = None;
            self.active.store(false, Ordering::SeqCst);
            return result;
        }
        self.start_rx = None;
        self.active.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Просит остановиться, не блокируя UI (портал может ждать диалог).
    pub fn request_stop(&mut self) {
        self.force_reset();
    }
}

impl Drop for WaylandPortalRecorder {
    /// Гарантирует остановку портальной сессии.
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Открывает портал и получает PipeWire FD + node id.
async fn open_portal(
    source: PortalSource,
    stop_rx: &Receiver<()>,
) -> Result<(Screencast, Session<Screencast>, OwnedFd, u32, (u32, u32)), String> {
    let connection = portal::connect().await?;
    let proxy = Screencast::with_connection(connection)
        .await
        .map_err(|error| format!("Screencast: {error}"))?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|error| format!("CreateSession: {error}"))?;
    // Портал может не ответить (зависший диалог, другой ScreenCast-сеанс).
    // Таймаут обязателен, иначе worker остаётся в ожидании навсегда.
    // Остановку проверяем прямо во время диалога, чтобы не копить
    // зависшие запросы и не блокировать следующий запуск.
    let stream = tokio::select! {
        result = tokio::time::timeout(
            Duration::from_secs(60),
            start_stream(&proxy, &session, source),
        ) => result,
        () = wait_for_stop(stop_rx) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
            return Err("остановлено пользователем".to_string());
        }
    };
    match stream {
        Ok(Ok((fd, node_id, stream_size))) => Ok((proxy, session, fd, node_id, stream_size)),
        Ok(Err(error)) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
            Err(error)
        }
        Err(_) => {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
            Err("таймаут ожидания портала (60 с)".to_string())
        }
    }
}

/// Асинхронно ждёт сигнал остановки от UI.
async fn wait_for_stop(stop_rx: &Receiver<()>) {
    loop {
        if stop_requested(stop_rx) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Закрывает портальную сессию, не дожидаясь зависшего портала.
fn close_portal_session(runtime: &tokio::runtime::Runtime, session: &Session<Screencast>) {
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
    });
}

/// Запрашивает источник у портала и открывает PipeWire-поток.
async fn start_stream(
    proxy: &Screencast,
    session: &Session<Screencast>,
    source: PortalSource,
) -> Result<(OwnedFd, u32, (u32, u32)), String> {
    let source_type = match source {
        PortalSource::Monitor => SourceType::Monitor,
        PortalSource::Window => SourceType::Window,
    };
    // Для монитора сохраняем разрешение: тогда GNOME не показывает диалог
    // при каждом запуске. Для окна токен не используем — пользователь
    // каждый раз выбирает нужное окно.
    let restore_token = match source {
        PortalSource::Monitor => portal_token::load(source),
        PortalSource::Window => None,
    };
    let persist_mode = match source {
        PortalSource::Monitor => Some(PersistMode::ExplicitlyRevoked),
        PortalSource::Window => Some(PersistMode::DoNot),
    };
    proxy
        .select_sources(
            session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from(source_type))
                .set_multiple(false)
                .set_persist_mode(persist_mode)
                .set_restore_token(restore_token.as_deref()),
        )
        .await
        .map_err(|error| format!("SelectSources: {error}"))?;
    let response = proxy
        .start(session, None, Default::default())
        .await
        .map_err(|error| format!("Start: {error}"))?
        .response()
        .map_err(|error| match error {
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
                PORTAL_CANCELLED.to_string()
            }
            error => format!("Start response: {error}"),
        })?;
    // Токен одноразовый: сохраняем новый, иначе следующий запуск снова
    // покажет диалог.
    if source == PortalSource::Monitor {
        portal_token::save(source, response.restore_token());
    }
    let stream: &Stream = response
        .streams()
        .first()
        .ok_or_else(|| "портал не вернул поток".to_string())?;
    let node_id = stream.pipe_wire_node_id();
    let stream_size = stream.size().map_or((0, 0), |(w, h)| (w as u32, h as u32));
    let fd = proxy
        .open_pipe_wire_remote(session, Default::default())
        .await
        .map_err(|error| format!("OpenPipeWireRemote: {error}"))?;
    Ok((fd, node_id, stream_size))
}

/// Проверяет, просили ли воркер остановиться.
///
/// Закрытый канал тоже считается остановкой: `force_reset` мог уронить
/// отправителя, пока воркер ещё висел в диалоге портала.
fn stop_requested(stop_rx: &Receiver<()>) -> bool {
    match stop_rx.try_recv() {
        Ok(()) | Err(mpsc::TryRecvError::Disconnected) => true,
        Err(mpsc::TryRecvError::Empty) => false,
    }
}

/// Запускает GStreamer-пайплайн записи PipeWire в MKV.
fn spawn_gst(
    fd: i32,
    node_id: u32,
    output: &Path,
    fps: u32,
    crop: Option<Region>,
    stream_size: (u32, u32),
) -> Result<Child, String> {
    let args = build_gst_args(fd, node_id, output, fps, crop, stream_size);
    let log_path = output.with_extension("gst.log");
    let log = std::fs::File::create(&log_path)
        .map_err(|error| format!("лог GStreamer {}: {error}", log_path.display()))?;
    Command::new("gst-launch-1.0")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| format!("gst-launch-1.0: {error}"))
}

/// Читает последние строки лога GStreamer.
fn read_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(8).collect();
    if lines.is_empty() {
        "нет вывода gst-launch".to_string()
    } else {
        lines.into_iter().rev().collect::<Vec<_>>().join(" | ")
    }
}

/// Собирает аргументы `gst-launch-1.0`.
fn build_gst_args(
    fd: i32,
    node_id: u32,
    output: &Path,
    fps: u32,
    crop: Option<Region>,
    stream_size: (u32, u32),
) -> Vec<String> {
    let mut args = vec![
        "-e".to_string(),
        "pipewiresrc".to_string(),
        format!("fd={fd}"),
        format!("path={node_id}"),
        "always-copy=true".to_string(),
        "do-timestamp=true".to_string(),
        "!".to_string(),
        "queue".to_string(),
        "!".to_string(),
        "videoconvert".to_string(),
        "!".to_string(),
    ];
    if let (Some(region), (stream_w, stream_h)) = (crop, stream_size)
        && stream_w > 1
        && stream_h > 1
        && region.width > 0
        && region.height > 0
    {
        // I420 требует чётные ширину и высоту, иначе x264enc может
        // отказаться запускать конвейер. Округляем область вниз.
        let left = region.x.min(stream_w - 2) & !1;
        let top = region.y.min(stream_h - 2) & !1;
        let width = (region.width.min(stream_w - left) & !1).max(2);
        let height = (region.height.min(stream_h - top) & !1).max(2);
        let right = stream_w.saturating_sub(left + width);
        let bottom = stream_h.saturating_sub(top + height);
        args.extend([
            "videocrop".to_string(),
            format!("left={left}"),
            format!("right={right}"),
            format!("top={top}"),
            format!("bottom={bottom}"),
            "!".to_string(),
        ]);
    }
    args.extend([
        "videorate".to_string(),
        "!".to_string(),
        format!("video/x-raw,format=I420,framerate={fps}/1"),
        "!".to_string(),
        "x264enc".to_string(),
        "tune=zerolatency".to_string(),
        "speed-preset=veryfast".to_string(),
        "bitrate=8000".to_string(),
        "key-int-max=40".to_string(),
        "!".to_string(),
        "h264parse".to_string(),
        "!".to_string(),
        "splitmuxsink".to_string(),
        format!(
            "location={}",
            super::segments::segment_pattern(output).display()
        ),
        "max-size-time=60000000000".to_string(),
        "muxer=matroskamux".to_string(),
    ]);
    args
}

/// Останавливает GStreamer через SIGINT и ждёт завершения.
fn stop_process(child: &mut Child) -> Result<(), String> {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        for _ in 0..50 {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(error) => return Err(format!("ожидание gst: {error}")),
            }
        }
        child.kill().map_err(|error| format!("kill gst: {error}"))?;
    }
    #[cfg(not(unix))]
    {
        child.kill().map_err(|error| format!("kill gst: {error}"))?;
    }
    child.wait().map_err(|error| format!("wait gst: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что pipeline содержит нужные элементы.
    #[test]
    fn builds_gst_pipeline() {
        let args = build_gst_args(7, 42, Path::new("/tmp/out.mkv"), 20, None, (1920, 1080));
        assert!(args.contains(&"pipewiresrc".to_string()));
        assert!(args.contains(&"fd=7".to_string()));
        assert!(args.contains(&"path=42".to_string()));
        assert!(args.contains(&"videorate".to_string()));
        assert!(args.contains(&"video/x-raw,format=I420,framerate=20/1".to_string()));
        assert!(args.contains(&"splitmuxsink".to_string()));
        assert!(args.contains(&"location=/tmp/out-%03d.mkv".to_string()));
    }

    /// Проверяет обрезку области через videocrop.
    #[test]
    fn builds_crop_pipeline() {
        let args = build_gst_args(
            7,
            42,
            Path::new("/tmp/out.mkv"),
            20,
            Some(Region {
                x: 100,
                y: 50,
                width: 800,
                height: 600,
            }),
            (1920, 1080),
        );
        assert!(args.contains(&"videocrop".to_string()));
        assert!(args.contains(&"left=100".to_string()));
        assert!(args.contains(&"top=50".to_string()));
        assert!(args.contains(&"right=1020".to_string()));
        assert!(args.contains(&"bottom=430".to_string()));
    }

    /// Проверяет, что нечётная область округляется до чётных размеров.
    #[test]
    fn rounds_odd_crop_to_even() {
        let args = build_gst_args(
            7,
            42,
            Path::new("/tmp/out.mkv"),
            20,
            Some(Region {
                x: 101,
                y: 51,
                width: 801,
                height: 601,
            }),
            (1920, 1080),
        );
        assert!(args.contains(&"left=100".to_string()));
        assert!(args.contains(&"top=50".to_string()));
        assert!(args.contains(&"right=1020".to_string()));
        assert!(args.contains(&"bottom=430".to_string()));
    }

    /// Проверяет, что закрытый канал остановки трактуется как отмена.
    #[test]
    fn disconnected_stop_channel_cancels_worker() {
        let (sender, receiver) = mpsc::channel::<()>();
        drop(sender);
        assert!(stop_requested(&receiver));
    }

    /// Проверяет, что пустой канал не считается остановкой.
    #[test]
    fn empty_stop_channel_keeps_worker_running() {
        let (_sender, receiver) = mpsc::channel::<()>();
        assert!(!stop_requested(&receiver));
    }
}
