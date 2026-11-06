//! Фоновый сервис Screen Recorder.
//!
//! Сервис владеет записью, аудио, очередью, историей и треем. Окно
//! подключается к нему через Unix-сокет, отображает снимок состояния и
//! отправляет команды. Закрытие окна не останавливает запись.

use crate::ipc;
use crate::protocol::{HotkeyAction, PROTOCOL_VERSION, Request, Response};
use crate::{ScreenRecorderApp, TrayCommand};
use screenrecorder_adapters::Paths;
use screenrecorder_application::Settings;
use std::io::{BufRead, BufReader, Write};
#[cfg(windows)]
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// Событие от клиентского соединения.
enum Event {
    Request {
        request: Box<Request>,
        reply: Sender<Response>,
    },
    Disconnected,
}

/// Запускает фоновый сервис до завершения.
pub fn run_daemon(paths: Paths, settings: Settings) -> Result<(), String> {
    #[cfg(windows)]
    let _lock = match crate::win32::daemon_lock()? {
        Some(lock) => lock,
        None => {
            tracing::info!("сервис уже запущен (mutex)");
            return Ok(());
        }
    };
    let endpoint = ipc::socket_path(&paths);
    ipc::ensure_socket_dir(&endpoint)?;
    #[cfg(unix)]
    let listener = bind_socket(&endpoint)?;
    #[cfg(windows)]
    let (listener, token) = match bind_tcp(&endpoint)? {
        Some((listener, token)) => (Some(listener), token),
        None => (None, String::new()),
    };
    let Some(listener) = listener else {
        tracing::info!("сервис уже запущен");
        return Ok(());
    };
    tracing::info!(endpoint = %endpoint.display(), "сервис запущен");

    let (events_tx, events_rx) = mpsc::channel();
    spawn_accept(listener, events_tx);

    let mut app = ScreenRecorderApp::new_daemon(paths, settings);
    #[cfg(windows)]
    {
        app.daemon_token = Some(token);
    }
    #[cfg(target_os = "macos")]
    {
        let mut app = app;
        let result = run_macos_loop(&mut app, &events_rx);
        let _ = std::fs::remove_file(&endpoint);
        tracing::info!("сервис остановлен");
        return result;
    }

    let mut last_tick = Instant::now();
    let result = loop {
        drain_events(&mut app, &events_rx);
        if app.quit_requested {
            break Ok(());
        }
        #[cfg(windows)]
        crate::win32::pump_messages();
        if last_tick.elapsed() >= Duration::from_millis(100) {
            app.tick_daemon();
            last_tick = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = std::fs::remove_file(&endpoint);
    tracing::info!("сервис остановлен");
    result
}

/// Создаёт Unix-сокет; `None`, если сервис уже обслуживает этот путь.
///
/// Stale-файл после `kill -9` удаляется: иначе новый сервис не смог бы
/// занять адрес.
#[cfg(unix)]
fn bind_socket(socket: &Path) -> Result<Option<UnixListener>, String> {
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Ok(None);
        }
        let _ = std::fs::remove_file(socket);
    }
    let listener = UnixListener::bind(socket)
        .map_err(|error| format!("сокет {}: {error}", socket.display()))?;
    let _ = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600));
    Ok(Some(listener))
}

/// Создаёт локальный TCP-listener и endpoint-файл с портом и токеном.
#[cfg(windows)]
fn bind_tcp(endpoint: &Path) -> Result<Option<(TcpListener, String)>, String> {
    if endpoint.exists() {
        if let Ok(raw) = std::fs::read_to_string(endpoint)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
            && let Some(port) = value.get("port").and_then(|value| value.as_u64())
            && TcpStream::connect(("127.0.0.1", port as u16)).is_ok()
        {
            return Ok(None);
        }
        let _ = std::fs::remove_file(endpoint);
    }
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|error| format!("локальный порт: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("локальный порт: {error}"))?
        .port();
    let token = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({"port": port, "token": token});
    let raw = serde_json::to_vec(&body).map_err(|error| format!("endpoint: {error}"))?;
    std::fs::write(endpoint, raw)
        .map_err(|error| format!("endpoint {}: {error}", endpoint.display()))?;
    Ok(Some((listener, token)))
}

/// Принимает клиентов в отдельном потоке.
#[cfg(unix)]
fn spawn_accept(listener: UnixListener, events: Sender<Event>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let events = events.clone();
            std::thread::spawn(move || handle_client(stream, events));
        }
    });
}

/// Принимает клиентов локального TCP-порта.
#[cfg(windows)]
fn spawn_accept(listener: TcpListener, events: Sender<Event>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let events = events.clone();
            std::thread::spawn(move || handle_client(stream, events));
        }
    });
}

/// Обслуживает одно соединение окна.
fn handle_client(stream: ipc::Stream, events: Sender<Event>) {
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    let mut accepted = false;
    loop {
        let mut line = String::new();
        let Ok(read) = reader.read_line(&mut line) else {
            break;
        };
        if read == 0 {
            break;
        }
        let Ok(request) = serde_json::from_str::<Request>(&line) else {
            tracing::warn!("некорректный запрос окна");
            let response = Response::Error {
                message: "некорректный запрос".to_string(),
            };
            let _ = write_response(&mut writer, &response);
            break;
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        if events
            .send(Event::Request {
                request: Box::new(request),
                reply: reply_tx,
            })
            .is_err()
        {
            break;
        }
        let response = reply_rx
            .recv_timeout(Duration::from_secs(120))
            .unwrap_or_else(|_| Response::Error {
                message: "сервис не ответил вовремя".to_string(),
            });
        if let Response::Hello { accepted: true, .. } = &response {
            accepted = true;
        }
        let rejected = matches!(
            &response,
            Response::Hello {
                accepted: false,
                ..
            }
        );
        if write_response(&mut writer, &response).is_err() || rejected {
            break;
        }
    }
    if accepted {
        let _ = events.send(Event::Disconnected);
    }
}

/// Пишет один JSON-ответ в сокет.
fn write_response(writer: &mut impl Write, response: &Response) -> std::io::Result<()> {
    let line = serde_json::to_string(response).map_err(std::io::Error::other)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Разбирает накопившиеся события и отвечает клиентам.
fn drain_events(app: &mut ScreenRecorderApp, events: &Receiver<Event>) {
    while let Ok(event) = events.try_recv() {
        match event {
            Event::Disconnected => {
                app.ui_clients = app.ui_clients.saturating_sub(1);
                tracing::info!(clients = app.ui_clients, "окно отключилось");
                if app.ui_clients == 0 && app.tray.is_none() {
                    // Без трея закрытое окно нельзя вернуть: не оставляем
                    // невидимый сервис, который продолжил бы запись.
                    app.request_quit();
                }
            }
            Event::Request { request, reply } => {
                let response = handle_request(app, *request);
                let _ = reply.send(response);
            }
        }
    }
}

/// Выполняет один запрос окна.
fn handle_request(app: &mut ScreenRecorderApp, request: Request) -> Response {
    match request {
        Request::Hello { version, token } => {
            if version != PROTOCOL_VERSION {
                return Response::Hello {
                    accepted: false,
                    version: PROTOCOL_VERSION,
                };
            }
            if let Some(expected) = &app.daemon_token
                && token.as_deref() != Some(expected.as_str())
            {
                tracing::warn!("окно с неверным токеном отклонено");
                return Response::Hello {
                    accepted: false,
                    version: PROTOCOL_VERSION,
                };
            }
            if app.ui_clients > 0 {
                tracing::warn!("повторное окно отклонено");
                Response::Hello {
                    accepted: false,
                    version: PROTOCOL_VERSION,
                }
            } else {
                app.ui_clients += 1;
                tracing::info!(clients = app.ui_clients, "окно подключилось");
                Response::Hello {
                    accepted: true,
                    version: PROTOCOL_VERSION,
                }
            }
        }
        Request::Snapshot => Response::Snapshot(app.snapshot()),
        Request::Start(start) => {
            tracing::info!(mode = ?start.mode, region = ?start.region, "запрос старта записи");
            app.request_start(start);
            Response::Ok
        }
        Request::Stop => {
            tracing::info!("запрос остановки записи");
            app.stop_recording();
            Response::Ok
        }
        Request::CancelMl => {
            tracing::info!("запрос отмены ML");
            app.cancel_ml();
            Response::Ok
        }
        Request::DeleteSession { session_id } => match app.delete_session_by_id(&session_id) {
            Ok(()) => Response::Ok,
            Err(message) => Response::Error { message },
        },
        Request::ReloadSettings { settings } => {
            app.apply_settings(*settings);
            Response::Ok
        }
        Request::AckShow => {
            app.show_requested = false;
            Response::Ok
        }
        Request::AckAction => {
            app.ui_action = None;
            Response::Ok
        }
        Request::Hotkey { action } => {
            let command = match action {
                HotkeyAction::FullScreen => TrayCommand::FullScreen,
                HotkeyAction::Area => TrayCommand::Area,
                HotkeyAction::Program => TrayCommand::Program,
                HotkeyAction::AudioOnly => TrayCommand::AudioOnly,
                HotkeyAction::Stop => TrayCommand::Stop,
                HotkeyAction::Show => TrayCommand::Show,
            };
            app.execute_tray_command(command);
            Response::Ok
        }
        Request::HotkeyParent { xid } => {
            #[cfg(target_os = "linux")]
            if let Some(sender) = &app.portal_parent_tx {
                let _ = sender.send(xid);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = xid;
            Response::Ok
        }
        Request::ShowUi => {
            app.request_show_ui();
            Response::Ok
        }
        Request::Quit => {
            tracing::info!("запрос завершения сервиса");
            app.request_quit();
            Response::Ok
        }
    }
}

/// Запускает сервис внутри NSApplication-run-loop на macOS.
///
/// tray-icon и global-hotkey на macOS требуют нативного цикла приложения;
/// обычный `loop { sleep }` не доставляет события меню/клавиш.
#[cfg(target_os = "macos")]
struct MacDaemon<'a> {
    app: &'a mut ScreenRecorderApp,
    events: &'a Receiver<Event>,
    last_tick: Instant,
}

#[cfg(target_os = "macos")]
impl winit::application::ApplicationHandler for MacDaemon<'_> {
    /// Создаёт трей и клавиши после инициализации NSApplication.
    fn resumed(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.app.init_tray();
        self.app.init_hotkeys();
    }

    /// Обслуживает IPC, запись и трей без окна.
    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        drain_events(self.app, &self.events);
        if self.app.quit_requested {
            event_loop.exit();
            return;
        }
        if self.last_tick.elapsed() >= Duration::from_millis(100) {
            self.app.tick_daemon();
            self.last_tick = Instant::now();
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(20),
        ));
    }
}

/// Запускает macOS-run-loop сервиса.
#[cfg(target_os = "macos")]
fn run_macos_loop(app: &mut ScreenRecorderApp, events: &Receiver<Event>) -> Result<(), String> {
    let event_loop = winit::event_loop::EventLoop::builder()
        .build()
        .map_err(|error| format!("event loop macOS: {error}"))?;
    let mut daemon = MacDaemon {
        app,
        events,
        last_tick: Instant::now(),
    };
    event_loop
        .run_app(&mut daemon)
        .map_err(|error| format!("run loop macOS: {error}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Проверяет, что stale-файл сокета заменяется рабочим.
    #[test]
    fn replaces_stale_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("daemon.sock");
        std::fs::write(&socket, b"stale").expect("stale");
        let listener = bind_socket(&socket).expect("bind").expect("listener");
        assert!(UnixStream::connect(&socket).is_ok());
        drop(listener);
    }

    /// Проверяет, что второй сервис не занимает тот же сокет.
    #[test]
    fn detects_running_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("daemon.sock");
        let _listener = UnixListener::bind(&socket).expect("bind");
        assert!(bind_socket(&socket).expect("check").is_none());
    }
}
