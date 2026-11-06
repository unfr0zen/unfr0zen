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
fn credits() -> [(&'static str, &'static str, &'static str); 10] {
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
    ]
}

pub fn page(ui: &mut egui::Ui) {
    let frame = egui::Frame::new().fill(super::SIDE).corner_radius(8)
        .inner_margin(egui::Margin::same(super::DETAIL_MARGIN as i8));
    egui::Panel::right("about_libraries").exact_size(460.0).resizable(false)
        .show_separator_line(false).frame(frame).show(ui, libraries);

    super::name_heading(ui, &crate::tr!("about.heading", app = crate::NAME));
    ui.add_space(2.0);
    ui.label(super::BUILD_VERSION);
    ui.hyperlink_to(PROJECT, PROJECT);
    ui.add_space(12.0);
    ui.strong(crate::tr!("about.licenses"));
    for notice in licenses() {
        ui.label(egui::RichText::new(notice).size(12.0).color(super::MUTED));
    }
    ui.add_space(12.0);
    ui.strong(crate::tr!("about.credits"));
    for (what, author, url) in credits() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new(what).size(12.0).color(super::MUTED));
            ui.hyperlink_to(egui::RichText::new(author).size(12.0), url);
        });
    }
}

/// The open-source libraries the binary is built from, from the generated
/// list: the crate name on the left, its license on the right, one per row.
fn libraries(ui: &mut egui::Ui) {
    ui.strong(crate::tr!("about.libraries", count = THIRD_PARTY.len()));
    ui.label(egui::RichText::new(crate::tr!("about.libraries_hint"))
        .size(11.0).color(super::MUTED));
    ui.add_space(8.0);
    let row = 17.0;
    let name_font = egui::FontId::proportional(11.5);
    let license_font = egui::FontId::proportional(10.5);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row, THIRD_PARTY.len(), |ui, range| {
        let width = ui.available_width();
        let text_color = ui.visuals().text_color();
        for (name, license) in &THIRD_PARTY[range] {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row), egui::Sense::hover());
            let y = rect.center().y;
            let license_galley = ui.painter().layout_no_wrap(license.to_string(), license_font.clone(), super::MUTED);
            let license_x = rect.right() - license_galley.size().x;
            ui.painter().galley(egui::pos2(license_x, y - license_galley.size().y / 2.0), license_galley, super::MUTED);
            let name_galley = super::truncated(ui, name.to_string(), name_font.clone(), text_color, license_x - rect.left() - 8.0);
            ui.painter().galley(egui::pos2(rect.left(), y - name_galley.size().y / 2.0), name_galley, text_color);
        }
    });
}
