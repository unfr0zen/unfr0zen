//! The thermostat's screen, taken from nlclient through its hand-over
//! (platform/common/nlclient/README.md, "Screen hand-over"): while the
//! process named in screen.owner lives, nlclient keeps controlling the
//! heating but shows nothing, and the owner draws on fb0 and grabs the ring
//! and the button, as Doom does (platform/common/doom/doomgeneric_nest.c).
//! nestlocal outlives what it shows, so it removes screen.owner itself when
//! done; were it to die first, the live check hands the screen back.

pub mod fb;
mod font;
pub mod page;

use std::collections::VecDeque;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::logf;

/// Where the screen's owner leaves its pid.
const OWNER: &str = "screen.owner";
/// Where nlclient leaves its pid first, once its hand-over runs.
const KEEPER: &str = "keeper.pid";

/// evdev's codes, as Doom reads them.
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
/// The gen 2's ring, which counts the other way to the dial.
const REL_X: u16 = 0x00;
/// The gen 3's dial.
const REL_DIAL: u16 = 0x07;
/// The gen 3's button (KEY_ENTER) and the gen 2's press (KEY_POWER).
const BUTTON: u16 = 28;
const POWER: u16 = 116;
const EVIOCGRAB: u32 = 0x40044590;
/// Counts a step takes: two detents, as the thermostat's menus.
const STEP: i32 = 240;
/// An evdev event as the kernel lays it out: its time, two longs, then
/// type, code and value.
const TIME: usize = 2 * std::mem::size_of::<libc::c_long>();
const EVENT: usize = TIME + 8;

/// Why the screen cannot be taken.
#[derive(Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Another process owns it.
    InUse,
    /// nlclient has no hand-over running, so taking the screen would fight it.
    NoHandover,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unavailable::InUse => "another process owns the screen",
            Unavailable::NoHandover => "nlclient hands no screen over",
        })
    }
}

/// What the ring or dial and the button did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Steps turned; positive is down (clockwise).
    Turn(i32),
    /// The button released after a down.
    Press,
}

/// A live process's name, from /proc; none for a pid that is gone.
type Comm = fn(u32) -> Option<String>;

fn proc_comm(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|comm| comm.trim_end().to_string())
}

/// The first number in path, as nlclient and owners leave their pids.
fn first_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.split_whitespace().next()?.parse().ok()
}

/// Whether the screen may be taken: keeper.pid names the running nlclient,
/// and screen.owner is missing, names no live process, or names this one.
pub fn available(dir: &Path) -> Result<(), Unavailable> {
    available_with(dir, proc_comm)
}

fn available_with(dir: &Path, comm: Comm) -> Result<(), Unavailable> {
    if first_pid(&dir.join(KEEPER)).and_then(comm).as_deref() != Some("nlclient") {
        return Err(Unavailable::NoHandover);
    }
    match first_pid(&dir.join(OWNER)) {
        Some(pid) if pid != std::process::id() && comm(pid).is_some() => Err(Unavailable::InUse),
        _ => Ok(()),
    }
}

/// Removes, as nestlocal starts, a screen.owner an earlier nestlocal left:
/// one naming no live process, or one whose pid a nestlocal now runs under
/// (this one, or another reusing it), which would keep nlclient's display
/// dark for good.
pub fn clear_stale(dir: &Path) {
    clear_stale_with(dir, proc_comm)
}

fn clear_stale_with(dir: &Path, comm: Comm) {
    let path = dir.join(OWNER);
    if !path.exists() {
        return;
    }
    if first_pid(&path).and_then(comm).is_none_or(|name| name == "nestlocal") {
        match std::fs::remove_file(&path) {
            Ok(()) => logf!("screen: removed a stale {OWNER}"),
            Err(e) => logf!("screen: {}: {e}", path.display()),
        }
    }
}

/// The screen while this process owns it: given back when dropped.
pub struct Taken {
    owner: PathBuf,
    fb: fb::Framebuffer,
    inputs: Vec<File>,
    controls: Controls,
    pending: VecDeque<Input>,
}

/// Opens the panel and grabs the inputs.
type Opener = fn() -> std::io::Result<(fb::Framebuffer, Vec<File>)>;

/// Takes the screen: screen.owner names this process, fb0 is drawn on and
/// every input device is grabbed. Check available first.
pub fn take(dir: &Path) -> std::io::Result<Taken> {
    take_with(dir, devices)
}

fn take_with(dir: &Path, open: Opener) -> std::io::Result<Taken> {
    let owner = dir.join(OWNER);
    std::fs::write(&owner, format!("{}\n", std::process::id()))?;
    match open() {
        Ok((fb, inputs)) => Ok(Taken { owner, fb, inputs, controls: Controls::default(), pending: VecDeque::new() }),
        Err(e) => {
            release(&owner);
            Err(e)
        }
    }
}

fn devices() -> std::io::Result<(fb::Framebuffer, Vec<File>)> {
    let fb = fb::Framebuffer::open()?;
    Ok((fb, grab_inputs()))
}

/// Every /dev/input/event*, opened without blocking and grabbed, so that
/// nlclient sees none of their events.
fn grab_inputs() -> Vec<File> {
    let Ok(entries) = std::fs::read_dir("/dev/input") else { return Vec::new() };
    entries.flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with("event")).filter_map(|entry| {
        let input = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(entry.path()).ok()?;
        if let Err(e) = ioctl_int(&input, EVIOCGRAB, 1) {
            logf!("screen: {}: grab: {e}", entry.path().display());
        }
        Some(input)
    }).collect()
}

/// Removes owner if it still names this process.
fn release(owner: &Path) {
    if first_pid(owner) == Some(std::process::id()) {
        let _ = std::fs::remove_file(owner);
    }
}

impl Taken {
    #[cfg(test)]
    pub fn draw(&mut self, page: &page::Page) {
        let pixels = self.render(page);
        self.put(&pixels);
    }

    /// page's pixels for this panel, as put shows them.
    pub fn render(&self, page: &page::Page) -> Vec<u32> {
        page::render(page, self.fb.face())
    }

    /// Shows pixels render made.
    pub fn put(&mut self, pixels: &[u32]) {
        self.fb.put(pixels);
    }

    /// The next input, waiting up to wait for one.
    pub fn input(&mut self, wait: Duration) -> Option<Input> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(input) = self.pending.pop_front() {
                return Some(input);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let mut fds: Vec<libc::pollfd> = self.inputs.iter()
                .map(|input| libc::pollfd { fd: input.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
            // SAFETY: fds holds fds.len() pollfds.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, left.as_millis().min(i32::MAX as u128) as _) };
            if ready <= 0 {
                return None;
            }
            // A device that went away would wake every poll: it is let go.
            let gone: Vec<bool> = fds.iter().map(|fd| fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0).collect();
            for (input, fd) in self.inputs.iter_mut().zip(&fds) {
                if fd.revents & libc::POLLIN != 0 {
                    read_events(input, &mut self.controls, &mut self.pending);
                }
            }
            let mut gone = gone.into_iter();
            self.inputs.retain(|_| !gone.next().unwrap_or(false));
            if self.pending.is_empty() && Instant::now() >= deadline {
                return None;
            }
        }
    }
}

/// Feeds what input has to read to controls.
fn read_events(mut input: &File, controls: &mut Controls, out: &mut VecDeque<Input>) {
    use std::io::Read;
    let mut buf = [0u8; EVENT * 16];
    while let Ok(n) = input.read(&mut buf) {
        if n == 0 {
            return;
        }
        for event in buf[..n].as_chunks::<EVENT>().0 {
            let kind = u16::from_ne_bytes([event[TIME], event[TIME + 1]]);
            let code = u16::from_ne_bytes([event[TIME + 2], event[TIME + 3]]);
            let value = i32::from_ne_bytes([event[TIME + 4], event[TIME + 5], event[TIME + 6], event[TIME + 7]]);
            controls.feed(kind, code, value, out);
        }
    }
}

impl Drop for Taken {
    /// Puts back what nlclient showed, lets go of the inputs, then hands the
    /// screen back.
    fn drop(&mut self) {
        self.fb.restore();
        for input in &self.inputs {
            let _ = ioctl_int(input, EVIOCGRAB, 0);
        }
        release(&self.owner);
    }
}

/// Turns evdev events into inputs: counts into steps, and a key's release
/// after its down into a press.
#[derive(Default)]
struct Controls {
    /// Counts turned toward the next step; positive is down.
    turned: i32,
    down: bool,
}

impl Controls {
    fn feed(&mut self, kind: u16, code: u16, value: i32, out: &mut VecDeque<Input>) {
        match (kind, code) {
            // The dial's counts run negative clockwise, the ring's positive;
            // clockwise goes down.
            (EV_REL, REL_DIAL) => self.turn(value.saturating_neg(), out),
            (EV_REL, REL_X) => self.turn(value, out),
            // 2 is a key's repeat.
            (EV_KEY, BUTTON | POWER) if value != 2 => {
                if self.down && value == 0 {
                    out.push_back(Input::Press);
                }
                self.down = value != 0;
            }
            _ => {}
        }
    }

    fn turn(&mut self, counts: i32, out: &mut VecDeque<Input>) {
        self.turned = self.turned.saturating_add(counts);
        let steps = self.turned / STEP;
        if steps != 0 {
            self.turned -= steps * STEP;
            out.push_back(Input::Turn(steps));
        }
    }
}

/// An ioctl whose argument is a pointer to what it reads or fills.
fn ioctl_ptr<T>(fd: &impl AsRawFd, request: u32, arg: &mut T) -> std::io::Result<()> {
    // SAFETY: arg is a live T, which is what each request used here takes.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg as *mut T) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// An ioctl whose argument is an integer.
fn ioctl_int(fd: &impl AsRawFd, request: u32, arg: libc::c_int) -> std::io::Result<()> {
    // SAFETY: the requests used here read their integer argument alone.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    const NLCLIENT: u32 = 100;
    const EARLIER_NESTLOCAL: u32 = 200;
    const DOOM: u32 = 300;
    const DEAD: u32 = 999;

    fn comm(pid: u32) -> Option<String> {
        match pid {
            NLCLIENT => Some("nlclient".into()),
            EARLIER_NESTLOCAL => Some("nestlocal".into()),
            DOOM => Some("doom".into()),
            _ => None,
        }
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-screen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn no_keeper_means_no_handover() {
        let dir = dir("nokeeper");
        assert_eq!(available_with(&dir, comm), Err(Unavailable::NoHandover));
        for keeper in [format!("{DEAD} 0x1234\n"), format!("{DOOM}\n"), "nlclient\n".into()] {
            write(&dir, KEEPER, &keeper);
            assert_eq!(available_with(&dir, comm), Err(Unavailable::NoHandover), "{keeper:?}");
        }
        write(&dir, KEEPER, &format!("{NLCLIENT} 0x1234\n"));
        assert_eq!(available_with(&dir, comm), Ok(()));
    }

    #[test]
    fn a_live_owner_means_in_use() {
        let dir = dir("inuse");
        write(&dir, KEEPER, &format!("{NLCLIENT}\n"));
        write(&dir, OWNER, &format!("{DOOM}\n"));
        assert_eq!(available_with(&dir, comm), Err(Unavailable::InUse));
        write(&dir, OWNER, &format!("{DEAD}\n"));
        assert_eq!(available_with(&dir, comm), Ok(()));
        write(&dir, OWNER, &format!("{}\n", std::process::id()));
        assert_eq!(available_with(&dir, comm), Ok(()));
    }

    #[test]
    fn a_stale_owner_from_an_earlier_nestlocal_is_cleared_at_start() {
        let dir = dir("stale");
        write(&dir, KEEPER, &format!("{NLCLIENT}\n"));
        for stale in [EARLIER_NESTLOCAL, DEAD] {
            write(&dir, OWNER, &format!("{stale}\n"));
            clear_stale_with(&dir, comm);
            assert!(!dir.join(OWNER).exists(), "{stale}");
            assert_eq!(available_with(&dir, comm), Ok(()));
        }
        // Doom's launcher owns it while Doom plays.
        write(&dir, OWNER, &format!("{DOOM}\n"));
        clear_stale_with(&dir, comm);
        assert!(dir.join(OWNER).exists());
        clear_stale_with(&dir, comm);
        std::fs::remove_file(dir.join(OWNER)).unwrap();
        clear_stale_with(&dir, comm);
    }

    fn stand_in() -> std::io::Result<(fb::Framebuffer, Vec<std::fs::File>)> {
        let geometry = fb::Geometry { xres: 320, yres: 320, stride: 320 * 4, bytes_pp: 4,
                                      red: (16, 8), green: (8, 8), blue: (0, 8), pages: 2 };
        Ok((fb::Framebuffer::over(geometry, lights_dir()), Vec::new()))
    }

    fn lights_dir() -> PathBuf {
        std::env::temp_dir().join(format!("nestlocal-backlights-taken-{}", std::process::id()))
    }

    #[test]
    fn taking_writes_our_pid_and_dropping_removes_it() {
        let dir = dir("take");
        let mut taken = take_with(&dir, stand_in).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(OWNER)).unwrap().trim(), std::process::id().to_string());
        taken.draw(&page::Page { lines: vec![("12345678".into(), page::Size::Large)], choices: Vec::new(), selected: 0 });
        assert_eq!(taken.input(std::time::Duration::ZERO), None);
        drop(taken);
        assert!(!dir.join(OWNER).exists());
        // An owner that took the screen since is left to it.
        let taken = take_with(&dir, stand_in).unwrap();
        write(&dir, OWNER, &format!("{DOOM}\n"));
        drop(taken);
        assert_eq!(std::fs::read_to_string(dir.join(OWNER)).unwrap(), format!("{DOOM}\n"));
    }

    #[test]
    fn handing_back_puts_a_backlight_lit_from_dark_back_to_dark() {
        let dir = dir("lights");
        let lights = fb::tests::backlights("taken", &[("panel", 0)]);
        assert_eq!(lights, lights_dir());
        let mut taken = take_with(&dir, stand_in).unwrap();
        taken.fb.keep_lit(Instant::now() + Duration::from_secs(60));
        assert_eq!(std::fs::read_to_string(lights.join("panel/brightness")).unwrap(), "255\n");
        drop(taken);
        assert_eq!(std::fs::read_to_string(lights.join("panel/brightness")).unwrap(), "0\n");
    }

    #[test]
    fn two_detents_move_one_step_and_a_release_presses() {
        let mut controls = Controls::default();
        let mut out = std::collections::VecDeque::new();
        controls.feed(EV_REL, REL_DIAL, -120, &mut out);
        assert!(out.is_empty());
        controls.feed(EV_REL, REL_DIAL, -120, &mut out);
        assert_eq!(out.pop_front(), Some(Input::Turn(1)), "the dial clockwise goes down");
        controls.feed(EV_REL, REL_X, -480, &mut out);
        assert_eq!(out.pop_front(), Some(Input::Turn(-2)), "the ring counts the other way");
        controls.feed(EV_KEY, BUTTON, 1, &mut out);
        controls.feed(EV_KEY, BUTTON, 2, &mut out);
        assert!(out.is_empty());
        controls.feed(EV_KEY, BUTTON, 0, &mut out);
        assert_eq!(out.pop_front(), Some(Input::Press));
        controls.feed(EV_KEY, POWER, 0, &mut out);
        assert!(out.is_empty(), "a release without a down is no press");
        controls.feed(EV_KEY, POWER, 1, &mut out);
        controls.feed(EV_KEY, POWER, 0, &mut out);
        assert_eq!(out.pop_front(), Some(Input::Press));
    }
}
