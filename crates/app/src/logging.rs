//! Файловое логирование приложения.
//!
//! Окно и фоновый сервис пишут в один `app.log` с метками времени и
//! уровнями. Это нужно для разбора проблем в рабочем режиме, когда
//! stderr процесса недоступен.

use screenrecorder_adapters::Paths;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// Ограничение размера текущего лога перед ротацией.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Возвращает путь к общему логу приложения.
pub fn path(paths: &Paths) -> PathBuf {
    paths.log_dir.join("app.log")
}

/// Инициализирует запись в `app.log`.
pub fn init(paths: &Paths, role: &str) -> Result<(), String> {
    std::fs::create_dir_all(&paths.log_dir)
        .map_err(|error| format!("каталог логов {}: {error}", paths.log_dir.display()))?;
    let log_path = path(paths);
    rotate(&log_path);
    let writer = FileMakeWriter {
        path: log_path.clone(),
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "warn,screenrecorder=debug,screenrecorder_ui=debug,\
             screenrecorder_adapters=debug,screenrecorder_application=debug,\
             screenrecorder_ml=debug,screenrecorder_ml_worker=debug",
        )
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .try_init();
    set_panic_hook(log_path);
    tracing::info!(
        role,
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "запуск"
    );
    Ok(())
}

/// Инициализирует stderr-логирование для процесса ML-воркера.
pub fn init_stderr() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .try_init();
}

/// Пишет панику в лог, чтобы она не терялась в stderr.
fn set_panic_hook(log_path: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let message = format!(
            "{} PANIC pid={} {info}\n",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
            std::process::id()
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = file.write_all(message.as_bytes());
        }
    }));
}

/// Ротирует лог, если он превысил лимит.
fn rotate(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_LOG_BYTES {
        return;
    }
    let previous = path.with_extension("log.1");
    let _ = std::fs::remove_file(&previous);
    let _ = std::fs::rename(path, previous);
}

/// Открывает файл на каждую запись: два процесса могут писать одновременно.
#[derive(Clone)]
struct FileMakeWriter {
    path: PathBuf,
}

impl<'a> MakeWriter<'a> for FileMakeWriter {
    type Writer = FileWriter;

    /// Создаёт writer для одной записи.
    fn make_writer(&'a self) -> Self::Writer {
        FileWriter {
            file: std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok(),
        }
    }
}

/// Writer с откатом в stderr, если файл недоступен.
struct FileWriter {
    file: Option<std::fs::File>,
}

impl Write for FileWriter {
    /// Пишет в файл или stderr при ошибке открытия.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.file.as_mut() {
            Some(file) => file.write(buffer),
            None => io::stderr().write(buffer),
        }
    }

    /// Сбрасывает буфер.
    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => io::stderr().flush(),
        }
    }
}
