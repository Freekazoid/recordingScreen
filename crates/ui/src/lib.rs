//! Нативный интерфейс v4 на egui/eframe.
//!
//! Внешний вид повторяет текущее приложение: тёмная тема, вкладки
//! «Запись / Настройки / О авторе», крупные кнопки режимов, блок экспорта
//! и журнал внизу. UI вызывает только прикладные сервисы и не работает с
//! файлами/процессами напрямую.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke,
    TextStyle, Vec2,
};
use screenrecorder_adapters::Paths;
use screenrecorder_application::{Settings, SettingsStore};

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
}

impl ScreenRecorderApp {
    /// Создаёт приложение с уже загруженными настройками и путями.
    pub fn new(paths: Paths, settings: Settings) -> Self {
        Self {
            paths,
            settings,
            screen: Screen::Record,
            status: "Модели не скачаны".to_string(),
            log: "Готово. Проверьте систему и настройки.".to_string(),
            output_format: "mp4".to_string(),
            video_quality: "Хорошее (CRF 23)".to_string(),
            audio_track_mode: "Как есть (без пережатия)".to_string(),
        }
    }

    /// Рисует строку состояния моделей.
    fn status_row(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(32.0, 24.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 9.0, RED);
            ui.add_space(4.0);
            ui.label(RichText::new(&self.status).size(14.0).color(TEXT));
        });
    }

    /// Рисует вкладки в стиле текущего приложения.
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width() / 3.0;
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            for (screen, label) in [
                (Screen::Record, "Запись"),
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
    }

    /// Рисует кнопку режима записи с иконкой.
    fn record_button(ui: &mut egui::Ui, image: egui::Image<'_>, label: &str) -> egui::Response {
        ui.add(
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
        egui::Frame::NONE
            .stroke(Stroke::new(1.0, BTN))
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                ui.label(RichText::new(title).strong().color(TEXT));
                add_contents(ui)
            })
            .inner
    }

    /// Рисует выпадающий список.
    fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[&str]) {
        egui::ComboBox::from_id_salt(id)
            .width(200.0)
            .selected_text(value.clone())
            .show_ui(ui, |ui| {
                for option in options {
                    ui.selectable_value(value, option.to_string(), *option);
                }
            });
    }

    /// Рисует строку модели в настройках.
    fn model_row(ui: &mut egui::Ui, title: &str, ready: bool, description: &str) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, 20.0), Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 5.0, if ready { GREEN } else { RED });
            ui.vertical(|ui| {
                ui.label(RichText::new(title).color(TEXT));
                ui.label(RichText::new(description).size(11.0).color(HINT));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled(false, egui::Button::new("Скачать"));
            });
        });
    }

    /// Рисует вкладку «Запись».
    fn draw_record(&mut self, ui: &mut egui::Ui) {
        self.status_row(ui);
        ui.add_space(6.0);

        ui.vertical_centered(|ui| {
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/fullscreen.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                "Весь экран",
            )
            .clicked()
            {
                self.log = "Режим «Весь экран» будет добавлен на этапе 2.".to_string();
            }
            ui.add_space(8.0);
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/area.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                "Область экрана",
            )
            .clicked()
            {
                self.log = "Режим «Область экрана» будет добавлен на этапе 2.".to_string();
            }
            ui.add_space(8.0);
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/program.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                "Программа",
            )
            .clicked()
            {
                self.log = "Режим «Программа» будет добавлен на этапе 2.".to_string();
            }
            ui.add_space(8.0);
            if Self::record_button(
                ui,
                egui::Image::new(egui::include_image!("../../../assets/empty.png"))
                    .fit_to_exact_size(Vec2::new(24.0, 24.0)),
                "Только аудио",
            )
            .clicked()
            {
                self.log = "Режим «Только аудио» будет добавлен на этапе 2.".to_string();
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
            ui.add(
                egui::TextEdit::multiline(&mut self.log)
                    .desired_rows(4)
                    .font(TextStyle::Monospace)
                    .text_color(LOG_TEXT)
                    .interactive(false)
                    .frame(egui::Frame::NONE.fill(BG)),
            );
        });
    }

    /// Рисует вкладку «Настройки».
    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            Self::section(ui, "Модели", |ui| {
                Self::model_row(ui, "Whisper", false, "large-v3-turbo · whisper.cpp");
                ui.add_space(4.0);
                Self::model_row(
                    ui,
                    "Диаризация",
                    false,
                    "sherpa-onnx · сегментация + эмбеддинги",
                );
            });

            ui.add_space(6.0);
            Self::section(ui, "Распознавание", |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Транскрипция:");
                        Self::combo(
                            ui,
                            "transcription_engine",
                            &mut self.settings.transcription.engine,
                            &["whisper-rs"],
                        );
                        ui.end_row();

                        ui.label("Спикеры:");
                        Self::combo(
                            ui,
                            "diarization_engine",
                            &mut self.settings.diarization.engine,
                            &["sherpa-onnx", "none"],
                        );
                        ui.end_row();

                        ui.label("Устройство:");
                        Self::combo(
                            ui,
                            "compute_device",
                            &mut self.settings.transcription.compute_device,
                            &["auto", "cpu", "gpu"],
                        );
                        ui.end_row();

                        ui.label("Язык:");
                        Self::combo(
                            ui,
                            "language",
                            &mut self.settings.transcription.language,
                            &["auto", "ru", "en"],
                        );
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.settings.transcription.enabled,
                    "Включить транскрипцию",
                );
                ui.checkbox(
                    &mut self.settings.diarization.enabled,
                    "Включить диаризацию",
                );
            });

            ui.add_space(6.0);
            Self::section(ui, "Папки", |ui| {
                ui.label("Папка моделей:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.models_dir)
                        .hint_text("по умолчанию"),
                );
                ui.add_space(4.0);
                ui.label("Папка результатов:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.output_dir)
                        .hint_text("по умолчанию"),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("Файл: {}", self.paths.settings_path().display()))
                        .size(11.0)
                        .color(HINT),
                );
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Сохранить настройки").clicked() {
                    let store = SettingsStore::new(self.paths.settings_path());
                    self.status = match store.save(&self.settings) {
                        Ok(()) => "Настройки сохранены".to_string(),
                        Err(error) => format!("Ошибка сохранения: {error}"),
                    };
                }
                if ui.button("Проверить систему").clicked() {
                    self.status = self.diagnostics();
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new(&self.status).color(HINT));
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

    /// Проверяет пути и возвращает текстовый отчёт.
    fn diagnostics(&self) -> String {
        let check = |path: &std::path::Path| {
            self.paths
                .ensure_writable(path)
                .map(|_| "доступен".to_string())
                .unwrap_or_else(|error| format!("ошибка: {error}"))
        };
        format!(
            "Data: {} ({})\nConfig: {} ({})\nOutput: {} ({})",
            self.paths.data_dir.display(),
            check(&self.paths.data_dir),
            self.paths.config_dir.display(),
            check(&self.paths.config_dir),
            self.paths.default_output_dir.display(),
            check(&self.paths.default_output_dir),
        )
    }
}

impl eframe::App for ScreenRecorderApp {
    /// Обновляет интерфейс на каждом кадре.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::same(5)))
            .show(ui, |ui| {
                self.tab_bar(ui);
                ui.add_space(4.0);
                match self.screen {
                    Screen::Record => self.draw_record(ui),
                    Screen::Settings => self.draw_settings(ui),
                    Screen::About => self.draw_about(ui),
                }
            });
    }
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
