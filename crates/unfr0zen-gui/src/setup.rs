//! The Patch page: one guided flow from a unit on USB to a patched one.
//! Connect, Integrations, Features and Review are steps the person moves
//! through; the run started from Review shows in place of them, and its
//! success ends on Done.

use std::f32::consts::TAU;

use eframe::egui;
use unfr0zen_host::Device;

use crate::bus::Bus;
use crate::{medium, tr, App, Page, Task, ACCENT, BAD, CARD, DIM, INSET, LINE, MUTED, OK, ON_ACCENT, TEXT, WARN};

/// The flow's steps, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Step {
    #[default]
    Connect,
    Integrations,
    Features,
    Review,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Connect, Step::Integrations, Step::Features, Step::Review];

    fn title(self) -> &'static str {
        match self {
            Step::Connect => tr!("setup.step.connect"),
            Step::Integrations => tr!("setup.step.integrations"),
            Step::Features => tr!("setup.step.features"),
            Step::Review => tr!("setup.step.review"),
        }
    }

    fn index(self) -> usize {
        Step::ALL.iter().position(|&s| s == self).unwrap()
    }

    fn next(self) -> Step {
        Step::ALL[(self.index() + 1).min(Step::ALL.len() - 1)]
    }

    fn back(self) -> Step {
        Step::ALL[self.index().saturating_sub(1)]
    }
}

/// The steps' widest column.
const COLUMN: f32 = 720.0;
/// A panel inside a card: the backup note.
const NOTE: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x1e, 0x25);
/// Small labels: group headings.
const LABEL: egui::Color32 = egui::Color32::from_rgb(0x86, 0x8f, 0x9e);
/// The rings' track.
const TRACK: egui::Color32 = egui::Color32::from_rgb(0x2a, 0x30, 0x3b);
/// The rings' face, and the lighter disc on it.
const FACE: egui::Color32 = egui::Color32::from_rgb(0x1c, 0x20, 0x27);
const FACE_LIGHT: egui::Color32 = egui::Color32::from_rgb(0x23, 0x28, 0x31);
/// The accent dimmed: a passed step, a chosen card, the done mark.
const ACCENT_DIM: egui::Color32 = egui::Color32::from_rgb(0x1d, 0x34, 0x44);
const ACCENT_TINT: egui::Color32 = egui::Color32::from_rgb(0x1c, 0x26, 0x30);
/// A switch that is off, and an outlined button's edge.
const OFF: egui::Color32 = egui::Color32::from_rgb(0x35, 0x3b, 0x47);
/// A done step's mark.
const OK_FILL: egui::Color32 = egui::Color32::from_rgb(0x16, 0x2a, 0x1d);
const OK_LIGHT: egui::Color32 = egui::Color32::from_rgb(0x9e, 0xe0, 0xa3);

/// What the Patch page shows now.
#[derive(Debug, PartialEq, Eq)]
enum Screen {
    Step(Step),
    Running,
    Failed,
    Done,
}

impl App {
    fn screen(&self) -> Screen {
        if !self.flow_run {
            return Screen::Step(self.step);
        }
        match (self.running(), &self.logs.outcome) {
            // The wait for download mode called off: back to the step.
            (false, Some(Err(_))) if self.cancelled => Screen::Step(self.step),
            (true, _) | (false, None) => Screen::Running,
            (false, Some(Err(_))) => Screen::Failed,
            (false, Some(Ok(_))) => Screen::Done,
        }
    }

    #[cfg(test)]
    pub(crate) fn screen_name(&self) -> String {
        format!("{:?}", self.screen())
    }

    /// One unit unfr0zen supports is on USB: the steps past Connect need it.
    fn connected(&self) -> bool {
        matches!(self.bus.current(), Bus::One(seen) if seen.device != Device::Onyx)
    }

    pub(crate) fn setup_page(&mut self, ui: &mut egui::Ui) {
        // The unit leaving before a run takes the flow back to Connect.
        if !self.flow_run && self.step != Step::Connect && !self.connected() {
            self.step = Step::Connect;
        }
        match self.screen() {
            Screen::Step(step) => self.step_page(ui, step),
            Screen::Running => self.running_page(ui, None),
            Screen::Failed => {
                let error = self.logs.outcome.clone().and_then(Result::err);
                self.running_page(ui, error);
            }
            Screen::Done => self.done_page(ui),
        }
    }

    /// Starts the run the review step asked for: the patch, with the whole
    /// NAND backed up first when chosen, in the same entry into recovery.
    pub(crate) fn start_flow(&mut self) {
        if !self.idle() {
            return;
        }
        self.flow_backup = None;
        self.start(if self.form.backup_first { Task::BackupPatch(self.library.fresh_path()) } else { Task::Patch });
        self.flow_run = true;
    }

    /// The flow's run is still backing up: its image is not hashed yet.
    pub(crate) fn backing_up(&self) -> bool {
        matches!(self.task, Some(Task::BackupPatch(_)))
            && !self.logs.rows.iter().any(|row| row.label == unfr0zen_host::nand::HASH_STAGE && row.done.is_some())
    }

    /// Back to the first step, for another unit; the choices stay.
    fn restart_flow(&mut self) {
        self.flow_run = false;
        self.flow_backup = None;
        self.step = Step::Connect;
    }

    fn step_page(&mut self, ui: &mut egui::Ui, step: Step) {
        // The bar first, so the steps' content takes the room above it.
        egui::Panel::bottom("setup_actions").show_separator_line(false).frame(egui::Frame::new())
            .show(ui, |ui| self.action_bar(ui, step));
        stepper(ui, step);
        ui.add_space(18.0);
        scrolled(ui, egui::Id::new(("setup", step.index())), |ui| {
            if matches!(step, Step::Integrations | Step::Features) {
                ui.set_max_width(COLUMN);
            }
            match step {
                Step::Connect => self.connect_step(ui),
                Step::Integrations => {
                    heading(ui, tr!("setup.integrations_title"), tr!("setup.integrations_about"));
                    self.form.integrations_ui(ui);
                }
                Step::Features => {
                    heading(ui, tr!("setup.features_title"), tr!("setup.features_about"));
                    let bus = self.bus.current();
                    self.form.features_ui(ui, &bus);
                }
                Step::Review => self.review_step(ui),
            }
            ui.add_space(16.0);
        });
    }

    fn action_bar(&mut self, ui: &mut egui::Ui, step: Step) {
        let can_continue = match step {
            Step::Connect => self.connected(),
            Step::Integrations => self.form.integrations_done(),
            _ => true,
        };
        separator(ui);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if step != Step::Connect && pill_button(ui, tr!("setup.back"), false, true).clicked() {
                self.step = step.back();
            }
            let hint = match step {
                Step::Connect if !can_continue => Some(tr!("setup.connect_hint")),
                Step::Integrations if !can_continue => Some(tr!("setup.mqtt_needs_broker")),
                Step::Integrations if !self.form.any_integration() => Some(tr!("setup.integrations_skip_hint")),
                _ => None,
            };
            if let Some(hint) = hint {
                ui.label(egui::RichText::new(hint).color(MUTED).size(13.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if step == Step::Review {
                    let label = if self.form.backup_first {
                        tr!("setup.start_backup_patch")
                    } else {
                        tr!("setup.start_patch")
                    };
                    if pill_button(ui, label, true, self.idle()).on_hover_text(tr!("patch.start_hint"))
                        .on_disabled_hover_text(tr!("patch.start_disabled")).clicked() {
                        self.start_flow();
                    }
                    ui.label(egui::RichText::new(tr!("setup.keep_plugged_in")).color(MUTED).size(13.0));
                } else {
                    // Integrations are optional: with none chosen, the step is skipped.
                    let label = if step == Step::Integrations && !self.form.any_integration() {
                        tr!("setup.skip")
                    } else {
                        tr!("setup.continue")
                    };
                    if pill_button(ui, label, true, can_continue).clicked() {
                        self.step = step.next();
                    }
                }
            });
        });
        ui.add_space(12.0);
    }

    fn connect_step(&mut self, ui: &mut egui::Ui) {
        let bus = self.bus.current();
        ui.horizontal_top(|ui| {
            let left = (ui.available_width() - 300.0).max(320.0);
            ui.vertical(|ui| {
                ui.set_width(left);
                heading(ui, tr!("setup.connect_title"), tr!("setup.connect_about"));
                for (number, (title, about)) in [
                    (tr!("setup.connect_1"), tr!("setup.connect_1_about")),
                    (tr!("setup.connect_2"), tr!("setup.connect_2_about")),
                    (tr!("setup.connect_3"), tr!("setup.connect_3_about")),
                ].into_iter().enumerate() {
                    card(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            ui.label(egui::RichText::new((number + 1).to_string()).font(crate::display(22.0)).color(ACCENT));
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(title).font(medium(14.5)));
                                ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED)).wrap());
                            });
                        });
                    });
                    ui.add_space(10.0);
                }
            });
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                unit_ring(ui, &bus);
                ui.add_space(16.0);
                bus_pill(ui, &bus);
                ui.add_space(8.0);
                if let Bus::One(seen) = &bus {
                    let mode = if seen.mode == "app" { tr!("setup.mode_app").to_string() } else { seen.mode.to_string() };
                    ui.label(egui::RichText::new(format!("{}\n{mode}", seen.device.description())).color(MUTED).size(12.5));
                    if seen.device == Device::Onyx {
                        ui.add_space(10.0);
                        ui.add(egui::Label::new(egui::RichText::new(tr!("setup.unsupported_unit")).color(WARN).size(13.0)).wrap());
                    }
                }
            });
        });
    }

    fn review_step(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            let left = (ui.available_width() - 340.0).max(320.0);
            ui.vertical(|ui| {
                ui.set_width(left);
                heading(ui, tr!("setup.review_title"), tr!("setup.review_about"));
                card(ui, |ui| {
                    let rows = self.form.summary();
                    let count = rows.len();
                    for (i, (name, value, step)) in rows.into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.set_min_height(32.0);
                            let left = egui::Layout::left_to_right(egui::Align::Center);
                            ui.allocate_ui_with_layout(egui::vec2(230.0, 20.0), left, |ui| {
                                ui.set_width(230.0);
                                ui.add(egui::Label::new(egui::RichText::new(name).color(MUTED)).truncate());
                            });
                            let room = ui.available_width() - 50.0;
                            ui.allocate_ui_with_layout(egui::vec2(room, 20.0), left, |ui| {
                                ui.set_width(room);
                                ui.add(egui::Label::new(value).truncate());
                            });
                            if link(ui, tr!("setup.edit")).clicked() {
                                self.step = step;
                            }
                        });
                        if i + 1 < count {
                            separator(ui);
                        }
                    }
                });
                ui.add_space(10.0);
                card(ui, |ui| {
                    let header = ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        chevron(ui.painter(), rect.center(), self.developer_open, MUTED);
                        ui.label(egui::RichText::new(tr!("setup.developer")).font(medium(15.0)).color(TEXT));
                    }).response.interact(egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(tr!("setup.developer_hint"));
                    if header.clicked() {
                        self.developer_open = !self.developer_open;
                    }
                    if self.developer_open {
                        ui.add_space(10.0);
                        ui.spacing_mut().item_spacing.y = 8.0;
                        let bus = self.bus.current();
                        self.form.developer_ui(ui, &bus);
                    }
                });
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(tr!("setup.backup_question")).font(medium(15.0)));
                ui.add_space(8.0);
                let latest = self.library.entries.first()
                    .map(|e| tr!("setup.backup_latest", taken = e.taken_label(), device = e.device_label()))
                    .unwrap_or_else(|| tr!("setup.backup_none").to_string());
                if choice_card(ui, self.form.backup_first, tr!("setup.backup_yes"), tr!("setup.backup_yes_about"), Some(tr!("setup.recommended"))).clicked() {
                    self.form.backup_first = true;
                }
                ui.add_space(10.0);
                let skip_about = format!("{} {latest}", tr!("setup.backup_no_about"));
                if choice_card(ui, !self.form.backup_first, tr!("setup.backup_no"), &skip_about, None).clicked() {
                    self.form.backup_first = false;
                }
            });
        });
    }

    /// The run in place of the steps: a ring with what it does and how far
    /// it is, the steps so far beside it; a failure says why and offers the
    /// way back.
    fn running_page(&mut self, ui: &mut egui::Ui, error: Option<String>) {
        let backing_up = self.backing_up();
        ui.add_space(((ui.available_height() - 300.0) * 0.4).max(20.0));
        ui.horizontal_top(|ui| {
            ui.add_space(16.0);
            let (fraction, label, big, under) = if error.is_some() {
                (None, tr!("setup.ring_stopped"), "!".to_string(), String::new())
            } else if let Some(t) = self.logs.transfer.as_ref().filter(|t| t.shown() < t.total) {
                let fraction = t.shown() as f32 / t.total.max(1) as f32;
                let left = t.speed().map(|rate| ((t.total.saturating_sub(t.shown())) as f64 / rate / 60.0).ceil() as u64);
                let under = left.map_or_else(String::new, |m| tr!("setup.minutes_left", minutes = m.max(1)));
                let label = if backing_up { tr!("setup.ring_backing_up") } else { tr!("setup.ring_patching") };
                (Some(fraction), label, format!("{}", (fraction * 100.0).floor() as u32), under)
            } else {
                let label = if backing_up { tr!("setup.ring_backing_up") } else { tr!("setup.ring_patching") };
                let seconds = self.logs.rows.first().map_or(0, |r| r.started.elapsed().as_secs());
                (None, label, format!("{}:{:02}", seconds / 60, seconds % 60), String::new())
            };
            progress_ring(ui, 260.0, fraction, label, &big, &under, error.is_some());
            ui.add_space(40.0);
            ui.vertical(|ui| {
                ui.add_space(20.0);
                let (title, about) = match &error {
                    Some(_) => (tr!("setup.failed_title"), tr!("setup.failed_about")),
                    None => (tr!("setup.running_title"), tr!("setup.running_about")),
                };
                ui.label(egui::RichText::new(title).font(crate::display(30.0)));
                ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED)).wrap());
                ui.add_space(14.0);
                if let Some(backup) = &self.flow_backup {
                    let name = backup.file_name().unwrap_or_default().to_string_lossy();
                    step_row(ui, StepMark::Done, &tr!("setup.backup_saved", name = name));
                }
                // The latest steps; the log has them all.
                let rows = &self.logs.rows;
                for row in rows.iter().skip(rows.len().saturating_sub(7)) {
                    let mark = match row.done {
                        Some((_, true)) => StepMark::Done,
                        Some((_, false)) => StepMark::Failed,
                        None => StepMark::Running,
                    };
                    step_row(ui, mark, &row.label);
                }
                if let Some(waiting) = &self.logs.waiting {
                    ui.add_space(4.0);
                    ui.add(egui::Label::new(egui::RichText::new(waiting).color(ACCENT)).wrap());
                }
                if let Some(error) = &error {
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(error).color(BAD)).wrap());
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if link(ui, tr!("setup.show_log")).clicked() {
                        self.show(Page::Logs);
                    }
                    if error.is_some() {
                        ui.add_space(16.0);
                        if pill_button(ui, tr!("setup.back_to_review"), true, self.idle()).clicked() {
                            self.flow_run = false;
                            self.step = Step::Review;
                        }
                    }
                });
            });
        });
        if self.running() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn done_page(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("done_actions").show_separator_line(false).frame(egui::Frame::new()).show(ui, |ui| {
            separator(ui);
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if link(ui, tr!("setup.view_log")).clicked() {
                    self.show(Page::Logs);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if pill_button(ui, tr!("setup.another"), false, self.idle()).clicked() {
                        self.restart_flow();
                    }
                });
            });
            ui.add_space(12.0);
        });
        scrolled(ui, egui::Id::new("done"), |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                done_mark(ui);
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(tr!("setup.done_title")).font(crate::display(32.0)));
                    let about = if self.flow_backup.is_some() {
                        tr!("setup.done_about_backup")
                    } else {
                        tr!("setup.done_about")
                    };
                    ui.label(egui::RichText::new(about).color(MUTED));
                });
            });
            ui.add_space(16.0);
            if let Some(backup) = self.flow_backup.clone() {
                inset(ui, |ui| {
                    ui.horizontal(|ui| {
                        let name = backup.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        let room = ui.available_width() - 120.0;
                        ui.allocate_ui_with_layout(egui::vec2(room, 20.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                            ui.set_width(room);
                            ui.add(egui::Label::new(egui::RichText::new(tr!("setup.backup_note", name = name)).size(13.5)).wrap());
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if link(ui, tr!("setup.open_backups")).clicked() {
                                self.library.selected = Some(backup.clone());
                                self.show(Page::Backups);
                            }
                        });
                    });
                });
            }
            let next = self.form.next_steps();
            if next.is_empty() {
                return;
            }
            group(ui, tr!("setup.whats_next"));
            ui.columns(next.len(), |columns| {
                for (i, (title, about)) in next.into_iter().enumerate() {
                    next_card(&mut columns[i], i + 1, title, &about);
                }
            });
        });
    }
}

/// The room kept between the page's content and its scroll bar.
const GUTTER: i8 = 16;

/// A scrolled column whose content keeps clear of the scroll bar, which
/// egui floats over the content's right edge.
fn scrolled(ui: &mut egui::Ui, id: egui::Id, body: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical().id_salt(id).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin { right: GUTTER, ..egui::Margin::ZERO }).show(ui, body);
    });
}

/// The steps across the top: done ones ticked on a dim blue, the current
/// one filled, those to come outlined; lines between, blue once passed.
fn stepper(ui: &mut egui::Ui, current: Step) {
    ui.add_space(4.0);
    let height = 26.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    let painter = ui.painter();
    let labels: Vec<_> = Step::ALL.iter().map(|step| {
        let strong = *step == current;
        let font = if strong { medium(13.0) } else { egui::FontId::proportional(13.0) };
        painter.layout_no_wrap(step.title().to_string(), font, if strong { egui::Color32::WHITE } else { MUTED })
    }).collect();
    let items: f32 = labels.iter().map(|g| 24.0 + 8.0 + g.size().x).sum();
    let gap = ((rect.width() - items) / (Step::ALL.len() - 1) as f32).max(24.0);
    let mut x = rect.left();
    let y = rect.center().y;
    for (i, (step, label)) in Step::ALL.iter().zip(labels).enumerate() {
        let c = egui::pos2(x + 12.0, y);
        if *step == current {
            painter.circle_filled(c, 12.0, ACCENT);
            painter.text(c, egui::Align2::CENTER_CENTER, (i + 1).to_string(), medium(12.5), ON_ACCENT);
        } else if step.index() < current.index() {
            painter.circle_filled(c, 12.0, ACCENT_DIM);
            tick(painter, c, 4.5, ACCENT);
        } else {
            painter.circle_stroke(c, 11.0, egui::Stroke::new(1.0, OFF));
            painter.text(c, egui::Align2::CENTER_CENTER, (i + 1).to_string(), egui::FontId::proportional(12.5), MUTED);
        }
        let label_x = x + 32.0;
        let width = label.size().x;
        painter.galley(egui::pos2(label_x, y - label.size().y * 0.5), label, MUTED);
        x = label_x + width;
        if i + 1 < Step::ALL.len() {
            let line = if step.index() < current.index() { ACCENT } else { LINE };
            painter.line_segment([egui::pos2(x + 12.0, y), egui::pos2(x + gap - 12.0, y)], egui::Stroke::new(1.0, line));
            x += gap;
        }
    }
}

/// A small chevron: pointing right when closed, down when open.
fn chevron(painter: &egui::Painter, c: egui::Pos2, open: bool, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    let points = if open {
        [c + egui::vec2(-4.0, -2.0), c + egui::vec2(0.0, 2.5), c + egui::vec2(4.0, -2.0)]
    } else {
        [c + egui::vec2(-2.0, -4.0), c + egui::vec2(2.5, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    painter.line_segment([points[0], points[1]], stroke);
    painter.line_segment([points[1], points[2]], stroke);
}

fn tick(painter: &egui::Painter, c: egui::Pos2, r: f32, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.8, color);
    painter.line_segment([c + egui::vec2(-r, 0.0), c + egui::vec2(-r * 0.3, r * 0.7)], stroke);
    painter.line_segment([c + egui::vec2(-r * 0.3, r * 0.7), c + egui::vec2(r, -r * 0.7)], stroke);
}

/// A step's title in the display cut, its line under it.
fn heading(ui: &mut egui::Ui, title: &str, about: &str) {
    ui.label(egui::RichText::new(title).font(crate::display(30.0)));
    ui.add_space(2.0);
    ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED)).wrap());
    ui.add_space(12.0);
}

fn separator(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, LINE);
}

/// A lifted, rounded panel the width of its column.
pub(crate) fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // The width there is now, fixed: content that asks for more must not
    // widen the column, and every later card with it.
    let width = ui.available_width() - 36.0;
    egui::Frame::new().fill(CARD).corner_radius(14).inner_margin(egui::Margin::symmetric(18, 14)).show(ui, |ui| {
        ui.set_width(width);
        ui.set_max_width(width);
        body(ui)
    }).inner
}

/// A note on the page: the backup Done names.
fn inset(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().fill(NOTE).corner_radius(10).inner_margin(egui::Margin::symmetric(14, 12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 8.0;
        body(ui);
    });
}

/// A field's edge: one that shows on the card, so it reads as somewhere to type.
const FIELD_EDGE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x41, 0x4e);

/// A text field with its label over it: a rounded box `width` wide whose
/// edge lights while it is typed in.
pub(crate) fn field(ui: &mut egui::Ui, label: &str, edit: egui::TextEdit<'_>, width: f32) -> egui::Response {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.label(egui::RichText::new(label).size(12.5).color(LABEL));
        let frame = egui::Frame::new().fill(INSET).stroke(egui::Stroke::new(1.0, FIELD_EDGE)).corner_radius(9)
            .inner_margin(egui::Margin::symmetric(11, 7));
        let mut prepared = frame.begin(ui);
        let room = (width - 24.0).max(40.0);
        let response = prepared.content_ui.add(edit.frame(egui::Frame::NONE).desired_width(room).font(medium(14.0)));
        if response.has_focus() {
            prepared.frame = prepared.frame.stroke(egui::Stroke::new(1.0, ACCENT));
        }
        prepared.end(ui);
        response
    }).inner
}

/// A drop-down with its label over it, drawn as the fields are: a rounded
/// box `width` wide. Greyed, keeping its choice, when not enabled.
pub(crate) fn select(ui: &mut egui::Ui, label: &str, id: &str, width: f32, selected: &str, enabled: bool,
                     rows: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.label(egui::RichText::new(label).size(12.5).color(if enabled { LABEL } else { DIM }));
        ui.add_enabled_ui(enabled, |ui| {
            let widgets = &mut ui.style_mut().visuals.widgets;
            for widget in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
                widget.corner_radius = egui::CornerRadius::same(9);
            }
            widgets.inactive.bg_stroke = egui::Stroke::new(1.0, FIELD_EDGE);
            ui.spacing_mut().button_padding = egui::vec2(11.0, 7.0);
            crate::dropdown(id, width - 22.0).selected_text(egui::RichText::new(selected).font(medium(14.0))).show_ui(ui, rows).response
        }).inner
    }).inner
}

/// A switch with its name and a line on what it does beside it: an option
/// inside a card.
pub(crate) fn switch_row(ui: &mut egui::Ui, on: &mut bool, title: &str, about: &str) -> egui::Response {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let response = switch(ui, on, true).on_hover_text(title);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(title);
            ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED).size(13.0)).wrap());
        });
        response
    }).inner
}

/// A group's heading among the cards.
pub(crate) fn group(ui: &mut egui::Ui, name: &str) {
    ui.add_space(12.0);
    ui.label(egui::RichText::new(name.to_uppercase()).size(11.5).color(LABEL).extra_letter_spacing(0.8));
    ui.add_space(4.0);
}

/// One muted line under a card.
pub(crate) fn muted_line(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).color(MUTED).size(13.0)).wrap());
    ui.add_space(4.0);
}

/// A switch: a blue track with the knob right when on, a grey one with it
/// left when off. Greyed when not enabled.
pub(crate) fn switch(ui: &mut egui::Ui, on: &mut bool, enabled: bool) -> egui::Response {
    let size = egui::vec2(44.0, 26.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if enabled && response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, ""));
    let t = ui.ctx().animate_bool_responsive(response.id, *on && enabled);
    let shade = if enabled { 1.0 } else { 0.45 };
    let track = OFF.lerp_to_gamma(ACCENT, t).gamma_multiply(shade);
    let painter = ui.painter();
    painter.rect_filled(rect, 13.0, track);
    let knob = egui::pos2(egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, t), rect.center().y);
    painter.circle_filled(knob, 10.0, MUTED.lerp_to_gamma(egui::Color32::WHITE, t).gamma_multiply(shade));
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// An option as a card: its name and what it does, a switch at the right,
/// and when on, `body` under them. Returns whether it is on (and enabled).
pub(crate) fn option_card(ui: &mut egui::Ui, on: &mut bool, enabled: bool, title: &str, about: &str,
                          body: impl FnOnce(&mut egui::Ui)) -> bool {
    card_with(ui, on, enabled, title, about, Some(Box::new(body)))
}

/// An option as a card with nothing under it.
pub(crate) fn toggle_card(ui: &mut egui::Ui, on: &mut bool, enabled: bool, title: &str, about: &str) -> bool {
    card_with(ui, on, enabled, title, about, None)
}

type Body<'a> = Box<dyn FnOnce(&mut egui::Ui) + 'a>;

fn card_with(ui: &mut egui::Ui, on: &mut bool, enabled: bool, title: &str, about: &str, body: Option<Body<'_>>) -> bool {
    let shown = card(ui, |ui| {
        ui.horizontal_top(|ui| {
            let room = ui.available_width() - 44.0 - 16.0 - 2.0 * ui.spacing().item_spacing.x;
            ui.vertical(|ui| {
                ui.set_width(room);
                ui.label(egui::RichText::new(title).font(medium(15.0)).color(if enabled { TEXT } else { DIM }));
                ui.add(egui::Label::new(egui::RichText::new(about).color(if enabled { MUTED } else { DIM })).wrap());
            });
            ui.add_space(16.0);
            switch(ui, on, enabled).on_hover_text(title);
        });
        let shown = *on && enabled;
        if let Some(body) = body.filter(|_| shown) {
            ui.add_space(10.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            body(ui);
        }
        shown
    });
    ui.add_space(10.0);
    shown
}

/// One of the review step's two backup answers: a card with a radio mark,
/// outlined in the accent when chosen.
fn choice_card(ui: &mut egui::Ui, chosen: bool, title: &str, about: &str, badge: Option<&str>) -> egui::Response {
    let fill = if chosen { ACCENT_TINT } else { CARD };
    let stroke = egui::Stroke::new(2.0, if chosen { ACCENT } else { egui::Color32::TRANSPARENT });
    let inner = egui::Frame::new().fill(fill).stroke(stroke).corner_radius(14).inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                let c = rect.center();
                ui.painter().circle_stroke(c, 8.0, egui::Stroke::new(1.5, if chosen { ACCENT } else { MUTED }));
                if chosen {
                    ui.painter().circle_filled(c, 4.5, ACCENT);
                }
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(title).font(medium(14.5)));
                    ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED).size(13.0)).wrap());
                    if let Some(badge) = badge {
                        ui.add_space(4.0);
                        egui::Frame::new().fill(ACCENT).corner_radius(10).inner_margin(egui::Margin::symmetric(9, 2)).show(ui, |ui| {
                            ui.label(egui::RichText::new(badge).font(medium(11.5)).color(ON_ACCENT));
                        });
                    }
                });
            });
        });
    let response = ui.interact(inner.response.rect, ui.id().with(title), egui::Sense::click());
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A rounded button: the accent with dark text for the step's action,
/// outlined for the others.
pub(crate) fn pill_button(ui: &mut egui::Ui, text: &str, primary: bool, enabled: bool) -> egui::Response {
    let (fill, color, stroke) = if primary {
        (if enabled { ACCENT } else { ACCENT.gamma_multiply(0.35) }, if enabled { ON_ACCENT } else { DIM }, egui::Stroke::NONE)
    } else {
        (egui::Color32::TRANSPARENT, if enabled { TEXT } else { DIM }, egui::Stroke::new(1.0, OFF))
    };
    let label = egui::RichText::new(text).font(medium(14.0)).color(color);
    ui.add_enabled(enabled, egui::Button::new(label).fill(fill).stroke(stroke).corner_radius(18).min_size(egui::vec2(96.0, 36.0)))
}

/// Accent text that acts.
pub(crate) fn link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let response = ui.add(egui::Label::new(egui::RichText::new(text).color(ACCENT).size(13.0)).sense(egui::Sense::click()));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The thermostat as the connect step draws it: with a unit on USB, its
/// ring lit and its generation in the middle; without one, a plug in the
/// middle and a short arc going round the ring, as the Nest's own ring
/// does while it searches.
fn unit_ring(ui: &mut egui::Ui, bus: &Bus) {
    let size = 200.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    painter.circle_filled(c, size * 0.5, TRACK);
    painter.circle_filled(c, size * 0.5 - 10.0, FACE);
    painter.circle_filled(c - egui::vec2(0.0, 12.0), size * 0.32, FACE_LIGHT);
    let found = match bus {
        Bus::One(seen) => Some(seen.device),
        _ => None,
    };
    let Some(device) = found else {
        // The search: a soft arc a quarter of the way round, every 2.4 s.
        let time = ui.input(|i| i.time) as f32;
        let r = size * 0.5 - 5.0;
        let head = (time / 2.4).rem_euclid(1.0) * TAU - TAU / 4.0;
        let steps = 24;
        for i in 0..steps {
            let (a0, a1) = (head - (i + 1) as f32 * 0.06, head - i as f32 * 0.06);
            let fade = 1.0 - i as f32 / steps as f32;
            painter.line_segment([c + egui::vec2(a0.cos(), a0.sin()) * r, c + egui::vec2(a1.cos(), a1.sin()) * r],
                                 egui::Stroke::new(6.0, ACCENT.gamma_multiply(0.85 * fade * fade)));
        }
        ui.ctx().request_repaint();
        plug_icon(painter, c - egui::vec2(0.0, 36.0), MUTED);
        painter.text(c + egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, tr!("setup.plug_in"), crate::display(30.0), TEXT);
        painter.text(c + egui::vec2(0.0, 40.0), egui::Align2::CENTER_CENTER, tr!("setup.data_cable"),
                     egui::FontId::proportional(12.0), MUTED);
        return;
    };
    let ring = if device == Device::Onyx { WARN } else { OK };
    painter.circle_stroke(c, size * 0.5 - 11.0, egui::Stroke::new(2.0, ring));
    let (big, name) = match device {
        Device::Diamond3 => (tr!("setup.gen3"), tr!("setup.learning_thermostat")),
        Device::J49 => (tr!("setup.gen2"), tr!("setup.learning_thermostat")),
        Device::Diamond => (tr!("setup.gen1"), tr!("setup.learning_thermostat")),
        Device::Onyx => (tr!("setup.gen_e"), tr!("setup.thermostat_e")),
    };
    painter.text(c - egui::vec2(0.0, 8.0), egui::Align2::CENTER_CENTER, big, crate::display(42.0), TEXT);
    painter.text(c + egui::vec2(0.0, 28.0), egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(12.0), MUTED);
}

/// A USB plug, pointing up, its cable trailing down, centred on `c`.
fn plug_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.8, color);
    let body = egui::Rect::from_center_size(c + egui::vec2(0.0, 2.0), egui::vec2(16.0, 14.0));
    painter.rect_stroke(body, 3.0, stroke, egui::StrokeKind::Middle);
    let tip = egui::Rect::from_min_max(egui::pos2(c.x - 5.5, body.top() - 8.0), egui::pos2(c.x + 5.5, body.top()));
    painter.rect_stroke(tip, 1.0, stroke, egui::StrokeKind::Middle);
    painter.line_segment([egui::pos2(c.x, body.bottom()), egui::pos2(c.x, body.bottom() + 8.0)], stroke);
}

/// Whether a unit is there, as a rounded status tag.
fn bus_pill(ui: &mut egui::Ui, bus: &Bus) {
    let (text, fg, bg, dot) = match bus {
        Bus::Nothing => (tr!("setup.waiting_for_unit"), TEXT, CARD, DIM),
        Bus::One(seen) if seen.device == Device::Onyx => (tr!("setup.unsupported_short"), WARN, egui::Color32::from_rgb(0x2a, 0x24, 0x14), WARN),
        Bus::One(_) => (tr!("setup.found_unit"), OK_LIGHT, OK_FILL, OK),
        Bus::Several(_) => (tr!("setup.several_units"), WARN, egui::Color32::from_rgb(0x2a, 0x24, 0x14), WARN),
    };
    let waiting = matches!(bus, Bus::Nothing);
    // Painted at its own size, so a centred column centres it; a frame
    // would take the column's width and sit at its left.
    let galley = ui.painter().layout_no_wrap(text.to_string(), medium(13.5), fg);
    let size = egui::vec2(14.0 + 8.0 + 8.0 + galley.size().x + 14.0, galley.size().y + 14.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 16.0, bg);
    let dot_at = egui::pos2(rect.left() + 18.0, rect.center().y);
    if waiting {
        // A slow pulse in the accent: still looking.
        let t = (ui.input(|i| i.time) as f32 * 2.6).sin() * 0.5 + 0.5;
        painter.circle_filled(dot_at, 4.0 + 3.0 * t, ACCENT.gamma_multiply(0.25 * (1.0 - t)));
        painter.circle_filled(dot_at, 4.0, ACCENT);
    } else {
        painter.circle_filled(dot_at, 4.0, dot);
    }
    painter.galley(egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y * 0.5), galley, fg);
}

/// The run's ring, as the thermostat's: a 300° track open at the bottom,
/// lit to `fraction` (shown as a percentage), or a lit arc that goes round
/// while the share done is unknown; what it does above a big number in the
/// middle.
fn progress_ring(ui: &mut egui::Ui, size: f32, fraction: Option<f32>, label: &str, big: &str, under: &str, failed: bool) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    let c = rect.center();
    let r = size * 0.5 - 12.0;
    painter.circle_filled(c, r - 6.0, FACE);
    painter.circle_filled(c - egui::vec2(0.0, 16.0), r * 0.62, FACE_LIGHT);
    let start = TAU * (120.0 / 360.0);
    let sweep = TAU * (300.0 / 360.0);
    let arc = |from: f32, to: f32, color: egui::Color32| {
        let steps = ((to - from).abs() / 0.03).ceil().max(2.0) as usize;
        let points: Vec<egui::Pos2> = (0..=steps).map(|i| {
            let a = from + (to - from) * i as f32 / steps as f32;
            c + egui::vec2(a.cos(), a.sin()) * r
        }).collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(14.0, color)));
    };
    arc(start, start + sweep, TRACK);
    let lit = if failed { BAD } else { ACCENT };
    match fraction {
        Some(f) if f > 0.0 => arc(start, start + sweep * f.clamp(0.0, 1.0), lit),
        Some(_) => {}
        None if failed => arc(start, start + sweep, lit.gamma_multiply(0.5)),
        None => {
            let time = ui.input(|i| i.time) as f32;
            let head = start + (time * 0.9).rem_euclid(1.0) * sweep;
            arc(head.max(start), (head + sweep * 0.22).min(start + sweep), lit);
            ui.ctx().request_repaint();
        }
    }
    painter.text(c - egui::vec2(0.0, 50.0), egui::Align2::CENTER_CENTER, label.to_uppercase(), medium(12.0), ACCENT);
    let percent = fraction.is_some();
    let number = painter.layout_no_wrap(big.to_string(), crate::display(if percent { 72.0 } else { 54.0 }), TEXT);
    let at = c - number.size() * 0.5 + egui::vec2(if percent { -10.0 } else { 0.0 }, 4.0);
    let right = at.x + number.size().x;
    painter.galley(at, number, TEXT);
    if percent {
        painter.text(egui::pos2(right + 2.0, c.y - 18.0), egui::Align2::LEFT_CENTER, "%", crate::display(26.0), TEXT);
    }
    if !under.is_empty() {
        painter.text(c + egui::vec2(0.0, 52.0), egui::Align2::CENTER_CENTER, under, egui::FontId::proportional(12.5), MUTED);
    }
}

enum StepMark {
    Done,
    Running,
    Failed,
}

/// One of the run's steps beside the ring.
fn step_row(ui: &mut egui::Ui, mark: StepMark, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        let c = rect.center();
        let painter = ui.painter();
        let color = match mark {
            StepMark::Done => {
                painter.circle_filled(c, 11.0, OK_FILL);
                tick(painter, c, 4.5, OK_LIGHT);
                MUTED
            }
            StepMark::Failed => {
                painter.circle_filled(c, 11.0, egui::Color32::from_rgb(0x3a, 0x16, 0x16));
                painter.text(c, egui::Align2::CENTER_CENTER, "!", medium(13.0), BAD);
                BAD
            }
            StepMark::Running => {
                let a = ui.input(|i| i.time) as f32 * 5.0;
                let points: Vec<egui::Pos2> = (0..=20).map(|i| {
                    let t = a + i as f32 / 20.0 * TAU * 0.7;
                    c + egui::vec2(t.cos(), t.sin()) * 8.0
                }).collect();
                painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, ACCENT)));
                ui.ctx().request_repaint();
                egui::Color32::WHITE
            }
        };
        ui.add(egui::Label::new(egui::RichText::new(text).color(color)).truncate());
    });
    ui.add_space(2.0);
}

/// The ticked circle at the head of Done.
fn done_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(76.0, 76.0), egui::Sense::hover());
    let c = rect.center();
    let painter = ui.painter();
    painter.circle_filled(c, 38.0, ACCENT_DIM);
    painter.circle_stroke(c, 37.0, egui::Stroke::new(2.0, ACCENT));
    let stroke = egui::Stroke::new(3.0, ACCENT);
    painter.line_segment([c + egui::vec2(-13.0, 1.0), c + egui::vec2(-4.0, 10.0)], stroke);
    painter.line_segment([c + egui::vec2(-4.0, 10.0), c + egui::vec2(14.0, -9.0)], stroke);
}

/// One of Done's next steps: its number, name and what to do.
fn next_card(ui: &mut egui::Ui, number: usize, title: &str, about: &str) {
    card(ui, |ui| {
        ui.set_min_height(170.0);
        // Ragged lines: ui.columns justifies, which spaces out the words.
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.label(egui::RichText::new(number.to_string()).font(crate::display(24.0)).color(ACCENT));
            ui.label(egui::RichText::new(title).font(medium(15.0)));
            ui.add(egui::Label::new(egui::RichText::new(about).color(MUTED)).wrap());
        });
    });
}
