//! unfr0zen desktop: a simple native window over the same recovery flow the
//! terminal runs (unfr0zen-host). The form collects the options, Start runs
//! the flow on a background thread, and the stages appear as a live list.
//! Start's menu swaps the form for a whole-NAND backup or restore,
//! with a progress bar over the transfer.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
#[cfg(target_os = "macos")]
mod macos_about;
mod run;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::flow::default_data_dir;
use unfr0zen_host::update;
use unfr0zen_host::WAIT_USB_BOOT_STAGE;
use unfr0zen_host::{Config, Device, PatchInputs, DEFAULT_MATTER_CODE, DEFAULT_NTP_SERVER};

use run::{Event, Run};

const WILL_TM: &str = "https://github.com/will-tm";
const BUILD_VERSION: &str = env!("UNFR0ZEN_BUILD_VERSION");

fn main() -> eframe::Result<()> {
    // On Linux and macOS any argument runs the command-line tool instead of the
    // window. A -psn_ argument is the one old macOS versions pass from the Finder.
    #[cfg(unix)]
    if std::env::args_os().skip(1).any(|arg| !arg.to_string_lossy().starts_with("-psn_")) {
        unfr0zen_cli::main();
        return Ok(());
    }
    if std::env::args().skip(1).any(|arg| arg == "--version" || arg == "-V") {
        println!("unfr0zen {BUILD_VERSION}");
        return Ok(());
    }
    unfr0zen_host::init_logging();
    // One window at a time: a recovery run drives a single USB device, and two
    // would fight over it. Held for the whole process. On macOS the name is
    // the lock file's path: in the temporary directory, not wherever the app
    // was started from (the current directory, or / from the Finder).
    let name = if cfg!(target_os = "macos") {
        std::env::temp_dir().join("dev.unfr0zen.gui.lock").to_string_lossy().into_owned()
    } else {
        "dev.unfr0zen.gui".to_string()
    };
    let instance = single_instance::SingleInstance::new(&name).ok();
    if instance.as_ref().is_some_and(|i| !i.is_single()) {
        eprintln!("unfr0zen is already running.");
        return Ok(());
    }
    let mut viewport = egui::ViewportBuilder::default()
        // The logo (72 monospace chars) is the widest element; this is the
        // narrowest the window can be without clipping it. Fixed, no resize.
        .with_inner_size([440.0, 620.0])
        .with_resizable(false)
        // eframe also names the macOS app menu after the title, so there it
        // stays the app's name; the header shows the version on every platform.
        .with_title(if cfg!(target_os = "macos") { "unfr0zen" } else { BUILD_VERSION })
        // The Wayland app_id / X11 WM class: how the compositor maps the window
        // to the installed .desktop entry for its taskbar/dock icon.
        .with_app_id("dev.unfr0zen.gui")
        // macOS: no title bar band, the dark window fills to the top and the
        // traffic lights float over it. No-op elsewhere. The native title sits
        // left-aligned there, so it stays hidden and the header draws the version centered.
        .with_fullsize_content_view(true)
        .with_title_shown(false)
        .with_titlebar_shown(false)
        .with_titlebar_buttons_shown(true);
    // eframe sets this as the macOS application icon too, over the bundle's
    // .icns, so macOS gets the same rounded-square icon the .icns holds.
    #[cfg(target_os = "macos")]
    let png = include_bytes!("../../../assets/icon/unfr0zen-macos.png");
    #[cfg(not(target_os = "macos"))]
    let png = include_bytes!("../../../assets/icon/unfr0zen.png");
    if let Ok(icon) = eframe::icon_data::from_png_bytes(png) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let restart = Arc::new(AtomicBool::new(false));
    let result = eframe::run_native("unfr0zen", options, Box::new({
        let restart = restart.clone();
        move |cc| {
        // Dark whatever the system uses: egui's widgets, and the parts the
        // OS draws (title bar, window appearance, Wayland decorations).
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        // Every popup frame (dialogs, the Start menu, tooltips) pads its
        // content by this much.
        cc.egui_ctx.all_styles_mut(|style| style.spacing.menu_margin = egui::Margin::same(12));
        #[cfg(target_os = "macos")]
        macos_about::install();
        Ok(Box::new(App { updates: Updates::check(&cc.egui_ctx, restart), ..App::default() }))
    }}));
    drop(instance);
    // The new version starts once this one has let go of the instance lock.
    if restart.load(Ordering::Relaxed) {
        if let Err(e) = update::relaunch(update::App::Gui) {
            log::error!("{e}");
        }
    }
    result
}

/// Where an update from GitHub's releases stands.
#[derive(Clone, Default)]
enum UpdateState {
    #[default]
    Checking,
    None,
    Available(update::Available),
    Installing(String),
    Installed(String),
    Failed(String, update::Available),
}

/// The update check made at start, and the install it can lead to; both on
/// background threads, so the window never waits on the network.
#[derive(Default)]
struct Updates {
    state: Arc<Mutex<UpdateState>>,
    restart: Arc<AtomicBool>,
    /// The background check sets this to open the dialog automatically.
    auto_open: Arc<AtomicBool>,
    /// The update dialog is open.
    dialog_open: bool,
}

impl Updates {
    fn check(ctx: &egui::Context, restart: Arc<AtomicBool>) -> Updates {
        let updates = Updates { restart, ..Updates::default() };
        let (state, ctx) = (updates.state.clone(), ctx.clone());
        let auto_open = updates.auto_open.clone();
        std::thread::spawn(move || {
            let next = match update::check(update::App::Gui, BUILD_VERSION) {
                Ok(Some(release)) => UpdateState::Available(release),
                Ok(None) => UpdateState::None,
                Err(e) => { log::warn!("{e}"); UpdateState::None }
            };
            if matches!(next, UpdateState::Available(_)) {
                auto_open.store(true, Ordering::Relaxed);
            }
            *state.lock().unwrap() = next;
            ctx.request_repaint();
        });
        updates
    }

    fn install(&self, ctx: &egui::Context, release: update::Available) {
        *self.state.lock().unwrap() = UpdateState::Installing(release.tag.clone());
        let (state, ctx) = (self.state.clone(), ctx.clone());
        std::thread::spawn(move || {
            let next = match update::install(update::App::Gui, &release) {
                Ok(()) => UpdateState::Installed(release.tag),
                Err(e) => {
                    log::error!("{e}");
                    UpdateState::Failed(e.to_string(), release)
                }
            };
            *state.lock().unwrap() = next;
            ctx.request_repaint();
        });
    }

    /// The version label in the header bar.
    fn version_label(&self) -> String {
        let state = self.state.lock().unwrap().clone();
        match state {
            UpdateState::Checking => BUILD_VERSION.to_string(),
            UpdateState::None => BUILD_VERSION.to_string(),
            UpdateState::Available(ref r) => format!("{BUILD_VERSION} · {} available", r.tag),
            UpdateState::Installing(ref tag) => format!("{BUILD_VERSION} · installing {tag}…"),
            UpdateState::Installed(ref tag) => format!("{tag} installed — restart to finish"),
            UpdateState::Failed(..) => format!("{BUILD_VERSION} · update failed"),
        }
    }

    /// Whether there's something to show a dialog about.
    fn has_dialog(&self) -> bool {
        let state = self.state.lock().unwrap().clone();
        matches!(state, UpdateState::Available(_) | UpdateState::Installed(_) | UpdateState::Failed(..))
    }

    /// The update dialog, shown as a modal.
    fn dialog(&mut self, ctx: &egui::Context) {
        if self.auto_open.swap(false, Ordering::Relaxed) {
            self.dialog_open = true;
        }
        if !self.dialog_open { return; }
        let state = self.state.lock().unwrap().clone();
        let response = egui::Modal::new(egui::Id::new("update")).show(ctx, |ui| {
            match state {
                UpdateState::Available(ref release) => {
                    ui.set_width(400.0);
                    ui.heading(egui::RichText::new(format!("unfr0zen {} is available", release.tag))
                        .color(ui.visuals().strong_text_color()));
                    ui.label(format!("You're currently on {BUILD_VERSION}."));
                    ui.add_space(8.0);
                    if let Some(body) = &release.body {
                        let notes = body.trim();
                        if !notes.is_empty() {
                            ui.strong("Release Notes");
                            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                ui.label(notes);
                            });
                            ui.add_space(8.0);
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Install Update").clicked() {
                            self.install(ui.ctx(), release.clone());
                        }
                        if ui.button("Not Now").clicked() {
                            self.dialog_open = false;
                        }
                        ui.hyperlink_to("View on GitHub", &release.url);
                    });
                    false
                }
                UpdateState::Installing(ref tag) => {
                    ui.heading(egui::RichText::new(format!("Installing {tag}…"))
                        .color(ui.visuals().strong_text_color()));
                    ui.spinner();
                    false
                }
                UpdateState::Installed(ref tag) => {
                    ui.heading(egui::RichText::new(format!("{tag} installed"))
                        .color(ui.visuals().strong_text_color()));
                    ui.label("Restart unfr0zen to use the new version.");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Restart Now").clicked() {
                            self.restart.store(true, Ordering::Relaxed);
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        ui.button("Later").clicked()
                    }).inner
                }
                UpdateState::Failed(ref error, ref release) => {
                    ui.heading(egui::RichText::new("Update failed")
                        .color(ui.visuals().warn_fg_color));
                    ui.label(error.as_str());
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Try Again").clicked() {
                            self.install(ui.ctx(), release.clone());
                        }
                        ui.button("Close").clicked()
                    }).inner
                }
                _ => {
                    self.dialog_open = false;
                    false
                }
            }
        });
        if response.inner {
            self.dialog_open = false;
        }
    }
}

/// The patch options the form offers, as strings the user types.
#[derive(Default)]
struct Form {
    backplate_sim: bool,
    block_cloud: bool,
    ntp: Toggle,
    mqtt: Toggle,
    mqtt_user: String,
    mqtt_password: String,
    mqtt_tls: bool,
    mqtt_ca: Option<PathBuf>,
    matter: MatterCode,
    matter_thread: bool,
    zigbee: bool,
    ssh: bool,
    usb_shell: bool,
    root_password: String,
}

/// Matter, with the pairing code to enter on the phone (prefilled).
struct MatterCode {
    on: bool,
    code: String,
}

impl Default for MatterCode {
    fn default() -> Self {
        MatterCode { on: false, code: DEFAULT_MATTER_CODE.into() }
    }
}

/// An option that is off, or on with a value.
#[derive(Default)]
struct Toggle {
    on: bool,
    value: String,
}

impl Form {
    fn inputs(&self) -> PatchInputs {
        let text = |s: &str| (!s.is_empty()).then(|| s.to_string());
        PatchInputs {
            root_password: text(&self.root_password),
            ntp_server: self.ntp.on.then(|| self.ntp.value.clone()),
            cloud_url: None,  // the CLI's --cloud-url has no window control
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            overlay: None,
            mqtt: self.mqtt.on.then(|| self.mqtt.value.clone()),
            mqtt_user: self.mqtt.on.then(|| text(&self.mqtt_user)).flatten(),
            mqtt_password: self.mqtt.on.then(|| text(&self.mqtt_password)).flatten(),
            mqtt_tls: self.mqtt.on && self.mqtt_tls,
            // The CA field only shows under TLS, so it only sends under TLS.
            mqtt_ca: (self.mqtt.on && self.mqtt_tls).then(|| self.mqtt_ca.clone()).flatten(),
            matter: self.matter.on,
            matter_thread: self.matter.on && self.matter_thread,
            matter_code: self.matter.on.then(|| self.matter.code.clone()),
            zigbee: self.zigbee,
            doom: false,  // the CLI's --doom has no window control
            client_ng: false,  // nor its --client-ng
            ssh: self.ssh,
            usb_shell: self.usb_shell,
        }
    }

    fn config(&self) -> Result<Config, String> {
        let options = self.inputs().build().map_err(|e| e.to_string())?;
        let data_dir = default_data_dir();
        Ok(Config {
            options,
            omap: None,
            slot: None,
            dry_run: false,
            backup: false,
            wait_usb: WAIT_USB,
            wait_shell: Duration::from_secs(180),
            work_dir: data_dir.join("runs"),
            data_dir,
        })
    }
}

/// How long a run waits for the unit to appear in USB download mode.
const WAIT_USB: Duration = Duration::from_secs(120);

/// What Start does: patch the unit, or a whole-NAND job.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Patch,
    Backup,
    Restore,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Patch, Mode::Backup, Mode::Restore];

    fn button(self) -> &'static str {
        match self {
            Mode::Patch => "Start",
            Mode::Backup => "Back up",
            Mode::Restore => "Restore",
        }
    }

    fn menu(self) -> &'static str {
        match self {
            Mode::Patch => "Patch",
            Mode::Backup => "Back up NAND",
            Mode::Restore => "Restore NAND",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Mode::Patch => "",
            Mode::Backup => "Reads the whole NAND into an image, checks it against a second read, and writes its \
                             SHA-256 beside it. Choosing the same file again resumes an interrupted backup.",
            Mode::Restore => "Writes a backup, checked against its .sha256, back to NAND. The chip's own bad \
                              blocks are kept. Asks before erasing anything.",
        }
    }
}

/// The running NAND stage's bytes, when they were last reported, and the
/// point its rate is measured from.
struct Transfer {
    done: u64,
    next: u64,
    total: u64,
    at: Instant,
    since: Instant,
    since_done: u64,
}

impl Transfer {
    /// Bytes per second from the stage's first report to its latest, once
    /// bytes have moved.
    fn rate(&self) -> Option<f64> {
        let secs = (self.at - self.since).as_secs_f64();
        (self.done > self.since_done && secs > 0.0).then(|| (self.done - self.since_done) as f64 / secs)
    }

    /// Where the bar stands: the bytes done, plus the piece under way advanced
    /// at the measured rate but never past its end, which only a report
    /// confirms.
    fn shown(&self) -> u64 {
        let moving = self.rate().map_or(0, |rate| (self.at.elapsed().as_secs_f64() * rate) as u64);
        self.done + moving.min(self.next)
    }
}

#[derive(Default)]
struct App {
    form: Form,
    about_open: bool,
    mode: Mode,
    /// The NAND image a backup writes or a restore reads.
    image: Option<PathBuf>,
    run: Option<Run>,
    logs: LogContext,
    saved_logs: [LogContext; 3],
    /// The question a restore waits on before it writes, and where the answer goes.
    confirm: Option<(String, Sender<bool>)>,
    /// When the wait for USB download mode began: it shows as a popup, not a step.
    sdp_wait: Option<Instant>,
    drawer_open: bool,
    drawer_dragging: bool,
    drawer_dismissed: bool,
    drawer_height: Option<f32>,
    drawer_content_height: f32,
    updates: Updates,
}

#[derive(Default)]
struct LogContext {
    /// The stages so far, newest last; the last may still be running.
    rows: Vec<Row>,
    /// Status lines since the last step began or ended; the next one clears them.
    notes: Vec<String>,
    /// The prompt the run is waiting on, until the step it belongs to ends.
    waiting: Option<String>,
    transfer: Option<Transfer>,
    outcome: Option<Result<String, String>>,
    /// The unit the run found: device and silicon, once read.
    device: Option<String>,
}

struct Row {
    label: String,
    done: Option<(Duration, bool)>,
}

impl App {
    fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Start can run: a NAND job needs its image chosen.
    fn ready(&self) -> bool {
        self.mode == Mode::Patch || self.image.is_some()
    }

    /// Switches what Start does. A backup gets a fresh file name in the data
    /// directory; a restore starts with none, never the backup's.
    fn select(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        self.image = match mode {
            Mode::Backup => Some(default_data_dir().join("backups").join(nand::default_backup())),
            _ => None,
        };
        self.saved_logs[self.mode as usize] = std::mem::take(&mut self.logs);
        self.logs = std::mem::take(&mut self.saved_logs[mode as usize]);
        self.mode = mode;
        self.collapse_logs();
        self.drawer_height = None;
        self.drawer_content_height = 0.0;
    }

    fn choose_image(&mut self) {
        let dialog = rfd::FileDialog::new().add_filter("NAND image", &["bin"]);
        let chosen = match self.mode {
            Mode::Backup => {
                let current = self.image.clone().unwrap_or_else(nand::default_backup);
                let dialog = match current.parent().filter(|dir| dir.is_dir()) {
                    Some(dir) => dialog.set_directory(dir),
                    None => dialog,
                };
                dialog.set_file_name(current.file_name().unwrap_or_default().to_string_lossy()).save_file()
            }
            _ => dialog.pick_file(),
        };
        if chosen.is_some() {
            self.image = chosen;
        }
    }

    fn nand_job(&self) -> Result<Job, String> {
        let image = self.image.clone().ok_or("Choose an image first")?;
        Ok(match self.mode {
            Mode::Backup => {
                if let Some(dir) = image.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                Job::Backup(image)
            }
            Mode::Restore => Job::Restore(image),
            Mode::Patch => unreachable!("the patch flow is not a NAND job"),
        })
    }

    fn start(&mut self) {
        let run = match self.mode {
            Mode::Patch => self.form.config().map(Run::start),
            _ => self.nand_job().map(|job| Run::nand(job, WAIT_USB)),
        };
        match run {
            Ok(run) => {
                self.logs = LogContext::default();
                self.drawer_open = false;
                self.drawer_dismissed = false;
                self.drawer_height = None;
                self.drawer_content_height = 0.0;
                self.run = Some(run);
            }
            Err(message) => {
                self.logs.outcome = Some(Err(message));
                self.logs_arrived();
            }
        }
    }

    /// A new report restarts the rate when a new stage begins: fewer bytes
    /// done than before, or a different total.
    fn on_transfer(&mut self, done: u64, next: u64, total: u64) {
        let now = Instant::now();
        match &mut self.logs.transfer {
            Some(t) if done >= t.done && total == t.total => (t.done, t.next, t.at) = (done, next, now),
            _ => self.logs.transfer = Some(Transfer { done, next, total, at: now, since: now, since_done: done }),
        }
    }

    fn drain(&mut self) {
        let Some(run) = &self.run else { return };
        for event in run.events() {
            self.on_event(event);
        }
    }

    fn logs_arrived(&mut self) {
        if !self.drawer_dismissed {
            self.drawer_open = true;
        }
    }

    fn collapse_logs(&mut self) {
        self.drawer_open = false;
        self.drawer_dismissed = true;
    }

    fn on_event(&mut self, event: Event) {
        let has_log = match &event {
            Event::Begin(label) => label != WAIT_USB_BOOT_STAGE,
            Event::End { label, ok, .. } => label != WAIT_USB_BOOT_STAGE || !ok,
            Event::Status(_) | Event::Waiting(_) | Event::Device(_) | Event::Done(_) => true,
            _ => false,
        };
        match event {
            // Only the steps stay: a status line lasts until the next
            // step begins or ends, a prompt until its step ends. The flow
            // sends a prompt just before the step that waits on it begins.
            Event::Begin(label) => {
                self.logs.notes.clear();
                if label == WAIT_USB_BOOT_STAGE {
                    self.sdp_wait = Some(Instant::now());
                } else {
                    self.logs.rows.push(Row { label, done: None });
                }
            }
            Event::End { label, elapsed, ok } => {
                if label == WAIT_USB_BOOT_STAGE {
                    // The popup closes; only a failed wait joins the steps.
                    self.sdp_wait = None;
                    if !ok {
                        self.logs.rows.push(Row { label, done: Some((elapsed, ok)) });
                    }
                } else if let Some(row) = self.logs.rows.iter_mut().rev().find(|r| r.label == label && r.done.is_none()) {
                    row.done = Some((elapsed, ok));
                }
                self.logs.notes.clear();
                self.logs.waiting = None;
            }
            Event::Status(message) => self.logs.notes.push(message),
            Event::Device(description) => self.logs.device = Some(description),
            Event::Waiting(message) => self.logs.waiting = Some(message),
            Event::Transfer { done, next, total } => self.on_transfer(done, next, total),
            Event::Confirm(question, reply) => self.confirm = Some((question, reply)),
            Event::Done(result) => {
                self.sdp_wait = None;
                self.logs.outcome = Some(result);
                self.run = None;
            }
        }
        if has_log {
            self.logs_arrived();
        }
    }
}

// The diamond3 boot logo's icy blue, shared by the logo and the Start button.
const LOGO_BLUE: egui::Color32 = egui::Color32::from_rgb(0x81, 0xce, 0xfa);
const ACCENT: egui::Color32 = LOGO_BLUE;
const OK: egui::Color32 = egui::Color32::from_rgb(0x2e, 0xa0, 0x43);
const BAD: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x33, 0x2e);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();
        if self.running() {
            // Often enough for the progress bar to glide through a piece under way.
            let moving = self.logs.transfer.as_ref().is_some_and(|t| t.next > 0);
            ui.ctx().request_repaint_after(Duration::from_millis(if moving { 33 } else { 120 }));
        }

        egui::Panel::top("header").show_separator_line(false).show(ui, |ui| {
            // The row clears macOS's traffic lights; its hidden native title
            // is replaced with the version in the center.
            ui.allocate_ui(egui::vec2(ui.available_width(), 22.0), |ui| {
                let rect = ui.max_rect();
                if cfg!(target_os = "macos") {
                    let label = self.updates.version_label();
                    let has_update = self.updates.has_dialog();
                    ui.centered_and_justified(|ui| {
                        if has_update {
                            if ui.link(&label).clicked() {
                                self.updates.dialog_open = true;
                            }
                        } else {
                            ui.weak(&label);
                        }
                    });
                }
                let button_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 12.0, rect.center().y), egui::vec2(24.0, 22.0));
                let button = ui.interact(button_rect, ui.id().with("about"), egui::Sense::click());
                button.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "About unfr0zen"));
                ui.painter().circle_stroke(button_rect.center(), 6.0, egui::Stroke::new(1.1, LOGO_BLUE));
                ui.painter().text(button_rect.center() + egui::vec2(0.0, 0.5), egui::Align2::CENTER_CENTER,
                    "i", egui::FontId::proportional(12.0), LOGO_BLUE);
                if button.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("About unfr0zen").clicked() {
                    self.about_open = true;
                }
            });
            ui.add_space(8.0);
            logo(ui);
            ui.add_space(6.0);
        });

        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(LEFT_MARGIN);
                self.start_button(ui);
                if self.running() {
                    ui.add(egui::Spinner::new());
                    ui.label("running; keep the unit connected");
                } else if let Some(path) = unfr0zen_host::log_path() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("open log folder").clicked() {
                            if let Some(dir) = path.parent() {
                                let _ = open_path(dir);
                            }
                        }
                    });
                }
            });
            ui.add_space(4.0);
        });

        let drawer = self.log_drawer(ui);

        let options_frame = egui::Frame::central_panel(ui.style())
            .inner_margin(egui::Margin { bottom: 0, ..egui::Margin::same(8) });
        let options = egui::CentralPanel::default().frame(options_frame).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("options_scroll")
                .auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(LEFT_MARGIN);
                        ui.vertical(|ui| match self.mode {
                            Mode::Patch => {
                                ui.add_enabled_ui(!self.running(), |ui| self.form.ui(ui));
                            }
                            _ => self.nand_ui(ui),
                        });
                    });
                });
        });

        if drawer.height() > DRAWER_BAR_HEIGHT {
            let shadow_rect = egui::Rect::from_min_max(
                drawer.left_top() - egui::vec2(0.0, 16.0), drawer.right_top());
            let mut shadow = egui::Mesh::default();
            shadow.colored_vertex(shadow_rect.left_top(), egui::Color32::TRANSPARENT);
            shadow.colored_vertex(shadow_rect.right_top(), egui::Color32::TRANSPARENT);
            shadow.colored_vertex(shadow_rect.left_bottom(), egui::Color32::from_black_alpha(72));
            shadow.colored_vertex(shadow_rect.right_bottom(), egui::Color32::from_black_alpha(72));
            shadow.add_triangle(0, 1, 2);
            shadow.add_triangle(2, 1, 3);
            ui.painter().add(shadow);
        }

        if self.drawer_open && ui.input(|i| i.pointer.any_pressed())
            && ui.rect_contains_pointer(options.response.rect) {
            self.collapse_logs();
        }

        about::show(ui.ctx(), &mut self.about_open);
        self.updates.dialog(ui.ctx());
        self.sdp_ui(ui.ctx());
        self.confirm_ui(ui.ctx());
    }
}

const DRAWER_BAR_HEIGHT: f32 = 12.0;

/// Start's text colour, dark on the accent fill.
const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0b, 0x12, 0x1e);

impl App {
    fn log_drawer(&mut self, ui: &mut egui::Ui) -> egui::Rect {
        let has_logs = !self.logs.rows.is_empty() || !self.logs.notes.is_empty() || self.logs.outcome.is_some() || self.logs.device.is_some()
            || (self.logs.waiting.is_some() && self.sdp_wait.is_none());
        if !has_logs {
            self.drawer_open = false;
        }
        let max_height = (ui.available_height() * 0.6).max(80.0);
        let expanded_height = self.drawer_height
            .unwrap_or(self.drawer_content_height + DRAWER_BAR_HEIGHT + 12.0)
            .clamp(80.0, max_height);
        let openness = ui.ctx().animate_bool_with_time_and_easing(
            egui::Id::new("drawer_animation"), self.drawer_open,
            if self.drawer_dragging { 0.0 } else { 0.222 }, egui::emath::easing::cubic_out);
        let height = egui::lerp(DRAWER_BAR_HEIGHT..=expanded_height, openness);
        egui::Panel::bottom("log_drawer").exact_size(height)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(egui::Color32::from_gray(8))
                .inner_margin(egui::Margin::symmetric(8, 2)))
            .show(ui, |ui| {
                let (rect, handle) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 8.0),
                    if has_logs { egui::Sense::click_and_drag() } else { egui::Sense::hover() });
                let center = rect.center();
                ui.painter().line_segment([
                    center - egui::vec2(18.0, 0.0), center + egui::vec2(18.0, 0.0)],
                    egui::Stroke::new(3.0, ui.visuals().widgets.noninteractive.bg_stroke.color));
                let handle = if has_logs {
                    handle.on_hover_cursor(egui::CursorIcon::ResizeVertical)
                } else {
                    handle
                };
                self.drawer_dragging = handle.dragged();
                if self.drawer_dragging {
                    self.drawer_open = true;
                    self.drawer_dismissed = false;
                    self.drawer_height = Some((height - ui.input(|i| i.pointer.delta().y))
                        .clamp(80.0, max_height));
                }
                if handle.clicked() {
                    if self.drawer_open {
                        self.collapse_logs();
                    } else {
                        self.drawer_open = true;
                        self.drawer_dismissed = false;
                    }
                }
                if height > DRAWER_BAR_HEIGHT && ui.available_height() > 0.0 {
                    let scroll = egui::ScrollArea::vertical().id_salt(("progress_scroll", self.mode as usize))
                        .min_scrolled_height(0.0).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(LEFT_MARGIN);
                                ui.vertical(|ui| self.progress_ui(ui));
                            });
                        });
                    if (self.drawer_content_height - scroll.content_size.y).abs() > 0.5 {
                        self.drawer_content_height = scroll.content_size.y;
                        ui.ctx().request_repaint();
                    }
                }
            }).response.rect
    }

    /// Start, labelled for the mode, as one button with a ▾ inside its right
    /// end: a click there opens the menu that picks the mode, anywhere else
    /// starts. The menu stays usable while Start waits for an image.
    fn start_button(&mut self, ui: &mut egui::Ui) {
        const ARROW_WIDTH: f32 = 26.0;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(130.0, 28.0), egui::Sense::click());
        let split = rect.right() - ARROW_WIDTH;
        let (main, arrow) = (rect.with_max_x(split), rect.with_min_x(split));
        let running = self.running();
        let can_start = !running && self.ready();
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let on_arrow = pointer.is_some_and(|p| arrow.contains(p));
        if response.clicked() && !on_arrow && can_start {
            self.start();
        }
        let response = match on_arrow {
            true if !running => response.on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Patch, or back up or restore the NAND"),
            false if can_start => response.on_hover_cursor(egui::CursorIcon::PointingHand),
            _ => response,
        };

        let painter = ui.painter();
        let radius = ui.visuals().widgets.inactive.corner_radius;
        painter.rect_filled(rect, radius, if running { ACCENT.gamma_multiply(0.5) } else { ACCENT });
        // The hovered half lightens, as a separate button would.
        let lit = match (pointer.filter(|_| response.hovered()), on_arrow) {
            (Some(_), true) if !running => Some(arrow),
            (Some(_), false) if can_start => Some(main),
            _ => None,
        };
        if let Some(part) = lit {
            painter.rect_filled(part, radius, egui::Color32::from_white_alpha(40));
        }
        let ink = |on| if on { ON_ACCENT } else { ON_ACCENT.gamma_multiply(0.45) };
        painter.text(main.center(), egui::Align2::CENTER_CENTER, self.mode.button(),
                     egui::FontId::proportional(14.0), ink(can_start));
        painter.line_segment([egui::pos2(split, rect.top() + 7.0), egui::pos2(split, rect.bottom() - 7.0)],
                             egui::Stroke::new(1.0, ON_ACCENT.gamma_multiply(0.35)));
        // Drawn, not a glyph: the bundled fonts may not have ▾.
        let c = arrow.center();
        painter.add(egui::Shape::convex_polygon(
            vec![c + egui::vec2(-5.0, -2.5), c + egui::vec2(5.0, -2.5), c + egui::vec2(0.0, 3.5)],
            ink(!running), egui::Stroke::NONE));

        let toggle = response.clicked() && on_arrow && !running;
        egui::Popup::menu(&response).open_memory(toggle.then_some(egui::SetOpenCommand::Toggle)).show(|ui| {
            for mode in Mode::ALL {
                if ui.selectable_label(self.mode == mode, mode.menu()).clicked() {
                    self.select(mode);
                }
            }
        });
    }

    /// The NAND panel in place of the form: what the job does, its image, what
    /// is known of the NAND, and the transfer.
    fn nand_ui(&mut self, ui: &mut egui::Ui) {
        // Everything stops at the scroll bar's gutter: long values truncate
        // rather than widen the panel past the window.
        let width = ui.available_width() - SCROLLBAR_GUTTER;
        ui.set_max_width(width);
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.add_space(2.0);
        ui.add(egui::Label::new(self.mode.about()).wrap());
        let running = self.running();
        fact_row(ui, "Image", |ui| {
            if ui.add_enabled(!running, egui::Button::new("Choose…")).clicked() {
                self.choose_image();
            }
            if let Some(path) = &self.image {
                truncated(ui, &path.file_name().unwrap_or_default().to_string_lossy(), &path.display().to_string());
            }
        });
        for (label, value) in image_facts(self.mode, self.image.as_deref()) {
            fact_row(ui, label, |ui| truncated(ui, &value, &value));
        }
        // Where a backup lands, in full: wrapped rather than cut, and a link
        // to the folder once it exists.
        if let Some(dir) = self.image.as_deref().filter(|_| self.mode == Mode::Backup).and_then(|i| i.parent()) {
            fact_row(ui, "Folder", |ui| {
                let text = egui::RichText::new(dir.display().to_string());
                if dir.is_dir() {
                    let link = ui.add(egui::Label::new(text.color(ui.visuals().hyperlink_color)).wrap()
                        .sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
                    if link.clicked() {
                        let _ = open_path(dir);
                    }
                } else {
                    ui.add(egui::Label::new(text.weak()).wrap());
                }
            });
        }

        ui.add_space(4.0);
        let stage = self.logs.rows.iter().rev().find(|r| r.done.is_none()).map(|r| r.label.as_str());
        ui.label(egui::RichText::new(stage.unwrap_or(if running { "Starting" } else { "Idle" })).strong());
        let fraction = self.logs.transfer.as_ref().map_or(0.0, |t| t.shown() as f32 / t.total.max(1) as f32);
        // Until a rate is measured there is nothing to advance by; the bar
        // animates to show the job is alive.
        let measuring = running && self.logs.transfer.as_ref().is_none_or(|t| t.rate().is_none());
        // egui puts a bar's own text at its left end; the percentage is
        // painted over the middle instead.
        // No fill at all before progress: egui draws an empty fill as a dot.
        let fill = if fraction > 0.0 || measuring { ACCENT.gamma_multiply(0.7) } else { egui::Color32::TRANSPARENT };
        let bar = ui.add(egui::ProgressBar::new(fraction).desired_width(width).fill(fill).animate(measuring));
        ui.painter().text(bar.rect.center(), egui::Align2::CENTER_CENTER, format!("{:.0}%", fraction * 100.0),
                          egui::TextStyle::Button.resolve(ui.style()), ui.visuals().strong_text_color());
        if let Some(t) = &self.logs.transfer {
            ui.add(egui::Label::new(egui::RichText::new(transfer_line(t.done, t.next, t.total, t.rate())).weak())
                .wrap());
        }
    }

    /// The restore's last question, as a modal: nothing else can be clicked
    /// until it is answered.
    fn sdp_ui(&self, ctx: &egui::Context) {
        let Some(since) = self.sdp_wait else { return };
        egui::Modal::new(egui::Id::new("enter_sdp")).show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(egui::RichText::new("Put the device in USB download mode").strong().size(16.0));
            let step = |ui: &mut egui::Ui, number: &str, text: &str| {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new(number).strong());
                    ui.add(egui::Label::new(text).wrap());
                });
            };
            step(ui, "1.", "Connect the USB port on the back of the display to this computer with a data cable.");
            if let Some(prompt) = &self.logs.waiting {
                step(ui, "2.", &format!("{prompt}."));
            }
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                let left = WAIT_USB.saturating_sub(since.elapsed()).as_secs();
                ui.label(egui::RichText::new(format!("Waiting for the unit, {}:{:02} left", left / 60, left % 60)).weak());
            });
        });
    }

    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some((question, _)) = &self.confirm else { return };
        let question = question.clone();
        let answer = egui::Modal::new(egui::Id::new("confirm_restore")).show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.label(egui::RichText::new("Restore NAND?").strong().size(16.0));
            ui.add(egui::Label::new(question).wrap());
            let mut answer = None;
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
                let erase = egui::Button::new(egui::RichText::new("Erase and restore").color(egui::Color32::WHITE)).fill(BAD);
                if ui.add(erase).clicked() {
                    answer = Some(true);
                }
            });
            answer
        }).inner;
        if let Some(yes) = answer {
            if let Some((_, reply)) = self.confirm.take() {
                let _ = reply.send(yes);
            }
        }
    }
}

/// The NAND panel's label column, wide enough for its longest label.
const FACT_LABEL_WIDTH: f32 = 60.0;

/// A label in a fixed column, then the row's content in what is left of the
/// line.
fn fact_row(ui: &mut egui::Ui, label: &str, content: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(egui::vec2(FACT_LABEL_WIDTH, height), egui::Layout::left_to_right(egui::Align::Center),
                                   |ui| {
            ui.set_min_width(FACT_LABEL_WIDTH);
            ui.label(label);
        });
        content(ui);
    });
}

/// `text`, cut with an ellipsis at the line's end, `full` on hover.
fn truncated(ui: &mut egui::Ui, text: &str, full: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).weak()).truncate()).on_hover_text(full);
}

const MIB: f64 = (1u64 << 20) as f64;

/// What is known of the NAND and the image before the unit is reached.
fn image_facts(mode: Mode, image: Option<&std::path::Path>) -> Vec<(&'static str, String)> {
    let mut facts = vec![("NAND", "the whole chip; OMAP units as raw pages".to_string())];
    let Some(image) = image else { return facts };
    let checksum = PathBuf::from(format!("{}.sha256", image.display()));
    if mode == Mode::Backup {
        facts.push(("Writes", "image + .sha256 beside it".to_string()));
        return facts;
    }
    facts.push(("Size", match std::fs::metadata(image) {
        Ok(m) => match nand::image_device(m.len()) {
            Some(Device::J49) => format!("{:.0} MiB, an OMAP raw NAND (gen 1/2)", m.len() as f64 / MIB),
            Some(device) => format!("{:.0} MiB, a {}'s NAND", m.len() as f64 / MIB, device.name()),
            None => format!("{:.1} MiB, not a whole-NAND image", m.len() as f64 / MIB),
        },
        Err(e) => e.to_string(),
    }));
    facts.push(("SHA-256", if checksum.exists() {
        "beside it; checked before a restore".to_string()
    } else if mode == Mode::Restore {
        "missing; restore needs it".to_string()
    } else {
        "none beside it".to_string()
    }));
    facts
}

/// Bytes done of the total, the piece under way, and the rate and time left
/// once bytes have moved.
fn transfer_line(done: u64, next: u64, total: u64, rate: Option<f64>) -> String {
    let mut line = format!("{:.1} / {:.1} MiB", done as f64 / MIB, total as f64 / MIB);
    if next > 0 {
        line += &format!(", {:.1} MiB under way", next as f64 / MIB);
    }
    if let Some(rate) = rate.filter(|r| *r > 0.0) {
        let left = (total.saturating_sub(done) as f64 / rate).round() as u64;
        line += &format!(", {:.1} MiB/s, ", rate / MIB);
        line += &if left >= 60 { format!("{}m {:02}s left", left / 60, left % 60) } else { format!("{left}s left") };
    }
    line
}

/// Draws a check or cross in a small square, so it renders regardless of the
/// font (no bundled font has U+2713/U+2717, which showed as empty boxes).
fn outcome_mark(ui: &mut egui::Ui, ok: bool) {
    let size = 14.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let stroke = egui::Stroke::new(2.0, if ok { OK } else { BAD });
    let painter = ui.painter();
    if ok {
        // A check: a short stroke down to the vertex, a longer one up to the right.
        let p = |fx: f32, fy: f32| egui::pos2(rect.left() + size * fx, rect.top() + size * fy);
        painter.line_segment([p(0.20, 0.55), p(0.42, 0.74)], stroke);
        painter.line_segment([p(0.42, 0.74), p(0.80, 0.28)], stroke);
    } else {
        let pad = egui::vec2(2.5, 2.5);
        painter.line_segment([rect.left_top() + pad, rect.right_bottom() - pad], stroke);
        painter.line_segment([rect.right_top() + egui::vec2(-pad.x, pad.y), rect.left_bottom() + egui::vec2(pad.x, -pad.y)], stroke);
    }
}

impl App {
    fn progress_ui(&self, ui: &mut egui::Ui) {
        if let Some(device) = &self.logs.device {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Device").weak());
                ui.label(egui::RichText::new(device).strong());
            });
        }
        for row in &self.logs.rows {
            ui.horizontal(|ui| {
                match row.done {
                    None => {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(&row.label);
                    }
                    Some((elapsed, ok)) => {
                        outcome_mark(ui, ok);
                        ui.label(&row.label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Keep clear of the scroll bar on the right edge.
                            ui.add_space(SCROLLBAR_GUTTER);
                            ui.label(egui::RichText::new(format!("{:.1}s", elapsed.as_secs_f64())).weak());
                        });
                    }
                }
            });
        }
        for note in &self.logs.notes {
            ui.label(egui::RichText::new(note).weak().italics());
        }
        if let Some(prompt) = self.logs.waiting.as_ref().filter(|_| self.sdp_wait.is_none()) {
            // The current prompt the run is waiting on: white until it is met.
            // The SDP popup shows its own.
            ui.label(egui::RichText::new(prompt).color(egui::Color32::WHITE).strong());
        }
        if let Some(outcome) = &self.logs.outcome {
            // Spaced as a step; long paths wrap short of the scroll bar, each
            // line starting after the mark.
            ui.horizontal_top(|ui| {
                ui.set_max_width(ui.available_width() - SCROLLBAR_GUTTER);
                outcome_mark(ui, outcome.is_ok());
                let text = match outcome {
                    Ok(message) => egui::RichText::new(message).color(OK).strong(),
                    Err(message) => egui::RichText::new(message).color(BAD),
                };
                ui.add(egui::Label::new(text).wrap());
            });
        }
    }
}

impl Form {
    fn ui(&mut self, ui: &mut egui::Ui) {
        // One vertical gap everywhere, so every checkbox is spaced the same.
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.add_space(2.0);
        ui.checkbox(&mut self.backplate_sim, "Backplate simulator (1H1C, simulated temperature)");
        ui.checkbox(&mut self.block_cloud, "Block Nest and Google servers");
        // time.nest.com is blocked too, so time has to come from elsewhere.
        if self.block_cloud {
            self.ntp.on = true;
            if self.ntp.value.is_empty() {
                self.ntp.value = DEFAULT_NTP_SERVER.into();
            }
        }

        toggle_field(ui, &mut self.ntp, "Custom NTP server", "pool.ntp.org");

        ui.checkbox(&mut self.mqtt.on, "Relay to MQTT broker");
        if self.mqtt.on {
            // A two-column grid, indented as sub-items, so the labels line
            // up in one column and the inputs in the next.
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                egui::Grid::new("mqtt_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Broker");
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt.value).hint_text("host[:port]").desired_width(220.0));
                    ui.end_row();
                    ui.label("User");
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt_user).desired_width(220.0));
                    ui.end_row();
                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.mqtt_password).password(true).desired_width(220.0));
                    ui.end_row();
                    // "TLS" in the label column; the checkbox at the input
                    // column's left edge, the CA button at its right edge.
                    ui.label("TLS");
                    let height = ui.spacing().interact_size.y;
                    ui.allocate_ui_with_layout(egui::vec2(220.0, height),
                        egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.checkbox(&mut self.mqtt_tls, "");
                            if self.mqtt_tls {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("CA certificate").clicked() {
                                        if let Some(chosen) = rfd::FileDialog::new().add_filter("certificate", &["pem", "crt"]).pick_file() {
                                            self.mqtt_ca = Some(chosen);
                                        }
                                    }
                                    if let Some(chosen) = self.mqtt_ca.clone() {
                                        if ui.small_button("clear").clicked() {
                                            self.mqtt_ca = None;
                                        }
                                        ui.label(egui::RichText::new(chosen.file_name()
                                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())).weak());
                                    }
                                });
                            }
                        });
                    ui.end_row();
                });
            });
        }

        if ui.checkbox(&mut self.matter.on, "Matter").changed() && !self.matter.on {
            self.matter_thread = false;
        }
        if self.matter.on {
            // As MQTT's fields: the label in one column, the input in the next.
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                egui::Grid::new("matter_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Pairing code");
                    ui.add(egui::TextEdit::singleline(&mut self.matter.code).hint_text(DEFAULT_MATTER_CODE)
                        .desired_width(220.0));
                    ui.end_row();
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(SUBITEM_INDENT);
                ui.add_enabled(!self.zigbee,
                    egui::Checkbox::new(&mut self.matter_thread, "Thread support (replaces Heat Link)"));
            });
        }

        ui.add_enabled(!self.matter_thread,
            egui::Checkbox::new(&mut self.zigbee, "Zigbee (replaces Heat Link)"));

        ui.checkbox(&mut self.ssh, "SSH server (root login)");
        ui.checkbox(&mut self.usb_shell, "USB root shell");
        if self.ssh || self.usb_shell {
            ui.horizontal(|ui| {
                ui.label("Root password");
                ui.add(egui::TextEdit::singleline(&mut self.root_password).password(true)
                    .hint_text("unfr0zen").desired_width(220.0));
            });
        }
    }
}

/// Sub-items under a checkbox are indented this far.
const SUBITEM_INDENT: f32 = 20.0;
/// Left margin for the window content.
const LEFT_MARGIN: f32 = 12.0;
/// Right gutter so the scroll bar does not sit over the step durations.
const SCROLLBAR_GUTTER: f32 = 14.0;

fn toggle_field(ui: &mut egui::Ui, toggle: &mut Toggle, label: &str, hint: &str) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut toggle.on, label);
    });
    if toggle.on {
        ui.horizontal(|ui| {
            ui.add_space(20.0);
            ui.add(egui::TextEdit::singleline(&mut toggle.value).hint_text(hint).desired_width(220.0));
        });
    }
}

/// The boot logo's colour at height `t` (0 top, 1 bottom).
fn logo_gradient(t: f32) -> egui::Color32 {
    let [r, g, b] = unfr0zen_host::logo_color(t);
    egui::Color32::from_rgb(r, g, b)
}

// The frost shimmer, SHIMMER_DELAY after launch and then after a random pause
// in SHIMMER_PAUSE: a bright diagonal band sweeping left to right across the
// art, in columns, leaning SHIMMER_SLANT columns per row.
const SHIMMER_DELAY: f64 = 0.4;
const SHIMMER_PAUSE: std::ops::Range<f64> = 5.0..15.0;
const SHIMMER_SWEEP: f64 = 1.2;
const SHIMMER_HALF_WIDTH: f32 = 6.0;
const SHIMMER_SLANT: f32 = 1.5;

/// `color` under the shimmer band at `progress` (0 to 1 across the sweep) for
/// the character at `col`, `row`.
fn shimmer(color: egui::Color32, col: usize, row: usize, cols: usize, rows: usize, progress: f32) -> egui::Color32 {
    let span = cols as f32 + rows as f32 * SHIMMER_SLANT + 2.0 * SHIMMER_HALF_WIDTH;
    let center = progress * span - SHIMMER_HALF_WIDTH;
    let pos = col as f32 + (rows - 1 - row) as f32 * SHIMMER_SLANT;
    let glint = (1.0 - (pos - center).abs() / SHIMMER_HALF_WIDTH).max(0.0);
    color.lerp_to_gamma(egui::Color32::WHITE, glint * glint * 0.85)
}

/// The unfr0zen logo: the logo.txt block art in monospace, coloured with the
/// boot logo's gradient top to bottom, centred, with the clickable handle.
fn logo(ui: &mut egui::Ui) {
    let mut lines: Vec<&str> = unfr0zen_host::LOGO.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let last = lines.len().saturating_sub(1).max(1) as f32;
    let font = egui::FontId::monospace(9.0);
    let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let rows = lines.len();
    // The current or next sweep's start, kept across frames.
    let now = ui.input(|i| i.time);
    let id = egui::Id::new("logo_shimmer");
    let mut start = ui.data(|d| d.get_temp(id)).unwrap_or(SHIMMER_DELAY);
    if now - start >= SHIMMER_SWEEP {
        use std::hash::{BuildHasher, Hasher};
        let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
        let pause = SHIMMER_PAUSE.start + (SHIMMER_PAUSE.end - SHIMMER_PAUSE.start) * (random as f64 / u64::MAX as f64);
        start = now + pause;
        ui.data_mut(|d| d.insert_temp(id, start));
    }
    let elapsed = now - start;
    let progress = (elapsed >= 0.0).then(|| (elapsed / SHIMMER_SWEEP) as f32);
    match progress {
        Some(_) => ui.ctx().request_repaint(),
        None => ui.ctx().request_repaint_after(Duration::from_secs_f64(-elapsed)),
    }
    // The art in its row's colour, each character under the shimmer while it
    // plays.
    let art_text = |text: &str, row: usize, color: egui::Color32| -> egui::WidgetText {
        let Some(progress) = progress else {
            return egui::RichText::new(text).font(font.clone()).color(color).into();
        };
        let mut job = egui::text::LayoutJob::default();
        for (col, c) in text.chars().enumerate() {
            let format = egui::TextFormat::simple(font.clone(), shimmer(color, col, row, cols, rows, progress));
            job.append(c.encode_utf8(&mut [0; 4]), 0.0, format);
        }
        job.into()
    };
    // Hack's monospace advance is about 0.6 of the font size; enough to centre.
    let char_w = 9.0 * 0.6;
    let width = char_w * cols as f32;
    let pad = ((ui.available_width() - width) * 0.5).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, line) in lines.iter().enumerate() {
                // The byline is lighter than the dark bottom of the gradient;
                // the art beside it keeps its row's colour.
                let art = logo_gradient(i as f32 / last);
                let byline = egui::Color32::from_rgb(0xa8, 0xd4, 0xec);
                let part = |text: &str, color| egui::RichText::new(text).font(font.clone()).color(color);
                let plain = |ui: &mut egui::Ui, text: &str, color| ui.add(egui::Label::new(part(text, color))
                    .selectable(false).wrap_mode(egui::TextWrapMode::Extend));
                // The byline's handle opens the author's GitHub.
                match line.split_once("by @will-tm") {
                    Some((before, after)) => {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.add(egui::Label::new(art_text(before, i, art)).selectable(false)
                                .wrap_mode(egui::TextWrapMode::Extend));
                            plain(ui, "by ", byline);
                            let handle = ui.add(egui::Label::new(part("@will-tm", byline).underline())
                                .selectable(false).sense(egui::Sense::click()))
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if handle.clicked() {
                                let _ = open_url(WILL_TM);
                            }
                            if !after.is_empty() {
                                plain(ui, after, byline);
                            }
                        });
                    }
                    None => {
                        ui.add(egui::Label::new(art_text(line, i, art)).selectable(false)
                            .wrap_mode(egui::TextWrapMode::Extend));
                    }
                }
            }
        });
    });
}

/// Opens a folder in the platform's file manager.
fn open_path(path: &std::path::Path) -> std::io::Result<()> {
    opener().arg(path).spawn().map(drop)
}

/// Opens a URL in the default browser.
fn open_url(url: &str) -> std::io::Result<()> {
    opener().arg(url).spawn().map(drop)
}

/// The platform command that opens a path or URL.
fn opener() -> std::process::Command {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_logs_survive_switching_and_return_folded() {
        let mut app = App::default();
        for mode in Mode::ALL {
            app.select(mode);
            assert!(app.logs.rows.is_empty() && app.logs.notes.is_empty() && app.logs.outcome.is_none());
            assert!(app.logs.transfer.is_none());
            app.on_event(Event::Begin(mode.menu().into()));
            app.on_event(Event::End { label: mode.menu().into(), elapsed: Duration::from_secs(1), ok: true });
            app.on_event(Event::Status(format!("{} complete", mode.menu())));
            app.on_event(Event::Transfer { done: mode as u64 + 1, next: 0, total: 10 });
            app.on_event(Event::Done(Ok(mode.menu().into())));
        }
        for mode in [Mode::Patch, Mode::Backup, Mode::Restore, Mode::Patch] {
            app.select(mode);
            assert!(!app.drawer_open && app.drawer_dismissed);
            assert_eq!(app.logs.rows.len(), 1);
            assert_eq!(app.logs.rows[0].label, mode.menu());
            assert_eq!(app.logs.notes, [format!("{} complete", mode.menu())]);
            assert_eq!(app.logs.outcome.as_ref().unwrap().as_ref().unwrap(), mode.menu());
            assert_eq!(app.logs.transfer.as_ref().unwrap().done, mode as u64 + 1);
            app.drawer_open = true;
        }
    }

    #[test]
    fn dismissed_logs_stay_closed_until_reopened() {
        let mut app = App::default();
        app.on_event(Event::Begin("Read unit".into()));
        assert!(app.drawer_open);
        app.collapse_logs();
        app.on_event(Event::Status("Reading root0".into()));
        assert!(!app.drawer_open);
        app.on_event(Event::Done(Ok("Finished".into())));
        assert!(!app.drawer_open);
    }

    #[test]
    fn drawer_animation_eases_out_and_finishes_in_222ms() {
        let context = egui::Context::default();
        let mut app = App::default();
        let mut time = 0.0;
        let mut render = |app: &mut App| {
            let mut height = 0.0;
            let _ = context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(440.0, 500.0))),
                time: Some(time),
                ..Default::default()
            }, |ui| { height = app.log_drawer(ui).height(); });
            time += 1.0 / 60.0;
            height
        };
        assert_eq!(render(&mut app), DRAWER_BAR_HEIGHT);
        app.on_event(Event::Begin("Read unit".into()));
        app.drawer_height = Some(200.0);
        let opening: Vec<_> = (0..14).map(|_| render(&mut app)).collect();
        assert!(opening[0] > DRAWER_BAR_HEIGHT && opening[0] < 200.0);
        assert!(opening[6] > 150.0, "opening slows towards the end");
        assert!((opening[13] - 200.0).abs() < 0.1);
        app.collapse_logs();
        let closing: Vec<_> = (0..14).map(|_| render(&mut app)).collect();
        assert!(closing[0] < 200.0 && closing[0] > DRAWER_BAR_HEIGHT);
        assert!(closing[6] < 60.0, "closing slows towards the end");
        assert!((closing[13] - DRAWER_BAR_HEIGHT).abs() < 0.1);
        app.drawer_open = true;
        app.drawer_dragging = true;
        assert_eq!(render(&mut app), 200.0, "dragging bypasses animation");
    }

    #[test]
    fn drawer_grows_with_logs_and_preserves_options_space() {
        let context = egui::Context::default();
        let mut app = App::default();
        let render = |app: &mut App| {
            let mut options_height = 0.0;
            for _ in 0..30 {
                let _ = context.run_ui(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(440.0, 500.0))),
                    ..Default::default()
                }, |ui| {
                    app.log_drawer(ui);
                    let panel = egui::CentralPanel::default().show(ui, |_| {});
                    options_height = panel.response.rect.height();
                });
            }
            options_height
        };
        let closed = render(&mut app);
        app.drawer_open = true;
        assert_eq!(render(&mut app), closed, "an empty drawer stays folded");
        assert!(!app.drawer_open);
        app.on_event(Event::Begin("Read unit".into()));
        let first_log = render(&mut app);
        assert!(first_log < closed);
        for i in 0..30 {
            app.on_event(Event::Begin(format!("Stage {i}")));
        }
        let many_logs = render(&mut app);
        assert!(many_logs < first_log);
        assert!(many_logs >= 190.0, "options retain space when logs reach their cap");
        app.drawer_height = Some(120.0);
        assert!(render(&mut app) > many_logs, "manual height overrides content growth");
        app.collapse_logs();
        assert_eq!(render(&mut app), closed);
    }

    #[test]
    fn long_outcome_wraps_within_window() {
        let context = egui::Context::default();
        let message = "the volume has too little free space: 22626304 bytes available, 23236503 bytes required including staging and reserve";
        let app = App { logs: LogContext { outcome: Some(Err(message.into())), ..LogContext::default() }, ..App::default() };
        let output = context.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))),
            ..Default::default()
        }, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(LEFT_MARGIN);
                        ui.vertical(|ui| app.progress_ui(ui));
                    });
                });
            });
        });
        let galley = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == message => Some(&text.galley),
            _ => None,
        }).expect("error text is rendered");
        assert!(galley.rows.len() > 1);
        assert_eq!(galley.job.text, message);
        assert!(galley.rect.width() < 400.0);
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
    fn a_backup_gets_a_fresh_file_and_does_not_hand_it_to_a_restore() {
        let mut app = App::default();
        assert!(app.ready(), "patching needs no image");
        app.select(Mode::Backup);
        let image = app.image.clone().unwrap();
        assert!(image.starts_with(default_data_dir().join("backups")));
        assert!(image.file_name().unwrap().to_string_lossy().starts_with("nand-"));
        app.select(Mode::Restore);
        assert!(app.image.is_none() && !app.ready());
        app.image = Some("nand.bin".into());
        app.select(Mode::Patch);
        assert!(app.image.is_none() && app.ready());
    }

    #[test]
    fn transfer_line_adds_rate_and_time_once_bytes_move() {
        let mib = 1u64 << 20;
        assert_eq!(transfer_line(0, 5 * mib, 300 * mib, None), "0.0 / 300.0 MiB, 5.0 MiB under way");
        assert_eq!(transfer_line(60 * mib, 0, 300 * mib, Some(2.0 * mib as f64)),
                   "60.0 / 300.0 MiB, 2.0 MiB/s, 2m 00s left");
        assert_eq!(transfer_line(290 * mib, 0, 300 * mib, Some(2.0 * mib as f64)),
                   "290.0 / 300.0 MiB, 2.0 MiB/s, 5s left");
    }

    #[test]
    fn a_new_stage_restarts_the_rate() {
        let mut app = App::default();
        app.on_transfer(0, 10, 100);
        app.on_transfer(50, 10, 100);
        assert_eq!(app.logs.transfer.as_ref().map(|t| (t.done, t.since_done)), Some((50, 0)));
        app.on_transfer(0, 10, 100);
        assert_eq!(app.logs.transfer.as_ref().map(|t| (t.done, t.since_done)), Some((0, 0)));
        app.on_transfer(5, 1, 20);
        assert_eq!(app.logs.transfer.as_ref().map(|t| (t.done, t.total, t.since_done)), Some((5, 20, 5)));
    }

    #[test]
    fn the_bar_advances_through_the_piece_under_way_but_not_past_it() {
        let now = Instant::now();
        let ago = |secs| now - Duration::from_secs(secs);
        // 20 bytes confirmed over the 10 s to the last report: 2 bytes/s.
        let transfer = Transfer { done: 20, next: 8, total: 100, at: ago(2), since: ago(12), since_done: 0 };
        assert_eq!(transfer.rate(), Some(2.0));
        let shown = transfer.shown();
        assert!((24..=25).contains(&shown), "{shown}");
        let stalled = Transfer { at: ago(60), since: ago(70), ..transfer };
        assert_eq!(stalled.shown(), 28, "held at the end of the piece until it is confirmed");
        let first = Transfer { done: 0, next: 8, total: 100, at: ago(5), since: ago(5), since_done: 0 };
        assert_eq!((first.rate(), first.shown()), (None, 0), "no rate yet, nothing to advance by");
    }

    #[test]
    fn the_wait_for_sdp_mode_is_a_popup_and_only_a_failure_is_listed() {
        let mut app = App::default();
        app.on_event(Event::Waiting("Hold the button".into()));
        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        assert!(app.sdp_wait.is_some() && app.logs.rows.is_empty());
        assert_eq!(app.logs.waiting.as_deref(), Some("Hold the button"), "the popup shows the prompt");
        app.on_event(Event::End { label: WAIT_USB_BOOT_STAGE.into(), elapsed: Duration::from_secs(3), ok: true });
        assert!(app.sdp_wait.is_none() && app.logs.rows.is_empty() && app.logs.waiting.is_none());

        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        app.on_event(Event::End { label: WAIT_USB_BOOT_STAGE.into(), elapsed: Duration::from_secs(120), ok: false });
        assert!(app.sdp_wait.is_none());
        assert_eq!(app.logs.rows.iter().map(|r| (r.label.as_str(), r.done.map(|d| d.1))).collect::<Vec<_>>(),
                   [(WAIT_USB_BOOT_STAGE, Some(false))]);
    }

    #[test]
    fn the_device_stays_for_the_run_and_its_silicon_replaces_it() {
        let mut app = App::default();
        app.on_event(Event::Device("Nest gen 3 (diamond3)".into()));
        app.on_event(Event::Begin("Connect to recovery console".into()));
        app.on_event(Event::Device("Nest gen 3 (diamond3): i.MX6SoloX rev 1.2".into()));
        app.on_event(Event::End { label: "Connect to recovery console".into(), elapsed: Duration::ZERO, ok: true });
        assert_eq!(app.logs.device.as_deref(), Some("Nest gen 3 (diamond3): i.MX6SoloX rev 1.2"));
    }

    #[test]
    fn status_lines_last_until_the_next_step_and_prompts_until_theirs_ends() {
        let mut app = App::default();
        app.on_event(Event::Status("Detected app mode".into()));
        app.on_event(Event::Waiting("Plug it in".into()));
        app.on_event(Event::Begin("Load recovery U-Boot".into()));
        assert!(app.logs.notes.is_empty() && app.logs.waiting.is_some());
        app.on_event(Event::End { label: "Load recovery U-Boot".into(), elapsed: Duration::ZERO, ok: true });
        assert!(app.logs.waiting.is_none());
        app.on_event(Event::Status("Matter pairing code: 1234".into()));
        app.on_event(Event::Done(Ok("done".into())));
        assert_eq!(app.logs.notes, ["Matter pairing code: 1234"], "a line after the last step stays");
    }

    #[test]
    fn config_runs_the_real_write_on_the_boot_order_slot() {
        let config = Form::default().config().unwrap();
        assert_eq!(config.slot, None);
        assert!(!config.dry_run && !config.backup);
    }
}
