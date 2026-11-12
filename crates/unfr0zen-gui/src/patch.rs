//! The patch page's form: the options the run offers, as the person types
//! them, and the [`Config`] they build.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use unfr0zen_host::diamond3::usb::EntryPoint;
use unfr0zen_host::flow::default_data_dir;
use unfr0zen_host::{Config, Device, PatchInputs, DEFAULT_MATTER_CODE, DEFAULT_NTP_SERVER};

use crate::bus::Bus;
use crate::{spawn_dialog, WAIT_USB};

/// The patch options the form offers, as strings the user types.
#[derive(Default)]
pub struct Form {
    backplate_sim: bool,
    block_cloud: bool,
    ntp: Toggle,
    pub mqtt: Toggle,
    mqtt_user: String,
    mqtt_password: String,
    mqtt_tls: bool,
    mqtt_ca: FilePick,
    pub matter: MatterCode,
    matter_thread: bool,
    zigbee: bool,
    wwn_boiler: bool,
    pub entry_point: EntryPoint,
    pub ssh: bool,
    usb_shell: bool,
    root_password: String,
    /// Back up the whole NAND before patching (the review step's choice).
    pub backup_first: bool,
    // The developer options: the CLI's options a run rarely needs.
    /// The device to expect instead of the one USB names.
    device: Option<Device>,
    /// The device last known, kept while no unit is on the bus.
    last_device: Option<Device>,
    pub doom: bool,
    external_temperature: bool,
    dark_theme: bool,
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
        self.button(ui, label, kind, extensions);
        self.chosen(ui);
    }

    fn button(&mut self, ui: &mut egui::Ui, label: impl Into<egui::WidgetText>, kind: &str, extensions: &[&str]) {
        if ui.add(egui::Button::new(label))
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
    /// The form the window opens with: every option off, and the review
    /// step's backup first.
    pub fn new() -> Self {
        Form { backup_first: true, ..Form::default() }
    }
}

impl Form {
    /// Whether nestlocal takes the radio from a Heat Link.
    fn holds_radio(&self) -> bool {
        self.zigbee || (self.matter.on && self.matter_thread)
    }

    pub fn inputs(&self) -> PatchInputs {
        let text = |s: &str| (!s.is_empty()).then(|| s.to_string());
        PatchInputs {
            root_password: text(&self.root_password),
            ntp_server: self.ntp.on.then(|| self.ntp.value.clone()),
            cloud_url: None,
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            overlay: None,
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
            external_temperature: self.external_temperature && self.supports_external_temperature(),
            // Only for the gen 2 the checkbox was offered for.
            dark_theme: self.dark_theme && self.device.or(self.last_device) == Some(Device::J49),
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
            slot: None,
            dry_run: false,
            backup: false,
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
        [&self.mqtt_ca, &self.ssh_key].iter().any(|pick| pick.pending.is_some())
    }

    fn drain_dialogs(&mut self) {
        for pick in [&mut self.mqtt_ca, &mut self.ssh_key] {
            pick.drain();
        }
    }

    /// Whether the integrations step can be left: with MQTT on, a broker
    /// must be named.
    pub fn integrations_done(&self) -> bool {
        !self.mqtt.on || !self.mqtt.value.trim().is_empty()
    }

    /// Whether any integration is chosen; with none, the step is skipped.
    pub fn any_integration(&self) -> bool {
        self.mqtt.on || self.matter.on || self.zigbee
    }

    /// What to do once the unit is patched, for Done: put it back, then a
    /// card per integration chosen.
    pub fn next_steps(&self) -> Vec<(&'static str, String)> {
        let mut steps = vec![(crate::tr!("setup.next_mount"), crate::tr!("setup.next_mount_about").to_string())];
        if self.mqtt.on {
            steps.push((crate::tr!("setup.next_ha"), crate::tr!("setup.next_ha_about").to_string()));
        }
        if self.matter.on {
            steps.push((crate::tr!("setup.next_matter"), crate::tr!("setup.next_matter_about", code = self.matter.code.trim())));
        }
        if self.zigbee {
            steps.push((crate::tr!("setup.next_zigbee"), crate::tr!("setup.next_zigbee_about").to_string()));
        }
        steps
    }

    /// The integrations step: MQTT, Matter and Zigbee, and what rides on them.
    pub fn integrations_ui(&mut self, ui: &mut egui::Ui) {
        self.drain_dialogs();
        crate::option_card(ui, &mut self.mqtt.on, true, crate::tr!("setup.mqtt"), crate::tr!("setup.mqtt_about"), |ui| {
            // The broker across the card, the sign-in side by side under it.
            let width = ui.available_width();
            let half = (width - 14.0) / 2.0;
            crate::field(ui, crate::tr!("patch.mqtt.broker"),
                         egui::TextEdit::singleline(&mut self.mqtt.value).hint_text("host[:port]"), width)
                .on_hover_text(crate::tr!("patch.mqtt.broker_hint"));
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                crate::field(ui, crate::tr!("patch.mqtt.user"),
                             egui::TextEdit::singleline(&mut self.mqtt_user).hint_text(crate::tr!("setup.optional")), half);
                crate::field(ui, crate::tr!("patch.mqtt.password"),
                             egui::TextEdit::singleline(&mut self.mqtt_password).password(true).hint_text(crate::tr!("setup.optional")), half);
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                crate::switch(ui, &mut self.mqtt_tls, true).on_hover_text(crate::tr!("patch.mqtt.tls_hint"));
                ui.label(crate::tr!("setup.mqtt_tls"));
                if self.mqtt_tls {
                    self.mqtt_ca.ui(ui, crate::tr!("patch.mqtt.ca_certificate"), crate::tr!("patch.file.certificate"), &["pem", "crt"]);
                }
            });
        });

        let matter_was = self.matter.on;
        crate::option_card(ui, &mut self.matter.on, true, crate::tr!("patch.matter"), crate::tr!("setup.matter_about"), |ui| {
            crate::field(ui, crate::tr!("patch.matter.pairing_code"),
                         egui::TextEdit::singleline(&mut self.matter.code).hint_text(DEFAULT_MATTER_CODE), FIELD_WIDTH)
                .on_hover_text(crate::tr!("patch.matter.pairing_code_hint"));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let free = !self.zigbee;
                crate::switch(ui, &mut self.matter_thread, free).on_hover_text(if free {
                    crate::tr!("patch.matter.thread_hint")
                } else {
                    crate::tr!("patch.matter.thread_disabled_hint")
                });
                ui.label(egui::RichText::new(crate::tr!("patch.matter.thread")).color(if free { crate::TEXT } else { crate::DIM }));
            });
        });
        if matter_was && !self.matter.on {
            self.matter_thread = false;
        }
        let thread = self.matter_thread;
        crate::toggle_card(ui, &mut self.zigbee, !thread, crate::tr!("patch.zigbee"), crate::tr!("setup.zigbee_about"));
        if thread {
            crate::muted_line(ui, crate::tr!("patch.zigbee_disabled_hint"));
        }
        // A Heat Link model whose radio nestlocal takes heats through W1.
        if self.holds_radio() {
            crate::toggle_card(ui, &mut self.wwn_boiler, true, crate::tr!("patch.wwn_boiler"), crate::tr!("patch.wwn_boiler_hint"));
        }

        crate::group(ui, crate::tr!("setup.group.sensors"));
        // Always shown, greyed out without a unit that takes it or an
        // integration to send temperatures through; a gen 3 is told its
        // Bluetooth goes.
        let label = match self.device.or(self.last_device) {
            Some(Device::Diamond3) => crate::tr!("patch.external_temperature_ble"),
            _ => crate::tr!("patch.external_temperature"),
        };
        let enabled = self.external_temperature_enabled();
        let hint = self.external_temperature_hint();
        crate::toggle_card(ui, &mut self.external_temperature, enabled, label, hint);
    }

    /// The features step: privacy, remote access, and the extras.
    pub fn features_ui(&mut self, ui: &mut egui::Ui, bus: &Bus) {
        self.drain_dialogs();
        crate::group(ui, crate::tr!("setup.group.privacy"));
        let block = crate::option_card(ui, &mut self.block_cloud, true, crate::tr!("patch.block_cloud"), crate::tr!("setup.block_cloud_about"), |ui| {
            crate::field(ui, crate::tr!("setup.time_server"),
                         egui::TextEdit::singleline(&mut self.ntp.value).hint_text(DEFAULT_NTP_SERVER), FIELD_WIDTH)
                .on_hover_text(crate::tr!("setup.time_server_why"));
        });
        // time.nest.com is blocked too, so time has to come from elsewhere.
        if block {
            self.ntp.on = true;
            if self.ntp.value.is_empty() {
                self.ntp.value = DEFAULT_NTP_SERVER.into();
            }
        } else {
            crate::option_card(ui, &mut self.ntp.on, true, crate::tr!("patch.ntp"), crate::tr!("patch.ntp_hint"), |ui| {
                crate::field(ui, crate::tr!("setup.time_server"),
                             egui::TextEdit::singleline(&mut self.ntp.value).hint_text(DEFAULT_NTP_SERVER), FIELD_WIDTH);
            });
        }

        crate::group(ui, crate::tr!("setup.group.access"));
        crate::option_card(ui, &mut self.ssh, true, crate::tr!("setup.ssh"), crate::tr!("setup.ssh_about"), |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::tr!("patch.ssh_key")).color(crate::MUTED));
                self.ssh_key.ui(ui, crate::tr!("patch.file.choose"), crate::tr!("patch.file.openssh_public_key"), &["pub"]);
            });
            root_password_field(ui, &mut self.root_password);
        });
        crate::option_card(ui, &mut self.usb_shell, true, crate::tr!("setup.usb_console"), crate::tr!("setup.usb_console_about"), |ui| {
            // One password for both: the SSH card shows it when SSH is on.
            if !self.ssh {
                root_password_field(ui, &mut self.root_password);
            }
        });

        crate::group(ui, crate::tr!("setup.group.extras"));
        crate::toggle_card(ui, &mut self.doom, true, crate::tr!("patch.doom"), crate::tr!("setup.doom_about"));
        // Offered only with a gen 2 chosen or on the bus.
        if self.known_device(bus) == Some(Device::J49) {
            crate::toggle_card(ui, &mut self.dark_theme, true, crate::tr!("patch.dark_theme"), crate::tr!("patch.dark_theme_hint"));
        }
    }

    /// What the run will do, a line per choice, for the review step, with
    /// the step that sets it.
    pub fn summary(&self) -> Vec<(&'static str, String, crate::setup::Step)> {
        use crate::setup::Step;
        let on_off = |on: bool| if on { crate::tr!("setup.on") } else { crate::tr!("setup.off") }.to_string();
        let mut rows = vec![(crate::tr!("setup.mqtt"), if self.mqtt.on { self.mqtt.value.trim().to_string() } else { on_off(false) }, Step::Integrations)];
        let mut radio: Vec<&str> = Vec::new();
        if self.matter.on { radio.push(if self.matter_thread { crate::tr!("setup.matter_thread_short") } else { crate::tr!("patch.matter") }); }
        if self.zigbee { radio.push(crate::tr!("setup.zigbee")); }
        if self.wwn_boiler && self.holds_radio() { radio.push(crate::tr!("patch.wwn_boiler")); }
        if self.inputs().external_temperature && self.external_temperature_enabled() { radio.push(crate::tr!("patch.external_temperature")); }
        rows.push((crate::tr!("setup.other_integrations"), joined(&radio), Step::Integrations));
        rows.push((crate::tr!("patch.block_cloud"), on_off(self.block_cloud), Step::Features));
        if self.ntp.on {
            rows.push((crate::tr!("setup.time_server"), self.ntp.value.clone(), Step::Features));
        }
        let mut extras: Vec<&str> = Vec::new();
        if self.ssh { extras.push(crate::tr!("setup.ssh")); }
        if self.usb_shell { extras.push(crate::tr!("setup.usb_console")); }
        if self.doom { extras.push(crate::tr!("patch.doom")); }
        if self.inputs().dark_theme { extras.push(crate::tr!("patch.dark_theme")); }
        rows.push((crate::tr!("setup.extras"), joined(&extras), Step::Features));
        rows
    }

    /// Whether the external temperature option applies: to a gen 2 or 3, and
    /// to any unit while none is known.
    fn supports_external_temperature(&self) -> bool {
        matches!(self.device.or(self.last_device), None | Some(Device::Diamond3 | Device::J49))
    }

    /// Whether the external temperature switch can be turned on: a unit that
    /// takes it, and an integration to send the temperatures through.
    fn external_temperature_enabled(&self) -> bool {
        self.supports_external_temperature() && (self.mqtt.on || self.matter.on || self.zigbee)
    }

    /// The external temperature card's line: why it is greyed out, else
    /// what it does on the unit known.
    fn external_temperature_hint(&self) -> &'static str {
        if !self.supports_external_temperature() {
            return crate::tr!("patch.external_temperature_unsupported_hint");
        }
        if !self.external_temperature_enabled() {
            return crate::tr!("patch.external_temperature_needs_integration_hint");
        }
        match self.device.or(self.last_device) {
            Some(Device::Diamond3) => crate::tr!("patch.external_temperature_ble_hint"),
            Some(_) => crate::tr!("patch.external_temperature_hint"),
            None => crate::tr!("patch.external_temperature_any_hint"),
        }
    }

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

    /// The developer options, folded away on the review step: the CLI's
    /// options a run rarely needs, as the steps draw theirs.
    pub fn developer_ui(&mut self, ui: &mut egui::Ui, bus: &Bus) {
        let width = FIELD_WIDTH;
        crate::select(ui, crate::tr!("patch.device"), "device", width, self.device.map_or(crate::tr!("patch.auto"), Device::name), true, |ui| {
            ui.selectable_value(&mut self.device, None, crate::tr!("patch.auto"));
            for device in [Device::Diamond, Device::J49, Device::Diamond3] {
                ui.selectable_value(&mut self.device, Some(device), device.name());
            }
        }).on_hover_text(crate::tr!("patch.device_hint"));
        // Greyed without a gen 3, keeping the choice: the unit may only
        // have been unplugged, and other generations ignore it.
        let gen3 = self.known_device(bus) == Some(Device::Diamond3);
        crate::select(ui, crate::tr!("patch.entry_point"), "technique", width, entry_point_label(self.entry_point), gen3, |ui| {
            for entry_point in [EntryPoint::Auto, EntryPoint::OfficialLoader, EntryPoint::Cve20177932, EntryPoint::Cve20177936] {
                ui.selectable_value(&mut self.entry_point, entry_point, entry_point_label(entry_point));
            }
        }).on_hover_text(crate::tr!("patch.entry_point_choice_hint"))
            .on_disabled_hover_text(crate::tr!("patch.entry_point_disabled_hint"));
        ui.add_space(4.0);
        crate::switch_row(ui, &mut self.backplate_sim, crate::tr!("patch.backplate_sim"), crate::tr!("patch.backplate_sim_hint"));
    }
}

/// The items, joined, or None.
fn joined(items: &[&str]) -> String {
    if items.is_empty() { crate::tr!("setup.none").to_string() } else { items.join(" · ") }
}

/// Root's password, shared by SSH and the USB console.
fn root_password_field(ui: &mut egui::Ui, password: &mut String) {
    crate::field(ui, crate::tr!("patch.root_password"),
                 egui::TextEdit::singleline(password).password(true).hint_text("unfr0zen"), FIELD_WIDTH)
        .on_hover_text(crate::tr!("patch.root_password_hint"));
}

/// Text fields in the steps' cards.
const FIELD_WIDTH: f32 = 260.0;

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

    /// The texts the integrations step draws for the form, and where.
    fn integrations_texts(form: &mut Form) -> Vec<(String, f32)> {
        let context = egui::Context::default();
        crate::install_fonts(&context);
        crate::install_style(&context);
        let mut texts = Vec::new();
        for _ in 0..3 {
            let output = context.run_ui(egui::RawInput::default(), |ui| form.integrations_ui(ui));
            texts = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some((text.galley.job.text.clone(), text.pos.y)),
                _ => None,
            }).collect();
        }
        texts
    }

    #[test]
    fn enabling_tls_keeps_the_mqtt_card_the_same_height() {
        let matter_top = |tls| {
            let mut form = Form { mqtt: Toggle { on: true, value: String::new() }, mqtt_tls: tls, ..Form::default() };
            let texts = integrations_texts(&mut form);
            assert_eq!(tls, texts.iter().any(|(t, _)| t == "CA certificate"), "the CA button shows under TLS only");
            texts.iter().find(|(t, _)| t == "Matter").expect("Matter is drawn below the MQTT card").1
        };
        assert_eq!(matter_top(false), matter_top(true));
    }

    #[test]
    fn the_integrations_step_is_skippable_and_waits_only_for_a_chosen_brokers_address() {
        let mut form = Form::new();
        assert!(!form.any_integration() && form.integrations_done(), "nothing on: skipped");
        form.mqtt.on = true;
        assert!(!form.integrations_done(), "MQTT on with no broker");
        form.mqtt.value = "10.0.0.2".into();
        assert!(form.any_integration() && form.integrations_done());
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
    fn external_temperature_is_always_shown_and_enabled_only_for_a_unit_that_takes_it() {
        let plain = crate::tr!("patch.external_temperature").to_string();
        let ble = crate::tr!("patch.external_temperature_ble").to_string();
        for (device, shown, enabled) in [(None, &plain, true), (Some(Device::J49), &plain, true), (Some(Device::Diamond3), &ble, true),
                                         (Some(Device::Diamond), &plain, false), (Some(Device::Onyx), &plain, false)] {
            let mut form = Form { mqtt: Toggle { on: true, value: "tcp://broker:1883".into() }, last_device: device, ..Form::default() };
            let texts: Vec<String> = integrations_texts(&mut form).into_iter().map(|(t, _)| t).collect();
            assert_eq!([&plain, &ble].into_iter().find(|label| texts.contains(label)), Some(shown), "{device:?}");
            assert_eq!(form.external_temperature_enabled(), enabled, "{device:?}");
        }
        // Without an integration to send temperatures through, it waits greyed out.
        assert!(!Form::default().external_temperature_enabled());
    }

    #[test]
    fn external_temperatures_hint_says_why_it_is_greyed_out() {
        let mqtt = || Toggle { on: true, value: "tcp://broker:1883".into() };
        let hint = |form: Form| form.external_temperature_hint();
        assert_eq!(hint(Form { last_device: Some(Device::Diamond3), ..Form::default() }),
                   crate::tr!("patch.external_temperature_needs_integration_hint"));
        assert_eq!(hint(Form { mqtt: mqtt(), last_device: Some(Device::Diamond), ..Form::default() }),
                   crate::tr!("patch.external_temperature_unsupported_hint"));
        assert_eq!(hint(Form { mqtt: mqtt(), last_device: Some(Device::Diamond3), ..Form::default() }),
                   crate::tr!("patch.external_temperature_ble_hint"));
        assert_eq!(hint(Form { mqtt: mqtt(), last_device: Some(Device::J49), ..Form::default() }),
                   crate::tr!("patch.external_temperature_hint"));
        assert_eq!(hint(Form { mqtt: mqtt(), ..Form::default() }), crate::tr!("patch.external_temperature_any_hint"));
    }

    #[test]
    fn external_temperature_reaches_the_run_only_for_a_unit_that_takes_it() {
        for (device, sent) in [(None, true), (Some(Device::Diamond3), true), (Some(Device::J49), true), (Some(Device::Diamond), false), (Some(Device::Onyx), false)] {
            let form = Form { mqtt: Toggle { on: true, value: "tcp://broker:1883".into() }, external_temperature: true, last_device: device, ..Form::default() };
            assert_eq!(form.inputs().external_temperature, sent, "{device:?}");
        }
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
    fn config_takes_the_device_from_usb() {
        assert_eq!(Form::default().config().unwrap().omap, None);
    }

    #[test]
    fn the_developer_options_map_to_the_cli_options() {
        let mut form = Form { doom: true, device: Some(Device::J49), backplate_sim: true, ..Form::default() };
        form.ssh_key.path = Some("key.pub".into());
        let config = form.config().unwrap();
        assert_eq!(config.omap, Some(Device::J49));
        assert!(config.options.doom.is_some() && config.options.backplate_sim);
        assert!(form.inputs().ssh_public_key.is_none(), "the key only goes with the SSH server");
        form.ssh = true;
        assert_eq!(form.inputs().ssh_public_key.as_deref(), Some(std::path::Path::new("key.pub")));
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
        form.dark_theme = true;
        assert!(form.inputs().dark_theme && form.config().unwrap().options.dark_theme);
        form.device = Some(Device::Diamond3);
        assert!(!form.inputs().dark_theme, "only for a gen 2");
        assert!(!form.dialog_pending());
    }
}
