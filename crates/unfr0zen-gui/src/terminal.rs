//! A terminal on one of the unit's USB serial ports: the root shell, the
//! recovery console or U-Boot's prompt. A thread owns the open port, reads
//! it into the scrollback and writes what the window sends it: every key
//! pressed while the terminal has focus, as a serial terminal would.

use std::io::{ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use unfr0zen_host::device;
use unfr0zen_host::usb::{self, Id, Port};

/// A USB serial port on the bus, named by the unit it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub id: Id,
}

impl Candidate {
    pub fn label(&self) -> String {
        match device::identify(self.id) {
            Some(dev) => crate::tr!("terminal.port_label", device = dev.description(),
                                   mode = device::mode_name(dev, self.id).unwrap_or(crate::tr!("terminal.unknown_mode")),
                                   port = self.name),
            None => crate::tr!("terminal.unidentified_port_label", id = format!("{:04x}:{:04x}", self.id.0, self.id.1), port = self.name),
        }
    }
}

/// The ports with one of the identities, by name.
fn with_ids(ports: Vec<Candidate>, ids: &[Id]) -> Vec<Candidate> {
    let mut ports: Vec<Candidate> = ports.into_iter().filter(|p| ids.contains(&p.id)).collect();
    ports.sort_by_key(|p| p.name.clone());
    ports
}

/// The application's root shells: the USB shell of a patched unit, and the
/// gen 3's initramfs shell.
pub const SHELLS: &[Id] = &[
    unfr0zen_host::diamond3::usb::APP_ACM, unfr0zen_host::diamond3::usb::INITRAMFS,
    unfr0zen_host::j49::usb::APP, unfr0zen_host::j49::usb::DIAMOND_APP,
];

/// Recovery U-Boot's console.
pub const RECOVERY: &[Id] = &[unfr0zen_host::diamond3::usb::RECOVERY];

/// What the window keeps of an open port.
struct Connection {
    name: String,
    to_port: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A character as printed, in the colour the unit chose for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    c: char,
    color: Option<egui::Color32>,
}

/// What the unit printed, as a terminal shows it: finished lines, and the
/// line the cursor is on, where a carriage return goes back to its start
/// and text overwrites. How the thread ended, once it has.
#[derive(Default)]
struct Scrollback {
    /// The finished lines, each ending in a newline.
    done: Vec<Cell>,
    line: Vec<Cell>,
    /// The cursor's column on `line`.
    col: usize,
    /// The colour set by the last SGR sequence, if any, and whether bold.
    color: Option<egui::Color32>,
    bold: bool,
    /// Bytes of an escape sequence or a UTF-8 character still arriving.
    pending: Vec<u8>,
    closed: Option<String>,
}

/// The most text kept; older lines go when it is exceeded.
const SCROLLBACK: usize = 256 * 1024;

pub struct Terminal {
    /// The identities this terminal lists.
    ids: &'static [Id],
    pub ports: Vec<Candidate>,
    listed: Option<Instant>,
    pub chosen: Option<String>,
    connection: Option<Connection>,
    scrollback: Arc<Mutex<Scrollback>>,
    /// A port to connect to as soon as it is listed: the recovery console
    /// once U-Boot is loaded.
    pub auto_connect: Option<Id>,
    pub error: Option<String>,
}

const LIST_EVERY: Duration = Duration::from_secs(1);

impl Terminal {
    pub fn new(ids: &'static [Id]) -> Terminal {
        Terminal {
            ids, ports: Vec::new(), listed: None, chosen: None, connection: None,
            scrollback: Arc::default(), auto_connect: None, error: None,
        }
    }

    pub fn connected(&self) -> bool {
        self.connection.is_some()
    }

    /// The port in use.
    pub fn port_name(&self) -> Option<&str> {
        self.connection.as_ref().map(|c| c.name.as_str())
    }

    /// Lists the terminal's ports again once a second; the chosen one stays
    /// while it is listed, else the first takes its place.
    fn refresh(&mut self) {
        if self.listed.is_some_and(|at| at.elapsed() < LIST_EVERY) {
            return;
        }
        self.listed = Some(Instant::now());
        self.ports = with_ids(usb::acm_ports().into_iter().map(|(name, id)| Candidate { name, id }).collect(), self.ids);
        if self.chosen.as_ref().is_some_and(|c| !self.ports.iter().any(|p| &p.name == c)) {
            self.chosen = None;
        }
        if self.chosen.is_none() {
            self.chosen = self.ports.first().map(|p| p.name.clone());
        }
    }

    pub fn connect(&mut self, ctx: egui::Context) {
        let Some(name) = self.chosen.clone() else { return };
        self.error = None;
        let port = match Port::open(&name) {
            Ok(port) => port,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let (to_port, from_window) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        *self.scrollback.lock().unwrap() = Scrollback::default();
        thread::spawn({
            let (scrollback, stop) = (self.scrollback.clone(), stop.clone());
            move || pump(port, from_window, scrollback, stop, ctx)
        });
        self.connection = Some(Connection { name, to_port, stop });
        // A line return, so the unit shows its prompt right away.
        self.send(b"\r".to_vec());
    }

    pub fn disconnect(&mut self) {
        self.connection = None;
    }

    pub fn send(&mut self, bytes: Vec<u8>) {
        if let Some(connection) = &self.connection {
            if connection.to_port.send(bytes).is_err() {
                self.connection = None;
            }
        }
    }

    /// The text shown, laid out in its colours, and the cursor's place in
    /// it, in characters.
    fn job(&self, ui: &egui::Ui) -> (egui::text::LayoutJob, usize) {
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let scrollback = self.scrollback.lock().unwrap();
        (scrollback.job(font, ui.visuals().text_color()), scrollback.cursor())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, idle: bool) {
        self.refresh();
        if let Some(id) = self.auto_connect {
            if let Some(port) = self.ports.iter().find(|p| p.id == id) {
                self.chosen = Some(port.name.clone());
                self.auto_connect = None;
                if idle && !self.connected() {
                    self.connect(ui.ctx().clone());
                }
            }
        }
        // The thread's end closes the connection on the window's side.
        let closed = self.scrollback.lock().unwrap().closed.take();
        if let Some(reason) = closed {
            self.connection = None;
            if !reason.is_empty() {
                self.error = Some(reason);
            }
        }
        ui.horizontal(|ui| {
            let connected = self.connected();
            ui.add_enabled_ui(!connected, |ui| {
                let selected = self.ports.iter().find(|p| Some(&p.name) == self.chosen.as_ref())
                    .map_or_else(|| crate::tr!("terminal.no_port").to_string(), Candidate::label);
                crate::dropdown("serial_port", 380.0).selected_text(selected).show_ui(ui, |ui| {
                    for port in &self.ports {
                        ui.selectable_value(&mut self.chosen, Some(port.name.clone()), port.label());
                    }
                }).response.on_hover_text(crate::tr!("terminal.port_hint"));
            });
            // Both buttons the accent button's height, so the row, and the
            // terminal under it, keep their size across connect and disconnect.
            let size = egui::vec2(120.0, 28.0);
            if connected {
                if ui.add(egui::Button::new(crate::tr!("terminal.disconnect")).min_size(size))
                    .on_hover_text(crate::tr!("terminal.disconnect_hint"))
                    .clicked() {
                    self.disconnect();
                }
            } else if crate::accent_button(ui, crate::tr!("terminal.connect"), idle && self.chosen.is_some())
                .on_hover_text(crate::tr!("terminal.connect_hint"))
                .on_disabled_hover_text(crate::tr!("terminal.connect_disabled_hint"))
                .clicked() {
                self.connect(ui.ctx().clone());
            }
            if ui.add(egui::Button::new(crate::tr!("terminal.clear")).min_size(egui::vec2(0.0, 28.0)))
                .on_hover_text(crate::tr!("terminal.clear_hint"))
                .clicked() {
                self.scrollback.lock().unwrap().clear();
            }
            // An error on the row, not a line of its own.
            if let Some(error) = &self.error {
                ui.add(egui::Label::new(egui::RichText::new(error).size(12.0).color(crate::BAD)).truncate())
                    .on_hover_text(error);
            }
        });
        ui.add_space(6.0);
        // The terminal takes the keys while it has focus; a click gives it.
        let id = ui.id().with("terminal");
        let focused = ui.memory(|m| m.has_focus(id));
        let stroke = if focused && self.connected() { egui::Stroke::new(1.0, crate::ACCENT) } else { egui::Stroke::new(1.0, crate::LINE) };
        let frame = egui::Frame::new().fill(crate::STATUS).stroke(stroke).corner_radius(8).inner_margin(egui::Margin::same(10));
        let height = ui.available_height().max(80.0);
        let area = frame.show(ui, |ui| {
            ui.set_min_height(height - 22.0);
            ui.set_max_height(height - 22.0);
            egui::ScrollArea::vertical().id_salt("terminal").auto_shrink([false, false]).stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let (job, cursor) = self.job(ui);
                    if job.text.is_empty() && !self.connected() {
                        ui.label(egui::RichText::new(crate::tr!("terminal.empty")).color(crate::DIM));
                    } else {
                        // Laid out here so the cursor's cell is known: a block
                        // that blinks while the console has focus, hollow else.
                        let text_color = ui.visuals().text_color();
                        let (pos, galley, response) = egui::Label::new(job).wrap().selectable(true).layout_in_ui(ui);
                        egui::text_selection::LabelSelectionState::label_text_selection(
                            ui, &response, pos, galley.clone(), text_color, egui::Stroke::NONE);
                        if self.connected() {
                            let caret = galley.pos_from_cursor(egui::text::CCursor::new(cursor)).translate(pos.to_vec2());
                            let font = egui::TextStyle::Monospace.resolve(ui.style());
                            let width = ui.fonts_mut(|f| f.glyph_width(&font, 'M'));
                            let cell = egui::Rect::from_min_size(caret.min, egui::vec2(width, caret.height()));
                            let lit = (ui.input(|i| i.time) % 1.0) < 0.5;
                            if focused && lit {
                                ui.painter().rect_filled(cell, 1.0, crate::ACCENT.gamma_multiply(0.85));
                            } else if !focused {
                                ui.painter().rect_stroke(cell, 1.0, egui::Stroke::new(1.0, crate::MUTED), egui::StrokeKind::Inside);
                            }
                            if focused {
                                ui.ctx().request_repaint_after(Duration::from_millis(500));
                            }
                        }
                    }
                });
        }).response;
        if ui.interact(area.rect, id, egui::Sense::click()).clicked() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        if focused && self.connected() {
            ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter {
                tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true,
            }));
            let mut bytes = Vec::new();
            ui.input_mut(|input| input.events.retain(|event| {
                let taken = key_bytes(event, &mut bytes);
                !taken
            }));
            if !bytes.is_empty() {
                self.send(bytes);
            }
        }
    }
}

/// The bytes a terminal sends for `event`, appended to `out`; whether the
/// event was the terminal's.
fn key_bytes(event: &egui::Event, out: &mut Vec<u8>) -> bool {
    use egui::{Event, Key};
    match event {
        Event::Text(text) | Event::Paste(text) => out.extend_from_slice(text.as_bytes()),
        Event::Key { key, pressed: true, modifiers, .. } => {
            if modifiers.ctrl && !modifiers.alt {
                // Ctrl with a letter: its control character.
                if let Some(c) = key.name().chars().next().filter(|c| c.is_ascii_alphabetic() && key.name().len() == 1) {
                    out.push(c.to_ascii_uppercase() as u8 & 0x1f);
                    return true;
                }
            }
            let seq: &[u8] = match key {
                Key::Enter => b"\r",
                Key::Backspace => b"\x7f",
                Key::Tab => b"\t",
                Key::Escape => b"\x1b",
                Key::ArrowUp => b"\x1b[A",
                Key::ArrowDown => b"\x1b[B",
                Key::ArrowRight => b"\x1b[C",
                Key::ArrowLeft => b"\x1b[D",
                Key::Home => b"\x1b[H",
                Key::End => b"\x1b[F",
                Key::Delete => b"\x1b[3~",
                _ => return false,
            };
            out.extend_from_slice(seq);
        }
        _ => return false,
    }
    true
}

/// The port's thread: writes what the window sends, reads what the unit
/// answers into the scrollback, until stopped or the port fails.
fn pump(mut port: Port, from_window: Receiver<Vec<u8>>, scrollback: Arc<Mutex<Scrollback>>, stop: Arc<AtomicBool>,
        ctx: egui::Context) {
    // Keys are written between reads: a short read keeps them prompt.
    if let Err(e) = port.set_timeout(Duration::from_millis(10)) {
        log::warn!("terminal read timeout: {e}");
    }
    let mut buffer = [0u8; 4096];
    let reason = 'run: loop {
        if stop.load(Ordering::Relaxed) {
            break String::new();
        }
        while let Ok(bytes) = from_window.try_recv() {
            if let Err(e) = port.write_all(&bytes).and_then(|_| port.flush()) {
                break 'run crate::tr!("terminal.port_closed", error = e);
            }
        }
        match port.read(&mut buffer) {
            Ok(0) => {}
            Ok(n) => {
                scrollback.lock().unwrap().append(&buffer[..n]);
                ctx.request_repaint();
            }
            Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => break crate::tr!("terminal.port_closed", error = e),
        }
    };
    scrollback.lock().unwrap().closed = Some(reason);
    ctx.request_repaint();
}

impl Scrollback {
    /// The cursor's place in the cells shown, in characters.
    fn cursor(&self) -> usize {
        self.done.len() + self.col
    }

    /// Every cell shown: the lines, the blanks past the cursor left off.
    fn cells(&self) -> impl Iterator<Item = &Cell> {
        let shown = self.line.iter().rposition(|cell| cell.c != ' ').map_or(0, |i| i + 1).max(self.col);
        self.done.iter().chain(&self.line[..shown.min(self.line.len())])
    }

    #[cfg(test)]
    fn text(&self) -> String {
        self.cells().map(|cell| cell.c).collect()
    }

    /// The cells as one layout, a section per run of one colour; `plain`
    /// is the colour of text the unit did not colour.
    fn job(&self, font: egui::FontId, plain: egui::Color32) -> egui::text::LayoutJob {
        let mut job = egui::text::LayoutJob::default();
        let mut run = String::new();
        let mut color = None;
        let mut flush = |run: &mut String, color: Option<egui::Color32>| {
            if !run.is_empty() {
                job.append(run, 0.0, egui::TextFormat::simple(font.clone(), color.unwrap_or(plain)));
                run.clear();
            }
        };
        for cell in self.cells() {
            if cell.color != color {
                flush(&mut run, color);
                color = cell.color;
            }
            run.push(cell.c);
        }
        flush(&mut run, color);
        job
    }

    fn clear(&mut self) {
        self.done.clear();
        self.line.clear();
        self.col = 0;
    }

    /// Applies a Select Graphic Rendition: the foreground colour and bold,
    /// which brightens it. Backgrounds and the other attributes are ignored.
    fn sgr(&mut self, params: &[u8]) {
        let codes: Vec<u32> = std::str::from_utf8(params).unwrap_or("").split(';')
            .map(|p| p.parse().unwrap_or(0)).collect();
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => { self.color = None; self.bold = false; }
                1 => self.bold = true,
                22 => self.bold = false,
                39 => self.color = None,
                30..=37 => self.color = Some(ANSI[(codes[i] - 30) as usize]),
                90..=97 => self.color = Some(ANSI[(codes[i] - 90 + 8) as usize]),
                38 => match codes.get(i + 1) {
                    Some(5) => {
                        self.color = codes.get(i + 2).map(|&n| color_256(n));
                        i += 2;
                    }
                    Some(2) => {
                        if let [r, g, b] = codes.get(i + 2..i + 5).unwrap_or(&[]) {
                            self.color = Some(egui::Color32::from_rgb(*r as u8, *g as u8, *b as u8));
                        }
                        i += 4;
                    }
                    _ => {}
                },
                _ => {}
            }
            i += 1;
        }
    }

    /// Adds the unit's bytes: escape sequences interpreted or dropped,
    /// text decoded as it completes and put at the cursor.
    fn append(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let bytes = std::mem::take(&mut self.pending);
        let mut i = 0;
        let mut plain = Vec::new();
        while i < bytes.len() {
            if bytes[i] != 0x1b {
                plain.push(bytes[i]);
                i += 1;
                continue;
            }
            self.put_text(&mut plain);
            // ESC [ params final, or ESC and one byte; incomplete waits.
            let Some(&next) = bytes.get(i + 1) else { self.pending = bytes[i..].to_vec(); break };
            if next != b'[' {
                i += 2;
                continue;
            }
            let Some(end) = bytes[i + 2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                self.pending = bytes[i..].to_vec();
                break;
            };
            let (params, last) = (&bytes[i + 2..i + 2 + end], bytes[i + 2 + end]);
            match last {
                b'm' => self.sgr(params),
                // Erase to the end of the line, or the whole line.
                b'K' => match params {
                    b"" | b"0" => self.line.truncate(self.col),
                    _ => { self.line.clear(); self.col = 0; }
                },
                // Clear the screen.
                b'J' => self.clear(),
                _ => {}
            }
            i += end + 3;
        }
        self.put_text(&mut plain);
    }

    /// Decodes `plain` and puts it at the cursor; a character cut short by
    /// the chunk's end waits for the rest.
    fn put_text(&mut self, plain: &mut Vec<u8>) {
        let bytes = std::mem::take(plain);
        match std::str::from_utf8(&bytes) {
            Ok(text) => text.chars().for_each(|c| self.put(c)),
            Err(e) => {
                let valid = e.valid_up_to();
                std::str::from_utf8(&bytes[..valid]).unwrap().chars().for_each(|c| self.put(c));
                match e.error_len() {
                    None => {
                        let mut rest = bytes[valid..].to_vec();
                        rest.extend(std::mem::take(&mut self.pending));
                        self.pending = rest;
                    }
                    Some(bad) => {
                        self.put('\u{fffd}');
                        let mut rest = bytes[valid + bad..].to_vec();
                        self.put_text(&mut rest);
                    }
                }
            }
        }
    }

    fn put(&mut self, c: char) {
        match c {
            '\n' => {
                self.done.append(&mut self.line);
                self.done.push(Cell { c: '\n', color: None });
                self.col = 0;
                if self.done.len() > SCROLLBACK {
                    let cut = self.done.len() - SCROLLBACK;
                    let cut = self.done[cut..].iter().position(|cell| cell.c == '\n').map_or(cut, |n| cut + n + 1);
                    self.done.drain(..cut);
                }
            }
            '\r' => self.col = 0,
            '\x08' => self.col = self.col.saturating_sub(1),
            '\t' => self.put(' '),
            c if c.is_control() => {}
            c => {
                let color = self.color.map(|color| if self.bold { brighter(color) } else { color });
                let cell = Cell { c, color };
                if self.col < self.line.len() {
                    self.line[self.col] = cell;
                } else {
                    self.line.push(cell);
                }
                self.col += 1;
            }
        }
    }
}

/// The 16 ANSI colours, as a dark terminal shows them.
const ANSI: [egui::Color32; 16] = [
    egui::Color32::from_rgb(0x5c, 0x65, 0x75), egui::Color32::from_rgb(0xf0, 0x80, 0x80),
    egui::Color32::from_rgb(0x5f, 0xe0, 0x8a), egui::Color32::from_rgb(0xf3, 0xc5, 0x65),
    egui::Color32::from_rgb(0x7c, 0xc7, 0xf5), egui::Color32::from_rgb(0xd0, 0x9c, 0xf0),
    egui::Color32::from_rgb(0x6c, 0xd8, 0xd8), egui::Color32::from_rgb(0xc9, 0xd1, 0xd9),
    egui::Color32::from_rgb(0x8a, 0x94, 0xa6), egui::Color32::from_rgb(0xff, 0x9a, 0x9a),
    egui::Color32::from_rgb(0x8f, 0xf0, 0xbf), egui::Color32::from_rgb(0xff, 0xd3, 0x8a),
    egui::Color32::from_rgb(0xa8, 0xdc, 0xff), egui::Color32::from_rgb(0xe6, 0xb8, 0xff),
    egui::Color32::from_rgb(0x9a, 0xf0, 0xf0), egui::Color32::from_rgb(0xff, 0xff, 0xff),
];

/// A colour of the 256-colour table: the 16, the 6×6×6 cube, the greys.
fn color_256(n: u32) -> egui::Color32 {
    match n {
        0..=15 => ANSI[n as usize],
        16..=231 => {
            let n = n - 16;
            let level = |v: u32| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
            egui::Color32::from_rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let grey = (8 + 10 * (n.min(255) - 232)) as u8;
            egui::Color32::from_gray(grey)
        }
    }
}

/// `color`, lifted towards white, for bold.
fn brighter(color: egui::Color32) -> egui::Color32 {
    color.lerp_to_gamma(egui::Color32::WHITE, 0.3)
}

impl Scrollback {
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_is_overwritten_after_a_carriage_return_and_erased_on_request() {
        let mut s = Scrollback::default();
        s.append(b"\x1b[32mroot@diamond\x1b[0m:~# \r\nls\r\n");
        assert_eq!(s.text(), "root@diamond:~# \nls\n");
        s.append(b"a\x08b\x07\x1b");
        assert_eq!(s.text(), "root@diamond:~# \nls\nb");
        s.append(b"[K done");
        assert_eq!(s.text(), "root@diamond:~# \nls\nb done");
        // The prompt redrawn after a bare carriage return replaces the line.
        s.append(b"\r# ");
        assert_eq!(s.text(), "root@diamond:~# \nls\n# done");
        s.append(b"\x1b[K");
        assert_eq!(s.text(), "root@diamond:~# \nls\n# ");
        // busybox erases a typed character with backspace, space, backspace.
        s.append(b"ls\x08 \x08");
        assert_eq!(s.text(), "root@diamond:~# \nls\n# l");
        assert_eq!(s.cursor(), s.text().chars().count(), "the cursor sits after the l");
        s.append(b"\r");
        assert_eq!(s.cursor(), "root@diamond:~# \nls\n".len(), "a carriage return puts it at the line's start");
        s.append(b"\x1b[2J");
        assert_eq!(s.text(), "");
    }

    #[test]
    fn colours_follow_the_sgr_sequences() {
        let mut s = Scrollback::default();
        s.append(b"\x1b[32mok\x1b[0m plain \x1b[1;31mBAD\x1b[39m \x1b[0;38;2;0;172;193mroot\x1b[38;5;196m!\x1b[m");
        assert_eq!(s.text(), "ok plain BAD root!");
        let colors: Vec<Option<egui::Color32>> = s.cells().map(|c| c.color).collect();
        assert_eq!(colors[0], Some(ANSI[2]));
        assert_eq!(colors[3], None);
        assert_eq!(colors[9], Some(brighter(ANSI[1])));
        assert_eq!(colors[13], Some(egui::Color32::from_rgb(0, 172, 193)));
        assert_eq!(colors[17], Some(color_256(196)));
        assert_eq!(color_256(196), egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(color_256(244), egui::Color32::from_gray(128));
        let job = s.job(egui::FontId::monospace(12.0), egui::Color32::WHITE);
        assert_eq!(job.text, "ok plain BAD root!");
        assert_eq!(job.sections.len(), 6, "one section per run of colour");
        assert_eq!(job.sections[1].format.color, egui::Color32::WHITE);
    }

    #[test]
    fn a_character_split_across_chunks_is_completed() {
        let mut s = Scrollback::default();
        s.append("é".as_bytes()[..1].as_ref());
        assert_eq!(s.text(), "");
        s.append("é".as_bytes()[1..].as_ref());
        assert_eq!(s.text(), "é");
        s.append(b"\xff!");
        assert_eq!(s.text(), "é\u{fffd}!");
    }

    #[test]
    fn the_scrollback_keeps_the_last_lines() {
        let mut s = Scrollback::default();
        let line = "x".repeat(1000) + "\n";
        for _ in 0..300 {
            s.append(line.as_bytes());
        }
        let text = s.text();
        assert!(text.len() <= SCROLLBACK);
        assert!(text.starts_with('x') && text.ends_with('\n'));
        assert_eq!(text.len() % 1001, 0, "cut at a line");
    }

    #[test]
    fn each_terminal_lists_only_its_ports() {
        let bridge = Candidate { name: "/dev/cu.usbserial-1001".into(), id: (0x10c4, 0xea60) };
        let shell = Candidate { name: "/dev/cu.usbmodem14".into(), id: (0x2464, 0x0030) };
        let recovery = Candidate { name: "/dev/cu.usbmodem12".into(), id: (0x18d1, 0x4ee0) };
        let all = vec![bridge, shell.clone(), recovery.clone()];
        assert_eq!(with_ids(all.clone(), SHELLS), [shell]);
        assert_eq!(with_ids(all, RECOVERY), [recovery]);
    }

    #[test]
    fn a_candidate_is_named_by_its_unit() {
        let port = Candidate { name: "/dev/cu.usbmodem1".into(), id: (0x2464, 0x0030) };
        assert_eq!(port.label(), "Nest gen 3 (diamond3) · app — /dev/cu.usbmodem1");
        let other = Candidate { name: "COM3".into(), id: (0x1234, 0x0001) };
        assert_eq!(other.label(), "1234:0001 — COM3");
    }

    #[test]
    fn keys_become_what_a_serial_terminal_sends() {
        use egui::{Event, Key, Modifiers};
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut out = Vec::new();
        assert!(key_bytes(&Event::Text("ls".into()), &mut out));
        assert!(key_bytes(&key(Key::Enter, Modifiers::NONE), &mut out));
        assert!(key_bytes(&key(Key::C, Modifiers::CTRL), &mut out));
        assert!(key_bytes(&key(Key::ArrowUp, Modifiers::NONE), &mut out));
        assert!(key_bytes(&key(Key::Backspace, Modifiers::NONE), &mut out));
        assert_eq!(out, b"ls\r\x03\x1b[A\x7f");
        assert!(!key_bytes(&key(Key::C, Modifiers::COMMAND), &mut out), "copy stays with the window");
        assert!(!key_bytes(&Event::Copy, &mut out));
        assert!(!key_bytes(&key(Key::F1, Modifiers::NONE), &mut out));
    }

    #[test]
    fn a_dropped_thread_closes_the_connection_in_the_window() {
        let mut terminal = Terminal::new(SHELLS);
        assert!(!terminal.connected() && terminal.port_name().is_none());
        terminal.send(vec![1]);
        terminal.scrollback.lock().unwrap().closed = Some("port closed: gone".into());
        let context = egui::Context::default();
        crate::install_fonts(&context);
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| terminal.ui(ui, true));
        });
        assert_eq!(terminal.error.as_deref(), Some("port closed: gone"));
    }
}
