//! The patch page's form: the options the run offers, as the person types
//! them, and the [`Config`] they build.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use unfr0zen_host::diamond3::usb::EntryPoint;
use unfr0zen_host::flow::default_data_dir;
use unfr0zen_host::{Config, Device, PatchInputs, Slot, DEFAULT_MATTER_CODE, DEFAULT_NTP_SERVER};

use crate::bus::Bus;
use crate::{spawn_dialog, WAIT_USB};

/// The patch options the form offers, as strings the user types.
#[derive(Default)]
pub struct Form {
    backplate_sim: bool,
    block_cloud: bool,
    ntp: Toggle,
    mqtt: Toggle,
    mqtt_user: String,
    mqtt_password: String,
    mqtt_tls: bool,
    mqtt_ca: FilePick,
    pub matter: MatterCode,
    matter_thread: bool,
    zigbee: bool,
    wwn_boiler: bool,
    pub entry_point: EntryPoint,
    ssh: bool,
    usb_shell: bool,
    root_password: String,
    // The advanced section: the CLI's options a run rarely needs.
    dry_run: bool,
    /// Save the root volume before patching.
    backup_volume: bool,
    slot: Option<Slot>,
    /// The device to expect instead of the one USB names.
    device: Option<Device>,
    /// The device last known, kept while no unit is on the bus.
    last_device: Option<Device>,
    doom: bool,
    external_temperature: bool,
    overlay: FilePick,
    cloud_url: Toggle,
    ssh_key: FilePick,
}

/// A file chosen with the platform's picker: the path, and the dialog a
/// click opened with the channel its choice arrives on over the following
/// frames.
#[derive(Default)]
pub struct FilePick {
    path: Option<PathBuf>,
    pending: Option<Receiver<Option<PathBuf>>>,
}

impl FilePick {
    /// Takes the dialog's answer when it arrives.
    fn drain(&mut self) {
        if let Some(receiver) = self.pending.take() {
            match receiver.try_recv() {
                Ok(Some(path)) => self.path = Some(path),
                Ok(None) => {}
                Err(_) => self.pending = Some(receiver),
            }
        }
    }

    /// The button that opens the picker for `kind` files, then the chosen
    /// file's name with a clear button.
    fn ui(&mut self, ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, kind: &str, extensions: &[&str]) {
        self.ui_sized(ui, label, kind, extensions, 0.0);
    }

    /// The button at least `width` wide; the chosen file's name beside it,
    /// cut to the room left, with a clear button.
    fn ui_sized(&mut self, ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, kind: &str, extensions: &[&str], width: f32) {
        if width == 0.0 {
            self.button(ui, label, kind, extensions, 0.0);
            self.chosen(ui);
            return;
        }
        // In a column: the button, then the chosen file under it, within
        // the button's width.
        ui.vertical(|ui| {
            ui.set_max_width(width);
            self.button(ui, label, kind, extensions, width);
            if self.path.is_some() {
                ui.horizontal(|ui| self.chosen(ui));
            }
        });
    }

    fn button(&mut self, ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, kind: &str, extensions: &[&str], width: f32) {
        if ui.add(egui::Button::new(label).min_size(egui::vec2(width, 0.0)))
            .on_hover_text(crate::tr!("patch.file.choose_hint", kind = kind))
            .clicked() && self.pending.is_none() {
            let (sender, receiver) = std::sync::mpsc::channel();
            let dialog = rfd::AsyncFileDialog::new().add_filter(kind, extensions).pick_file();
            spawn_dialog(Box::new(dialog), sender);
            self.pending = Some(receiver);
        }
    }

    /// The chosen file's name, cut to the room left, with a clear button.
    fn chosen(&mut self, ui: &mut egui::Ui) {
        if let Some(chosen) = self.path.clone() {
            if ui.small_button(crate::tr!("patch.file.clear")).on_hover_text(crate::tr!("patch.file.clear_hint")).clicked() {
                self.path = None;
            }
            let name = chosen.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            ui.add(egui::Label::new(egui::RichText::new(name).weak().size(12.0)).truncate())
                .on_hover_text(chosen.display().to_string());
        }
    }
}

/// Matter, with the pairing code to enter on the phone (prefilled).
pub struct MatterCode {
    pub on: bool,
    pub code: String,
}

impl Default for MatterCode {
    fn default() -> Self {
        MatterCode { on: false, code: DEFAULT_MATTER_CODE.into() }
    }
}

/// An option that is off, or on with a value.
#[derive(Default)]
pub struct Toggle {
    pub on: bool,
    pub value: String,
}

impl Form {
    /// Whether nestlocal takes the radio from a Heat Link.
    fn holds_radio(&self) -> bool {
        self.zigbee || (self.matter.on && self.matter_thread)
    }

    /// Whether the options install nestlocal.
    fn nestlocal(&self) -> bool {
        self.mqtt.on || self.matter.on || self.zigbee || self.doom
    }

    pub fn inputs(&self) -> PatchInputs {
        let text = |s: &str| (!s.is_empty()).then(|| s.to_string());
        PatchInputs {
            root_password: text(&self.root_password),
            ntp_server: self.ntp.on.then(|| self.ntp.value.clone()),
            // nestlocal takes nlclient's cloud and builds its own overlay.
            cloud_url: (self.cloud_url.on && !self.nestlocal()).then(|| self.cloud_url.value.clone()),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            overlay: if self.nestlocal() { None } else { self.overlay.path.clone() },
            mqtt: self.mqtt.on.then(|| self.mqtt.value.clone()),
            mqtt_user: self.mqtt.on.then(|| text(&self.mqtt_user)).flatten(),
            mqtt_password: self.mqtt.on.then(|| text(&self.mqtt_password)).flatten(),
            mqtt_tls: self.mqtt.on && self.mqtt_tls,
            // The CA field only shows under TLS, so it only sends under TLS.
            mqtt_ca: (self.mqtt.on && self.mqtt_tls).then(|| self.mqtt_ca.path.clone()).flatten(),
            matter: self.matter.on,
            matter_thread: self.matter.on && self.matter_thread,
            matter_code: self.matter.on.then(|| self.matter.code.clone()),
            zigbee: self.zigbee,
            wwn_boiler: self.wwn_boiler && self.holds_radio(),
            entry_point: self.entry_point,
            doom: self.doom,
            external_temperature: self.external_temperature,
            ssh: self.ssh,
            ssh_public_key: self.ssh.then(|| self.ssh_key.path.clone()).flatten(),
            usb_shell: self.usb_shell,
        }
    }

    pub fn config(&self) -> Result<Config, String> {
        let options = self.inputs().build().map_err(|e| e.to_string())?;
        let data_dir = default_data_dir();
        Ok(Config {
            options,
            omap: self.device,
            slot: self.slot,
            dry_run: self.dry_run,
            backup: self.backup_volume,
            entry_point: self.entry_point,
            wait_usb: WAIT_USB,
            wait_shell: Duration::from_secs(180),
            work_dir: data_dir.join("runs"),
            data_dir,
        })
    }
}

impl Form {
    /// A file picker is still open: its choice would be missed by a run
    /// started now.
    pub fn dialog_pending(&self) -> bool {
        [&self.mqtt_ca, &self.overlay, &self.ssh_key].iter().any(|pick| pick.pending.is_some())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        for pick in [&mut self.mqtt_ca, &mut self.overlay, &mut self.ssh_key] {
            pick.drain();
        }
        // One vertical gap everywhere, so every checkbox is spaced the same.
        ui.spacing_mut().item_spacing.y = 6.0;
        crate::section_after(ui, crate::tr!("patch.section.network"), MAIN_SECTION_SPACE);
        crate::checkbox(ui, true, &mut self.block_cloud, crate::tr!("patch.block_cloud"))
            .on_hover_text(crate::tr!("patch.block_cloud_hint"));
        // time.nest.com is blocked too, so time has to come from elsewhere.
        if self.block_cloud {
            self.ntp.on = true;
            if self.ntp.value.is_empty() {
                self.ntp.value = DEFAULT_NTP_SERVER.into();
            }
        }

        toggle_field(ui, &mut self.ntp, crate::tr!("patch.ntp"), "pool.ntp.org");

        crate::section_after(ui, crate::tr!("patch.section.integrations"), MAIN_SECTION_SPACE);
        crate::checkbox(ui, true, &mut self.mqtt.on, crate::tr!("patch.mqtt"))
            .on_hover_text(crate::tr!("patch.mqtt_hint"));
        if self.mqtt.on {
            // A two-column grid, indented as sub-items, so the labels line
            // up in one column and the inputs in the next.
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                egui::Grid::new("mqtt_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(crate::tr!("patch.mqtt.broker"));
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt.value).hint_text("host[:port]").desired_width(220.0))
                        .on_hover_text(crate::tr!("patch.mqtt.broker_hint"));
                    ui.end_row();
                    ui.label(crate::tr!("patch.mqtt.user"));
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt_user).desired_width(220.0));
                    ui.end_row();
                    ui.label(crate::tr!("patch.mqtt.password"));
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt_password).password(true).desired_width(220.0));
                    ui.end_row();
                    // "TLS" in the label column; the checkbox at the input
                    // column's left edge, the CA button at its right edge.
                    ui.label(crate::tr!("patch.mqtt.tls"));
                    let height = ui.spacing().interact_size.y;
                    ui.allocate_ui_with_layout(egui::vec2(220.0, height),
                        egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            crate::checkbox(ui, true, &mut self.mqtt_tls, "")
                                .on_hover_text(crate::tr!("patch.mqtt.tls_hint"));
                            if self.mqtt_tls {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.spacing_mut().button_padding = egui::vec2(6.0, 0.0);
                                    self.mqtt_ca.ui(ui, egui::RichText::new(crate::tr!("patch.mqtt.ca_certificate")).size(12.0), crate::tr!("patch.file.certificate"), &["pem", "crt"]);
                                });
                            }
                        });
                    ui.end_row();
                });
            });
        }

        if crate::checkbox(ui, true, &mut self.matter.on, crate::tr!("patch.matter"))
            .on_hover_text(crate::tr!("patch.matter_hint"))
            .changed() && !self.matter.on {
            self.matter_thread = false;
        }
        if self.matter.on {
            // As MQTT's fields: the label in one column, the input in the next.
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                egui::Grid::new("matter_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(crate::tr!("patch.matter.pairing_code"));
                    ui.add(egui::TextEdit::singleline(&mut self.matter.code).hint_text(DEFAULT_MATTER_CODE)
                        .desired_width(220.0)).on_hover_text(crate::tr!("patch.matter.pairing_code_hint"));
                    ui.end_row();
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                crate::checkbox(ui, !self.zigbee, &mut self.matter_thread, crate::tr!("patch.matter.thread"))
                    .on_hover_text(if self.zigbee {
                        crate::tr!("patch.matter.thread_disabled_hint")
                    } else {
                        crate::tr!("patch.matter.thread_hint")
                    });
            });
        }

        crate::checkbox(ui, !self.matter_thread, &mut self.zigbee, crate::tr!("patch.zigbee"))
            .on_hover_text(if self.matter_thread {
                crate::tr!("patch.zigbee_disabled_hint")
            } else {
                crate::tr!("patch.zigbee_hint")
            });

        crate::section_after(ui, crate::tr!("patch.section.access"), MAIN_SECTION_SPACE);
        crate::checkbox(ui, true, &mut self.ssh, crate::tr!("patch.ssh"))
            .on_hover_text(crate::tr!("patch.ssh_hint"));
        crate::checkbox(ui, true, &mut self.usb_shell, crate::tr!("patch.usb_shell"))
            .on_hover_text(crate::tr!("patch.usb_shell_hint"));
        if self.ssh || self.usb_shell {
            ui.horizontal(|ui| {
                ui.label(crate::tr!("patch.root_password"));
                ui.add(egui::TextEdit::singleline(&mut self.root_password).password(true)
                    .hint_text("unfr0zen").desired_width(220.0))
                    .on_hover_text(crate::tr!("patch.root_password_hint"));
            });
        }
    }

    /// The advanced panel: the CLI's options a run rarely needs.
    /// The device a run is for: the one asked for, else the one on the bus.
    fn known_device(&self, bus: &Bus) -> Option<Device> {
        self.device.or(match bus {
            Bus::One(seen) => Some(seen.device),
            _ => None,
        })
    }

    /// Puts the entry point back to auto when the device changes for
    /// another; a unit unplugged, or several on the bus, is no change.
    pub fn track_device(&mut self, bus: &Bus) {
        if let Some(device) = self.known_device(bus) {
            if self.last_device.is_some_and(|last| last != device) {
                self.entry_point = EntryPoint::Auto;
            }
            self.last_device = Some(device);
        }
    }

    pub fn advanced_ui(&mut self, ui: &mut egui::Ui, bus: &Bus) {
        ui.spacing_mut().item_spacing.y = 6.0;
        // With nestlocal, the overlay and the cloud are its.
        let nestlocal_hint = crate::tr!("patch.nestlocal_owns_overlay_cloud");
        let nestlocal = self.nestlocal();
        // Each section's fields as the main form's: the label in one
        // column, the control beside it in the next, all one width.
        let grid = |id: &str| egui::Grid::new(id).num_columns(2).min_col_width(ADVANCED_LABEL_WIDTH).spacing([12.0, 6.0]);

        crate::section(ui, crate::tr!("patch.section.run"));
        crate::checkbox(ui, true, &mut self.dry_run, crate::tr!("patch.dry_run")).on_hover_text(crate::tr!("patch.dry_run_hint"));
        crate::checkbox(ui, true, &mut self.backup_volume, crate::tr!("patch.backup_volume")).on_hover_text(crate::tr!("patch.backup_volume_hint"));
        grid("advanced_run").show(ui, |ui| {
            ui.label(crate::tr!("patch.boot_slot"));
            crate::dropdown("slot", CONTROL_WIDTH)
                .selected_text(self.slot.map_or(crate::tr!("patch.auto").to_string(), |s| s.to_string())).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.slot, None, crate::tr!("patch.auto"));
                    for slot in [Slot::Root0, Slot::Root1] {
                        ui.selectable_value(&mut self.slot, Some(slot), slot.to_string());
                    }
                }).response.on_hover_text(crate::tr!("patch.boot_slot_hint"));
            ui.end_row();
            ui.label(crate::tr!("patch.device"));
            crate::dropdown("device", CONTROL_WIDTH)
                .selected_text(self.device.map_or(crate::tr!("patch.auto"), Device::name)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.device, None, crate::tr!("patch.auto"));
                    for device in [Device::Diamond, Device::J49, Device::Diamond3] {
                        ui.selectable_value(&mut self.device, Some(device), device.name());
                    }
                }).response.on_hover_text(crate::tr!("patch.device_hint"));
            ui.end_row();
            let gen3 = self.known_device(bus) == Some(Device::Diamond3);
            ui.label(crate::tr!("patch.entry_point")).on_hover_text(
                crate::tr!("patch.entry_point_hint"));
            // Greyed without a gen 3 on the bus, keeping the choice: the unit
            // may only have been unplugged, and other generations ignore it.
            ui.add_enabled_ui(gen3, |ui| {
                crate::dropdown("technique", CONTROL_WIDTH)
                    .selected_text(entry_point_label(self.entry_point)).show_ui(ui, |ui| {
                        for entry_point in [EntryPoint::Auto, EntryPoint::OfficialLoader, EntryPoint::Cve20177932, EntryPoint::Cve20177936] {
                            ui.selectable_value(&mut self.entry_point, entry_point, entry_point_label(entry_point));
                        }
                    }).response.on_hover_text(crate::tr!("patch.entry_point_choice_hint"));
            }).response.on_disabled_hover_text(crate::tr!("patch.entry_point_disabled_hint"));
            ui.end_row();
            ui.end_row();
        });

        crate::section(ui, crate::tr!("patch.section.extras"));
        crate::checkbox(ui, self.holds_radio(), &mut self.wwn_boiler, crate::tr!("patch.wwn_boiler")).on_hover_text(
            crate::tr!("patch.wwn_boiler_hint"));
        crate::checkbox(ui, true, &mut self.doom, crate::tr!("patch.doom")).on_hover_text(crate::tr!("patch.doom_hint"));
        // Offered for the generations that have it; a gen 3 is told its
        // Bluetooth goes. The device last seen keeps it while a unit is unplugged.
        let device = self.device.or(self.last_device);
        if matches!(device, Some(Device::Diamond3 | Device::J49)) {
            let enabled = self.mqtt.on || self.matter.on || self.zigbee;
            if device == Some(Device::Diamond3) {
                crate::checkbox(ui, enabled, &mut self.external_temperature, crate::tr!("patch.external_temperature_ble"))
                    .on_hover_text(crate::tr!("patch.external_temperature_ble_hint"));
            } else {
                crate::checkbox(ui, enabled, &mut self.external_temperature, crate::tr!("patch.external_temperature"))
                    .on_hover_text(crate::tr!("patch.external_temperature_hint"));
            }
        }
        grid("advanced_extras").show(ui, |ui| {
            ui.label(crate::tr!("patch.overlay"));
            ui.add_enabled_ui(!nestlocal, |ui| {
                ui.horizontal(|ui| self.overlay.ui_sized(ui, crate::tr!("patch.file.choose"), crate::tr!("patch.file.tar_archive"), &["tar"], CONTROL_WIDTH));
            }).response.on_disabled_hover_text(nestlocal_hint);
            ui.end_row();
            ui.label(crate::tr!("patch.ssh_key"));
            ui.add_enabled_ui(self.ssh, |ui| {
                ui.horizontal(|ui| self.ssh_key.ui_sized(ui, crate::tr!("patch.file.choose"), crate::tr!("patch.file.openssh_public_key"), &["pub"], CONTROL_WIDTH));
            }).response.on_disabled_hover_text(crate::tr!("patch.ssh_key_disabled_hint"));
            ui.end_row();
        });
        ui.add_enabled_ui(!nestlocal, |ui| {
            crate::checkbox(ui, true, &mut self.cloud_url.on, crate::tr!("patch.cloud"))
                .on_hover_text(crate::tr!("patch.cloud_hint"));
            if self.cloud_url.on {
                grid("advanced_cloud").show(ui, |ui| {
                    ui.label(crate::tr!("patch.cloud_url"));
                    ui.add(egui::TextEdit::singleline(&mut self.cloud_url.value).hint_text("https://frontdoor.example")
                        .desired_width(CONTROL_WIDTH));
                    ui.end_row();
                });
            }
        }).response.on_disabled_hover_text(nestlocal_hint);

        crate::section(ui, crate::tr!("patch.section.testing"));
        crate::checkbox(ui, true, &mut self.backplate_sim, crate::tr!("patch.backplate_sim"))
            .on_hover_text(crate::tr!("patch.backplate_sim_hint"));
    }
}

/// Space above each section heading of the main form.
const MAIN_SECTION_SPACE: f32 = 2.0;
/// Sub-items under a checkbox are indented this far.
const SUBITEM_INDENT: f32 = 20.0;
/// The advanced panel's label column, and its drop-downs and buttons, all
/// one width.
const ADVANCED_LABEL_WIDTH: f32 = 84.0;
const CONTROL_WIDTH: f32 = 180.0;

fn toggle_field(ui: &mut egui::Ui, toggle: &mut Toggle, label: &str, hint: &str) {
    ui.horizontal(|ui| {
        crate::checkbox(ui, true, &mut toggle.on, label)
            .on_hover_text(crate::tr!("patch.ntp_hint"));
    });
    if toggle.on {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.add(egui::TextEdit::singleline(&mut toggle.value).hint_text(hint).desired_width(220.0));
        });
    }
}

/// The entry-point labels offered by the GUI, independent of CLI option names.
fn entry_point_label(entry_point: EntryPoint) -> &'static str {
    match entry_point {
        EntryPoint::Auto => crate::tr!("patch.auto"),
        EntryPoint::Cve20177932 => crate::tr!("patch.technique.jump_certificates"),
        EntryPoint::Cve20177936 => crate::tr!("patch.technique.spray_stack"),
        EntryPoint::OfficialLoader => crate::tr!("patch.technique.official_loader"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabling_tls_keeps_the_mqtt_rows_the_same_height() {
        let matter_top = |tls| {
            let context = egui::Context::default();
            crate::install_fonts(&context);
            context.all_styles_mut(|style| {
                style.spacing.button_padding = egui::vec2(10.0, 4.0);
                style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
                style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            });
            let mut form = Form { mqtt: Toggle { on: true, value: String::new() }, mqtt_tls: tls, ..Form::default() };
            let mut top = None;
            for _ in 0..3 {
                let output = context.run_ui(egui::RawInput::default(), |ui| form.ui(ui));
                if tls {
                    let certificate = output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) if text.galley.job.text == "CA certificate" => Some(&text.galley),
                        _ => None,
                    }).expect("the CA certificate button is drawn");
                    for section in &certificate.job.sections {
                        assert_eq!(section.format.font_id.size, 12.0);
                    }
                }
                top = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) if text.galley.job.text == "Matter" => Some(text.pos.y),
                    _ => None,
                });
            }
            top.expect("Matter is drawn below the MQTT fields")
        };
        assert_eq!(matter_top(false), matter_top(true));
    }

    #[test]
    fn the_entry_point_goes_back_to_auto_only_when_the_device_changes() {
        use crate::bus::Seen;
        use unfr0zen_host::{device, diamond3::usb};
        let one = |id| Bus::One(Seen { device: device::identify(id).unwrap(), mode: "", port: "1".into(), id });
        let gen3 = one(usb::SDP);
        let gen2 = Bus::One(Seen { device: Device::J49, mode: "", port: "1".into(), id: (0, 0) });
        let mut form = Form::default();
        form.track_device(&gen3);
        form.entry_point = EntryPoint::Cve20177936;
        for bus in [Bus::Nothing, Bus::Several(Vec::new()), one(usb::RECOVERY), one(usb::APP_ACM), one(usb::INITRAMFS), gen3.clone()] {
            form.track_device(&bus);
            assert_eq!(form.entry_point, EntryPoint::Cve20177936, "{bus:?}");
        }
        form.track_device(&Bus::Nothing);
        form.track_device(&gen2);
        assert_eq!(form.entry_point, EntryPoint::Auto, "swapped while unplugged");
        form.track_device(&gen3);
        form.entry_point = EntryPoint::Cve20177936;
        form.device = Some(Device::J49);
        form.track_device(&gen3);
        assert_eq!(form.entry_point, EntryPoint::Auto, "another device asked for");
    }

    #[test]
    fn form_maps_only_enabled_options() {
        let mut form = Form { root_password: "s3cret".into(), backplate_sim: true, ..Form::default() };
        form.mqtt = Toggle { on: true, value: "broker".into() };
        form.mqtt_user = "u".into();
        form.mqtt_tls = true;
        let inputs = form.inputs();
        assert_eq!(inputs.mqtt.as_deref(), Some("broker"));
        assert_eq!(inputs.mqtt_user.as_deref(), Some("u"));
        assert!(inputs.mqtt_tls);
        assert_eq!(inputs.root_password.as_deref(), Some("s3cret"));
        assert!(inputs.backplate_sim);
        // Toggles that are off contribute nothing.
        assert!(inputs.ntp_server.is_none());

        let bare = Form::default().inputs();
        assert!(bare.mqtt.is_none() && bare.root_password.is_none() && !bare.backplate_sim);
    }

    #[test]
    fn mqtt_fields_are_dropped_when_mqtt_is_off() {
        let form = Form { mqtt_user: "u".into(), mqtt_tls: true, ..Form::default() };
        let inputs = form.inputs();
        assert!(inputs.mqtt.is_none() && inputs.mqtt_user.is_none() && !inputs.mqtt_tls);
        // build() would otherwise reject user-without-broker; here it is clean.
        assert!(inputs.build().is_ok());
    }

    #[test]
    fn block_cloud_moves_time_off_time_nest_com() {
        let mut form = Form { block_cloud: true, ..Form::default() };
        let options = form.config().unwrap().options;
        assert!(options.block_cloud && options.ntp_server.as_deref() == Some(""));
        form.ntp = Toggle { on: true, value: "ntp.lan".into() };
        assert_eq!(form.config().unwrap().options.ntp_server.as_deref(), Some("ntp.lan"));
    }

    #[test]
    fn thread_is_opt_in_and_requires_matter_and_exclusive_radio() {
        let mut form = Form::default();
        assert!(form.config().unwrap().options.thread.is_none());
        form.matter.on = true;
        assert!(form.config().unwrap().options.thread.is_none());
        form.matter_thread = true;
        let options = form.config().unwrap().options;
        assert!(options.thread.unwrap().files.iter().any(|(name, _, _)| name == "nestlabs/sbin/ot-daemon"));
        form.zigbee = true;
        assert!(form.config().err().unwrap().contains("cannot share the radio"));
        form.matter.on = false;
        assert!(form.config().unwrap().options.thread.is_none());
    }

    #[test]
    fn matter_takes_the_pairing_code_shown() {
        let mut form = Form::default();
        assert_eq!(form.matter.code, DEFAULT_MATTER_CODE, "prefilled");
        assert!(form.config().unwrap().options.matter.is_none(), "off by default");
        form.matter.on = true;
        let matter = form.config().unwrap().options.matter.unwrap();
        assert_eq!(matter.pairing_code(), DEFAULT_MATTER_CODE);
        form.matter.code = "1559-813-5763".into();
        assert_eq!(form.config().unwrap().options.matter.unwrap().passcode, 58596013);
        form.matter.code = "1559-813-5764".into();
        assert!(form.config().err().is_some_and(|e| e.contains("check digit")));
    }

    #[test]
    fn config_runs_the_real_write_on_the_boot_order_slot() {
        let config = Form::default().config().unwrap();
        assert_eq!((config.slot, config.omap), (None, None));
        assert!(!config.dry_run && !config.backup);
    }

    #[test]
    fn the_advanced_section_maps_to_the_cli_options() {
        let mut form = Form { dry_run: true, backup_volume: true, doom: true, slot: Some(Slot::Root1),
                              device: Some(Device::J49), ..Form::default() };
        form.ssh_key.path = Some("key.pub".into());
        let config = form.config().unwrap();
        assert!(config.dry_run && config.backup);
        assert_eq!((config.slot, config.omap), (Some(Slot::Root1), Some(Device::J49)));
        assert!(config.options.doom.is_some());
        assert!(form.inputs().ssh_public_key.is_none(), "the key only goes with the SSH server");
        form.ssh = true;
        assert_eq!(form.inputs().ssh_public_key.as_deref(), Some(std::path::Path::new("key.pub")));
        form.cloud_url = Toggle { on: true, value: "https://cloud.example".into() };
        form.overlay.path = Some("extra.tar".into());
        let inputs = form.inputs();
        assert!(inputs.cloud_url.is_none() && inputs.overlay.is_none(), "doom installs nestlocal, which has its own");
        form.doom = false;
        let inputs = form.inputs();
        assert_eq!(inputs.cloud_url.as_deref(), Some("https://cloud.example"));
        assert_eq!(inputs.overlay.as_deref(), Some(std::path::Path::new("extra.tar")));
        form.doom = true;
        form.wwn_boiler = true;
        assert!(!form.inputs().wwn_boiler, "only for a radio nestlocal holds");
        form.zigbee = true;
        assert!(form.inputs().wwn_boiler);
        form.ssh = false;
        assert!(form.config().unwrap().options.wwn_boiler);
        form.zigbee = false;
        form.wwn_boiler = false;
        form.entry_point = EntryPoint::Cve20177936;
        assert_eq!(form.inputs().entry_point, EntryPoint::Cve20177936);
        assert_eq!(form.config().unwrap().entry_point, EntryPoint::Cve20177936);
        form.entry_point = EntryPoint::Auto;
        assert!(!form.dialog_pending());
        form.cloud_url.on = false;
        form.overlay.path = None;
    }
}
