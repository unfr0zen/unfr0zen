//! The terminal front end: the logo, and a [`Progress`] that renders stages as
//! cyan lines with a braille activity mark and a checkmark or red cross.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use unfr0zen_host::Progress;

use unfr0zen_host::LOGO;
const CYAN: &str = "\x1b[38;2;168;212;236m"; // the logo's @will-tm blue (#a8d4ec)
const RED: &str = "\x1b[38;2;255;65;65m";
const RESET: &str = "\x1b[0m";
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn terminal() -> bool {
    std::io::stdout().is_terminal()
}

fn colored(value: &str, color: &str) -> String {
    if terminal() { format!("{color}{value}{RESET}") } else { value.to_string() }
}

/// Lets a Windows console show the colours.
pub fn enable_colors() {
    #[cfg(windows)]
    let _ = enable_ansi_support::enable_ansi_support();
}

fn line(message: &str, marker: char) {
    println!("{}", colored(&format!("{marker} {message}"), CYAN));
}

pub fn done(message: &str) {
    line(message, '✓');
}

pub fn waiting(message: &str) {
    line(message, '⠿');
}

pub fn error(message: &str) {
    let value = format!("✗ {message}");
    if std::io::stderr().is_terminal() {
        eprintln!("{RED}{value}{RESET}");
    } else {
        eprintln!("{value}");
    }
}

/// The logo with a white-to-cyan gradient.
pub fn logo() {
    let mut lines: Vec<&str> = LOGO.lines().collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let last = lines.len().saturating_sub(1).max(1) as f32;
    for (index, text) in lines.iter().enumerate() {
        if terminal() {
            // The byline is lighter than the dark bottom of the gradient; the
            // art beside it keeps its row's colour.
            let (art, byline) = text.find("by @will-tm").map_or((*text, ""), |at| text.split_at(at));
            let [r, g, b] = unfr0zen_host::logo_color(index as f32 / last);
            println!("\x1b[38;2;{r};{g};{b}m{art}{CYAN}{byline}{RESET}");
        } else {
            println!("{text}");
        }
    }
    println!();
}

/// A running stage's byte progress, as the flow last reported it.
struct Transfer {
    done: u64,
    next: u64,
    total: u64,
    /// When `next` got under way.
    started: Instant,
    /// Bytes landed and the seconds they took, for the rate.
    measured: (u64, f64),
}

impl Transfer {
    /// Where the bar is: between reports it moves at the rate measured so far
    /// and stops just short of the end of the bytes under way.
    fn position(&self) -> u64 {
        let (bytes, secs) = self.measured;
        if secs == 0.0 {
            return self.done;
        }
        let guess = (bytes as f64 / secs * self.started.elapsed().as_secs_f64()) as u64;
        self.done + guess.min(self.next * 99 / 100)
    }
}

/// A running stage's line: a bar for a transfer, else its elapsed time.
fn running(label: &str, secs: u64, transfer: Option<&Transfer>) -> String {
    const WIDTH: u64 = 30;
    let Some(transfer) = transfer else {
        return format!("{label} ({secs}s)");
    };
    let (position, total) = (transfer.position(), transfer.total.max(1));
    let filled = (WIDTH * position / total) as usize;
    format!("{label} [{}{}] {:3}% {}/{} MiB {}:{:02}", "█".repeat(filled), "░".repeat(WIDTH as usize - filled),
            100 * position / total, position >> 20, total >> 20, secs / 60, secs % 60)
}

/// Renders the flow's progress on the terminal: a running stage animates with
/// its elapsed time or its bar, a line every five seconds when output is
/// redirected.
#[derive(Default)]
pub struct TerminalProgress {
    heartbeat: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
    transfer: Arc<Mutex<Option<Transfer>>>,
}

impl Progress for TerminalProgress {
    fn begin(&mut self, label: &str) {
        let tty = terminal();
        if tty {
            print!("{}", colored(&format!("{} {label} (0s)", FRAMES[0]), CYAN));
            let _ = std::io::stdout().flush();
        } else {
            line(label, FRAMES[0]);
        }
        *self.transfer.lock().unwrap() = None;
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (stop, label, started) = (stop.clone(), label.to_string(), Instant::now());
            let transfer = self.transfer.clone();
            thread::spawn(move || {
                let (mut tick, mut last_line) = (0usize, 0u64);
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(200));
                    let secs = started.elapsed().as_secs();
                    let text = running(&label, secs, transfer.lock().unwrap().as_ref());
                    if tty {
                        print!("\r\x1b[2K{}", colored(&format!("{} {text}", FRAMES[tick % FRAMES.len()]), CYAN));
                        let _ = std::io::stdout().flush();
                        tick += 1;
                    } else if secs >= last_line + 5 {
                        line(&text, FRAMES[tick % FRAMES.len()]);
                        tick += 1;
                        last_line += 5;
                    }
                }
            })
        };
        self.heartbeat = Some((stop, worker));
    }

    fn end(&mut self, label: &str, elapsed: Duration, ok: bool) {
        if let Some((stop, worker)) = self.heartbeat.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = worker.join();
        }
        if terminal() {
            print!("\r\x1b[2K");
        }
        let secs = elapsed.as_secs_f64();
        let transfer = self.transfer.lock().unwrap().take();
        match (ok, transfer) {
            (true, Some(t)) => line(&format!("{label} {} MiB ({secs:.1}s)", t.total >> 20), '✓'),
            (true, None) => line(&format!("{label} ({secs:.1}s)"), '✓'),
            (false, Some(t)) => error(&format!("{label} failed at {}/{} MiB ({secs:.1}s)", t.done >> 20, t.total >> 20)),
            (false, None) => error(&format!("{label} failed ({secs:.1}s)")),
        }
    }

    fn transfer(&mut self, done: u64, next: u64, total: u64) {
        let mut slot = self.transfer.lock().unwrap();
        let mut measured = (0, 0.0);
        if let Some(last) = slot.as_ref() {
            measured = last.measured;
            if last.next > 0 && done >= last.done + last.next {
                measured.0 += last.next;
                measured.1 += last.started.elapsed().as_secs_f64();
            }
        }
        *slot = Some(Transfer { done, next, total, started: Instant::now(), measured });
    }

    fn status(&mut self, message: &str) {
        line(message, '✓');
    }

    fn waiting(&mut self, message: &str) {
        line(message, '⠿');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn a_transfer_draws_a_bar() {
        let transfer = Transfer { done: 160 * MIB, next: 64 * MIB, total: 512 * MIB, started: Instant::now(), measured: (0, 0.0) };
        assert_eq!(running("Back up NAND", 107, Some(&transfer)),
                   format!("Back up NAND [{}{}]  31% 160/512 MiB 1:47", "█".repeat(9), "░".repeat(21)));
        assert_eq!(running("Connect to recovery console", 3, None), "Connect to recovery console (3s)");
    }

    #[test]
    fn the_bar_stops_short_of_bytes_not_landed() {
        // Under way for a second: read at once, it may not have moved at all.
        let started = Instant::now() - Duration::from_secs(1);
        let transfer = Transfer { done: 0, next: 64 * MIB, total: 512 * MIB, started, measured: (64 * MIB, 1e-9) };
        assert_eq!(transfer.position(), 64 * MIB * 99 / 100);
    }

    #[test]
    fn only_bytes_moved_count_for_the_rate() {
        let mut progress = TerminalProgress::default();
        // Chunks 1 and 4 were kept from an earlier run; 2 and 3 moved.
        progress.transfer(64 * MIB, 64 * MIB, 512 * MIB);
        progress.transfer(128 * MIB, 64 * MIB, 512 * MIB);
        progress.transfer(256 * MIB, 64 * MIB, 512 * MIB);
        let measured = progress.transfer.lock().unwrap().as_ref().unwrap().measured;
        assert_eq!(measured.0, 128 * MIB);
    }
}
