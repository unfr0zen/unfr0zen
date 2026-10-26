use eframe::egui;

const PROJECT: &str = "https://github.com/unfr0zen";
const LICENSES: &[&str] = &[
    "unfr0zen (GPLv3)",
    "BusyBox (GPLv2)",
    "U-Boot (GPLv2; see individual files)",
    "Linux (GPLv2)",
    "X-loader (GPLv2 or later)",
    "lzokay (MIT), copyright 2021 Luke Street",
];

pub fn show(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let response = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.heading(egui::RichText::new("About unfr0zen").color(ui.visuals().strong_text_color()));
        ui.label(super::BUILD_VERSION);
        ui.hyperlink_to(PROJECT, PROJECT);
        ui.separator();
        ui.strong("Licenses");
        for notice in LICENSES {
            ui.label(*notice);
        }
        ui.separator();
        ui.button("Close").clicked()
    });
    if response.inner || response.should_close() {
        *open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(ctx: &egui::Context, open: &mut bool, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(440.0, 620.0))),
            events,
            ..Default::default()
        }, |ui| show(ui.ctx(), open))
    }

    fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            _ => None,
        }).unwrap_or_else(|| panic!("missing text: {label}"))
    }

    fn click(ctx: &egui::Context, open: &mut bool, pos: egui::Pos2) -> egui::FullOutput {
        let event = |pressed| egui::Event::PointerButton {
            pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE,
        };
        render(ctx, open, vec![egui::Event::PointerMoved(pos), event(true)]);
        render(ctx, open, vec![event(false)])
    }

    #[test]
    fn dialog_fits_and_dismisses() {
        let ctx = egui::Context::default();
        let mut open = true;
        for _ in 0..3 { render(&ctx, &mut open, vec![]); }
        let output = render(&ctx, &mut open, vec![]);
        let close = text_rect(&output, "Close");
        assert!(egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(440.0, 620.0)).contains_rect(close));
        click(&ctx, &mut open, close.center());
        assert!(!open);

        open = true;
        render(&ctx, &mut open, vec![]);
        render(&ctx, &mut open, vec![egui::Event::Key {
            key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!open);
    }

}
