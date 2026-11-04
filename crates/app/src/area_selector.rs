//! Полноэкранный выбор области по снимку экрана.
//!
//! Запускается отдельным режимом `--select-area`: показывает скриншот на
//! весь экран, пользователь выделяет прямоугольник, координаты пишутся в
//! файл. Основной процесс читает их и запускает запись области.

use anyhow::Context;
use eframe::egui;
use std::path::{Path, PathBuf};

/// Запускает полноэкранное окно выбора области.
pub fn run(image_path: &Path, out_path: &Path) -> anyhow::Result<()> {
    let image = image::open(image_path)
        .with_context(|| format!("не удалось открыть скриншот {}", image_path.display()))?;
    let rgba = image.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let out = out_path.to_path_buf();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_fullscreen(true)
            .with_inner_size([size[0] as f32, size[1] as f32])
            .with_position([0.0, 0.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_title("Выбор области"),
        ..Default::default()
    };

    eframe::run_native(
        "Выбор области",
        options,
        Box::new(move |cc| {
            let texture = cc.egui_ctx.load_texture(
                "area-screenshot",
                color_image,
                egui::TextureOptions::LINEAR,
            );
            Ok(Box::new(AreaSelector {
                texture,
                image_size: size,
                start: None,
                current: None,
                out,
                finished: false,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("окно выбора области: {error}"))
}

/// Приложение полноэкранного выделения.
struct AreaSelector {
    texture: egui::TextureHandle,
    image_size: [usize; 2],
    start: Option<egui::Pos2>,
    current: Option<egui::Pos2>,
    out: PathBuf,
    finished: bool,
}

impl AreaSelector {
    /// Завершает выбор и сохраняет координаты.
    fn finish(&mut self, ui: &egui::Ui, x: u32, y: u32, w: u32, h: u32) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = std::fs::write(&self.out, format!("{x},{y},{w},{h}"));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for AreaSelector {
    /// Рисует скриншот, затемнение и рамку выделения.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let pointer = ui.input(|input| input.pointer.interact_pos());

        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.finished = true;
            let _ = std::fs::write(&self.out, "");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if response.drag_started() {
            self.start = pointer;
            self.current = pointer;
        } else if response.dragged() || response.drag_stopped() {
            self.current = pointer;
        }

        ui.painter().image(
            self.texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        if let (Some(start), Some(current)) = (self.start, self.current) {
            let selection = egui::Rect::from_two_pos(start, current).intersect(rect);
            let shade = egui::Color32::from_black_alpha(120);
            if selection.is_positive() {
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, selection.min.y)),
                    0.0,
                    shade,
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, selection.max.y),
                        egui::pos2(rect.max.x, rect.max.y),
                    ),
                    0.0,
                    shade,
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, selection.min.y),
                        egui::pos2(selection.min.x, selection.max.y),
                    ),
                    0.0,
                    shade,
                );
                ui.painter().rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(selection.max.x, selection.min.y),
                        egui::pos2(rect.max.x, selection.max.y),
                    ),
                    0.0,
                    shade,
                );
                ui.painter().rect_stroke(
                    selection,
                    0.0,
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(0x8c, 0xc8, 0xff)),
                    egui::StrokeKind::Inside,
                );
            }

            if response.drag_stopped() && selection.width() > 8.0 && selection.height() > 8.0 {
                let width = (selection.width() / rect.width() * self.image_size[0] as f32)
                    .round()
                    .max(2.0) as u32;
                let height = (selection.height() / rect.height() * self.image_size[1] as f32)
                    .round()
                    .max(2.0) as u32;
                let x = ((selection.min.x - rect.min.x) / rect.width() * self.image_size[0] as f32)
                    .round()
                    .max(0.0) as u32;
                let y = ((selection.min.y - rect.min.y) / rect.height() * self.image_size[1] as f32)
                    .round()
                    .max(0.0) as u32;
                self.finish(ui, x, y, width, height);
            }
        }
    }
}
