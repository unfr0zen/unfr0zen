//! unfr0zen desktop: a workbench over the same recovery flow the terminal
//! runs (unfr0zen-host). The sidebar picks a page: the patch form, the NAND
//! backups folder, the unit on USB, the run's log, and the advanced
//! commands. One task runs at a time, on a background thread; the status
//! bar follows it from any page.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod backdrop;
mod backups;
mod bus;
mod locale;
mod logs;
#[cfg(target_os = "macos")]
mod macos_about;
mod nandmap;
mod patch;
mod run;
mod terminal;
mod when;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use eframe::egui;
use unfr0zen_host::diamond3::boot;
use unfr0zen_host::flow::default_data_dir;
use unfr0zen_host::nand::Job;
use unfr0zen_host::update;
use unfr0zen_host::WAIT_USB_BOOT_STAGE;
use unfr0zen_host::Device;

use backups::Library;
use bus::{Bus, Watcher};
use patch::Form;
use run::{Event, Run};
use terminal::Terminal;

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
    unfr0zen_host::init_logging(BUILD_VERSION);
    update::remove_previous();
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
    if let Err(error) = unfr0zen_host::diamond3::usb::init_hid() {
        log::warn!("Could not start hidapi: {error}");
    }
    let mut viewport = egui::ViewportBuilder::default()
        // Fixed: every page is laid out for this size.
        .with_inner_size([WINDOW[0], WINDOW[1]])
        .with_resizable(false)
        // eframe also names the macOS app menu after the title, so there it
        // stays the app's name; the sidebar shows the version on every platform.
        .with_title(if cfg!(target_os = "macos") { "unfr0zen" } else { BUILD_VERSION })
        // The Wayland app_id / X11 WM class: how the compositor maps the window
        // to the installed .desktop entry for its taskbar/dock icon.
        .with_app_id("dev.unfr0zen.gui")
        // macOS: no title bar band, the dark window fills to the top and the
        // traffic lights float over the title bar the window draws.
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
        install_fonts(&cc.egui_ctx);
        no_zoom_keys(&cc.egui_ctx);
        #[cfg(debug_assertions)]
        no_id_shift_outlines(&cc.egui_ctx);
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals.panel_fill = BG;
            // No grid fitting: hinting thins and narrows the glyphs at 1x,
            // where the system's own text is unhinted.
            style.visuals.text_options.font_hinting = false;
            // Rounded as the design has it: widgets 4 px (the checkboxes share
            // it), surfaces 8 px.
            for widget in [&mut style.visuals.widgets.noninteractive, &mut style.visuals.widgets.inactive,
                           &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active,
                           &mut style.visuals.widgets.open] {
                widget.corner_radius = egui::CornerRadius::same(4);
            }
            style.visuals.window_corner_radius = egui::CornerRadius::same(8);
            style.visuals.menu_corner_radius = egui::CornerRadius::same(8);
            // Text as the design has it: near white, not egui's grey.
            style.visuals.widgets.noninteractive.fg_stroke.color = TEXT;
            style.visuals.widgets.inactive.fg_stroke.color = TEXT;
            style.visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
            style.visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;
            // Buttons, drop-downs and fields as the checkboxes: an inset box
            // with a thin border, lit on hover.
            let widgets = &mut style.visuals.widgets;
            widgets.inactive.bg_fill = INSET;
            widgets.inactive.weak_bg_fill = INSET;
            widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
            widgets.hovered.bg_fill = NAV_HOVER;
            widgets.hovered.weak_bg_fill = NAV_HOVER;
            widgets.hovered.bg_stroke = egui::Stroke::new(1.0, MUTED);
            widgets.active.bg_fill = NAV_SELECTED;
            widgets.active.weak_bg_fill = NAV_SELECTED;
            widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT);
            widgets.open.bg_fill = NAV_SELECTED;
            widgets.open.weak_bg_fill = NAV_SELECTED;
            widgets.open.bg_stroke = egui::Stroke::new(1.0, ACCENT);
            style.visuals.extreme_bg_color = INSET;
            style.interaction.selectable_labels = false;
            // Every popup frame (dialogs, menus, tooltips) sits on the page's
            // colour, so its widgets look as they do on a page, and pads its
            // content by this much.
            style.visuals.window_fill = BG;
            style.visuals.window_stroke = egui::Stroke::new(1.0, LINE);
            style.spacing.menu_margin = egui::Margin::same(12);
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 4.0);
            use egui::TextStyle::*;
            style.text_styles.insert(Small, egui::FontId::proportional(11.0));
            style.text_styles.insert(Body, egui::FontId::proportional(14.0));
            style.text_styles.insert(Button, egui::FontId::proportional(14.0));
            style.text_styles.insert(Monospace, egui::FontId::monospace(12.5));
            style.text_styles.insert(Heading, medium(20.0));
        });
        #[cfg(target_os = "macos")]
        macos_about::install();
        let mut app = App::new(&cc.egui_ctx, restart);
        app.backdrop = backdrop::Backdrop::new(cc.wgpu_render_state.as_ref());
        Ok(Box::new(app))
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

/// A drop-down whose rows keep one height. egui takes the inactive border's
/// width out of a row's padding but draws no frame on an inactive row, so with
/// the theme's border a row grows under the pointer and overflows the list.
pub(crate) fn dropdown(id: &str, width: f32) -> egui::ComboBox {
    egui::ComboBox::from_id_salt(id).width(width).popup_style(egui::style::StyleModifier::new(|style| {
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    }))
}

/// Leaves egui's zoom shortcuts unanswered: every page is laid out for the
/// window's one size.
fn no_zoom_keys(ctx: &egui::Context) {
    ctx.options_mut(|options| options.zoom_with_keyboard = false);
}

/// Leaves out the red outline and log line debug builds give a widget whose
/// auto id changed in place: a row coming or going shifts the ids of those
/// after it, which keeps no state of theirs from working here. Release builds
/// have neither, nor this option.
#[cfg(debug_assertions)]
fn no_id_shift_outlines(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| style.debug.warn_if_rect_changes_id = false);
}

/// The window's type: the platform's own UI font where it has one, Inter
/// behind it, each with a medium cut for headings and names; JetBrains Mono
/// for logs and the terminal. egui's own fonts stay last for the glyphs
/// they lack.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let data = |bytes: &'static [u8]| Arc::new(egui::FontData::from_static(bytes));
    fonts.font_data.insert("Inter".into(), data(include_bytes!("../../../assets/fonts/Inter-Regular.ttf")));
    fonts.font_data.insert("Inter-Medium".into(), data(include_bytes!("../../../assets/fonts/Inter-Medium.ttf")));
    fonts.font_data.insert("JetBrainsMono".into(), data(include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")));
    fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "Inter".into());
    fonts.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "JetBrainsMono".into());
    let mut medium = vec!["Inter-Medium".to_string()];
    medium.extend(fonts.families[&egui::FontFamily::Proportional].iter().cloned());
    if let Some((regular, medium_cut)) = system_font() {
        fonts.font_data.insert("system".into(), Arc::new(regular));
        fonts.font_data.insert("system-medium".into(), Arc::new(medium_cut));
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "system".into());
        medium.insert(0, "system-medium".into());
    }
    fonts.families.insert(egui::FontFamily::Name(MEDIUM.into()), medium);
    ctx.set_fonts(fonts);
}

/// The platform's UI font, read at start, as its regular and medium cuts:
/// San Francisco on macOS (one variable file, its weight axis set for the
/// medium), Segoe UI on Windows. None elsewhere.
fn system_font() -> Option<(egui::FontData, egui::FontData)> {
    if cfg!(target_os = "macos") {
        // At its text optical size, where the letters are spaced for
        // reading; the file's default is the tight display cut.
        let bytes = std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()?;
        let cut = |weight: f32| egui::FontTweak {
            coords: egui::epaint::text::VariationCoords::new([(b"opsz", 13.0), (b"wght", weight)]),
            ..egui::FontTweak::default()
        };
        Some((egui::FontData::from_owned(bytes.clone()).tweak(cut(400.0)), egui::FontData::from_owned(bytes).tweak(cut(500.0))))
    } else if cfg!(windows) {
        let dir = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
        let regular = std::fs::read(dir.join("segoeui.ttf")).ok()?;
        let medium = std::fs::read(dir.join("seguisb.ttf")).ok()?;
        Some((egui::FontData::from_owned(regular), egui::FontData::from_owned(medium)))
    } else {
        None
    }
}

/// The family name of the medium cut.
const MEDIUM: &str = "medium";

/// Inter Medium at `size`: headings, names, the selected page.
pub(crate) fn medium(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(MEDIUM.into()))
}

/// Runs a file dialog to its answer on its own thread and sends the chosen
/// path. The dialog must not run inside the frame: its modal panel pumps
/// its own event loop, which macOS delivers re-entrantly into winit's
/// event handler and aborts the process.
fn spawn_dialog(
    dialog: Box<dyn Future<Output = Option<rfd::FileHandle>> + Send>,
    sender: Sender<Option<PathBuf>>,
) {
    std::thread::spawn(move || {
        let mut dialog = Box::into_pin(dialog);
        loop {
            let mut context = Context::from_waker(Waker::noop());
            match dialog.as_mut().poll(&mut context) {
                Poll::Ready(Some(handle)) => {
                    let _ = sender.send(Some(handle.path().to_path_buf()));
                    return;
                }
                Poll::Ready(None) => {
                    let _ = sender.send(None);
                    return;
                }
                Poll::Pending => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    });
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

    /// The line under the version in the sidebar, once there is news.
    fn notice(&self) -> Option<String> {
        let state = self.state.lock().unwrap().clone();
        match state {
            UpdateState::Checking | UpdateState::None => None,
            UpdateState::Available(ref r) => Some(tr!("update.notice_available", version = r.tag)),
            UpdateState::Installing(ref tag) => Some(tr!("update.notice_installing", version = tag)),
            UpdateState::Installed(ref tag) => Some(tr!("update.notice_installed", version = tag)),
            UpdateState::Failed(..) => Some(tr!("update.notice_failed").into()),
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
        let response = backdrop::modal(ctx, "update").show(ctx, |ui| {
            match state {
                UpdateState::Available(ref release) => {
                    ui.set_width(400.0);
                    ui.heading(egui::RichText::new(tr!("update.available_heading", version = release.tag))
                        .color(ui.visuals().strong_text_color()));
                    ui.label(tr!("update.current_version", version = BUILD_VERSION));
                    ui.add_space(8.0);
                    if let Some(body) = &release.body {
                        let notes = body.trim();
                        if !notes.is_empty() {
                            ui.strong(tr!("update.release_notes"));
                            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                                ui.label(notes);
                            });
                            ui.add_space(8.0);
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(tr!("update.install")).clicked() {
                            self.install(ui.ctx(), release.clone());
                        }
                        if ui.button(tr!("update.not_now")).clicked() {
                            self.dialog_open = false;
                        }
                        ui.hyperlink_to(tr!("update.github"), &release.url);
                    });
                    false
                }
                UpdateState::Installing(ref tag) => {
                    ui.heading(egui::RichText::new(tr!("update.installing_heading", version = tag))
                        .color(ui.visuals().strong_text_color()));
                    ui.spinner();
                    false
                }
                UpdateState::Installed(ref tag) => {
                    ui.heading(egui::RichText::new(tr!("update.installed_heading", version = tag))
                        .color(ui.visuals().strong_text_color()));
                    ui.label(tr!("update.restart_description"));
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(tr!("update.restart")).clicked() {
                            self.restart.store(true, Ordering::Relaxed);
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        ui.button(tr!("update.later")).clicked()
                    }).inner
                }
                UpdateState::Failed(ref error, ref release) => {
                    ui.heading(egui::RichText::new(tr!("update.failed"))
                        .color(ui.visuals().warn_fg_color));
                    ui.label(error.as_str());
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(tr!("update.retry")).clicked() {
                            self.install(ui.ctx(), release.clone());
                        }
                        ui.button(tr!("common.close")).clicked()
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

/// How long a run waits for the unit to appear in USB download mode.
const WAIT_USB: Duration = Duration::from_secs(120);

/// The window, fixed: every page is laid out for it.
const WINDOW: [f32; 2] = [1080.0, 680.0];

/// The sidebar's pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Page {
    #[default]
    Patch,
    Backups,
    Logs,
    About,
    Uboot,
    Boot,
    Terminal,
}

impl Page {
    const MAIN: [Page; 4] = [Page::Patch, Page::Backups, Page::Logs, Page::About];
    const ADVANCED: [Page; 2] = [Page::Boot, Page::Terminal];

    fn title(self) -> &'static str {
        match self {
            Page::Patch => tr!("page.patch"),
            Page::Backups => tr!("page.backups"),
            Page::Logs => tr!("page.logs"),
            Page::About => tr!("page.about"),
            Page::Uboot => tr!("page.uboot"),
            Page::Boot => tr!("page.boot"),
            Page::Terminal => tr!("page.terminal"),
        }
    }
}

/// What a run does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Task {
    Patch,
    Backup(PathBuf),
    Restore(PathBuf),
    Uboot,
    Boot,
}

impl Task {
    fn title(&self) -> &'static str {
        match self {
            Task::Patch => tr!("page.patch"),
            Task::Backup(_) => tr!("task.backup"),
            Task::Restore(_) => tr!("task.restore"),
            Task::Uboot => tr!("task.uboot"),
            Task::Boot => tr!("task.boot"),
        }
    }

    fn log_title(&self) -> &'static str {
        match self {
            Task::Patch => "Patch",
            Task::Backup(_) => "Back up NAND",
            Task::Restore(_) => "Restore NAND",
            Task::Uboot => "Load recovery U-Boot",
            Task::Boot => "Boot the application",
        }
    }

    /// What the task is doing, for the run's card.
    fn doing(&self) -> &'static str {
        match self {
            Task::Patch => tr!("task.patching"),
            Task::Backup(_) => tr!("task.backing_up"),
            Task::Restore(_) => tr!("task.restoring"),
            Task::Uboot => tr!("task.loading_uboot"),
            Task::Boot => tr!("task.booting"),
        }
    }

    /// The page the task was started from, where its summary shows.
    fn page(&self) -> Page {
        match self {
            Task::Patch => Page::Patch,
            Task::Backup(_) | Task::Restore(_) => Page::Backups,
            Task::Uboot => Page::Uboot,
            Task::Boot => Page::Boot,
        }
    }

    fn is_nand(&self) -> bool {
        matches!(self, Task::Backup(_) | Task::Restore(_))
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

    /// The speed to show: the stage's average, lowered once the piece under
    /// way outlasts it, so that a stall shows before its report comes.
    fn speed(&self) -> Option<f64> {
        let rate = self.rate()?;
        if self.next == 0 {
            return Some(rate);
        }
        let reachable = (self.done + self.next - self.since_done) as f64 / self.since.elapsed().as_secs_f64();
        Some(rate.min(reachable))
    }
}

struct App {
    page: Page,
    form: Form,
    library: Library,
    bus: Watcher,
    run: Option<Run>,
    /// The task under way, or the last one: the log belongs to it.
    task: Option<Task>,
    logs: LogContext,
    /// The question a restore waits on before it writes, and where the answer goes.
    confirm: Option<(String, Sender<bool>)>,
    /// The backup a click asked to delete, until the question is answered.
    delete: Option<PathBuf>,
    /// The restore-from-file dialog a click opened, and the channel its
    /// choice arrives on over the following frames.
    pending_image: Option<Receiver<Option<PathBuf>>>,
    /// When the wait for USB download mode began: it shows as a popup, not a step.
    sdp_wait: Option<Instant>,
    /// The patch page's advanced panel is out.
    advanced_open: bool,
    /// The Serial console page's terminal, on the application's shells.
    shell: Terminal,
    /// The U-Boot page's terminal, on the recovery console.
    uboot: Terminal,
    maps: nandmap::Maps,
    /// The runs in the logs folder, newest first, and the one shown on the
    /// Logs page: None for the live run while there is a task.
    history: Vec<logs::Run>,
    history_selected: Option<String>,
    /// The shown run's log text, read once.
    log_text: Option<(String, String)>,
    log_filter: String,
    /// The delete-old-logs question is open.
    delete_logs: bool,
    /// When the task ended, for the HUD's short stay after a success.
    finished_at: Option<Instant>,
    /// The run's card was closed for this task.
    popup_dismissed: bool,
    /// What the modals sit on.
    backdrop: backdrop::Backdrop,
    /// The card's content height last frame, which sizes its region.
    card_height: f32,
    /// The wait for USB download mode was called off: the run's failure
    /// is the person's choice, not an error.
    cancelled: bool,
    /// The finished run kept a log. One that acted on nothing has none, and
    /// the Logs page does not list it.
    logged: bool,
    updates: Updates,
}

impl Default for App {
    fn default() -> Self {
        App {
            page: Page::default(),
            form: Form::default(),
            library: Library::new(default_data_dir().join("backups")),
            bus: Watcher::default(),
            run: None,
            task: None,
            logs: LogContext::default(),
            confirm: None,
            delete: None,
            pending_image: None,
            sdp_wait: None,
            advanced_open: false,
            shell: Terminal::new(terminal::SHELLS),
            uboot: Terminal::new(terminal::RECOVERY),
            maps: nandmap::Maps::default(),
            history: Vec::new(),
            history_selected: None,
            log_text: None,
            log_filter: String::new(),
            delete_logs: false,
            finished_at: None,
            popup_dismissed: false,
            backdrop: Default::default(),
            card_height: 0.0,
            cancelled: false,
            logged: false,
            updates: Updates::default(),
        }
    }
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

impl LogContext {
    /// The step under way.
    fn current(&self) -> Option<&str> {
        self.rows.iter().rev().find(|r| r.done.is_none()).map(|r| r.label.as_str())
    }
}

struct Row {
    label: String,
    started: Instant,
    done: Option<(Duration, bool)>,
}

impl App {
    fn new(ctx: &egui::Context, restart: Arc<AtomicBool>) -> App {
        let mut app = App { updates: Updates::check(ctx, restart), bus: Watcher::start(ctx.clone()), ..App::default() };
        app.library.refresh();
        app
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    /// The Logs page lists the current run: while it runs, and after if it
    /// kept a log.
    fn live_listed(&self) -> bool {
        self.task.is_some() && (self.running() || self.logged)
    }

    /// A task can start: nothing runs, the terminal holds no port a run
    /// would need, and no dialog is still on its way with a choice the task
    /// would miss.
    fn idle(&self) -> bool {
        !self.running() && !self.shell.connected() && !self.uboot.connected() && self.pending_image.is_none()
            && !self.form.dialog_pending()
    }

    fn show(&mut self, page: Page) {
        match page {
            Page::Backups => self.library.refresh(),
            Page::Logs => self.refresh_history(),
            _ => {}
        }
        self.page = page;
    }

    fn logs_dir() -> PathBuf {
        default_data_dir().join("logs")
    }

    /// Reads the logs folder again; a selection that vanished goes.
    fn refresh_history(&mut self) {
        self.history = logs::scan(&Self::logs_dir());
        self.preselect_latest_run();
    }

    /// With nothing validly selected, show the most recent log, so the pane is
    /// never empty when there are runs. A live run heads the list on its own,
    /// so leave it selected then.
    fn preselect_latest_run(&mut self) {
        let valid = self.history_selected.as_ref().is_some_and(|s| self.history.iter().any(|r| &r.stamp == s));
        if !valid {
            self.history_selected = (!self.live_listed()).then(|| self.history.first().map(|r| r.stamp.clone())).flatten();
        }
    }

    fn start(&mut self, task: Task) {
        if !self.idle() {
            return;
        }
        unfr0zen_host::end_log();
        unfr0zen_host::set_selection(match &task {
            Task::Patch => format!("Selected: Patch, {}", self.form.inputs().describe()),
            task => format!("Selected: {}", task.log_title()),
        });
        let entry_point = self.form.entry_point;
        let run = match &task {
            Task::Patch => self.form.config().map(Run::start),
            Task::Backup(out) => {
                let dir = out.parent().filter(|dir| !dir.as_os_str().is_empty());
                dir.map_or(Ok(()), |dir| std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display())))
                    .map(|_| Run::nand(Job::Backup(out.clone()), WAIT_USB, entry_point))
            }
            Task::Restore(image) => Ok(Run::nand(Job::Restore(image.clone()), WAIT_USB, entry_point)),
            Task::Uboot => Ok(Run::task(move |progress| boot::uboot(WAIT_USB, entry_point, progress))),
            Task::Boot => Ok(Run::task(move |progress| boot::boot(WAIT_USB, entry_point, progress))),
        };
        self.logs = LogContext::default();
        self.task = Some(task);
        self.finished_at = None;
        self.popup_dismissed = false;
        self.card_height = 0.0;
        self.cancelled = false;
        self.logged = false;
        self.history_selected = None;
        self.log_text = None;
        match run {
            Ok(run) => self.run = Some(run),
            Err(message) => self.logs.outcome = Some(Err(message)),
        }
    }

    /// Opens the picker for an image to restore from outside the folder.
    fn choose_restore_image(&mut self) {
        if self.pending_image.is_some() {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new().add_filter(tr!("backup.image_filter"), &["bin"]);
        let (sender, receiver) = std::sync::mpsc::channel();
        spawn_dialog(Box::new(dialog.pick_file()), sender);
        self.pending_image = Some(receiver);
    }

    /// Takes the restore dialog's answer when it arrives and starts the restore.
    fn drain_image_dialog(&mut self) {
        if let Some(receiver) = self.pending_image.take() {
            match receiver.try_recv() {
                Ok(Some(path)) => self.start(Task::Restore(path)),
                Ok(None) => {}
                Err(_) => self.pending_image = Some(receiver),
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

    fn on_event(&mut self, event: Event) {
        match event {
            // Only the steps stay: a status line lasts until the next
            // step begins or ends, a prompt until its step ends. The flow
            // sends a prompt just before the step that waits on it begins.
            Event::Begin(label) => {
                self.logs.notes.clear();
                if label == WAIT_USB_BOOT_STAGE {
                    self.sdp_wait = Some(Instant::now());
                } else {
                    self.logs.rows.push(Row { label, started: Instant::now(), done: None });
                }
            }
            Event::End { label, elapsed, ok } => {
                if label == WAIT_USB_BOOT_STAGE {
                    // The popup closes; only a failed wait joins the steps.
                    self.sdp_wait = None;
                    if !ok {
                        self.logs.rows.push(Row { label, started: Instant::now(), done: Some((elapsed, ok)) });
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
                // A run called off at the download prompt leaves no log.
                if self.cancelled {
                    if let Some(folder) = unfr0zen_host::log_path().and_then(|p| p.parent().map(Path::to_path_buf)) {
                        unfr0zen_host::end_log();
                        let _ = std::fs::remove_dir_all(folder);
                    }
                }
                self.logged = unfr0zen_host::log_path().is_some();
                // U-Boot at its prompt: the page's terminal opens its console.
                if self.task == Some(Task::Uboot) && result.is_ok() {
                    self.uboot.auto_connect = Some(unfr0zen_host::diamond3::usb::RECOVERY);
                }
                self.logs.outcome = Some(result);
                self.run = None;
                self.finished_at = Some(Instant::now());
                if self.page == Page::Logs {
                    self.refresh_history();
                }
                // A backup landed, or a restore read the folder's image.
                if let Some(Task::Backup(out)) = &self.task {
                    self.maps.forget(out);
                }
                if self.task.as_ref().is_some_and(Task::is_nand) {
                    self.library.refresh();
                }
            }
        }
    }
}

// The diamond3 boot logo's icy blue: the accent of every primary button.
const LOGO_BLUE: egui::Color32 = egui::Color32::from_rgb(0x81, 0xce, 0xfa);
pub(crate) const ACCENT: egui::Color32 = LOGO_BLUE;
/// Text on the accent fill.
const ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0b, 0x12, 0x1e);
const OK: egui::Color32 = egui::Color32::from_rgb(0x2e, 0xa0, 0x43);
pub(crate) const BAD: egui::Color32 = egui::Color32::from_rgb(0xd0, 0x33, 0x2e);
const WARN: egui::Color32 = egui::Color32::from_rgb(0xf3, 0xc5, 0x65);
// The shell's surfaces: the page, the sidebar, the title bar, the status bar.
const BG: egui::Color32 = egui::Color32::from_rgb(0x18, 0x1b, 0x21);
pub(crate) const SIDE: egui::Color32 = egui::Color32::from_rgb(0x14, 0x17, 0x1c);
const BAR: egui::Color32 = egui::Color32::from_rgb(0x1e, 0x22, 0x29);
pub(crate) const STATUS: egui::Color32 = egui::Color32::from_rgb(0x0e, 0x10, 0x14);
const INSET: egui::Color32 = egui::Color32::from_rgb(0x12, 0x15, 0x1a);
pub(crate) const LINE: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x2f, 0x39);
const NAV_SELECTED: egui::Color32 = egui::Color32::from_rgb(0x26, 0x2b, 0x35);
const NAV_HOVER: egui::Color32 = egui::Color32::from_rgb(0x1e, 0x22, 0x29);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xe6, 0xe8, 0xec);
pub(crate) const MUTED: egui::Color32 = egui::Color32::from_rgb(0x9a, 0xa3, 0xb2);
const MAP_ROOT0: egui::Color32 = egui::Color32::from_rgb(0x3f, 0x7f, 0xc4);
const MAP_ROOT1: egui::Color32 = egui::Color32::from_rgb(0x3f, 0xb5, 0x9f);
const MAP_SYSTEM_CONFIG: egui::Color32 = egui::Color32::from_rgb(0xb5, 0x8c, 0xf0);
const MAP_OTHER: egui::Color32 = egui::Color32::from_rgb(0x6b, 0x74, 0x86);
const MAP_ERASED: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x36, 0x45);
pub(crate) const DIM: egui::Color32 = egui::Color32::from_rgb(0x5c, 0x65, 0x75);

const TITLE_HEIGHT: f32 = 36.0;
const STATUS_HEIGHT: f32 = 24.0;
const SIDEBAR_WIDTH: f32 = 176.0;
const NAV_HEIGHT: f32 = 26.0;
/// The width macOS's traffic lights take at the title bar's left.
const TRAFFIC_LIGHTS: f32 = 66.0;
/// The backup detail pane's margin on each side of the map.
pub(crate) const DETAIL_MARGIN: f32 = 12.0;
/// The patch page's advanced panel.
const ADVANCED_WIDTH: f32 = 340.0;
/// The run's card: its width, the most of it the steps take, and how long
/// it stays after a success.
const POPUP_WIDTH: f32 = 420.0;
/// The close glyph's square.
const CLOSE_SIZE: f32 = 18.0;
const POPUP_MAX_HEIGHT: f32 = 600.0;
const POPUP_LINGER: Duration = Duration::from_millis(1500);
/// The Logs page's list: its text size, the gap between columns and the
/// padding at a row's left.
const HISTORY_FONT: f32 = 11.5;
/// The text of the steps in the Logs page's run pane.
const RUN_STEP_FONT: f32 = 12.0;
/// The share of the run pane's height its title and steps take at most.
const RUN_TOP_SHARE: f32 = 3.0 / 8.0;
const HISTORY_GAP: f32 = 10.0;
const HISTORY_PAD: f32 = 8.0;

/// A row of the Logs page's list: the folder's stamp (None for the live
/// run), when, the task, the result and its colour.
type HistoryRow = (Option<String>, String, String, &'static str, egui::Color32);

/// The widths the list's columns need: the widest of each, the result's
/// as its widest tag.
fn history_columns(ui: &egui::Ui, rows: &[HistoryRow]) -> [f32; 3] {
    let width = |text: &str, font: egui::FontId| ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, DIM).size().x);
    let widest = |pick: fn(&HistoryRow) -> &str, font: egui::FontId, least: f32| {
        rows.iter().map(|row| width(pick(row), font.clone())).fold(least, f32::max).ceil()
    };
    let when = widest(|row| &row.1, medium(HISTORY_FONT), width(&tr!("logs.run_column").to_uppercase(), egui::FontId::proportional(10.5))) + HISTORY_PAD;
    let task = widest(|row| &row.2, egui::FontId::proportional(HISTORY_FONT), width(&tr!("logs.task_column").to_uppercase(), egui::FontId::proportional(10.5)));
    // To the widest badge actually shown, not a fixed "running": with none
    // running, the column shrinks and the run pane takes the slack.
    let result = widest(|row| row.3, egui::FontId::proportional(11.0), width(&tr!("logs.result_column").to_uppercase(), egui::FontId::proportional(10.5))) + 14.0;
    [when, task, result]
}
/// Delete old logs removes runs older than this.
const LOG_RETENTION: Duration = Duration::from_secs(30 * 24 * 3600);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }
}

impl App {
    /// One frame: the shell around the page shown, then the dialogs.
    fn frame_ui(&mut self, ui: &mut egui::Ui) {
        self.drain();
        self.drain_image_dialog();
        self.form.track_device(&self.bus.current());
        if self.running() || self.pending_image.is_some() || self.form.dialog_pending() {
            // Often enough for the progress bar to glide through a piece under
            // way, and for a pending file dialog's answer to land the frame it
            // arrives in.
            let moving = self.logs.transfer.as_ref().is_some_and(|t| t.next > 0);
            ui.ctx().request_repaint_after(Duration::from_millis(if moving { 33 } else { 120 }));
        }

        self.title_bar(ui);
        self.status_bar(ui);
        self.sidebar(ui);
        let frame = egui::Frame::new().fill(BG)
            .inner_margin(egui::Margin { left: 20, right: 20, top: 14, bottom: 10 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| match self.page {
            Page::Patch => self.patch_page(ui),
            Page::Backups => self.backups_page(ui),
            Page::Logs => self.logs_page(ui),
            Page::Uboot => self.uboot_page(ui),
            Page::Boot => self.command_page(ui, Task::Boot,
                tr!("boot.description"),
                tr!("task.boot")),
            Page::Terminal => self.terminal_page(ui),
            Page::About => about::page(ui),
        });

        // One popup at a time: what needs an answer before the run's card,
        // and the update offer once the others have gone.
        let ctx = ui.ctx();
        if self.confirm.is_some() {
            self.confirm_ui(ctx);
        } else if self.sdp_wait.is_some() {
            self.sdp_ui(ctx);
        } else if self.delete.is_some() {
            self.delete_ui(ctx);
        } else if self.delete_logs {
            self.delete_logs_ui(ctx);
        } else if !self.run_popup(ctx) {
            self.updates.dialog(ctx);
        }
        let window = ui.ctx().content_rect();
        let between_bars = egui::Rect::from_min_max(window.min + egui::vec2(0.0, TITLE_HEIGHT),
                                                    window.max - egui::vec2(0.0, STATUS_HEIGHT));
        self.backdrop.show(ui.ctx(), between_bars);
    }

    /// The breadcrumb.
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(BAR).inner_margin(egui::Margin::symmetric(14, 0));
        egui::Panel::top("title").exact_size(TITLE_HEIGHT).show_separator_line(false).frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                if cfg!(target_os = "macos") {
                    ui.add_space(TRAFFIC_LIGHTS);
                }
                // The name, with the zero in the accent and a glint now and then.
                let font = egui::TextStyle::Body.resolve(ui.style());
                let time = ui.input(|input| input.time);
                // One section per letter whatever the colours: sections are
                // shaped apart, so merging equal neighbours, as append does,
                // would move the letters and the title bar as a glint passes.
                let mut name = egui::text::LayoutJob::single_section(NAME.into(), egui::TextFormat::default());
                name.sections = (0..NAME.len()).map(|index| egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: egui::text::ByteIndex(index)..egui::text::ByteIndex(index + 1),
                    format: egui::TextFormat::simple(font.clone(), name_color(index, time)),
                }).collect();
                ui.label(name);
                ui.ctx().request_repaint_after(glint_wait(time));
                ui.label(egui::RichText::new("›").color(DIM));
                ui.label(egui::RichText::new(self.page.title()).font(medium(13.5)));
            });
        });
    }

    /// The pages, and the version at the foot.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(SIDE).inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 8 });
        egui::Panel::left("sidebar").exact_size(SIDEBAR_WIDTH).resizable(false).show_separator_line(false).frame(frame)
            .show(ui, |ui| {
                for page in Page::MAIN {
                    self.nav_item(ui, page);
                }
                section(ui, tr!("common.advanced"));
                for page in Page::ADVANCED {
                    self.nav_item(ui, page);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        if ui.add(egui::Label::new(egui::RichText::new(BUILD_VERSION).size(11.0).color(DIM))
                            .sense(egui::Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(tr!("nav.about_hint")).clicked() {
                            self.show(Page::About);
                        }
                    });
                    if let Some(notice) = self.updates.notice() {
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            let text = egui::RichText::new(notice).size(11.0).color(ACCENT);
                            let label = egui::Label::new(text).sense(egui::Sense::click());
                            if ui.add(label).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                                && self.updates.has_dialog() {
                                self.updates.dialog_open = true;
                            }
                        });
                    }
                });
            });
    }

    /// One row of the sidebar: the page's name, lit when shown, with the
    /// backup count at its right end.
    fn nav_item(&mut self, ui: &mut egui::Ui, page: Page) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), NAV_HEIGHT), egui::Sense::click());
        let selected = self.page == page;
        let fill = if selected { NAV_SELECTED } else if response.hovered() { NAV_HOVER } else { egui::Color32::TRANSPARENT };
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, fill);
        let color = if selected { ui.visuals().strong_text_color() } else { MUTED };
        let font = if selected { medium(13.5) } else { egui::FontId::proportional(13.5) };
        nav_icon(painter, page, rect.left_center() + egui::vec2(16.0, 0.0), color);
        painter.text(rect.left_center() + egui::vec2(30.0, 0.0), egui::Align2::LEFT_CENTER, page.title(), font, color);
        let end = rect.right_center() - egui::vec2(10.0, 0.0);
        if page == Page::Backups && !self.library.entries.is_empty() {
            let text = self.library.entries.len().to_string();
            let galley = painter.layout_no_wrap(text, egui::FontId::proportional(10.5), MUTED);
            let badge = egui::Rect::from_center_size(end - egui::vec2(galley.size().x * 0.5 + 6.0, 0.0),
                                                     galley.size() + egui::vec2(12.0, 4.0));
            painter.rect_filled(badge, 8.0, LINE);
            painter.galley(badge.center() - galley.size() * 0.5, galley, MUTED);
        }
        let hint = match page {
            Page::Patch => tr!("nav.patch_hint"),
            Page::Backups => tr!("nav.backups_hint"),
            Page::Logs => tr!("nav.logs_hint"),
            Page::About => tr!("nav.about_page_hint"),
            Page::Uboot => tr!("nav.uboot_hint"),
            Page::Boot => tr!("nav.boot_hint"),
            Page::Terminal => tr!("nav.terminal_hint"),
        };
        if response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(hint).clicked() {
            self.show(page);
        }
        ui.add_space(2.0);
    }

    /// The unit on USB, and the task under way or how the last one ended.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(STATUS).inner_margin(egui::Margin::symmetric(12, 0));
        egui::Panel::bottom("status").exact_size(STATUS_HEIGHT).show_separator_line(false).frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let bus = self.bus.current();
                dot(ui, bus_color(&bus));
                ui.label(egui::RichText::new(bus.summary()).size(11.5).color(MUTED));
                if self.shell.port_name().or(self.uboot.port_name()).is_some() {
                    ui.separator();
                    ui.label(egui::RichText::new(tr!("status.console_open")).size(11.5).color(ACCENT));
                }
                if let Some(task) = &self.task {
                    ui.separator();
                    if self.running() {
                        ui.add(egui::Spinner::new().size(11.0).color(ACCENT));
                        ui.label(egui::RichText::new(task.doing()).monospace().size(11.0).color(MUTED));
                    } else if self.cancelled {
                        ui.label(egui::RichText::new(tr!("status.cancelled", task = task.title())).monospace().size(11.0).color(MUTED));
                    } else if let Some(outcome) = &self.logs.outcome {
                        let ok = outcome.is_ok();
                        outcome_mark(ui, ok, 11.0);
                        let text = if ok {
                            tr!("status.done", task = task.title())
                        } else {
                            tr!("status.failed", task = task.title())
                        };
                        ui.label(egui::RichText::new(text).monospace().size(11.0)
                            .color(if ok { MUTED } else { BAD }));
                    }
                }
            });
        });
    }

    /// The spinner and step of the task started from `page` while it runs,
    /// or how it ended.
    fn task_summary(&self, ui: &mut egui::Ui, page: Page) {
        if self.task.as_ref().map(Task::page) != Some(page) {
            return;
        }
        if self.running() {
            ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
            ui.label(egui::RichText::new(self.logs.current().unwrap_or(tr!("status.starting"))).color(MUTED));
        } else if let Some(outcome) = &self.logs.outcome {
            let (ok, text) = match outcome { Ok(t) => (true, t.as_str()), Err(t) => (false, t.as_str()) };
            outcome_mark(ui, ok, 14.0);
            let text = egui::RichText::new(text).color(if ok { OK } else { BAD });
            ui.add(egui::Label::new(text).truncate()).on_hover_text(text_of(outcome));
        }
    }

    fn patch_page(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new(tr!("page.patch")).strong());
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(tr!("patch.about")).color(MUTED)).wrap());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(egui::RichText::new(tr!("common.advanced")).size(12.0)).on_hover_text(tr!("patch.advanced_hint")).clicked() {
                    self.advanced_open = !self.advanced_open;
                }
            });
        });
        let frame = egui::Frame::new().inner_margin(egui::Margin { top: 10, ..egui::Margin::ZERO });
        egui::Panel::bottom("patch_actions").show_separator_line(true).frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                if accent_button(ui, tr!("patch.start"), self.idle())
                    .on_hover_text(tr!("patch.start_hint"))
                    .on_disabled_hover_text(tr!("patch.start_disabled"))
                    .clicked() {
                    self.start(Task::Patch);
                }
                if self.running() && self.task == Some(Task::Patch) {
                    running_notice(ui);
                }
            });
        });
        let frame = egui::Frame::new().fill(SIDE).corner_radius(8)
            .inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 10 });
        let running = self.running();
        egui::Panel::right("patch_advanced").exact_size(ADVANCED_WIDTH).resizable(false).show_separator_line(false)
            .frame(frame).show_collapsible(ui, &mut self.advanced_open, |ui| {
                egui::ScrollArea::vertical().id_salt("advanced").auto_shrink([false, false]).show(ui, |ui| {
                    let bus = self.bus.current();
                    ui.add_enabled_ui(!running, |ui| self.form.advanced_ui(ui, &bus));
                });
            });
        egui::ScrollArea::vertical().id_salt("patch").auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(2.0);
            ui.add_enabled_ui(!running, |ui| self.form.ui(ui));
        });
    }

    fn backups_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("backup.title")).font(medium(20.0)));
            ui.add_space(4.0);
            let dir = self.library.dir.clone();
            let short = short_path(&dir);
            if pill(ui, &short, ACCENT, INSET, true).on_hover_text(dir.display().to_string()).clicked() {
                let _ = std::fs::create_dir_all(&dir).and_then(|_| open_path(&dir));
            }
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new(
            tr!("backup.description")).color(MUTED));
        ui.add_space(10.0);
        let frame = egui::Frame::new().inner_margin(egui::Margin { top: 10, ..egui::Margin::ZERO });
        egui::Panel::bottom("backup_actions").show_separator_line(true).frame(frame).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Wide enough for the longer label, so the two match.
                let size = egui::vec2(130.0, 28.0);
                if accent_button_sized(ui, tr!("backup.create"), self.idle(), size)
                    .on_hover_text(tr!("backup.create_hint"))
                    .clicked() {
                    let out = self.library.fresh_path();
                    self.start(Task::Backup(out));
                }
                if ui.add_enabled(self.idle(), egui::Button::new(tr!("backup.restore_file")).min_size(size))
                    .on_hover_text(tr!("backup.restore_file_hint"))
                    .clicked() {
                    self.choose_restore_image();
                }
            });
        });
        // The pane holds the map exactly, whatever the display's pixel ratio.
        let detail_width = nandmap::width(ui.pixels_per_point()) + 2.0 * DETAIL_MARGIN;
        let frame = egui::Frame::new().fill(SIDE).corner_radius(8).inner_margin(egui::Margin::same(DETAIL_MARGIN as i8));
        egui::Panel::right("backup_detail").exact_size(detail_width).resizable(false).show_separator_line(false)
            .frame(frame).show(ui, |ui| self.backup_detail(ui));
        egui::ScrollArea::vertical().id_salt("backups").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 16.0);
            self.backup_table(ui);
        });
    }

    /// The folder's images, one row each, the selected one lit.
    fn backup_table(&mut self, ui: &mut egui::Ui) {
        if self.library.entries.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tr!("backup.empty")).font(medium(14.0)));
            ui.label(egui::RichText::new(tr!("backup.empty_hint")).color(MUTED));
            return;
        }
        // The name takes what the fixed columns leave.
        // Each column as wide as its widest value, its title at least; the
        // name takes what they leave.
        const GAP: f32 = 8.0;
        const ROW: f32 = 28.0;
        const CELL: f32 = 11.5;
        const RIGHT_MARGIN: f32 = 4.0;
        let header_font = egui::FontId::proportional(10.5);
        let measure = |text: &str, font: &egui::FontId| ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font.clone(), DIM).size().x);
        let column = |title: &'static str, values: &mut dyn Iterator<Item = String>| -> (&'static str, f32) {
            let widest = values.map(|v| measure(&v, &egui::FontId::proportional(CELL))).fold(0.0, f32::max);
            (title, widest.max(measure(&title.to_uppercase(), &header_font)).ceil())
        };
        let fixed: [(&str, f32); 6] = [
            column(tr!("common.device"), &mut self.library.entries.iter().map(|e| e.device_label().to_string())),
            column(tr!("backup.taken"), &mut self.library.entries.iter().map(|e| e.taken_label())),
            column(tr!("backup.size_column"), &mut self.library.entries.iter().map(|e| backups::size_label(e.size))),
            ("SHA-256", measure(tr!("backup.verified"), &egui::FontId::proportional(11.0)).max(measure("SHA-256", &header_font)) + 14.0),
            ("", 16.0),
            ("", 16.0),
        ];
        let width = ui.available_width();
        let name_width = (width - RIGHT_MARGIN - fixed.iter().map(|(_, w)| w + GAP).sum::<f32>()).max(80.0);
        let columns: Vec<(&str, f32)> = std::iter::once((tr!("backup.image_column"), name_width)).chain(fixed).collect();
        // The header.
        let (header, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
        let mut x = header.left();
        for (title, w) in &columns {
            ui.painter().text(egui::pos2(x, header.center().y), egui::Align2::LEFT_CENTER, title.to_uppercase(),
                              egui::FontId::proportional(10.5), DIM);
            x += w + GAP;
        }
        ui.painter().hline(header.x_range(), header.bottom(), egui::Stroke::new(1.0, LINE));
        let idle = self.idle();
        let mut clicked = None;
        let mut delete = None;
        for (i, entry) in self.library.entries.iter().enumerate() {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), egui::Sense::click());
            // The icons, on top of the row so their clicks do not select it.
            let icon_at = |column: usize| egui::pos2(rect.left() + name_width + GAP + fixed[..column].iter().map(|(_, w)| w + GAP).sum::<f32>() + 8.0, rect.center().y);
            let icon_rect = |at: egui::Pos2| egui::Rect::from_center_size(at, egui::vec2(24.0, 24.0));
            let (reveal_at, trash_at) = (icon_at(4), icon_at(5));
            let reveal = ui.interact(icon_rect(reveal_at), reveal_id(&entry.path), egui::Sense::click());
            let sense = if idle { egui::Sense::click() } else { egui::Sense::hover() };
            let trash = ui.interact(icon_rect(trash_at), delete_id(&entry.path), sense);
            let selected = Some(&entry.path) == self.library.selected.as_ref();
            let fill = if selected { NAV_SELECTED } else if response.hovered() || reveal.hovered() || trash.hovered() { NAV_HOVER } else { egui::Color32::TRANSPARENT };
            ui.painter().rect_filled(rect, 6.0, fill);
            ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, INSET));
            let text_color = ui.visuals().text_color();
            let cells = [entry.device_label().to_string(), entry.taken_label(), backups::size_label(entry.size)];
            let mut x = rect.left() + 8.0;
            let name = truncated(ui, entry.name(), medium(12.0), ui.visuals().strong_text_color(), name_width - 8.0);
            ui.painter().galley(egui::pos2(x, rect.center().y - name.size().y * 0.5), name, text_color);
            x += name_width + GAP - 8.0;
            for (text, (_, w)) in cells.iter().zip(&fixed) {
                let galley = truncated(ui, text.clone(), egui::FontId::proportional(CELL), text_color, *w);
                ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y * 0.5), galley, text_color);
                x += w + GAP;
            }
            let (text, fg, bg) = match &entry.checksum {
                Some(_) => (tr!("backup.verified"), OK, OK.gamma_multiply(0.18)),
                None => (tr!("backup.no_checksum"), WARN, WARN.gamma_multiply(0.18)),
            };
            paint_pill(ui, egui::pos2(x, rect.center().y), text, fg, bg);
            folder_icon(ui.painter(), reveal_at, ACCENT);
            trash_icon(ui.painter(), trash_at, if idle { BAD } else { BAD.gamma_multiply(0.45) });
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                clicked = Some(i);
            }
            if reveal.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("backup.reveal_hint")).clicked() {
                let _ = entry.path.parent().map(open_path);
            }
            if idle && trash.on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tr!("backup.delete_hint")).clicked() {
                delete = Some(i);
            }
        }
        if let Some(i) = clicked {
            self.library.selected = Some(self.library.entries[i].path.clone());
        }
        if let Some(i) = delete {
            self.delete = Some(self.library.entries[i].path.clone());
        }
    }

    /// The selected backup: what is known of it, and what can be done with it.
    fn backup_detail(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = self.library.selected().cloned() else {
            ui.label(egui::RichText::new(tr!("backup.select")).color(MUTED));
            return;
        };
        ui.spacing_mut().item_spacing.y = 8.0;
        // Cut rather than wrapped: a name has no break in it, and one wider
        // than the pane would widen everything under it.
        ui.add(egui::Label::new(egui::RichText::new(entry.name()).font(medium(12.5))).truncate())
            .on_hover_text(entry.name());
        match entry.device.and_then(|device| self.maps.get(&entry.path, device, ui.ctx())) {
            Some(map) => {
                let colors = nandmap::Colors {
                    uboot: ACCENT, env: WARN, system_config: MAP_SYSTEM_CONFIG,
                    root0: MAP_ROOT0, root1: MAP_ROOT1, other: MAP_OTHER, erased: MAP_ERASED, bad: BAD,
                };
                nandmap::draw(ui, &map, colors);
                nandmap::legend(ui, &map, colors);
                ui.label(egui::RichText::new(map.summary()).size(12.0).color(MUTED));
            }
            None => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(ACCENT));
                    ui.label(egui::RichText::new(tr!("backup.reading")).size(12.0).color(MUTED));
                });
            }
        }
        ui.add_space(2.0);
        ui.label(egui::RichText::new("SHA-256").size(11.0).color(DIM));
        match &entry.checksum {
            Some(sum) => { ui.add(egui::Label::new(egui::RichText::new(sum).monospace().size(11.5).color(MUTED)).wrap()); }
            None => { ui.label(egui::RichText::new(tr!("backup.checksum_required")).size(12.0).color(WARN)); }
        }
        ui.add(egui::Label::new(egui::RichText::new(short_path(&entry.path)).size(11.5).color(DIM)).wrap());
        let idle = self.idle();
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let size = egui::vec2(ui.available_width(), 28.0);
            if accent_button_sized(ui, tr!("backup.restore"), idle && entry.checksum.is_some(), size)
                .on_hover_text(tr!("backup.restore_hint"))
                .on_disabled_hover_text(if entry.checksum.is_none() {
                    tr!("backup.restore_disabled_checksum")
                } else {
                    tr!("run.disabled_busy")
                }).clicked() {
                self.start(Task::Restore(entry.path.clone()));
            }
        });
    }

    /// The running NAND stage and its transfer.
    fn nand_progress(&self, ui: &mut egui::Ui) {
        let running = self.running();
        let stage = self.logs.current().unwrap_or(if running { tr!("run.starting") } else { tr!("run.done") });
        ui.label(egui::RichText::new(stage).strong());
        // The bar and the line under it together: reports can be 64 MiB apart.
        // A run that ended stands at what it reported.
        let position = |t: &Transfer| if running { t.shown() } else { t.done };
        let fraction = self.logs.transfer.as_ref().map_or(0.0, |t| position(t) as f32 / t.total.max(1) as f32);
        // No fill at all before progress: egui draws an empty fill as a dot.
        let fill = if fraction > 0.0 { ACCENT.gamma_multiply(0.7) } else { egui::Color32::TRANSPARENT };
        let width = ui.available_width();
        let bar = ui.add(egui::ProgressBar::new(fraction).desired_width(width).fill(fill));
        ui.painter().text(bar.rect.center(), egui::Align2::CENTER_CENTER, tr!("transfer.percent", percent = format!("{:.0}", fraction * 100.0)),
                          egui::TextStyle::Button.resolve(ui.style()), ui.visuals().strong_text_color());
        if let Some(t) = &self.logs.transfer {
            let (size, speed, left) = transfer_texts(position(t), t.next, t.total, t.speed());
            let font = egui::FontId::proportional(11.5);
            let (row, _) = ui.allocate_exact_size(egui::vec2(width, font.size + 4.0), egui::Sense::hover());
            let painter = ui.painter();
            painter.text(row.left_center(), egui::Align2::LEFT_CENTER, size, font.clone(), MUTED);
            painter.text(row.center(), egui::Align2::CENTER_CENTER, speed, font.clone(), MUTED);
            painter.text(row.right_center(), egui::Align2::RIGHT_CENTER, left, font, MUTED);
        }
    }

    /// The runs: the live one and every log folder, with the shown run's
    /// steps and log beside them.
    fn logs_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("page.logs")).font(medium(20.0)));
            ui.add_space(4.0);
            let dir = Self::logs_dir();
            if pill(ui, &short_path(&dir), ACCENT, INSET, true).on_hover_text(dir.display().to_string()).clicked() {
                let _ = std::fs::create_dir_all(&dir).and_then(|_| open_path(&dir));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = egui::RichText::new(tr!("logs.delete_old")).size(12.0);
                if ui.add_enabled(!self.history.is_empty(), egui::Button::new(label))
                    .on_hover_text(tr!("logs.delete_old_hint"))
                    .clicked() {
                    self.delete_logs = true;
                }
            });
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new(
            tr!("logs.description"))
            .color(MUTED));
        ui.add_space(10.0);
        // The list is as wide as its widest texts; the run pane takes the rest.
        let rows = self.history_rows();
        let columns = history_columns(ui, &rows);
        // Only enough past the last column for the scrollbar, then a small gap
        // to the pane, so the pane keeps as much width as it can.
        let list_width = columns.iter().sum::<f32>() + HISTORY_GAP * 2.0 + 8.0;
        let pane_width = (ui.available_width() - list_width - 6.0).max(200.0);
        let frame = egui::Frame::new().fill(SIDE).corner_radius(8).inner_margin(egui::Margin::same(14));
        egui::Panel::right("run_pane").exact_size(pane_width).resizable(false).show_separator_line(false)
            .frame(frame).show(ui, |ui| self.run_pane(ui));
        egui::ScrollArea::vertical().id_salt("history").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(ui.available_width() - 6.0);
            self.history_table(ui, rows, columns);
        });
    }

    /// The list's rows: the live run first, then the folder's; the live
    /// run's own folder is listed under it once it has one, so it is left
    /// out. Each: the folder's stamp (None for the live run), when, the
    /// task, the result and its colour.
    fn history_rows(&self) -> Vec<HistoryRow> {
        let live_stamp = unfr0zen_host::log_path().and_then(|p| p.parent()?.file_name().map(|n| n.to_string_lossy().into_owned()));
        let live = self.task.as_ref().filter(|_| self.live_listed()).map(|task| {
            let (result, color) = match (&self.logs.outcome, self.running()) {
                (_, true) => (tr!("logs.running"), ACCENT),
                (Some(Ok(_)), _) => (tr!("logs.ok"), OK),
                (Some(Err(_)), _) => (tr!("logs.failed"), BAD),
                (None, false) => ("—", MUTED),
            };
            (None::<String>, tr!("logs.now").to_string(), task.title().to_string(), result, color)
        });
        live.into_iter().chain(
            self.history.iter().filter(|r| self.task.is_none() || Some(&r.stamp) != live_stamp.as_ref()).map(|run| {
                let (result, color) = if run.ok() { (tr!("logs.ok"), OK) } else { (tr!("logs.failed"), BAD) };
                let when = tr!("logs.run_when", when = run.when, seconds = format!("{:.0}", run.elapsed()));
                (Some(run.stamp.clone()), when, run.title().to_string(), result, color)
            })).collect()
    }

    /// One row per run, the live one first, the shown one lit, in columns
    /// of the given widths.
    fn history_table(&mut self, ui: &mut egui::Ui, rows: Vec<HistoryRow>, columns: [f32; 3]) {
        const ROW: f32 = 26.0;
        let [when_width, task_width, result_width] = columns;
        let width = ui.available_width();
        let (header, _) = ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::hover());
        let mut x = header.left();
        for (title, w) in [(tr!("logs.run_column"), when_width), (tr!("logs.task_column"), task_width), (tr!("logs.result_column"), result_width)] {
            ui.painter().text(egui::pos2(x, header.center().y), egui::Align2::LEFT_CENTER, title.to_uppercase(),
                              egui::FontId::proportional(10.5), DIM);
            x += w + HISTORY_GAP;
        }
        ui.painter().hline(header.x_range(), header.bottom(), egui::Stroke::new(1.0, LINE));
        let text_color = ui.visuals().text_color();
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(tr!("logs.empty")).font(medium(14.0)));
            return;
        }
        let mut clicked = None;
        for (stamp, when, task, result, color) in rows {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), egui::Sense::click());
            let selected = stamp == self.history_selected && (stamp.is_some() || self.task.is_some());
            let fill = if selected { NAV_SELECTED } else if response.hovered() { NAV_HOVER } else { egui::Color32::TRANSPARENT };
            ui.painter().rect_filled(rect, 6.0, fill);
            ui.painter().hline(rect.x_range(), rect.bottom(), egui::Stroke::new(1.0, INSET));
            let mut x = rect.left() + HISTORY_PAD;
            let font = if stamp.is_none() { medium(HISTORY_FONT) } else { egui::FontId::proportional(HISTORY_FONT) };
            let galley = truncated(ui, when, font, text_color, when_width);
            ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y * 0.5), galley, text_color);
            x += when_width + HISTORY_GAP;
            let galley = truncated(ui, task, egui::FontId::proportional(HISTORY_FONT), text_color, task_width);
            ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y * 0.5), galley, text_color);
            x += task_width + HISTORY_GAP;
            paint_pill(ui, egui::pos2(x, rect.center().y), result, color, color.gamma_multiply(0.18));
            if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                clicked = Some(stamp);
            }
        }
        if let Some(stamp) = clicked {
            self.history_selected = stamp;
            self.log_text = None;
        }
    }

    /// The shown run: its steps, then its log with a filter, and the actions.
    fn run_pane(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 6.0;
        // The title and steps end here at most; the log has the rest.
        let top_end = ui.cursor().top() + ui.available_height() * RUN_TOP_SHARE;
        let live = self.history_selected.is_none() && self.live_listed();
        let run = self.history_selected.as_ref().and_then(|s| self.history.iter().find(|r| &r.stamp == s)).cloned();
        let path = match (&run, live) {
            (Some(run), _) => Some(run.log_path()),
            (None, true) => unfr0zen_host::log_path(),
            _ => None,
        };
        let (title, subtitle) = match (&run, live) {
            (Some(run), _) => (run.title().to_string(), tr!("logs.run_version", when = run.when, version = run.version)),
            (None, true) => (self.task.as_ref().map(Task::title).unwrap_or_default().to_string(), tr!("logs.now").to_string()),
            _ => {
                ui.label(egui::RichText::new(tr!("logs.select")).color(MUTED));
                return;
            }
        };
        ui.label(egui::RichText::new(title).font(medium(13.0)));
        ui.label(egui::RichText::new(subtitle).size(11.0).color(MUTED));
        ui.add_space(2.0);
        let steps_height = (top_end - ui.cursor().top()).max(40.0);
        egui::ScrollArea::vertical().id_salt("run_steps").max_height(steps_height).auto_shrink([false, true])
            .stick_to_bottom(live).show(ui, |ui| {
                ui.set_width(ui.available_width() - 12.0);
                // The live run and a past one read the same.
                ui.style_mut().text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(RUN_STEP_FONT));
                ui.spacing_mut().item_spacing.y = 4.0;
                if live {
                    self.progress_ui(ui);
                } else if let Some(run) = &run {
                    for step in &run.steps {
                        ui.horizontal(|ui| {
                            outcome_mark(ui, step.ok, 14.0);
                            ui.label(&step.label);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(egui::RichText::new(tr!("run.elapsed", seconds = format!("{:.1}", step.elapsed))).color(DIM));
                            });
                        });
                    }
                    if let Some(failure) = &run.failure {
                        ui.add(egui::Label::new(egui::RichText::new(failure).color(BAD)).wrap());
                    }
                }
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(path) = &path {
                    if ui.add(egui::Button::new(egui::RichText::new(tr!("logs.reveal")).size(12.0)))
                        .on_hover_text(tr!("logs.reveal_hint")).clicked() {
                        let _ = path.parent().map(open_path);
                    }
                }
                ui.add(egui::TextEdit::singleline(&mut self.log_filter).font(egui::FontId::proportional(12.0))
                    .hint_text(tr!("logs.filter")).desired_width(ui.available_width()))
                    .on_hover_text(tr!("logs.filter_hint"));
            });
        });
        // The log is read once per run shown; the live one again each frame
        // while it grows.
        let text = match path {
            Some(path) => {
                let key = path.display().to_string();
                if live || self.log_text.as_ref().is_none_or(|(k, _)| k != &key) {
                    self.log_text = Some((key.clone(), std::fs::read_to_string(&path).unwrap_or_default()));
                }
                self.log_text.as_ref().map(|(_, t)| t.clone()).unwrap_or_default()
            }
            None => String::new(),
        };
        let filter = self.log_filter.to_lowercase();
        let shown: String = if filter.is_empty() {
            text
        } else {
            text.lines().filter(|l| l.to_lowercase().contains(&filter)).collect::<Vec<_>>().join("\n")
        };
        let frame = egui::Frame::new().fill(STATUS).corner_radius(6).inner_margin(egui::Margin::same(8));
        frame.show(ui, |ui| {
            ui.set_min_height(ui.available_height() - 16.0);
            egui::ScrollArea::both().id_salt("run_log").auto_shrink([false, false]).stick_to_bottom(live).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(shown).monospace().size(11.0)).extend().selectable(true));
            });
        });
    }

    /// The question before old logs go.
    fn delete_logs_ui(&mut self, ctx: &egui::Context) {
        if !self.delete_logs {
            return;
        }
        let response = backdrop::modal(ctx, "delete_logs").show(ctx, |ui| {
            ui.set_max_width(340.0);
            ui.label(egui::RichText::new(tr!("logs.delete_title")).strong().size(16.0));
            ui.add(egui::Label::new(tr!("logs.delete_description")).wrap());
            let mut answer = None;
            ui.horizontal(|ui| {
                if ui.button(tr!("common.cancel")).clicked() {
                    answer = Some(false);
                }
                let delete = egui::Button::new(egui::RichText::new(tr!("common.delete")).color(egui::Color32::WHITE)).fill(BAD);
                if ui.add(delete).clicked() {
                    answer = Some(true);
                }
            });
            answer
        });
        match response.inner {
            Some(true) => {
                let live = unfr0zen_host::log_path().and_then(|p| p.parent()?.file_name().map(|n| n.to_string_lossy().into_owned()));
                logs::delete_older(&self.history, LOG_RETENTION, live.as_deref());
                self.refresh_history();
                self.delete_logs = false;
            }
            Some(false) => self.delete_logs = false,
            None if response.should_close() => self.delete_logs = false,
            None => {}
        }
    }

    fn terminal_page(&mut self, ui: &mut egui::Ui) {
        page_header(ui, tr!("page.terminal"),
            tr!("console.description"));
        ui.add_space(8.0);
        let idle = !self.running() && !self.uboot.connected() && self.pending_image.is_none();
        self.shell.ui(ui, idle);
    }

    /// Recovery U-Boot: loaded over USB, then its prompt in the terminal.
    fn uboot_page(&mut self, ui: &mut egui::Ui) {
        self.command_page(ui, Task::Uboot,
            tr!("uboot.description"),
            tr!("task.uboot"));
        ui.add_space(10.0);
        let idle = !self.running() && !self.shell.connected() && self.pending_image.is_none();
        self.uboot.ui(ui, idle);
    }

    /// A 3rd generation is on USB, in any mode: the only unit the boot and
    /// recovery U-Boot commands have to act on.
    fn diamond3_on_bus(&self) -> bool {
        matches!(self.bus.current(), Bus::One(seen) if seen.device == Device::Diamond3)
    }

    /// An advanced command's page: what it does, its button, and how it went.
    fn command_page(&mut self, ui: &mut egui::Ui, task: Task, about: &str, button: &str) {
        page_header(ui, task.title(), about);
        let bus = self.bus.current();
        let other = match &bus { Bus::One(seen) if seen.device != Device::Diamond3 => Some(seen.device), _ => None };
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tr!("command.gen3_only")).color(MUTED));
        if let Some(device) = other {
            ui.label(egui::RichText::new(tr!("command.connected_device", device = device.description())).color(WARN));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            // Only with a 3rd generation on USB, in any mode, is there one to act on.
            if accent_button(ui, button, self.idle() && self.diamond3_on_bus())
                .on_hover_text(about)
                .on_disabled_hover_text(tr!("command.disabled"))
                .clicked() {
                self.start(task.clone());
            }
            // Booting says how it went on its card and in the status bar.
            if task != Task::Boot {
                self.task_summary(ui, task.page());
            }
        });
    }

    /// The run's steps in a card over the window, growing as they come:
    /// up from the first step, so a run that starts by waiting for download
    /// mode opens on that prompt alone; not to be dismissed while it runs,
    /// gone by itself a moment after a success, kept after a failure until
    /// closed, with a link to its log.
    /// The Logs page shows the run itself. Returns whether the card is up.
    fn run_popup(&mut self, ctx: &egui::Context) -> bool {
        let Some(task) = &self.task else { return false };
        if self.page == Page::Logs || self.popup_dismissed {
            return false;
        }
        let outcome = self.logs.outcome.as_ref();
        let shown = match (self.running(), outcome, self.finished_at) {
            (true, _, _) => !self.logs.rows.is_empty(),
            (false, Some(Ok(_)), Some(at)) => at.elapsed() < POPUP_LINGER,
            (false, Some(Err(_)), _) => true,
            _ => false,
        };
        if !shown {
            return false;
        }
        if !self.running() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let (doing, nand, failed) = (task.doing(), task.is_nand(), matches!(outcome, Some(Err(_))));
        let mut close = false;
        let mut to_logs = false;
        let response = backdrop::modal(ctx, "run_popup").show(ctx, |ui| {
            ui.set_width(POPUP_WIDTH);
            ui.spacing_mut().item_spacing.y = 5.0;
            // The steps a size smaller than the pages' text.
            ui.style_mut().text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
            // The steps' region takes its content's measured height, up to
            // the card's most: inside a card an area only grows when asked,
            // and a scroll region never asks.
            let height = self.card_height.clamp(ui.spacing().interact_size.y, POPUP_MAX_HEIGHT);
            let scroll = egui::ScrollArea::vertical().id_salt("run_popup_steps").min_scrolled_height(height).max_height(height)
                .auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                    ui.set_width(ui.available_width() - 12.0);
                    // The action in place of the word Device, before the unit;
                    // a failed run's close button at the right.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(doing).color(MUTED));
                        if let Some(device) = &self.logs.device {
                            // One line always: cut at what the close button leaves.
                            let room = ui.available_width() - if failed { CLOSE_SIZE + ui.spacing().item_spacing.x } else { 0.0 };
                            let height = ui.spacing().interact_size.y;
                            ui.allocate_ui_with_layout(egui::vec2(room, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                ui.add(egui::Label::new(egui::RichText::new(device).strong()).truncate()).on_hover_text(device);
                            });
                        }
                        if failed {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                close = close_button(ui).clicked();
                            });
                        }
                    });
                    // A success's line, with its path and checksum, is for
                    // the log: the card leaves too soon to read it.
                    self.steps_ui(ui, failed);
                    if nand && self.logs.transfer.is_some() {
                        ui.add_space(6.0);
                        self.nand_progress(ui);
                    }
                    if failed && self.logged {
                        let link = egui::Label::new(egui::RichText::new(tr!("run.logs_link")).size(12.5).color(ACCENT)).sense(egui::Sense::click());
                        to_logs = ui.add(link).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                    }
                });
            let content = scroll.content_size.y;
            if (content - self.card_height).abs() > 0.5 {
                self.card_height = content;
                ctx.request_repaint();
            }
        });
        // Only a failed run's card closes on the backdrop or Escape.
        if close || to_logs || (failed && response.should_close()) {
            self.popup_dismissed = true;
        }
        if to_logs {
            self.show(Page::Logs);
        }
        true
    }

    /// The USB download mode dialog. Clicking the backdrop or pressing Escape
    /// dismisses it and cancels the wait.
    fn sdp_ui(&mut self, ctx: &egui::Context) {
        let Some(since) = self.sdp_wait else { return };
        let response = backdrop::modal(ctx, "enter_sdp").show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(egui::RichText::new(tr!("download.title")).strong().size(16.0));
            let step = |ui: &mut egui::Ui, number: &str, text: &str| {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new(number).strong());
                    ui.add(egui::Label::new(text).wrap());
                });
            };
            step(ui, "1.", tr!("download.connect"));
            if self.logs.waiting.is_some() {
                step(ui, "2.", tr!("download.hold_button"));
            }
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                let left = WAIT_USB.saturating_sub(since.elapsed()).as_secs();
                ui.label(egui::RichText::new(tr!("download.waiting", minutes = left / 60, seconds = format!("{:02}", left % 60))).weak());
            });
        });
        if response.should_close() {
            if let Some(run) = &self.run {
                run.cancel_wait();
                self.cancelled = true;
                self.popup_dismissed = true;
            }
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context) {
        let Some((question, _)) = &self.confirm else { return };
        let question = question.clone();
        let answer = backdrop::modal(ctx, "confirm_restore").show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.label(egui::RichText::new(tr!("backup.restore_title")).strong().size(16.0));
            ui.add(egui::Label::new(question).wrap());
            let mut answer = None;
            ui.horizontal(|ui| {
                if ui.button(tr!("common.cancel")).clicked() {
                    answer = Some(false);
                }
                let erase = egui::Button::new(egui::RichText::new(tr!("backup.erase_restore")).color(egui::Color32::WHITE)).fill(BAD);
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

    fn delete_ui(&mut self, ctx: &egui::Context) {
        let Some(path) = self.delete.clone() else { return };
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let response = backdrop::modal(ctx, "confirm_delete").show(ctx, |ui| {
            ui.set_max_width(320.0);
            ui.label(egui::RichText::new(tr!("backup.delete_title")).strong().size(16.0));
            ui.add(egui::Label::new(tr!("backup.delete_description", name = name)).wrap());
            let mut answer = None;
            ui.horizontal(|ui| {
                if ui.button(tr!("common.cancel")).clicked() {
                    answer = Some(false);
                }
                let delete = egui::Button::new(egui::RichText::new(tr!("common.delete")).color(egui::Color32::WHITE)).fill(BAD);
                if ui.add(delete).clicked() {
                    answer = Some(true);
                }
            });
            answer
        });
        match response.inner {
            Some(true) => {
                self.maps.forget(&path);
                if let Err(e) = self.library.delete(&path) {
                    self.logs.outcome = Some(Err(format!("{}: {e}", path.display())));
                }
                self.delete = None;
            }
            Some(false) => self.delete = None,
            None if response.should_close() => self.delete = None,
            None => {}
        }
    }

    fn progress_ui(&self, ui: &mut egui::Ui) {
        if let Some(device) = &self.logs.device {
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new(tr!("common.device")).color(MUTED));
                ui.add(egui::Label::new(egui::RichText::new(device).strong()).wrap());
            });
        }
        self.steps_ui(ui, true);
    }

    /// The steps, notes and prompt, without the unit's line, and the
    /// outcome when `with_outcome`.
    fn steps_ui(&self, ui: &mut egui::Ui, with_outcome: bool) {
        for row in &self.logs.rows {
            ui.horizontal(|ui| {
                match row.done {
                    None => {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(&row.label);
                    }
                    Some((_, ok)) => {
                        outcome_mark(ui, ok, 14.0);
                        ui.label(&row.label);
                    }
                }
                // A running step counts up, repainted with the window.
                let elapsed = row.done.map_or_else(|| row.started.elapsed(), |(elapsed, _)| elapsed);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(tr!("run.elapsed", seconds = format!("{:.1}", elapsed.as_secs_f64()))).color(DIM));
                });
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
        if let Some(outcome) = self.logs.outcome.as_ref().filter(|_| with_outcome) {
            // Spaced as a step; long paths wrap, each line starting after
            // the mark.
            ui.horizontal_top(|ui| {
                outcome_mark(ui, outcome.is_ok(), 14.0);
                let text = match outcome {
                    Ok(message) => egui::RichText::new(message).color(OK).strong(),
                    Err(message) => egui::RichText::new(message).color(BAD),
                };
                ui.add(egui::Label::new(text).wrap());
            });
        }
    }
}

fn text_of(outcome: &Result<String, String>) -> String {
    match outcome { Ok(t) | Err(t) => t.clone() }
}

/// The page's title and what it does.
fn page_header(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.heading(egui::RichText::new(title).strong());
    ui.add_space(2.0);
    ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED)).wrap());
}

/// The page's icon, a line drawing 12 px across centred on `c`.
fn nav_icon(painter: &egui::Painter, page: Page, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let p = |x: f32, y: f32| c + egui::vec2(x, y);
    let path = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(points.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    };
    match page {
        // A bolt.
        Page::Patch => path(&[(1.0, -6.0), (-4.0, 1.0), (0.0, 1.0), (-1.0, 6.0), (4.0, -1.0), (0.0, -1.0), (1.0, -6.0)]),
        // Two drives.
        Page::Backups => {
            for y in [-5.0, 1.0] {
                painter.rect_stroke(egui::Rect::from_min_size(p(-6.0, y), egui::vec2(12.0, 4.0)), 1.0, stroke,
                                    egui::StrokeKind::Middle);
            }
        }
        // Lines of text.
        Page::Logs => {
            path(&[(-6.0, -4.0), (6.0, -4.0)]);
            path(&[(-6.0, 0.0), (2.0, 0.0)]);
            path(&[(-6.0, 4.0), (5.0, 4.0)]);
        }
        // A prompt.
        Page::Terminal => {
            path(&[(-6.0, -4.0), (-2.0, 0.0), (-6.0, 4.0)]);
            path(&[(0.0, 4.0), (6.0, 4.0)]);
        }
        // An "i" in a ring.
        Page::About => {
            let ring: Vec<(f32, f32)> = (0..=32).map(|i| {
                let a = 2.0 * std::f32::consts::PI * i as f32 / 32.0;
                (6.0 * a.cos(), 6.0 * a.sin())
            }).collect();
            path(&ring);
            path(&[(0.0, -0.5), (0.0, 3.0)]);
            painter.circle_filled(p(0.0, -3.0), 1.0, color);
        }
        // A chip with its pins.
        Page::Uboot => {
            painter.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(8.0, 8.0)), 1.0, stroke, egui::StrokeKind::Middle);
            for at in [-2.0, 2.0] {
                path(&[(at, -6.5), (at, -4.0)]);
                path(&[(at, 4.0), (at, 6.5)]);
                path(&[(-6.5, at), (-4.0, at)]);
                path(&[(4.0, at), (6.5, at)]);
            }
        }
        // The power sign: an open ring and the line through its gap.
        Page::Boot => {
            let arc: Vec<(f32, f32)> = (0..=28).map(|i| {
                let a = -std::f32::consts::FRAC_PI_2 + 0.6 + (2.0 * std::f32::consts::PI - 1.2) * i as f32 / 28.0;
                (6.0 * a.cos(), 6.0 * a.sin())
            }).collect();
            path(&arc);
            path(&[(0.0, -6.5), (0.0, -1.0)]);
        }
    }
}

/// The id of the delete icon on a backup's row.
fn delete_id(path: &Path) -> egui::Id {
    egui::Id::new(("delete_backup", path))
}

/// The id of the icon on a backup's row that opens its folder.
fn reveal_id(path: &Path) -> egui::Id {
    egui::Id::new(("reveal_backup", path))
}

/// What takes a backup row's icons, drawn on a 12 px grid, to 10 px across.
const ROW_ICON_SCALE: f32 = 10.0 / 12.0;

/// A folder, a line drawing centred on `c`.
fn folder_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let points = [(-6.0, -4.5), (-2.0, -4.5), (-0.5, -2.5), (6.0, -2.5), (6.0, 4.5), (-6.0, 4.5), (-6.0, -4.5)];
    painter.add(egui::Shape::line(points.iter().map(|&(x, y)| c + ROW_ICON_SCALE * egui::vec2(x, y)).collect(), egui::Stroke::new(1.25, color)));
}

/// A trash can, a line drawing centred on `c`.
fn trash_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.25, color);
    for points in [
        // The lid and its handle.
        &[(-6.0, -3.5), (6.0, -3.5)][..],
        &[(-2.0, -3.5), (-2.0, -6.0), (2.0, -6.0), (2.0, -3.5)],
        // The can, narrowing downwards, and its ribs.
        &[(-4.5, -3.5), (-3.5, 6.0), (3.5, 6.0), (4.5, -3.5)],
        &[(-1.25, -1.0), (-1.25, 3.5)],
        &[(1.25, -1.0), (1.25, 3.5)],
    ] {
        painter.add(egui::Shape::line(points.iter().map(|&(x, y)| c + ROW_ICON_SCALE * egui::vec2(x, y)).collect(), stroke));
    }
}

/// A section's name, in small capitals.
pub(crate) fn section(ui: &mut egui::Ui, name: &str) {
    section_after(ui, name, 10.0);
}

/// A section's heading, `space` below what comes before it.
pub(crate) fn section_after(ui: &mut egui::Ui, name: &str, space: f32) {
    ui.add_space(space);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(egui::RichText::new(name.to_uppercase()).size(10.0).strong().color(DIM));
    });
    ui.add_space(4.0);
}

const NAME: &str = "unfr0zen";
/// Seconds from launch to the name's first glint.
const GLINT_FIRST: f64 = 0.5;
/// Seconds between the starts of the name's glints.
const GLINT_PERIOD: f64 = 8.0;
/// Seconds a glint takes to cross the name.
const GLINT_CROSSING: f64 = 1.2;
/// Letters from the glint's centre to where it fades out.
const GLINT_REACH: f64 = 2.0;

/// Seconds into the glint under way at `time`; while none is, negative: the
/// seconds until the next.
fn glint_phase(time: f64) -> f64 {
    let since_first = time - GLINT_FIRST;
    if since_first < 0.0 {
        return since_first;
    }
    let phase = since_first.rem_euclid(GLINT_PERIOD);
    if phase < GLINT_CROSSING { phase } else { phase - GLINT_PERIOD }
}

/// The colour at `time` of the name's letter at `index`: white under the
/// glint, which enters and leaves past the ends so no letter jumps.
fn name_color(index: usize, time: f64) -> egui::Color32 {
    let rest = if NAME.as_bytes()[index] == b'0' { ACCENT } else { MUTED };
    let phase = glint_phase(time);
    if phase < 0.0 {
        return rest;
    }
    let center = phase / GLINT_CROSSING * (NAME.len() as f64 + 2.0 * GLINT_REACH) - GLINT_REACH;
    let glint = 1.0 - (index as f64 + 0.5 - center).abs() / GLINT_REACH;
    rest.lerp_to_gamma(egui::Color32::WHITE, glint.max(0.0) as f32)
}

/// A translated heading with the app name's animated colour spans.
pub(crate) fn name_heading(ui: &mut egui::Ui, text: &str) {
    let font = egui::TextStyle::Heading.resolve(ui.style());
    let time = ui.input(|input| input.time);
    let start = text.find(NAME).expect("the heading contains the app name");
    let mut job = egui::text::LayoutJob::single_section(text.to_string(), egui::TextFormat::default());
    // One section per letter: merging equal neighbours would reshape and jump
    // the letters as a glint passes.
    job.sections = std::iter::once(egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: egui::text::ByteIndex(0)..egui::text::ByteIndex(start),
        format: egui::TextFormat::simple(font.clone(), ui.visuals().strong_text_color()),
    }).chain((0..NAME.len()).map(|index| egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: egui::text::ByteIndex(start + index)..egui::text::ByteIndex(start + index + 1),
        format: egui::TextFormat::simple(font.clone(), name_color(index, time)),
    })).chain(std::iter::once(egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: egui::text::ByteIndex(start + NAME.len())..egui::text::ByteIndex(text.len()),
        format: egui::TextFormat::simple(font.clone(), ui.visuals().strong_text_color()),
    })).collect();
    ui.label(job);
    ui.ctx().request_repaint_after(glint_wait(time));
}

/// How long the name can go without a repaint: until the next glint, then
/// a smooth frame rate while it crosses.
fn glint_wait(time: f64) -> Duration {
    let phase = glint_phase(time);
    if phase < 0.0 { Duration::from_secs_f64(-phase) } else { Duration::from_millis(33) }
}

fn bus_color(bus: &Bus) -> egui::Color32 {
    match bus {
        Bus::Nothing => DIM,
        Bus::One(_) => OK,
        Bus::Several(_) => WARN,
    }
}

/// `text` on one line, cut with an ellipsis at `max_width`.
pub(crate) fn truncated(ui: &egui::Ui, text: String, font: egui::FontId, color: egui::Color32, max_width: f32)
             -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    ui.fonts_mut(|f| f.layout_job(job))
}

/// A rounded tag with `text` in it, as a widget; clickable when `click`.
fn pill(ui: &mut egui::Ui, text: &str, fg: egui::Color32, bg: egui::Color32, click: bool) -> egui::Response {
    let font = egui::FontId::proportional(11.5);
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, fg));
    let size = galley.size() + egui::vec2(16.0, 6.0);
    let sense = if click { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    ui.painter().rect(rect, 5.0, bg, egui::Stroke::new(1.0, LINE), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, fg);
    if click { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// A rounded tag painted with its left edge at `left`, centred on its y.
fn paint_pill(ui: &egui::Ui, left: egui::Pos2, text: &str, fg: egui::Color32, bg: egui::Color32) {
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), fg));
    let size = galley.size() + egui::vec2(14.0, 4.0);
    let rect = egui::Rect::from_min_size(egui::pos2(left.x, left.y - size.y * 0.5), size);
    ui.painter().rect_filled(rect, 5.0, bg);
    ui.painter().galley(rect.center() - galley.size() * 0.5, galley, fg);
}

/// `path` with the home directory as ~.
fn short_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A checkbox in the design's colours: the box on the accent with a dark
/// check when ticked, inset when not; the label beside it. Greyed when
/// not enabled.
pub(crate) fn checkbox(ui: &mut egui::Ui, enabled: bool, checked: &mut bool, text: &str) -> egui::Response {
    const BOX: f32 = 15.0;
    let enabled = enabled && ui.is_enabled();
    let font = egui::TextStyle::Body.resolve(ui.style());
    // The label is cut at the room there is, never run past it.
    let room = (ui.available_width() - BOX - 6.0).max(20.0);
    let galley = truncated(ui, text.to_string(), font, egui::Color32::WHITE, room);
    let height = ui.spacing().interact_size.y.max(BOX);
    let width = BOX + if text.is_empty() { 0.0 } else { 6.0 + galley.size().x };
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    if enabled && response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *checked, text));
    let shade = if enabled { 1.0 } else { 0.4 };
    let square = egui::Rect::from_center_size(egui::pos2(rect.left() + BOX * 0.5, rect.center().y), egui::vec2(BOX, BOX));
    let painter = ui.painter();
    if *checked {
        painter.rect_filled(square, 4.0, ACCENT.gamma_multiply(shade));
        let stroke = egui::Stroke::new(2.0, ON_ACCENT.gamma_multiply(shade));
        let p = |fx: f32, fy: f32| egui::pos2(square.left() + BOX * fx, square.top() + BOX * fy);
        painter.line_segment([p(0.24, 0.52), p(0.43, 0.72)], stroke);
        painter.line_segment([p(0.43, 0.72), p(0.78, 0.30)], stroke);
    } else {
        let hovered = enabled && response.hovered();
        painter.rect(square, 4.0, if hovered { NAV_HOVER } else { INSET },
                     egui::Stroke::new(1.0, if hovered { MUTED } else { LINE }), egui::StrokeKind::Inside);
    }
    if !text.is_empty() {
        let color = if enabled { ui.visuals().text_color() } else { DIM };
        painter.galley(egui::pos2(square.right() + 6.0, rect.center().y - galley.size().y * 0.5), galley, color);
    }
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// A close glyph: an × in a square that lights on hover.
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(CLOSE_SIZE, CLOSE_SIZE), egui::Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("common.close"));
    let color = if response.hovered() { ui.visuals().strong_text_color() } else { MUTED };
    if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, NAV_HOVER);
    }
    let stroke = egui::Stroke::new(1.5, color);
    let (c, r) = (rect.center(), 4.0);
    ui.painter().line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], stroke);
    ui.painter().line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], stroke);
    response
}


/// A small status dot.
fn dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// The primary action, on the accent fill.
pub(crate) fn accent_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    accent_button_sized(ui, text, enabled, egui::vec2(120.0, 28.0))
}

fn accent_button_sized(ui: &mut egui::Ui, text: &str, enabled: bool, size: egui::Vec2) -> egui::Response {
    let label = egui::RichText::new(text).size(14.0).color(if enabled { ON_ACCENT } else { ON_ACCENT.gamma_multiply(0.45) });
    let fill = if enabled { ACCENT } else { ACCENT.gamma_multiply(0.5) };
    ui.add_enabled(enabled, egui::Button::new(label).fill(fill).min_size(size))
}

/// The texts under the progress bar: bytes done of the total (and the piece
/// under way, until bytes have moved), then the rate and the time left once
/// they have.
fn transfer_texts(done: u64, next: u64, total: u64, rate: Option<f64>) -> (String, String, String) {
    const MIB: f64 = (1u64 << 20) as f64;
    let done_size = format!("{:.1}", done as f64 / MIB);
    let total_size = format!("{:.1}", total as f64 / MIB);
    let size = tr!("transfer.size", done = done_size, total = total_size);
    let Some(rate) = rate.filter(|r| *r > 0.0) else {
        let size = if next > 0 {
            tr!("transfer.size_under_way", done = done_size, total = total_size, next = format!("{:.1}", next as f64 / MIB))
        } else {
            size
        };
        return (size, String::new(), String::new());
    };
    let left = (total.saturating_sub(done) as f64 / rate).round() as u64;
    let left = if left >= 60 { tr!("transfer.minutes_left", minutes = left / 60, seconds = format!("{:02}", left % 60)) } else { tr!("transfer.seconds_left", seconds = left) };
    (size, tr!("transfer.speed", speed = format!("{:.1}", rate / MIB)), left)
}

/// Draws a check or cross in a small square, so it renders regardless of the
/// font (no bundled font has U+2713/U+2717, which showed as empty boxes).
fn outcome_mark(ui: &mut egui::Ui, ok: bool, size: f32) {
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

// The frost shimmer: a bright band sweeping left to right across the text,
// in columns.
const SHIMMER_SWEEP: f64 = 1.2;
const SHIMMER_HALF_WIDTH: f32 = 6.0;

/// `color` under the shimmer band at `progress` (0 to 1 across the sweep) for
/// the character at `col` of `cols`.
fn shimmer(color: egui::Color32, col: usize, cols: usize, progress: f32) -> egui::Color32 {
    let span = cols as f32 + 2.0 * SHIMMER_HALF_WIDTH;
    let center = progress * span - SHIMMER_HALF_WIDTH;
    let glint = (1.0 - (col as f32 - center).abs() / SHIMMER_HALF_WIDTH).max(0.0);
    color.lerp_to_gamma(egui::Color32::WHITE, glint * glint * 0.85)
}

/// The notice beside Start while a patch is under way, with the shimmer
/// sweeping across it every RUNNING_LOOP seconds as its sign of activity.
fn running_notice(ui: &mut egui::Ui) {
    let text = tr!("patch.running_notice");
    const RUNNING_LOOP: f64 = 2.0;
    let progress = ((ui.input(|i| i.time) % RUNNING_LOOP) / SHIMMER_SWEEP).min(1.0) as f32;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let cols = text.chars().count();
    let mut job = egui::text::LayoutJob::default();
    for (col, c) in text.chars().enumerate() {
        let format = egui::TextFormat::simple(font.clone(), shimmer(MUTED, col, cols, progress));
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, format);
    }
    ui.label(job);
    ui.ctx().request_repaint();
}

/// Opens a folder in the platform's file manager.
fn open_path(path: &Path) -> std::io::Result<()> {
    opener().arg(path).spawn().map(drop)
}

/// The platform command that opens a path.
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unfr0zen-gui-main-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn render(app: &mut App, size: [f32; 2]) -> egui::FullOutput {
        render_frames(app, size, 1)
    }

    /// `frames` frames on one context: an anchored area needs a second to
    /// be placed by its size.
    fn render_frames(app: &mut App, size: [f32; 2], frames: usize) -> egui::FullOutput {
        let context = egui::Context::default();
        install_fonts(&context);
        let mut output = None;
        for _ in 0..frames {
            output = Some(context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size[0], size[1]))),
                ..Default::default()
            }, |ui| app.frame_ui(ui)));
        }
        output.unwrap()
    }

    fn texts(output: &egui::FullOutput) -> Vec<String> {
        output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.job.text.clone()),
            _ => None,
        }).collect()
    }

    #[test]
    fn showing_the_backups_page_reads_the_folder() {
        let dir = temp_dir("backups-page");
        std::fs::File::create(dir.join("nand.bin")).unwrap().set_len(512 << 20).unwrap();
        let mut app = App { library: Library::new(dir.clone()), ..App::default() };
        assert!(app.library.entries.is_empty());
        app.show(Page::Backups);
        assert_eq!(app.page, Page::Backups);
        assert_eq!(app.library.entries.len(), 1);
        assert_eq!(app.library.selected().unwrap().name(), "nand.bin");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_finished_nand_task_refreshes_the_folder_and_keeps_the_page() {
        let dir = temp_dir("done");
        let mut app = App { library: Library::new(dir.clone()), page: Page::Backups, ..App::default() };
        app.task = Some(Task::Backup(dir.join("nand.bin")));
        std::fs::File::create(dir.join("nand.bin")).unwrap().set_len(512 << 20).unwrap();
        app.on_event(Event::Done(Ok("Backed up".into())));
        assert_eq!(app.library.entries.len(), 1);
        assert_eq!(app.page, Page::Backups);
        assert!(!app.running());
        let mut app = App { library: Library::new(dir.clone()), task: Some(Task::Patch), ..App::default() };
        app.on_event(Event::Done(Ok("Patched".into())));
        assert!(app.library.entries.is_empty(), "a patch does not touch the folder");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_task_names_the_page_its_summary_shows_on() {
        assert_eq!(Task::Patch.page(), Page::Patch);
        assert_eq!(Task::Backup("a".into()).page(), Page::Backups);
        assert_eq!(Task::Restore("a".into()).page(), Page::Backups);
        assert_eq!(Task::Uboot.page(), Page::Uboot);
        assert_eq!(Task::Boot.page(), Page::Boot);
        assert!(Task::Restore("a".into()).is_nand() && !Task::Boot.is_nand());
    }

    #[test]
    fn advanced_lists_boot_application_first_and_no_uboot_console() {
        let mut app = App::default();
        let texts = texts(&render(&mut app, WINDOW));
        let at = |title: &str| texts.iter().position(|t| t == title);
        assert!(at("Boot application").unwrap() < at("Serial console").unwrap(), "{texts:?}");
        assert_eq!(at("U-Boot console"), None, "{texts:?}");
    }

    #[test]
    fn a_failed_start_is_reported_without_a_run() {
        let mut app = App::default();
        app.form.matter.on = true;
        app.form.matter.code = "1559-813-5764".into();
        app.start(Task::Patch);
        assert!(!app.running());
        assert_eq!(app.task, Some(Task::Patch));
        assert!(app.logs.outcome.as_ref().unwrap().as_ref().err().unwrap().contains("check digit"));
    }

    #[test]
    fn every_page_renders_in_the_window() {
        let dir = temp_dir("pages");
        std::fs::File::create(dir.join("nand.bin")).unwrap().set_len(512 << 20).unwrap();
        for page in Page::MAIN.into_iter().chain(Page::ADVANCED) {
            let mut app = App { library: Library::new(dir.clone()), ..App::default() };
            app.task = Some(Task::Patch);
            app.logs.outcome = Some(Err("the volume has too little free space".into()));
            app.show(page);
            let output = render(&mut app, WINDOW);
            let texts = texts(&output);
            assert!(texts.iter().any(|t| t == page.title()), "{page:?} shows its title: {texts:?}");
            assert!(texts.iter().any(|t| t == "Disconnected"), "{page:?} shows the bus");
            for nav in Page::MAIN.into_iter().chain(Page::ADVANCED) {
                assert!(texts.iter().any(|t| t == nav.title()), "{page:?} lists {nav:?}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_backups_page_lists_the_folder_and_the_selection() {
        let dir = temp_dir("list");
        std::fs::File::create(dir.join("kitchen.bin")).unwrap().set_len(512 << 20).unwrap();
        let mut app = App { library: Library::new(dir.clone()), ..App::default() };
        app.show(Page::Backups);
        let texts = texts(&render(&mut app, WINDOW));
        assert!(texts.iter().filter(|t| *t == "kitchen.bin").count() >= 2, "in the table and the detail pane: {texts:?}");
        assert!(texts.iter().any(|t| t == "none"), "no checksum beside it");
        assert!(texts.iter().any(|t| t == "Restore to unit"));
        assert!(!texts.iter().any(|t| t == "Verify against unit"));
        assert!(!texts.iter().any(|t| t == "Delete" || t == "Reveal in Finder" || t == "Show in folder"), "the side panel has neither button: {texts:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_backups_buttons_sit_side_by_side_at_the_left_under_the_pane() {
        let dir = temp_dir("buttons");
        let mut app = App { library: Library::new(dir.clone()), ..App::default() };
        app.show(Page::Backups);
        let output = render_frames(&mut app, WINDOW, 2);
        let rect = |text: &str| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == text => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        }).unwrap_or_else(|| panic!("{text} is drawn"));
        const BUTTON: f32 = 130.0;
        let pane = rect("Select a backup");
        let (back_up, restore) = (rect("Back up unit"), rect("Restore from file"));
        assert!((back_up.center().x - (rect("NAND backups").left() + BUTTON / 2.0)).abs() < 1.0, "{back_up:?}");
        assert!((restore.center().x - back_up.center().x - (BUTTON + 8.0)).abs() < 1.0, "{back_up:?} {restore:?}");
        assert!((back_up.center().y - restore.center().y).abs() < 1.0 && back_up.top() > pane.bottom(), "{back_up:?} {restore:?} {pane:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_backups_trash_can_asks_to_delete_that_backup_and_leaves_the_selection() {
        let dir = temp_dir("trash");
        for name in ["kitchen.bin", "lounge.bin"] {
            std::fs::File::create(dir.join(name)).unwrap().set_len(512 << 20).unwrap();
        }
        let mut app = App { library: Library::new(dir.clone()), ..App::default() };
        app.show(Page::Backups);
        let selected = app.library.selected.clone();
        let target = app.library.entries.iter().map(|e| e.path.clone()).find(|p| Some(p) != selected.as_ref()).unwrap();
        let context = egui::Context::default();
        install_fonts(&context);
        let frame = |app: &mut App, events: Vec<egui::Event>| context.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
            events,
            ..Default::default()
        }, |ui| app.frame_ui(ui));
        frame(&mut app, Vec::new());
        frame(&mut app, Vec::new());
        let at = context.read_response(delete_id(&target)).unwrap().rect.center();
        let folder = context.read_response(reveal_id(&target)).unwrap().rect.center();
        assert!(folder.x < at.x && folder.y == at.y, "the folder icon sits left of the trash can: {folder:?} {at:?}");
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let click = |app: &mut App| {
            frame(app, vec![egui::Event::PointerMoved(at)]);
            frame(app, vec![press(true)]);
            frame(app, vec![press(false)]);
        };
        click(&mut app);
        assert_eq!(app.delete.as_ref(), Some(&target));
        assert_eq!(app.library.selected, selected);
        assert!(texts(&frame(&mut app, Vec::new())).iter().any(|t| t == "Delete this backup?"));

        // During a run it does nothing.
        app.delete = None;
        frame(&mut app, vec![egui::Event::PointerGone]);
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        click(&mut app);
        assert_eq!(app.delete, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_dropdowns_last_row_stays_put_under_the_pointer_with_no_scrollbar() {
        let mut app = App { page: Page::Patch, advanced_open: true, ..App::default() };
        let context = egui::Context::default();
        install_fonts(&context);
        // The app's theme gives inactive widgets this border.
        context.all_styles_mut(|style| style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE));
        let mut frame = |events: Vec<egui::Event>| context.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
            events,
            ..Default::default()
        }, |ui| app.frame_ui(ui));
        let text_at = |output: &egui::FullOutput, want: &str| output.shapes.iter().filter_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == want => Some((text.pos, clipped.clip_rect)),
            _ => None,
        }).collect::<Vec<_>>();
        let settled = (0..4).map(|_| frame(vec![])).last().unwrap();
        // Slot, then device, then entry point.
        let (device, _) = text_at(&settled, "auto")[1];
        let press = |pressed| egui::Event::PointerButton { pos: device, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(device)]);
        frame(vec![press(true)]);
        frame(vec![press(false)]);
        let open = (0..6).map(|_| frame(vec![])).last().unwrap();
        let (row, list) = *text_at(&open, "diamond3").last().unwrap();
        frame(vec![egui::Event::PointerMoved(row + egui::vec2(4.0, 4.0))]);
        let hovered = (0..30).map(|_| frame(vec![])).last().unwrap();
        assert_eq!(*text_at(&hovered, "diamond3").last().unwrap(), (row, list), "the row and the list keep their place and size");
        let bars = hovered.shapes.iter().filter(|clipped| matches!(&clipped.shape,
            egui::epaint::Shape::Rect(rect) if rect.rect.width() <= 4.0 && rect.rect.height() > 20.0 && list.intersects(rect.rect))).count();
        assert_eq!(bars, 0, "no scrollbar over the list");
    }

    #[test]
    fn a_run_called_off_with_no_log_leaves_no_row_on_the_logs_page() {
        let mut app = App { task: Some(Task::Restore("a".into())), page: Page::Logs, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(shown.iter().any(|t| t == "now"), "listed while it runs: {shown:?}");
        app.cancelled = true;
        app.popup_dismissed = true;
        app.on_event(Event::End { label: WAIT_USB_BOOT_STAGE.into(), elapsed: Duration::ZERO, ok: false });
        app.on_event(Event::Done(Err("no unit entered USB download mode".into())));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(!shown.iter().any(|t| t == "now" || t.contains("no unit entered")), "{shown:?}");
        assert!(shown.iter().any(|t| t == "Restore NAND: cancelled"), "the status bar still says: {shown:?}");
    }

    #[test]
    fn the_run_card_stays_while_running_and_after_a_failure_only() {
        let mut app = App { task: Some(Task::Backup("a".into())), page: Page::Patch, ..App::default() };
        // Longer than the test runs: the fake run must not finish and retire
        // itself while the assertions below still expect it live.
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(600)); Ok(String::new()) }));
        app.on_event(Event::Begin("Read NAND".into()));
        let card = |app: &mut App, text: &str| texts(&render_frames(app, WINDOW, 2)).iter().any(|t| t == text);
        assert!(card(&mut app, "Read NAND"));
        app.show(Page::Backups);
        assert!(card(&mut app, "Read NAND"), "on every page");
        app.show(Page::Logs);
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert_eq!(shown.iter().filter(|t| *t == "Read NAND").count(), 1, "the Logs page shows the run itself, not the card");
        app.show(Page::Patch);
        app.run = None;
        app.on_event(Event::Done(Ok("Backed up to a path with its checksum".into())));
        assert!(card(&mut app, "Read NAND") && !card(&mut app, "Backed up to a path with its checksum"),
                "a success keeps the card a moment, without its line");
        app.finished_at = Some(Instant::now() - POPUP_LINGER);
        assert!(!card(&mut app, "Read NAND"), "gone after lingering");
        app.logs.outcome = Some(Err("no unit".into()));
        assert!(card(&mut app, "no unit") && !card(&mut app, "Logs ›"), "a failure stays, with no link while it kept no log");
        app.logged = true;
        assert!(card(&mut app, "no unit") && card(&mut app, "Logs ›"), "with its link once it did");
        app.popup_dismissed = true;
        assert!(!card(&mut app, "no unit"));
        // A wait called off ends the run without an error shown.
        let mut app = App { task: Some(Task::Patch), page: Page::Patch, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(600)); Ok(String::new()) }));
        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        app.sdp_wait = None;
        app.cancelled = true;
        app.popup_dismissed = true;
        app.run = None;
        app.on_event(Event::Done(Err("no unit entered USB download mode".into())));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(!shown.iter().any(|t| t.contains("no unit entered")), "{shown:?}");
        assert!(shown.iter().any(|t| t == "Patch: cancelled"));
        assert!(unfr0zen_host::log_path().is_none(), "no log is kept of it");
        // The download prompt replaces the card until it is answered.
        let mut app = App { task: Some(Task::Restore("a".into())), page: Page::Backups, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        app.on_event(Event::Begin("Restart the unit from an earlier initramfs shell".into()));
        app.on_event(Event::End { label: "Restart the unit from an earlier initramfs shell".into(), elapsed: Duration::ZERO, ok: true });
        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(!shown.iter().any(|t| t == "Restart the unit from an earlier initramfs shell") && shown.iter().any(|t| t == "Put the device in USB download mode"), "{shown:?}");
        app.sdp_wait = None;
        assert!(card(&mut app, "Restart the unit from an earlier initramfs shell"), "the card is back");
        // So does the restore's confirmation.
        let (reply, _answer) = std::sync::mpsc::channel();
        app.on_event(Event::Confirm("Erase the NAND?".into(), reply));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(!shown.iter().any(|t| t == "Restart the unit from an earlier initramfs shell") && shown.iter().any(|t| t == "Restore NAND?"), "{shown:?}");
    }

    #[test]
    fn the_title_bar_names_the_app_with_its_zero_in_the_accent() {
        let output = render(&mut App::default(), WINDOW);
        let name = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == "unfr0zen" => Some(t.galley.clone()),
            _ => None,
        }).expect("the name is drawn");
        let colors: Vec<_> = name.job.sections.iter().map(|section| section.format.color).collect();
        assert_eq!(colors, [MUTED, MUTED, MUTED, MUTED, ACCENT, MUTED, MUTED, MUTED]);
    }

    #[test]
    fn a_failed_run_cards_logs_link_lines_up_with_its_title() {
        let mut app = App { task: Some(Task::Backup("a".into())), page: Page::Patch, finished_at: Some(Instant::now()), logged: true, ..App::default() };
        app.logs.outcome = Some(Err("no unit".into()));
        let output = render_frames(&mut app, WINDOW, 4);
        let left = |text: &str| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == text => Some(t.pos.x),
            _ => None,
        }).unwrap_or_else(|| panic!("{text} is drawn"));
        assert_eq!(left("Logs ›"), left("Backing up"));
    }

    #[test]
    fn about_is_a_page_below_logs_and_the_version_opens_it() {
        let context = egui::Context::default();
        install_fonts(&context);
        let mut app = App::default();
        let frame = |app: &mut App, events: Vec<egui::Event>| context.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
            events,
            ..Default::default()
        }, |ui| app.frame_ui(ui));
        let output = frame(&mut app, Vec::new());
        let shown = texts(&output);
        let order = |text: &str| shown.iter().position(|t| t == text).unwrap_or_else(|| panic!("{text} is drawn: {shown:?}"));
        assert!(order("Logs") < order("About") && order("About") < order("ADVANCED"), "{shown:?}");
        assert!(!shown.iter().any(|t| t == "i" || t == "About unfr0zen"), "no info button, no dialog: {shown:?}");
        let version = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == BUILD_VERSION => Some(t.galley.rect.translate(t.pos.to_vec2()).center()),
            _ => None,
        }).unwrap();
        let press = |pressed| egui::Event::PointerButton { pos: version, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&mut app, vec![egui::Event::PointerMoved(version)]);
        frame(&mut app, vec![press(true)]);
        frame(&mut app, vec![press(false)]);
        assert_eq!(app.page, Page::About);
        let shown = texts(&frame(&mut app, Vec::new()));
        for text in ["About unfr0zen", "Licenses", "unfr0zen (GPLv3)", "https://github.com/unfr0zen",
                     "Credits", "BusyBox —", "the BusyBox project",
                     "CVE-2017-7932 and CVE-2017-7936 —", "Quarkslab"] {
            assert!(shown.iter().any(|t| t == text), "{text}: {shown:?}");
        }
        assert!(!shown.iter().any(|t| t == "Close"), "{shown:?}");
        assert!(shown.iter().any(|t| t.starts_with("Open-source libraries")), "the libraries panel: {shown:?}");
    }

    #[test]
    fn main_page_controls_show_hints_on_hover() {
        for (page, label, hint) in [
            (Page::Patch, "Start", "Patch the connected thermostat with the selected options"),
            (Page::Patch, "Block Nest and Google servers", "Block the thermostat's access to Nest and Google servers; use a custom NTP server for time"),
            (Page::Boot, "Boot the application", "Connect one 3rd generation thermostat, finish any operation, close file pickers, and disconnect any open console first"),
        ] {
            let context = egui::Context::default();
            install_fonts(&context);
            let mut app = App { page, ..App::default() };
            let frame = |app: &mut App, time, events| context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
                time: Some(time),
                events,
                ..Default::default()
            }, |ui| app.frame_ui(ui));
            frame(&mut app, 0.0, Vec::new());
            let output = frame(&mut app, 0.1, Vec::new());
            assert!(!texts(&output).iter().any(|text| text == hint), "no hint before hovering");
            let at = output.shapes.iter().rev().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == label => Some(t.galley.rect.translate(t.pos.to_vec2()).center()),
                _ => None,
            }).unwrap_or_else(|| panic!("{label} is drawn"));
            frame(&mut app, 0.2, vec![egui::Event::PointerMoved(at)]);
            let mut shown = Vec::new();
            for time in [0.3, 1.0, 1.1, 1.2] {
                shown = texts(&frame(&mut app, time, Vec::new()));
            }
            assert!(shown.iter().any(|text| text == hint), "{label}: {shown:?}");
            assert!(app.task.is_none(), "hovering does not start a task");
        }
    }

    #[test]
    fn the_advanced_button_sits_on_the_patch_pages_description_line() {
        let output = render_frames(&mut App { page: Page::Patch, ..App::default() }, WINDOW, 2);
        let rect = |text: &str| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text == text && t.pos.x > SIDEBAR_WIDTH => Some(t.galley.rect.translate(t.pos.to_vec2())),
            _ => None,
        }).unwrap_or_else(|| panic!("{text} is drawn"));
        let (button, about) = (rect("Advanced"), rect(tr!("patch.about")));
        assert!((button.center().y - about.center().y).abs() < 1.0, "{button:?} {about:?}");
        assert!(button.left() > about.right());
        assert!(about.height() < 2.0 * button.height(), "the description keeps to one line: {about:?}");
        let framed = output.shapes.iter().any(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(r) => r.fill.a() > 0 && r.rect.contains_rect(button) && r.rect.width() < 2.0 * button.width(),
            _ => false,
        });
        assert!(framed, "drawn as a button while the panel is closed");
    }

    #[test]
    fn the_zoom_keys_leave_the_window_at_its_one_size() {
        let press = |context: &egui::Context| {
            let event = egui::Event::Key {
                key: egui::Key::Plus,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            };
            let _ = context.run_ui(egui::RawInput {
                modifiers: egui::Modifiers::COMMAND,
                events: vec![event],
                ..Default::default()
            }, |_| {});
            // A new zoom takes effect as the next frame begins.
            let _ = context.run_ui(egui::RawInput::default(), |_| {});
            context.zoom_factor()
        };
        assert!(press(&egui::Context::default()) > 1.0, "egui zooms unless told not to");
        let context = egui::Context::default();
        no_zoom_keys(&context);
        assert_eq!(press(&context), 1.0);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_widget_whose_id_shifts_draws_no_debug_outline() {
        // The same rect under another id, as when a widget before it comes or goes.
        let outlines = |context: &egui::Context| (0..3).map(|frame| {
            let output = context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
                ..Default::default()
            }, |ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(50.0, 20.0));
                let _ = ui.interact(rect, egui::Id::new(frame), egui::Sense::click());
            });
            output.shapes.iter().filter(|shape| matches!(&shape.shape,
                egui::epaint::Shape::Rect(r) if r.stroke.color == egui::Color32::RED)).count()
        }).sum::<usize>();
        assert!(outlines(&egui::Context::default()) > 0, "debug builds outline it unless told not to");
        let context = egui::Context::default();
        no_id_shift_outlines(&context);
        assert_eq!(outlines(&context), 0);
    }

    #[test]
    fn a_glint_leaves_the_title_bar_where_it_is() {
        let context = egui::Context::default();
        install_fonts(&context);
        let mut app = App::default();
        let mut at = |time: f64| {
            let output = context.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WINDOW[0], WINDOW[1]))),
                time: Some(time),
                ..Default::default()
            }, |ui| app.frame_ui(ui));
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == "›" => Some(t.pos.x),
                _ => None,
            }).expect("the separator is drawn")
        };
        let rest = at(0.0);
        for step in 0..=20 {
            let time = GLINT_FIRST + GLINT_CROSSING * f64::from(step) / 20.0;
            assert_eq!(at(time), rest, "at {time}");
        }
    }

    #[test]
    fn a_glint_comes_soon_after_launch_then_each_period_left_to_right() {
        let start = GLINT_FIRST;
        let next = GLINT_FIRST + GLINT_PERIOD;
        let rest = |index| if index == 4 { ACCENT } else { MUTED };
        let brightest = |time: f64| (0..NAME.len()).max_by_key(|&index| name_color(index, time).r()).unwrap();
        for time in [0.0, start - 0.01, start + GLINT_CROSSING + 0.01, next - 0.01] {
            assert!((0..NAME.len()).all(|index| name_color(index, time) == rest(index)), "at rest at {time}");
        }
        assert!((0..NAME.len()).all(|index| name_color(index, start) == rest(index)), "it enters without a jump");
        assert!(brightest(start + GLINT_CROSSING * 0.3) < brightest(start + GLINT_CROSSING * 0.7), "left to right");
        assert_eq!(name_color(0, start + GLINT_CROSSING * (GLINT_REACH + 0.5) / (NAME.len() as f64 + 2.0 * GLINT_REACH)),
                   egui::Color32::WHITE, "white at its centre");
        assert_eq!(name_color(4, next + GLINT_CROSSING * 0.5), name_color(4, start + GLINT_CROSSING * 0.5), "again each period");
        assert_eq!(glint_wait(0.0), Duration::from_secs_f64(GLINT_FIRST), "no repaints before the first");
        assert!((glint_wait(start + GLINT_CROSSING).as_secs_f64() - (GLINT_PERIOD - GLINT_CROSSING)).abs() < 1e-9, "nor between glints");
        assert_eq!(glint_wait(start + 0.1), Duration::from_millis(33));
    }

    #[test]
    fn a_run_waiting_for_download_mode_opens_on_the_prompt_without_the_card() {
        let mut app = App { task: Some(Task::Restore("a".into())), page: Page::Backups, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        app.on_event(Event::Device("Nest gen 3".into()));
        assert!(!texts(&render_frames(&mut app, WINDOW, 4)).iter().any(|t| t == "Nest gen 3"), "no card before a step");
        app.on_event(Event::Begin(WAIT_USB_BOOT_STAGE.into()));
        let shown = texts(&render_frames(&mut app, WINDOW, 4));
        assert!(shown.iter().any(|t| t == "Put the device in USB download mode"), "{shown:?}");
        assert!(!shown.iter().any(|t| t == "Nest gen 3"), "{shown:?}");
        app.on_event(Event::End { label: WAIT_USB_BOOT_STAGE.into(), elapsed: Duration::ZERO, ok: true });
        app.on_event(Event::Begin("Check the image's SHA-256".into()));
        assert!(texts(&render_frames(&mut app, WINDOW, 4)).iter().any(|t| t == "Nest gen 3"), "the card comes with the first step");
    }

    #[test]
    fn the_entry_point_outlasts_the_unit_leaving_the_bus() {
        let entry_point = unfr0zen_host::diamond3::usb::EntryPoint::Cve20177936;
        let mut app = App { page: Page::Patch, advanced_open: true, ..App::default() };
        app.form.entry_point = entry_point;
        let shown = texts(&render_frames(&mut app, WINDOW, 4));
        assert_eq!(app.form.entry_point, entry_point);
        assert!(shown.iter().any(|t| t == entry_point.label()), "the greyed choice is the real one: {shown:?}");
    }

    #[test]
    fn only_the_u_boot_page_repeats_its_outcome_beside_its_button() {
        for (task, page, beside) in [(Task::Boot, Page::Boot, false), (Task::Uboot, Page::Uboot, true)] {
            let mut app = App { task: Some(task), page, popup_dismissed: true, ..App::default() };
            app.logs.outcome = Some(Ok("It went well".into()));
            let shown = texts(&render_frames(&mut app, WINDOW, 2));
            assert_eq!(shown.iter().any(|t| t == "It went well"), beside, "{page:?}: {shown:?}");
        }
    }

    #[test]
    fn the_run_cards_first_line_stays_on_one_line_uncut() {
        const LONGEST: &str = "Nest gen 3 (diamond3): i.MX6SoloX rev 1.2";
        for task in [Task::Patch, Task::Backup("a".into()), Task::Restore("a".into()), Task::Uboot, Task::Boot] {
            for failed in [false, true] {
                let doing = task.doing();
                let mut app = App { task: Some(task.clone()), page: Page::Patch, ..App::default() };
                app.on_event(Event::Device(LONGEST.into()));
                if failed {
                    app.finished_at = Some(Instant::now());
                    app.logs.outcome = Some(Err("no unit".into()));
                } else {
                    app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
                    app.on_event(Event::Begin("Read NAND".into()));
                }
                let output = render_frames(&mut app, WINDOW, 4);
                let galley = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(t) if t.galley.job.text == LONGEST => Some(t.galley.clone()),
                    _ => None,
                }).unwrap_or_else(|| panic!("{doing}: the unit is drawn"));
                assert!(galley.rows.len() == 1 && !galley.elided, "{doing}, failed {failed}: {} rows, cut {}", galley.rows.len(), galley.elided);
            }
        }
    }

    #[test]
    fn the_run_card_grows_with_its_steps_until_its_most() {
        let mut app = App { task: Some(Task::Patch), page: Page::Patch, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        // Every step's label visible: inside its clip rect, not scrolled
        // away. The card takes a few frames to measure itself and settle.
        let visible = |app: &mut App, label: &str| {
            let output = render_frames(app, WINDOW, 4);
            output.shapes.iter().any(|shape| match &shape.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == label => {
                    shape.clip_rect.contains_rect(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => false,
            })
        };
        for i in 0..12 {
            app.on_event(Event::Begin(format!("Step number {i}")));
            app.on_event(Event::End { label: format!("Step number {i}"), elapsed: Duration::ZERO, ok: true });
        }
        assert!(visible(&mut app, "Step number 0") && visible(&mut app, "Step number 11"), "twelve steps fit the card");
        for i in 12..40 {
            app.on_event(Event::Begin(format!("Step number {i}")));
        }
        assert!(visible(&mut app, "Step number 39"), "the newest step shows once the card is full");
        assert!(!visible(&mut app, "Step number 0"), "the oldest has scrolled away");
    }

    #[test]
    fn the_run_pane_shows_live_and_past_steps_at_one_size() {
        let size_of = |app: &mut App, label: &str| {
            render_frames(app, WINDOW, 2).shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(t) if t.galley.job.text == label => Some(t.galley.job.sections[0].format.font_id.size),
                _ => None,
            })
        };
        let mut live = App { task: Some(Task::Patch), page: Page::Logs, logged: true, ..App::default() };
        live.on_event(Event::Device("Nest gen 3 (diamond3)".into()));
        live.on_event(Event::Begin("Load recovery U-Boot".into()));
        assert_eq!(size_of(&mut live, "Load recovery U-Boot"), Some(RUN_STEP_FONT));
        assert_eq!(size_of(&mut live, "Device"), Some(RUN_STEP_FONT));
        let text = "[ 1.0] Selected: Patch\n[ 2.0] Load recovery U-Boot: done (1.2s)\n";
        let run = logs::parse("20261104-120000", Path::new("/nonexistent"), text);
        let mut past = App { page: Page::Logs, history: vec![run], history_selected: Some("20261104-120000".into()), ..App::default() };
        assert_eq!(size_of(&mut past, "Load recovery U-Boot"), Some(RUN_STEP_FONT));
        assert_eq!(size_of(&mut past, "1.2s"), Some(RUN_STEP_FONT));
    }

    #[test]
    fn the_run_panes_top_part_takes_three_eighths_of_it_at_most() {
        let mut app = App { task: Some(Task::Patch), page: Page::Logs, logged: true, ..App::default() };
        for i in 0..40 {
            app.on_event(Event::Begin(format!("Step number {i}")));
            app.on_event(Event::End { label: format!("Step number {i}"), elapsed: Duration::ZERO, ok: true });
        }
        let output = render_frames(&mut app, WINDOW, 3);
        let pane = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Rect(r) if r.fill == SIDE && r.rect.left() > SIDEBAR_WIDTH => Some(r.rect),
            _ => None,
        }).expect("the run pane is drawn");
        let steps_end = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text.starts_with("Step number") => Some(shape.clip_rect.bottom()),
            _ => None,
        }).expect("a step is drawn");
        let inner = pane.shrink(14.0);
        let share = (steps_end - inner.top()) / inner.height();
        assert!((share - RUN_TOP_SHARE).abs() < 0.01, "the top part takes {share} of the pane");
    }

    #[test]
    fn boot_and_uboot_need_a_diamond3_on_the_bus() {
        let seen = |device| bus::Seen { device, mode: "app", port: "1".into(), id: (0, 0) };
        let d3 = App { bus: bus::Watcher::fixed(bus::Bus::One(seen(Device::Diamond3))), ..App::default() };
        assert!(d3.diamond3_on_bus(), "a 3rd generation is on the bus");
        let j49 = App { bus: bus::Watcher::fixed(bus::Bus::One(seen(Device::J49))), ..App::default() };
        assert!(!j49.diamond3_on_bus(), "another generation is not it");
        let none = App { bus: bus::Watcher::fixed(bus::Bus::Nothing), ..App::default() };
        assert!(!none.diamond3_on_bus(), "nothing on the bus");
    }

    #[test]
    fn the_logs_page_preselects_the_most_recent_run() {
        let newer = "20261105-100200";
        let mut app = App { history: vec![
            logs::parse(newer, Path::new("/x"), "[ 1.0] Back up NAND: done (18.3s)\n"),
            logs::parse("20261105-100100", Path::new("/x"), "[ 1.0] Restore NAND: failed (2.1s): no room\n"),
        ], ..App::default() };
        app.preselect_latest_run();
        assert_eq!(app.history_selected.as_deref(), Some(newer), "the newest run is shown");
        // A valid choice the user made is kept.
        app.history_selected = Some("20261105-100100".into());
        app.preselect_latest_run();
        assert_eq!(app.history_selected.as_deref(), Some("20261105-100100"));
        // A live run heads the list on its own, so nothing is preselected.
        app.history_selected = None;
        app.task = Some(Task::Patch);
        app.logged = true;
        app.preselect_latest_run();
        assert_eq!(app.history_selected, None);
        // With no runs, nothing is selected.
        let mut empty = App::default();
        empty.preselect_latest_run();
        assert_eq!(empty.history_selected, None);
    }

    #[test]
    fn the_logs_page_lists_the_folders_runs_and_the_live_one() {
        let dir = temp_dir("logs-page");
        std::fs::create_dir_all(dir.join("20261104-120000")).unwrap();
        std::fs::write(dir.join("20261104-120000/unfr0zen.log"),
                       "[ 1.0] Selected: Boot the application\n[ 2.0] Chainload stock U-Boot from NAND: done (1.2s)\n").unwrap();
        let mut app = App { page: Page::Logs, ..App::default() };
        app.history = logs::scan(&dir);
        let shown = texts(&render(&mut app, WINDOW));
        assert!(shown.iter().any(|t| t == "Boot the application"), "{shown:?}");
        assert!(shown.iter().any(|t| t == "Select a run"));
        app.history_selected = Some("20261104-120000".into());
        let shown = texts(&render(&mut app, WINDOW));
        assert!(shown.iter().any(|t| t == "Chainload stock U-Boot from NAND"), "{shown:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_status_bar_says_how_a_run_went_without_its_message() {
        const MESSAGE: &str = "NAND backup verified: /a/b/nand.bin (536870912 bytes, SHA-256 17f7)";
        const STEP: &str = "Check the FCB against the restored U-Boot copies";
        for (outcome, line) in [(Ok(MESSAGE.to_string()), "Back up NAND: done"), (Err(MESSAGE.to_string()), "Back up NAND: failed")] {
            let mut app = App { task: Some(Task::Backup("a".into())), page: Page::Patch, popup_dismissed: true, ..App::default() };
            app.logs.outcome = Some(outcome);
            let shown = texts(&render_frames(&mut app, WINDOW, 2));
            assert!(shown.iter().any(|t| t == line), "{shown:?}");
            assert!(!shown.iter().any(|t| t.contains(MESSAGE)), "{shown:?}");
        }
        let mut app = App { task: Some(Task::Backup("a".into())), page: Page::Patch, popup_dismissed: true, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(5)); Ok(String::new()) }));
        app.on_event(Event::Begin(STEP.into()));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(shown.iter().any(|t| t == "Backing up"), "{shown:?}");
        assert!(!shown.iter().any(|t| t.contains(STEP)), "{shown:?}");
    }

    #[test]
    fn long_outcome_wraps_within_the_logs_page() {
        let message = "the volume has too little free space: 22626304 bytes available, 23236503 bytes required \
                       including staging and reserve; free space on the unit or patch without nestlocal";
        let mut app = App { task: Some(Task::Patch), page: Page::Logs, logged: true, ..App::default() };
        app.logs.outcome = Some(Err(message.into()));
        let output = render(&mut app, WINDOW);
        let galley = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == message => Some(&text.galley),
            _ => None,
        }).expect("error text is rendered");
        assert!(galley.rows.len() > 1);
        assert!(galley.rect.width() < WINDOW[0] - SIDEBAR_WIDTH);
    }

    #[test]
    fn transfer_texts_add_rate_and_time_once_bytes_move() {
        let mib = 1u64 << 20;
        let texts = |done, next, total, rate| {
            let (size, speed, left) = transfer_texts(done, next, total, rate);
            [size, speed, left]
        };
        assert_eq!(texts(0, 5 * mib, 300 * mib, None), ["0.0 / 300.0 MiB, 5.0 MiB under way", "", ""]);
        assert_eq!(texts(60 * mib, 5 * mib, 300 * mib, Some(2.0 * mib as f64)),
                   ["60.0 / 300.0 MiB", "2.0 MiB/s", "2m 00s left"], "the speed replaces the piece under way");
        assert_eq!(texts(290 * mib, 0, 300 * mib, Some(2.0 * mib as f64)), ["290.0 / 300.0 MiB", "2.0 MiB/s", "5s left"]);
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
    fn the_speed_falls_once_the_piece_under_way_outlasts_it() {
        let now = Instant::now();
        let ago = |secs| now - Duration::from_secs(secs);
        let on_time = Transfer { done: 20, next: 8, total: 100, at: ago(2), since: ago(12), since_done: 0 };
        assert_eq!(on_time.speed(), Some(2.0), "the average while the piece can still make it");
        let stalled = Transfer { at: ago(60), since: ago(70), ..on_time };
        assert!((stalled.speed().unwrap() - 0.4).abs() < 0.01, "{:?}", stalled.speed());
        let finished = Transfer { next: 0, at: ago(60), since: ago(70), ..on_time };
        assert_eq!(finished.speed(), Some(2.0), "later stages leave the last speed alone");
        let first = Transfer { done: 0, at: ago(5), since: ago(5), ..on_time };
        assert_eq!(first.speed(), None);
    }

    #[test]
    fn the_texts_under_the_progress_bar_follow_the_bar_between_reports_in_three_columns() {
        const MIB: u64 = 1 << 20;
        let ago = |secs| Instant::now() - Duration::from_secs(secs);
        let mut app = App { task: Some(Task::Backup("a".into())), page: Page::Patch, ..App::default() };
        app.run = Some(Run::task(|_| { std::thread::sleep(Duration::from_secs(600)); Ok(String::new()) }));
        app.on_event(Event::Begin("Back up NAND".into()));
        // 2 MiB/s over the last piece, its report 4 s ago: 8 MiB past the 64.
        app.logs.transfer = Some(Transfer { done: 64 * MIB, next: 64 * MIB, total: 512 * MIB, at: ago(4), since: ago(36), since_done: 0 });
        let shapes = render_frames(&mut app, WINDOW, 2).shapes;
        let at = |wanted: &dyn Fn(&str) -> bool| shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if wanted(&t.galley.job.text) => Some((t.galley.job.text.clone(), t.visual_bounding_rect())),
            _ => None,
        }).unwrap();
        let (size, size_at) = at(&|t| t.contains("/ 512.0 MiB"));
        let (_, speed_at) = at(&|t| t == "2.0 MiB/s");
        let (left, left_at) = at(&|t| t.ends_with(" left"));
        assert!(size.starts_with("72.") && !size.contains("under way"), "{size}");
        assert!(left.starts_with("3m "), "{left}");
        let progress_at = shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(t) if t.galley.job.text.ends_with('%') => Some(t.visual_bounding_rect()),
            _ => None,
        }).unwrap();
        assert!((speed_at.center().x - progress_at.center().x).abs() < 1.0, "centred under the bar");
        assert!(size_at.right() < speed_at.left() && speed_at.right() < left_at.left(), "{size_at:?} {speed_at:?} {left_at:?}");
        // A failed run stands at what it reported, with the bar.
        app.run = None;
        app.logs.outcome = Some(Err("no unit".into()));
        let shown = texts(&render_frames(&mut app, WINDOW, 2));
        assert!(shown.iter().any(|t| t == "64.0 / 512.0 MiB"), "{shown:?}");
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
        assert_eq!(app.logs.current(), None);
    }

    #[test]
    fn status_lines_last_until_the_next_step_and_prompts_until_theirs_ends() {
        let mut app = App::default();
        app.on_event(Event::Status("Detected app mode".into()));
        app.on_event(Event::Waiting("Plug it in".into()));
        app.on_event(Event::Begin("Load recovery U-Boot".into()));
        assert!(app.logs.notes.is_empty() && app.logs.waiting.is_some());
        assert_eq!(app.logs.current(), Some("Load recovery U-Boot"));
        app.on_event(Event::End { label: "Load recovery U-Boot".into(), elapsed: Duration::ZERO, ok: true });
        assert!(app.logs.waiting.is_none());
        app.on_event(Event::Status("Matter pairing code: 1234".into()));
        app.on_event(Event::Done(Ok("done".into())));
        assert_eq!(app.logs.notes, ["Matter pairing code: 1234"], "a line after the last step stays");
    }
}
