//! Фоновый сервис Screen Recorder.
//!
//! Сервис владеет записью, аудио, очередью, историей и треем. Окно
//! подключается к нему через Unix-сокет, отображает снимок состояния и
//! отправляет команды. Закрытие окна не останавливает запись.

use crate::ScreenRecorderApp;
use crate::ipc;
use crate::protocol::{PROTOCOL_VERSION, Request, Response};
use screenrecorder_adapters::Paths;
use screenrecorder_application::Settings;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
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
    let socket = ipc::socket_path(&paths);
    ipc::ensure_socket_dir(&socket)?;
    let Some(listener) = bind_socket(&socket)? else {
        tracing::info!("сервис уже запущен");
        return Ok(());
    };
    tracing::info!(socket = %socket.display(), "сервис запущен");

    let (events_tx, events_rx) = mpsc::channel();
    spawn_accept(listener, events_tx);

    let mut app = ScreenRecorderApp::new_daemon(paths, settings);
    let mut last_tick = Instant::now();
    let result = loop {
        drain_events(&mut app, &events_rx);
        if app.quit_requested {
            break Ok(());
        }
        if last_tick.elapsed() >= Duration::from_millis(100) {
            app.tick_daemon();
            last_tick = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = std::fs::remove_file(&socket);
    tracing::info!("сервис остановлен");
    result
}

/// Создаёт сокет; `None`, если сервис уже обслуживает этот путь.
///
/// Stale-файл после `kill -9` удаляется: иначе новый сервис не смог бы
/// занять адрес.
fn bind_socket(socket: &std::path::Path) -> Result<Option<UnixListener>, String> {
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

/// Принимает клиентов в отдельном потоке.
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

/// Обслуживает одно соединение окна.
fn handle_client(stream: UnixStream, events: Sender<Event>) {
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
fn write_response(writer: &mut UnixStream, response: &Response) -> std::io::Result<()> {
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
        Request::Hello { version } => {
            if version != PROTOCOL_VERSION {
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
        Request::HotkeyParent { xid } => {
            if let Some(sender) = &app.portal_parent_tx {
                let _ = sender.send(xid);
            }
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

#[cfg(test)]
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
