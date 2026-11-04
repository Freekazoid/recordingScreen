//! Нативный интерфейс v4 на egui/eframe.
//!
//! UI вызывает только прикладные сервисы и не работает с файлами/процессами
//! напрямую. Это позволяет менять интерфейс без переписывания ядра.

use eframe::egui;
use screenrecorder_adapters::Paths;
use screenrecorder_application::Settings;

/// Экран, выбранный в боковом меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Record,
    Settings,
    Diagnostics,
}

/// Главное окно приложения.
pub struct ScreenRecorderApp {
    paths: Paths,
    settings: Settings,
    screen: Screen,
    status: String,
}

impl ScreenRecorderApp {
    /// Создаёт приложение с уже загруженными настройками и путями.
    pub fn new(paths: Paths, settings: Settings) -> Self {
        Self {
            paths,
            settings,
            screen: Screen::Record,
            status: "Готово к проверке системы".to_string(),
        }
    }

    /// Формирует текстовый отчёт о путях и правах на запись.
    fn diagnostics(&self) -> String {
        let data = self
            .paths
            .ensure_writable(&self.paths.data_dir)
            .map(|_| "доступен".to_string())
            .unwrap_or_else(|error| format!("ошибка: {error}"));
        let config = self
            .paths
            .ensure_writable(&self.paths.config_dir)
            .map(|_| "доступен".to_string())
            .unwrap_or_else(|error| format!("ошибка: {error}"));
        let output = self
            .paths
            .ensure_writable(&self.paths.default_output_dir)
            .map(|_| "доступен".to_string())
            .unwrap_or_else(|error| format!("ошибка: {error}"));

        format!(
            "Версия: {}\nData: {} ({data})\nConfig: {} ({config})\nOutput: {} ({output})\n\
             Транскрипция: {} / {}\nДиаризация: {} (enabled={})",
            env!("CARGO_PKG_VERSION"),
            self.paths.data_dir.display(),
            self.paths.config_dir.display(),
            self.paths.default_output_dir.display(),
            self.settings.transcription.engine,
            self.settings.transcription.model,
            self.settings.diarization.engine,
            self.settings.diarization.enabled,
        )
    }

    /// Рисует боковое меню и возвращает выбранный экран.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.heading("Screen Recorder");
        ui.label("4.0 · Rust + egui");
        ui.add_space(16.0);

        for (screen, label) in [
            (Screen::Record, "Запись"),
            (Screen::Settings, "Настройки"),
            (Screen::Diagnostics, "Диагностика"),
        ] {
            if ui.selectable_label(self.screen == screen, label).clicked() {
                self.screen = screen;
            }
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            if ui.button("Выход").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    /// Рисует экран записи.
    fn draw_record(&mut self, ui: &mut egui::Ui) {
        ui.heading("Запись");
        ui.label("Каркас v4: захват и раздельные дорожки будут добавлены на этапе 2.");
        ui.add_space(12.0);
        ui.add_enabled(false, egui::Button::new("Начать запись"));
        ui.add_space(12.0);
        ui.group(|ui| {
            ui.label("Планируемые дорожки:");
            ui.label("• system.wav — системный звук");
            ui.label("• mic.wav — микрофон");
            ui.label("• screen.mkv — видео");
        });
        ui.add_space(12.0);
        if ui.button("Проверить систему").clicked() {
            self.status = self.diagnostics();
        }
        ui.separator();
        ui.label(&self.status);
    }

    /// Рисует экран настроек.
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Настройки");
        ui.label(format!("Файл: {}", self.paths.settings_path().display()));
        ui.add_space(8.0);

        egui::Grid::new("settings_grid")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                ui.label("Язык");
                ui.label(&self.settings.language);
                ui.end_row();

                ui.label("FPS");
                ui.label(self.settings.capture.fps.to_string());
                ui.end_row();

                ui.label("Модель ASR");
                ui.label(&self.settings.transcription.model);
                ui.end_row();

                ui.label("Язык ASR");
                ui.label(&self.settings.transcription.language);
                ui.end_row();

                ui.label("Диаризация");
                ui.label(&self.settings.diarization.engine);
                ui.end_row();

                ui.label("Облако");
                ui.label(if self.settings.privacy.cloud_enabled {
                    "включено"
                } else {
                    "выключено"
                });
                ui.end_row();
            });

        ui.add_space(12.0);
        ui.collapsing("Показать JSON", |ui| {
            let json = serde_json::to_string_pretty(&self.settings)
                .unwrap_or_else(|error| format!("ошибка сериализации: {error}"));
            ui.monospace(json);
        });
    }

    /// Рисует экран диагностики.
    fn draw_diagnostics(&mut self, ui: &mut egui::Ui) {
        ui.heading("Диагностика");
        if ui.button("Обновить отчёт").clicked() {
            self.status = self.diagnostics();
        }
        ui.add_space(8.0);
        ui.monospace(&self.status);
        ui.add_space(8.0);
        ui.label(format!("Модели: {}", self.paths.models_dir().display()));
        ui.label(format!("Temp: {}", self.paths.temp_dir.display()));
    }
}

impl eframe::App for ScreenRecorderApp {
    /// Обновляет интерфейс на каждом кадре.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("sidebar")
            .resizable(false)
            .default_size(180.0)
            .show(ui, |ui| self.sidebar(ui));

        egui::CentralPanel::default().show(ui, |ui| match self.screen {
            Screen::Record => self.draw_record(ui),
            Screen::Settings => self.draw_settings(ui),
            Screen::Diagnostics => self.draw_diagnostics(ui),
        });
    }
}

/// Запускает нативное окно приложения.
pub fn run(paths: Paths, settings: Settings) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("Screen Recorder 4.0"),
        ..Default::default()
    };

    eframe::run_native(
        "Screen Recorder 4.0",
        options,
        Box::new(move |_cc| Ok(Box::new(ScreenRecorderApp::new(paths, settings)))),
    )
}
