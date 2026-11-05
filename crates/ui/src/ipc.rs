//! Транспорт между окном и фоновым сервисом.
//!
//! Linux-first: используется Unix-сокет в `XDG_RUNTIME_DIR` (или в каталоге
//! данных приложения). Формат — по одному JSON-сообщению на строку.

use crate::protocol::{PROTOCOL_VERSION, Request, Response};
use screenrecorder_adapters::Paths;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Путь к сокету фонового сервиса.
pub fn socket_path(paths: &Paths) -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime)
            .join("screenrecorder")
            .join("daemon.sock")
    } else {
        paths.data_dir.join("daemon.sock")
    }
}

/// Создаёт каталог для сокета.
pub fn ensure_socket_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("каталог сокета {}: {error}", parent.display()))?;
    }
    Ok(())
}

/// Подключение к сервису.
pub struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Подключается к уже запущенному сервису.
    pub fn connect(path: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(path)
            .map_err(|error| format!("сокет {}: {error}", path.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("таймаут сокета: {error}"))?;
        let reader_stream = stream
            .try_clone()
            .map_err(|error| format!("копия сокета: {error}"))?;
        Ok(Self {
            writer: stream,
            reader: BufReader::new(reader_stream),
        })
    }

    /// Отправляет запрос и читает ответ.
    pub fn request(&mut self, request: &Request) -> Result<Response, String> {
        let line = serde_json::to_string(request)
            .map_err(|error| format!("сериализация запроса: {error}"))?;
        self.writer
            .write_all(line.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .and_then(|_| self.writer.flush())
            .map_err(|error| format!("запись в сокет: {error}"))?;
        let mut response = String::new();
        self.reader
            .read_line(&mut response)
            .map_err(|error| format!("чтение ответа: {error}"))?;
        if response.is_empty() {
            return Err("сервис закрыл соединение".to_string());
        }
        serde_json::from_str(&response).map_err(|error| format!("ответ сервиса: {error}"))
    }

    /// Регистрирует окно и проверяет версию протокола.
    pub fn hello(&mut self) -> Result<bool, String> {
        match self.request(&Request::Hello {
            version: PROTOCOL_VERSION,
        })? {
            Response::Hello { accepted, version } => {
                if version != PROTOCOL_VERSION {
                    return Err(format!(
                        "версия сервиса {version}, окна {PROTOCOL_VERSION}; перезапустите приложение"
                    ));
                }
                Ok(accepted)
            }
            Response::Error { message } => Err(message),
            _ => Err("неожиданный ответ на hello".to_string()),
        }
    }
}

/// Запускает фоновый сервис, если он ещё не запущен.
pub fn spawn_daemon(paths: &Paths) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("не удалось найти исполняемый файл: {error}"))?;
    std::fs::create_dir_all(&paths.log_dir)
        .map_err(|error| format!("каталог логов {}: {error}", paths.log_dir.display()))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir.join("daemon.log"))
        .map_err(|error| format!("лог сервиса: {error}"))?;
    Command::new(exe)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| format!("запуск сервиса: {error}"))?;
    Ok(())
}

/// Подключается к сервису, при необходимости запуская его.
pub fn ensure_daemon(paths: &Paths) -> Result<Client, String> {
    let path = socket_path(paths);
    ensure_socket_dir(&path)?;
    let mut launched = false;
    let started = Instant::now();
    loop {
        match Client::connect(&path) {
            Ok(mut client) => match client.hello() {
                Ok(true) => return Ok(client),
                Ok(false) => {
                    let _ = client.request(&Request::ShowUi);
                    return Err("окно уже открыто".to_string());
                }
                Err(error) if error.contains("версия сервиса") => return Err(error),
                Err(_) if !launched => {
                    spawn_daemon(paths)?;
                    launched = true;
                }
                Err(error) => {
                    if started.elapsed() > Duration::from_secs(10) {
                        return Err(error);
                    }
                }
            },
            Err(_) => {
                if !launched {
                    spawn_daemon(paths)?;
                    launched = true;
                } else if started.elapsed() > Duration::from_secs(10) {
                    return Err("фоновый сервис не запустился за 10 секунд".to_string());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
