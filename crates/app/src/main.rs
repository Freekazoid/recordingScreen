//! Точка входа Screen Recorder 4.0.
//!
//! Поддерживает `--version` и `--diagnose` для проверки окружения без
//! запуска графического интерфейса.

use anyhow::Context;
use screenrecorder_adapters::Paths;
use screenrecorder_application::SettingsStore;
use serde_json::json;
use tracing_subscriber::EnvFilter;

/// Инициализирует структурное логирование в stderr.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// Печатает отчёт о версии, путях, настройках и правах на запись.
fn print_diagnostics() -> anyhow::Result<()> {
    let paths = Paths::discover().context("не удалось определить каталоги приложения")?;
    paths.ensure_all().context("не удалось создать каталоги")?;
    let store = SettingsStore::new(paths.settings_path());
    let settings = store
        .load_or_create()
        .context("не удалось загрузить настройки")?;

    let writable = |path: &std::path::Path| paths.ensure_writable(path).is_ok();
    let report = json!({
        "name": "Screen Recorder",
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "paths": {
            "data": paths.data_dir,
            "config": paths.config_dir,
            "cache": paths.cache_dir,
            "logs": paths.log_dir,
            "temp": paths.temp_dir,
            "output": paths.default_output_dir,
            "settings": store.path(),
        },
        "writable": {
            "data": writable(&paths.data_dir),
            "config": writable(&paths.config_dir),
            "output": writable(&paths.default_output_dir),
        },
        "settings": settings,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

/// Запускает приложение или диагностику.
fn main() -> anyhow::Result<()> {
    init_tracing();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("screenrecorder {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--diagnose") {
        return print_diagnostics();
    }

    let paths = Paths::discover().context("не удалось определить каталоги приложения")?;
    paths.ensure_all().context("не удалось создать каталоги")?;
    let settings = SettingsStore::new(paths.settings_path())
        .load_or_create()
        .context("не удалось загрузить настройки")?;

    screenrecorder_ui::run(paths, settings)
        .map_err(|error| anyhow::anyhow!("не удалось запустить интерфейс: {error}"))
}
