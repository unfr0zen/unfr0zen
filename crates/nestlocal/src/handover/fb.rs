//! fb0, driven as Doom's platform layer drives it
//! (platform/common/doom/doomgeneric_nest.c): the gen 3's 480×640 panel is
//! mounted on its side with the round face in its top 480×480, the gen 2's
//! is the face itself, square and upright. A frame is drawn on the hidden
//! page and panned to, or straight onto the only one. The page shown before
//! and the console's mode are put back when the framebuffer is restored.
//!
//! The panel is nlclient's to light: while the screen is owned it lights it
//! every 2 s, on its own wake timer (custom_screen.c, keeper and
//! custom_flip), and so puts it to sleep again after. Only a panel it leaves
//! dark is lit here (unblanked, and each backlight at 0 raised), and those
//! backlights are put back to 0 on restore, for nlclient, which thinks the
//! panel asleep, to find it so. The blank state is not put back: the kernel
//! does not report it, and a panel blanked that nlclient wakes through its
//! backlight alone would stay dark.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{ioctl_int, ioctl_ptr};

const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOPUT_VSCREENINFO: u32 = 0x4601;
const FBIOGET_FSCREENINFO: u32 = 0x4602;
const FBIOPAN_DISPLAY: u32 = 0x4606;
const FBIOBLANK: u32 = 0x4611;
const FB_BLANK_UNBLANK: libc::c_int = 0;
const KDSETMODE: u32 = 0x4b3a;
const KDGETMODE: u32 = 0x4b3b;
const KD_GRAPHICS: libc::c_int = 1;
const BACKLIGHTS: &str = "/sys/class/backlight";
/// How long nlclient has to light the panel before it is lit here: it does
/// every 2 s while the screen is owned.
const LIGHT_AFTER: Duration = Duration::from_secs(3);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// The kernel's fb_var_screeninfo.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    rest: [u32; 20],
}
const _: () = assert!(std::mem::size_of::<VarScreeninfo>() == 160);

/// The kernel's fb_fix_screeninfo.
#[repr(C)]
#[derive(Default)]
struct FixScreeninfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    kind_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

/// The panel's layout in memory.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub xres: usize,
    pub yres: usize,
    /// Bytes from one row to the next.
    pub stride: usize,
    pub bytes_pp: usize,
    /// Each colour's (offset, length) in a pixel.
    pub red: (u32, u32),
    pub green: (u32, u32),
    pub blue: (u32, u32),
    /// One, or two the panel pans between.
    pub pages: usize,
}

impl Geometry {
    fn gen3(&self) -> bool {
        self.xres == 480 && self.yres == 640
    }

    fn face(&self) -> usize {
        if self.gen3() { 480 } else { self.xres }
    }

    /// Whether the panel takes 0xRRGGBB words as they are.
    fn xrgb(&self) -> bool {
        self.bytes_pp == 4 && self.red == (16, 8) && self.green == (8, 8) && self.blue == (0, 8)
    }

    /// A 0xRRGGBB pixel as the panel lays it out.
    fn pixel(&self, rgb: u32) -> u32 {
        if self.xrgb() {
            return rgb;
        }
        let channel = |value: u32, (offset, length): (u32, u32)| (value & 0xff) >> 8u32.saturating_sub(length) << offset;
        channel(rgb >> 16, self.red) | channel(rgb >> 8, self.green) | channel(rgb, self.blue)
    }
}

enum Memory {
    /// fb0 mapped: its address and length.
    Mapped(*mut u8, usize),
    #[cfg(test)]
    Owned(Vec<u8>),
}

impl Memory {
    fn bytes(&mut self) -> &mut [u8] {
        match self {
            // SAFETY: the mapping is len bytes long, writable, and lives
            // until Memory drops.
            Memory::Mapped(address, len) => unsafe { std::slice::from_raw_parts_mut(*address, *len) },
            #[cfg(test)]
            Memory::Owned(bytes) => bytes,
        }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        match *self {
            // SAFETY: address and len are those mmap returned, and nothing
            // borrows the mapping past Memory.
            Memory::Mapped(address, len) => unsafe { libc::munmap(address.cast(), len); },
            #[cfg(test)]
            Memory::Owned(_) => {}
        }
    }
}

/// fb0 as it was before it was taken, to be put back.
struct Device {
    file: File,
    var: VarScreeninfo,
    /// The page shown, and its y offset.
    before: Option<(Vec<u8>, u32)>,
    /// tty0 and its mode.
    tty: Option<(File, libc::c_int)>,
}

pub struct Framebuffer {
    geometry: Geometry,
    memory: Memory,
    /// The page shown.
    page: usize,
    device: Option<Device>,
    lights: Lights,
    /// When the panel is lit here if nlclient left it dark; none once looked at.
    light_at: Option<Instant>,
}

impl Framebuffer {
    /// Opens fb0 as Doom does: two pages where the panel can pan between
    /// them, the page shown saved and the rest cleared, and tty0's text kept
    /// off it. The panel is lit LIGHT_AFTER on, if nlclient has not lit it.
    pub fn open() -> std::io::Result<Framebuffer> {
        let file = std::fs::OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let mut var = VarScreeninfo::default();
        ioctl_ptr(&file, FBIOGET_VSCREENINFO, &mut var)?;
        if !(var.xres == 480 && var.yres == 640) && var.xres != var.yres {
            return Err(std::io::Error::other(format!("fb0 is {}x{}, neither the gen 3's 480x640 nor a round face's square",
                                                     var.xres, var.yres)));
        }
        if ![16, 24, 32].contains(&var.bits_per_pixel) {
            return Err(std::io::Error::other(format!("fb0 is at {} bpp", var.bits_per_pixel)));
        }
        if var.yres_virtual < 2 * var.yres {
            let mut two = var;
            two.yres_virtual = 2 * var.yres;
            two.yoffset = 0;
            if ioctl_ptr(&file, FBIOPUT_VSCREENINFO, &mut two).is_ok() {
                ioctl_ptr(&file, FBIOGET_VSCREENINFO, &mut var)?;
            }
        }
        let mut fix = FixScreeninfo::default();
        ioctl_ptr(&file, FBIOGET_FSCREENINFO, &mut fix)?;
        let geometry = Geometry {
            xres: var.xres as usize,
            yres: var.yres as usize,
            stride: fix.line_length as usize,
            bytes_pp: var.bits_per_pixel as usize / 8,
            red: (var.red.offset, var.red.length),
            green: (var.green.offset, var.green.length),
            blue: (var.blue.offset, var.blue.length),
            pages: if var.yres_virtual >= 2 * var.yres { 2 } else { 1 },
        };
        let len = geometry.stride * var.yres_virtual as usize;
        // SAFETY: a shared mapping of fb0's len bytes; Memory unmaps it.
        let address = unsafe {
            libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0)
        };
        if address == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let mut memory = Memory::Mapped(address.cast(), len);
        let page_len = geometry.yres * geometry.stride;
        let shown = var.yoffset as usize * geometry.stride;
        let before = (var.yoffset + var.yres <= var.yres_virtual).then(|| (memory.bytes()[shown..shown + page_len].to_vec(), var.yoffset));
        memory.bytes().fill(0);
        let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty0").ok().and_then(|tty| {
            let mut mode: libc::c_int = 0;
            ioctl_ptr(&tty, KDGETMODE, &mut mode).ok()?;
            ioctl_int(&tty, KDSETMODE, KD_GRAPHICS).ok()?;
            Some((tty, mode))
        });
        let page = if geometry.pages == 2 && var.yoffset >= var.yres { 1 } else { 0 };
        Ok(Framebuffer {
            geometry, memory, page, device: Some(Device { file, var, before, tty }),
            lights: Lights::new(BACKLIGHTS.into()), light_at: Some(Instant::now() + LIGHT_AFTER),
        })
    }

    /// A framebuffer of geometry in memory alone, with the backlights in
    /// lights.
    #[cfg(test)]
    pub fn over(geometry: Geometry, lights: PathBuf) -> Framebuffer {
        let memory = Memory::Owned(vec![0; geometry.stride * geometry.yres * geometry.pages]);
        Framebuffer {
            geometry, memory, page: 0, device: None, lights: Lights::new(lights),
            light_at: Some(Instant::now() + LIGHT_AFTER),
        }
    }

    /// The upright face's side: 480 on the gen 3, the panel's on a square one.
    pub fn face(&self) -> usize {
        self.geometry.face()
    }

    /// Shows rgb, the upright face×face picture (0xRRGGBB, row by row), as
    /// the panel is mounted: on the gen 3 the face's (u, v) is the panel's
    /// (479 - v, u). The rest of the panel is black.
    pub fn put(&mut self, rgb: &[u32]) {
        self.keep_lit(Instant::now());
        let g = self.geometry;
        let face = g.face();
        let back = if g.pages == 2 { 1 - self.page } else { 0 };
        let page_len = g.yres * g.stride;
        let page = &mut self.memory.bytes()[back * page_len..(back + 1) * page_len];
        page.fill(0);
        for y in 0..face {
            let row = &mut page[y * g.stride..];
            for x in 0..face {
                let (u, v) = if g.gen3() { (y, face - 1 - x) } else { (x, y) };
                let value = g.pixel(rgb[v * face + u]).to_le_bytes();
                row[x * g.bytes_pp..(x + 1) * g.bytes_pp].copy_from_slice(&value[..g.bytes_pp]);
            }
        }
        if g.pages == 2 {
            match &mut self.device {
                Some(device) => {
                    device.var.yoffset = (back * g.yres) as u32;
                    if ioctl_ptr(&device.file, FBIOPAN_DISPLAY, &mut device.var).is_ok() {
                        self.page = back;
                    }
                }
                None => self.page = back,
            }
        }
    }

    /// The page shown.
    #[cfg(test)]
    pub fn visible(&mut self) -> &[u8] {
        let page_len = self.geometry.yres * self.geometry.stride;
        let start = self.page * page_len;
        &self.memory.bytes()[start..start + page_len]
    }

    /// Lights the panel once LIGHT_AFTER has passed, if a backlight is still
    /// at 0: nlclient left it dark.
    pub(super) fn keep_lit(&mut self, now: Instant) {
        if self.light_at.is_none_or(|at| now < at) {
            return;
        }
        self.light_at = None;
        if !self.lights.dark() {
            return;
        }
        if let Some(device) = &self.device {
            let _ = ioctl_int(&device.file, FBIOBLANK, FB_BLANK_UNBLANK);
        }
        self.lights.raise();
    }

    /// Puts back the console's mode, the backlights lit here, and the page
    /// shown before, and pans to it; once.
    pub fn restore(&mut self) {
        self.light_at = None;
        self.lights.restore();
        let Some(device) = &mut self.device else { return };
        if let Some((tty, mode)) = device.tty.take() {
            let _ = ioctl_int(&tty, KDSETMODE, mode);
        }
        if let Some((before, y)) = device.before.take() {
            let start = y as usize * self.geometry.stride;
            self.memory.bytes()[start..start + before.len()].copy_from_slice(&before);
            device.var.yoffset = y;
            let _ = ioctl_ptr(&device.file, FBIOPAN_DISPLAY, &mut device.var);
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        self.restore();
    }
}

/// The backlights under dir (/sys/class/backlight), and those raised here
/// from 0 with the brightness written.
struct Lights {
    dir: PathBuf,
    raised: Vec<(PathBuf, u32)>,
}

fn read_value(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_value(path: &Path, value: u32) {
    if let Err(e) = std::fs::write(path, format!("{value}\n")) {
        crate::logf!("screen: {}: {e}", path.display());
    }
}

impl Lights {
    fn new(dir: PathBuf) -> Lights {
        Lights { dir, raised: Vec::new() }
    }

    /// Each backlight's directory.
    fn each(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        entries.flatten().map(|entry| entry.path()).collect()
    }

    /// Whether a backlight is at 0.
    fn dark(&self) -> bool {
        self.each().iter().any(|light| read_value(&light.join("brightness")) == Some(0))
    }

    /// Each backlight at 0 up to its max.
    fn raise(&mut self) {
        for light in self.each() {
            let brightness = light.join("brightness");
            if read_value(&brightness) != Some(0) {
                continue;
            }
            let max = read_value(&light.join("max_brightness")).filter(|&max| max > 0).unwrap_or(1);
            write_value(&brightness, max);
            self.raised.push((brightness, max));
        }
    }

    /// Each backlight raised back to 0, unless nlclient set it since.
    fn restore(&mut self) {
        for (brightness, raised) in self.raised.drain(..) {
            if read_value(&brightness) == Some(raised) {
                write_value(&brightness, 0);
            }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn panel(xres: usize, yres: usize, bytes_pp: usize, red: (u32, u32), green: (u32, u32), blue: (u32, u32)) -> Framebuffer {
        Framebuffer::over(Geometry { xres, yres, stride: xres * bytes_pp, bytes_pp, red, green, blue, pages: 2 }, PathBuf::new())
    }

    /// A backlights directory: each light's brightness, its max 255.
    pub(in crate::handover) fn backlights(name: &str, lights: &[(&str, u32)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-backlights-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (light, brightness) in lights {
            std::fs::create_dir_all(dir.join(light)).unwrap();
            std::fs::write(dir.join(light).join("brightness"), format!("{brightness}\n")).unwrap();
            std::fs::write(dir.join(light).join("max_brightness"), "255\n").unwrap();
        }
        dir
    }

    fn brightness(dir: &Path, light: &str) -> u32 {
        read_value(&dir.join(light).join("brightness")).unwrap()
    }

    #[test]
    fn a_panel_nlclient_leaves_dark_is_lit_late_and_put_back_dark() {
        let dir = backlights("dark", &[("panel", 0), ("other", 100)]);
        let geometry = Geometry { xres: 320, yres: 320, stride: 640, bytes_pp: 2, red: (11, 5), green: (5, 6), blue: (0, 5), pages: 2 };
        let mut fb = Framebuffer::over(geometry, dir.clone());
        fb.keep_lit(Instant::now());
        assert_eq!(brightness(&dir, "panel"), 0, "nlclient has LIGHT_AFTER to light it");
        fb.keep_lit(Instant::now() + LIGHT_AFTER);
        assert_eq!((brightness(&dir, "panel"), brightness(&dir, "other")), (255, 100));
        fb.restore();
        assert_eq!((brightness(&dir, "panel"), brightness(&dir, "other")), (0, 100));
        // One nlclient lit since is left to it.
        let mut fb = Framebuffer::over(geometry, dir.clone());
        fb.keep_lit(Instant::now() + LIGHT_AFTER);
        std::fs::write(dir.join("panel/brightness"), "180\n").unwrap();
        drop(fb);
        assert_eq!(brightness(&dir, "panel"), 180);
    }

    #[test]
    fn a_panel_nlclient_lit_is_left_alone() {
        let dir = backlights("lit", &[("panel", 0)]);
        let geometry = Geometry { xres: 320, yres: 320, stride: 640, bytes_pp: 2, red: (11, 5), green: (5, 6), blue: (0, 5), pages: 2 };
        let mut fb = Framebuffer::over(geometry, dir.clone());
        std::fs::write(dir.join("panel/brightness"), "200\n").unwrap();
        fb.keep_lit(Instant::now() + LIGHT_AFTER);
        std::fs::write(dir.join("panel/brightness"), "0\n").unwrap();
        fb.keep_lit(Instant::now() + LIGHT_AFTER * 2);
        drop(fb);
        assert_eq!(brightness(&dir, "panel"), 0, "nlclient's own sleep, not raised here");
    }

    #[test]
    fn the_gen3_panel_is_drawn_rotated() {
        let mut fb = panel(480, 640, 4, (16, 8), (8, 8), (0, 8));
        assert_eq!(fb.face(), 480);
        let mut face = vec![0u32; 480 * 480];
        face[10] = 0x123456;
        fb.put(&face);
        let shown = fb.visible();
        let at = 10 * 480 * 4 + 479 * 4;
        assert_eq!(&shown[at..at + 4], &0x123456u32.to_le_bytes());
        let lit = shown.iter().filter(|&&b| b != 0).count();
        assert_eq!(lit, 3, "only the one pixel is drawn");
    }

    #[test]
    fn sixteen_bit_panels_pack_rgb565() {
        let mut fb = panel(320, 320, 2, (11, 5), (5, 6), (0, 5));
        assert_eq!(fb.face(), 320);
        let mut face = vec![0u32; 320 * 320];
        face[2 * 320 + 3] = 0xff8040;
        fb.put(&face);
        let shown = fb.visible();
        let at = 2 * 320 * 2 + 3 * 2;
        assert_eq!(&shown[at..at + 2], &[0x08, 0xfc]);
        assert_eq!(shown.iter().filter(|&&b| b != 0).count(), 2);
    }
}
