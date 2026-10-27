//! Gathers what the unfr0zen binary carries, so it needs nothing on the host:
//!
//! - the diamond3's recovery firmware scripts/build-firmware.sh makes in
//!   build/, checked against its pinned and recorded hashes;
//! - the j49's NAND flasher, dropbear, kernel and the x-loader its boot ROM
//!   takes over USB, built there too;
//! - Debian's static ARMv7 busybox for the initramfs (busybox-static
//!   1.37.0-6+b9 armhf, by SHA-256; the same .deb unfr0zen.py caches in
//!   .work/initramfs, fetched with curl when absent);
//! - nestlocal for --mqtt, the ARMv7 build scripts/build-firmware.sh makes in build/;
//! - Doom for --doom, built there too, and the shareware DOOM1.WAD v1.9 from
//!   Debian's doom-wad-shareware 1.9.fixed-5 (by SHA-256, cached the same way).

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const ROM_STAGE_SHA256: &str = "d60cc77f12d3bf217e800b9b3f576fa78cc2082836e48f8f6fc744a086e87146";
const BUSYBOX_DEB: &str = "busybox-static_1.37.0-6+b9_armhf.deb";
const BUSYBOX_URL: &str = "https://snapshot.debian.org/file/188a1b20b6150354ee95a5a6405d826e3e6fdaa5";
const BUSYBOX_SHA256: &str = "8b35210e08d559ecc0ddb660dd96f6f9694f69eb27f1dc608f5eeda56d94509e";
const DOOM_WAD_DEB: &str = "doom-wad-shareware_1.9.fixed-5_all.deb";
const DOOM_WAD_URL: &str = "https://snapshot.debian.org/file/732562ea15f0a6623611bdb739a03af72d8dbaf1";
const DOOM_WAD_DEB_SHA256: &str = "5802f176c0303e228095b5312def53de602781cf4c53e79842257484a0d9e938";
const DOOM_WAD_SHA256: &str = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771";

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn gzip(data: &[u8], path: &Path) {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    fs::write(path, encoder.finish().unwrap()).unwrap();
}

fn firmware(root: &Path, out: &Path) {
    let build = root.join("build");
    let read = |name: &str| {
        println!("cargo:rerun-if-changed={}", build.join(name).display());
        fs::read(build.join(name)).unwrap_or_else(|_| {
            panic!("build/{name} is missing: build the recovery firmware first (scripts/build-firmware.sh)")
        })
    };
    let rom_stage = read("rom-stage.imx");
    assert_eq!(sha256(&rom_stage), ROM_STAGE_SHA256, "build/rom-stage.imx is not the pinned ROM bootstrap");
    let recovery = read("u-boot-recovery.bin");
    let record = String::from_utf8(read("u-boot-recovery.json")).unwrap();
    let recorded: serde_json::Value = serde_json::from_str(&record).unwrap();
    assert_eq!(recorded["image_sha256"].as_str(), Some(sha256(&recovery).as_str()),
               "build/u-boot-recovery.bin does not match its build record");
    assert!(recovery.windows(25).any(|w| w == b"usb_recovery_acm_fastboot"),
            "build/u-boot-recovery.bin is not the ACM+fastboot recovery build");
    for (name, data) in [("rom-stage.imx", rom_stage), ("boot-wrapper.bin", read("boot-wrapper.bin")),
                         ("u-boot-recovery.bin", recovery)] {
        fs::write(out.join(name), data).unwrap();
    }
}

/// The member of an ar archive (a .deb) whose name starts with prefix.
fn ar_member<'a>(data: &'a [u8], prefix: &str) -> &'a [u8] {
    assert_eq!(&data[..8], b"!<arch>\n", "not an ar archive");
    let mut offset = 8;
    while offset + 60 <= data.len() {
        let name = String::from_utf8_lossy(&data[offset..offset + 16]).trim().trim_end_matches('/').to_string();
        let size: usize = String::from_utf8_lossy(&data[offset + 48..offset + 58]).trim().parse().unwrap();
        if name.starts_with(prefix) {
            return &data[offset + 60..offset + 60 + size];
        }
        offset += 60 + size + size % 2;
    }
    panic!("no {prefix} in the archive");
}

/// A pinned .deb: from .work/initramfs, else fetched with curl.
fn deb(root: &Path, out: &Path, name: &str, url: &str, sha: &str) -> Vec<u8> {
    pinned(&root.join(".work/initramfs"), out, name, url, sha)
}

/// A pinned download: from the cache directory, else fetched with curl.
fn pinned(cache: &Path, out: &Path, name: &str, url: &str, sha: &str) -> Vec<u8> {
    let path = [cache.join(name), out.join(name)].into_iter().find(|path| path.is_file())
        .unwrap_or_else(|| {
            // A read-only checkout keeps the download in OUT_DIR.
            let target = if fs::create_dir_all(cache).is_ok() { cache.join(name) } else { out.join(name) };
            // Per process: building several targets at once runs this concurrently.
            let partial = target.with_extension(format!("part.{}", std::process::id()));
            let status = Command::new("curl").args(["-fsSL", "-o"]).arg(&partial).arg(url).status()
                .unwrap_or_else(|_| panic!("curl is needed once to fetch {name}"));
            assert!(status.success(), "downloading {url} failed");
            fs::rename(&partial, &target).unwrap();
            target
        });
    let data = fs::read(&path).unwrap();
    assert_eq!(sha256(&data), sha, "{} is not the pinned {name}", path.display());
    data
}

/// A file of a .deb's data.tar.xz.
fn deb_file(deb: &[u8], path: &str) -> Vec<u8> {
    let mut tar = Vec::new();
    lzma_rs::xz_decompress(&mut &ar_member(deb, "data.tar")[..], &mut tar).expect("data.tar.xz");
    let mut archive = tar::Archive::new(&tar[..]);
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_str() == Some(path) {
            let mut file = Vec::new();
            entry.read_to_end(&mut file).unwrap();
            return file;
        }
    }
    panic!("no {path} in the package");
}

fn busybox(root: &Path, out: &Path) {
    let deb = deb(root, out, BUSYBOX_DEB, BUSYBOX_URL, BUSYBOX_SHA256);
    gzip(&deb_file(&deb, "./usr/bin/busybox"), &out.join("busybox.gz"));
}

fn doom(root: &Path, out: &Path) {
    let binary = root.join("build/doom-armv7");
    println!("cargo:rerun-if-changed={}", binary.display());
    let data = fs::read(&binary)
        .unwrap_or_else(|_| panic!("build/doom-armv7 is missing: build it first (scripts/build-firmware.sh)"));
    assert_eq!((&data[..4], data[18]), (&b"\x7fELF"[..], 40), "build/doom-armv7 is not an ARM ELF binary");
    gzip(&data, &out.join("doom.gz"));
    let deb = deb(root, out, DOOM_WAD_DEB, DOOM_WAD_URL, DOOM_WAD_DEB_SHA256);
    let wad = deb_file(&deb, "./usr/share/games/doom/doom1.wad");
    assert_eq!(sha256(&wad), DOOM_WAD_SHA256, "{DOOM_WAD_DEB} does not hold the v1.9 shareware DOOM1.WAD");
    gzip(&wad, &out.join("doom1.wad.gz"));
    fs::write(out.join("doom-logo.png"), doom_logo(&wad)).unwrap();
}

/// Doom's title graphic (the WAD's M_DOOM patch, in PLAYPAL's colours) as a
/// PNG, for the client's Doom page: the art stays in the WAD, never
/// committed.
fn doom_logo(wad: &[u8]) -> Vec<u8> {
    let u32_at = |d: &[u8], at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
    let lump = |name: &str| (0..u32_at(wad, 4)).map(|i| u32_at(wad, 8) + i * 16).find_map(|e| {
        let n = &wad[e + 8..e + 16];
        (n.split(|&b| b == 0).next() == Some(name.as_bytes())).then(|| &wad[u32_at(wad, e)..u32_at(wad, e) + u32_at(wad, e + 4)])
    }).unwrap_or_else(|| panic!("the WAD has no {name}"));
    let (palette, patch) = (lump("PLAYPAL"), lump("M_DOOM"));
    let (w, h) = (u16::from_le_bytes([patch[0], patch[1]]) as usize, u16::from_le_bytes([patch[2], patch[3]]) as usize);
    let mut rgba = vec![0u8; w * h * 4];
    for x in 0..w {
        let mut at = u32_at(patch, 8 + x * 4);
        while patch[at] != 0xff {
            let (top, n) = (patch[at] as usize, patch[at + 1] as usize);
            for i in 0..n {
                let c = patch[at + 3 + i] as usize * 3;
                let o = ((top + i) * w + x) * 4;
                rgba[o..o + 4].copy_from_slice(&[palette[c], palette[c + 1], palette[c + 2], 0xff]);
            }
            at += n + 4;
        }
    }
    let raw: Vec<u8> = rgba.chunks(w * 4).flat_map(|row| std::iter::once(0).chain(row.iter().copied())).collect();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&raw).unwrap();
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut c = (data.len() as u32).to_be_bytes().to_vec();
        c.extend(kind);
        c.extend(data);
        let mut crc = flate2::Crc::new();
        crc.update(&c[4..]);
        c.extend(crc.sum().to_be_bytes());
        c
    };
    let mut header = (w as u32).to_be_bytes().to_vec();
    header.extend((h as u32).to_be_bytes());
    header.extend([8, 6, 0, 0, 0]);
    [&b"\x89PNG\r\n\x1a\n"[..], &chunk(b"IHDR", &header), &chunk(b"IDAT", &z.finish().unwrap()), &chunk(b"IEND", &[])].concat()
}

/// client-ng as client-ng/build.sh stages it (build/client-ng), for
/// --client-ng: one gzipped stream of files, each its path's length (u32 LE),
/// path, mode and length, then its bytes.
fn client(root: &Path, out: &Path) {
    let stage = root.join("build/client-ng");
    println!("cargo:rerun-if-changed={}", stage.display());
    fn walk(dir: &Path, base: &Path, files: &mut Vec<(String, u32, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            println!("cargo:rerun-if-changed={}", path.display());
            if path.is_dir() {
                walk(&path, base, files);
            } else {
                let name = path.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                let mode = if name == "client-ng" { 0o755 } else { 0o644 };
                files.push((name, mode, fs::read(&path).unwrap()));
            }
        }
    }
    let binary = fs::read(stage.join("client-ng"))
        .unwrap_or_else(|_| panic!("build/client-ng is missing: build it first (scripts/build-firmware.sh)"));
    assert_eq!((&binary[..4], binary[18]), (&b"\x7fELF"[..], 40), "build/client-ng/client-ng is not an ARM ELF binary");
    let mut files = Vec::new();
    walk(&stage, &stage, &mut files);
    let mut data = Vec::new();
    for (name, mode, bytes) in files {
        data.extend((name.len() as u32).to_le_bytes());
        data.extend(name.as_bytes());
        data.extend(mode.to_le_bytes());
        data.extend((bytes.len() as u32).to_le_bytes());
        data.extend(bytes);
    }
    gzip(&data, &out.join("client.gz"));
}

fn j49(root: &Path, out: &Path) {
    let flasher = root.join("build/j49/nand-flasher.bin");
    println!("cargo:rerun-if-changed={}", flasher.display());
    let data = fs::read(&flasher)
        .unwrap_or_else(|_| panic!("build/j49/nand-flasher.bin is missing: build it first (scripts/build-firmware.sh)"));
    // Its entry: ldr sp, =... (ldr sp, [pc, #imm12]).
    let entry = u32::from_le_bytes(data[..4].try_into().unwrap());
    assert_eq!(entry & 0xffff_f000, 0xe59f_d000, "build/j49/nand-flasher.bin does not start at its entry");
    fs::write(out.join("nand-flasher.bin"), data).unwrap();
    // The kernel with a USB serial port needs Docker to build; without it the
    // app has none, and refuses --usb-shell on a j49.
    let kernel = root.join("build/j49/uImage");
    println!("cargo:rerun-if-changed={}", kernel.display());
    let kernel = fs::read(&kernel).unwrap_or_default();
    assert!(kernel.is_empty() || kernel[..4] == [0x27, 0x05, 0x19, 0x56], "build/j49/uImage is not a U-Boot image");
    fs::write(out.join("j49-uImage"), kernel).unwrap();
    let dropbear = root.join("build/j49/dropbearmulti");
    println!("cargo:rerun-if-changed={}", dropbear.display());
    let data = fs::read(&dropbear)
        .unwrap_or_else(|_| panic!("build/j49/dropbearmulti is missing: build it first (scripts/build-firmware.sh)"));
    assert_eq!((&data[..4], data[18]), (&b"\x7fELF"[..], 40), "build/j49/dropbearmulti is not an ARM ELF binary");
    gzip(&data, &out.join("dropbearmulti.gz"));
    // x-loader builds in Docker too; without it the app reaches no j49.
    let xloader = root.join("build/j49/x-load.bin");
    println!("cargo:rerun-if-changed={}", xloader.display());
    let xloader = fs::read(&xloader).unwrap_or_default();
    // Its reset vector: b reset.
    assert!(xloader.is_empty() || xloader[3] == 0xea, "build/j49/x-load.bin does not start with its vectors");
    fs::write(out.join("x-load.bin"), xloader).unwrap();
    let diamond = root.join("build/diamond/x-load.bin");
    println!("cargo:rerun-if-changed={}", diamond.display());
    let diamond = fs::read(&diamond).unwrap_or_default();
    assert!(diamond.is_empty() || diamond[3] == 0xea, "build/diamond/x-load.bin does not start with its vectors");
    fs::write(out.join("diamond-x-load.bin"), diamond).unwrap();
}

fn nestlocal(root: &Path, out: &Path) {
    let binary = root.join("build/nestlocal-armv7");
    println!("cargo:rerun-if-changed={}", binary.display());
    let data = fs::read(&binary)
        .unwrap_or_else(|_| panic!("build/nestlocal-armv7 is missing: build it first (scripts/build-firmware.sh)"));
    assert_eq!(&data[..4], b"\x7fELF", "nestlocal is not an ELF binary");
    assert_eq!(data[18], 40, "nestlocal is not an ARM binary");
    gzip(&data, &out.join("nestlocal.gz"));
}

fn custom_screen(root: &Path, out: &Path) {
    let object = root.join("build/custom-screen.o");
    println!("cargo:rerun-if-changed={}", object.display());
    let data = fs::read(&object)
        .unwrap_or_else(|_| panic!("build/custom-screen.o is missing: build it first (scripts/build-firmware.sh)"));
    assert_eq!(&data[..6], b"\x7fELF\x01\x01", "custom-screen.o is not a 32-bit little-endian ELF");
    assert_eq!((data[16], data[18]), (1, 40), "custom-screen.o is not an ARM relocatable object");
    fs::write(out.join("custom-screen.o"), data).unwrap();
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..").canonicalize().unwrap();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    firmware(&root, &out);
    j49(&root, &out);
    busybox(&root, &out);
    nestlocal(&root, &out);
    doom(&root, &out);
    client(&root, &out);
    custom_screen(&root, &out);
    // The gen3's radio image serves Thread and Zigbee alike, the gen2's
    // Thread image Thread alone; the Thread host tools beside them.
    for (dir, name, embedded) in [("build/radio", "radio.bin", "radio.bin"), ("build/radio", "radio.identity", "radio.identity"),
                                  ("build/radio-em357-thread", "radio.bin", "radio-em357.bin"),
                                  ("build/radio-em357-thread", "radio.identity", "radio-em357.identity"),
                                  ("build/thread", "thread-rcp", "thread-rcp"), ("build/thread", "ot-daemon", "ot-daemon"),
                                  ("build/thread", "ot-ctl", "ot-ctl")] {
        let source = root.join(dir).join(name);
        println!("cargo:rerun-if-changed={}", source.display());
        let data = fs::read(&source).unwrap_or_else(|e| panic!("{}: {e}; run platform/common/radio/thread/build.sh", source.display()));
        gzip(&data, &out.join(format!("thread-{embedded}.gz")));
    }
    println!("cargo:rerun-if-changed=build.rs");
}
