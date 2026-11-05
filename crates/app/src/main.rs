//! Точка входа Screen Recorder 4.0.
//!
//! Поддерживает окно, фоновый сервис, `--diagnose`, `--select-area` и
//! `--ml-worker`.

mod area_selector;
mod logging;

use anyhow::Context;
use screenrecorder_adapters::Paths;
use screenrecorder_application::{Settings, SettingsStore, legacy_settings_candidates};
use serde_json::json;
use std::path::Path;

/// Загружает настройки v4 или мигрирует legacy 3.x.
fn load_settings(paths: &Paths) -> anyhow::Result<Settings> {
    let legacy = legacy_settings_candidates(&paths.settings_path());
    SettingsStore::new(paths.settings_path())
        .load_or_create_migrating(&legacy)
        .context("не удалось загрузить настройки")
}

/// Печатает отчёт о версии, путях, настройках и правах на запись.
fn print_diagnostics(paths: &Paths) -> anyhow::Result<()> {
    let settings = load_settings(paths)?;
    let store = SettingsStore::new(paths.settings_path());

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
            "log_file": logging::path(paths),
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

/// Запускает приложение, сервис, диагностику или выбор области.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("screenrecorder {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--ml-worker") {
        logging::init_stderr();
        return screenrecorder_ml_worker::run_stdio();
    }

    let paths = Paths::discover().context("не удалось определить каталоги приложения")?;
    paths.ensure_all().context("не удалось создать каталоги")?;
    let role = if args.iter().any(|arg| arg == "--daemon") {
        "daemon"
    } else if args.iter().any(|arg| arg == "--select-area") {
        "area-selector"
    } else if args.iter().any(|arg| arg == "--diagnose") {
        "diagnose"
    } else {
        "ui"
    };
    if let Err(error) = logging::init(&paths, role) {
        eprintln!("Файловый лог недоступен: {error}");
    }
    tracing::info!(?args, role, "аргументы запуска");

    if let Some(index) = args.iter().position(|arg| arg == "--select-area") {
        let image = args
            .get(index + 1)
            .context("--select-area: не указан путь к скриншоту")?;
        let out = args
            .get(index + 2)
            .context("--select-area: не указан файл результата")?;
        return area_selector::run(Path::new(image), Path::new(out));
    }
    if args.iter().any(|arg| arg == "--diagnose") {
        return print_diagnostics(&paths);
    }

    let settings = load_settings(&paths)?;
    if args.iter().any(|arg| arg == "--daemon") {
        return screenrecorder_ui::run_daemon(paths, settings)
            .map_err(|error| anyhow::anyhow!("фоновый сервис: {error}"));
    }

    screenrecorder_ui::run(paths, settings)
        .map_err(|error| anyhow::anyhow!("не удалось запустить интерфейс: {error}"))
}
