use eframe::egui;

// Every crate the binary links, with its license: generated at
// build time by build.rs from `cargo metadata`, so it tracks `Cargo.lock`.
include!(concat!(env!("OUT_DIR"), "/third_party.rs"));

const PROJECT: &str = "https://github.com/unfr0zen";
fn licenses() -> [&'static str; 5] {
    [
        "unfr0zen (GPLv3)",
        "BusyBox (GPLv2)",
        crate::tr!("about.license.uboot"),
        "Linux (GPLv2)",
        crate::tr!("about.license.xloader"),
    ]
}

/// The projects and research unfr0zen is built on, each as "what — author":
/// the description, then the author whose name carries the link.
fn credits() -> [(&'static str, &'static str, &'static str); 11] {
    [
        ("U-Boot (i.MX) —", "NXP", "https://github.com/nxp-imx/uboot-imx"),
        ("BusyBox —", "the BusyBox project", "https://busybox.net"),
        ("rs-matter —", "Project CHIP", "https://github.com/project-chip/rs-matter"),
        ("OpenThread —", "the OpenThread project", "https://github.com/openthread/openthread"),
        ("Gecko SDK / EmberZNet —", "Silicon Labs", "https://github.com/SiliconLabs/gecko_sdk"),
        ("doomgeneric —", "ozkl", "https://github.com/ozkl/doomgeneric"),
        ("DOOM —", "id Software", "https://github.com/id-Software/DOOM"),
        ("CVE-2017-7932 and CVE-2017-7936 —", "Quarkslab", "https://quarkslab.com"),
        (crate::tr!("about.credit.diamond3_loader"), "@evan-hoddinott", "https://github.com/evan-hoddinott"),
        (crate::tr!("about.credit.cuckoo_loader"), "@ajb142", "https://github.com/ajb142"),
        (crate::tr!("about.credit.cuckoo_loader_gen1"), "@guyman70718", "https://github.com/guyman70718"),
    ]
}

pub fn page(ui: &mut egui::Ui) {
    let frame = egui::Frame::new().fill(super::CARD).corner_radius(14).inner_margin(egui::Margin::same(16));
    // The cards end short of the panel, which has their colour.
    let room = (ui.available_width() - LIBRARIES - 24.0).max(200.0);
    egui::Panel::right("about_libraries").exact_size(LIBRARIES).resizable(false)
        .show_separator_line(false).frame(frame).show(ui, libraries);
    ui.allocate_ui(egui::vec2(room, ui.available_height()), |ui| {
        ui.set_max_width(room);
        details(ui);
    });
}

/// The licenses' and credits' lines: lighter than muted, as they are the
/// cards' content.
const LIST: egui::Color32 = egui::Color32::from_rgb(0xc4, 0xc9, 0xd1);

/// The libraries panel's width.
const LIBRARIES: f32 = 460.0;

/// The left column: the version, the project, licenses and credits.
fn details(ui: &mut egui::Ui) {
    super::name_heading(ui, &crate::tr!("about.heading", app = crate::NAME));
    ui.add_space(2.0);
    ui.label(egui::RichText::new(super::BUILD_VERSION).color(super::MUTED));
    ui.hyperlink_to(PROJECT, PROJECT);
    ui.add_space(12.0);
    // Each list in a card under its own heading, a line a row.
    crate::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.label(egui::RichText::new(crate::tr!("about.licenses")).font(crate::medium(16.0)));
        for notice in licenses() {
            ui.label(egui::RichText::new(notice).size(14.0).color(LIST));
        }
    });
    ui.add_space(14.0);
    crate::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.label(egui::RichText::new(crate::tr!("about.credits")).font(crate::medium(16.0)));
        for (what, author, url) in credits() {
            // Wrapped, so a long credit keeps to the card's width.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(egui::RichText::new(what).size(14.0).color(LIST));
                ui.hyperlink_to(egui::RichText::new(author).size(14.0), url);
            });
        }
    });
}

/// The open-source libraries the binary is built from, from the generated
/// list: the crate name on the left, its license on the right, one per row.
fn libraries(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new(crate::tr!("about.libraries", count = THIRD_PARTY.len())).font(crate::medium(15.0)));
    ui.label(egui::RichText::new(crate::tr!("about.libraries_hint"))
        .size(11.0).color(super::MUTED));
    ui.add_space(8.0);
    let row = 22.0;
    let name_font = egui::FontId::proportional(12.5);
    let license_font = egui::FontId::proportional(11.5);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row, THIRD_PARTY.len(), |ui, range| {
        let width = ui.available_width();
        let text_color = ui.visuals().text_color();
        for (name, license) in &THIRD_PARTY[range] {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row), egui::Sense::hover());
            let y = rect.center().y;
            // A long expression is cut to the right half, so it never runs
            // past the pane or over the name.
            let license_galley = super::truncated(ui, license.to_string(), license_font.clone(), super::MUTED, width * 0.55);
            let license_x = rect.right() - license_galley.size().x;
            ui.painter().galley(egui::pos2(license_x, y - license_galley.size().y / 2.0), license_galley, super::MUTED);
            let name_galley = super::truncated(ui, name.to_string(), name_font.clone(), text_color, license_x - rect.left() - 8.0);
            ui.painter().galley(egui::pos2(rect.left(), y - name_galley.size().y / 2.0), name_galley, text_color);
        }
    });
}
