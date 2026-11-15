//! Small local file protocol with the injected native settings screen.
//! /var/run is ephemeral: requests and pairing credentials never survive boot.
use std::path::Path;

pub const DIR: &str = "/var/run/unfr0zen";

/// Where the files are: DIR, or a directory of the test run's own.
#[cfg(not(test))]
pub fn dir() -> &'static Path {
    Path::new(DIR)
}

#[cfg(test)]
pub fn dir() -> &'static Path {
    static TEST_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    TEST_DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("nestlocal-settings-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    })
}

pub fn publish(name: &str, data: &[u8]) -> std::io::Result<()> {
    let path = dir().join(name);
    let temporary = dir().join(format!("{name}.partial"));
    // nlclient renders this on the physical display; pairing credentials
    // and commands remain accessible only to root.
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&temporary)?;
    file.write_all(data)?;
    drop(file);
    std::fs::rename(temporary, path)
}

pub fn status(protocol: &str, text: &str) {
    if let Err(e) = publish(&format!("{protocol}.status"), text.as_bytes()) {
        crate::logf!("settings UI: {e}");
    }
}

pub fn remove(name: &str) {
    let _ = std::fs::remove_file(dir().join(name));
}

pub fn take_request(protocol: &str) -> bool { take_request_age(protocol).is_some() }

/// Takes protocol's request, with what the screen wrote in it: at most
/// limit bytes, trimmed.
pub fn take_request_text(protocol: &str, limit: u64) -> Option<String> {
    use std::io::Read;
    let path = dir().join(format!("{protocol}.request"));
    let mut text = Vec::new();
    std::fs::File::open(&path).ok()?.take(limit).read_to_end(&mut text).ok()?;
    std::fs::remove_file(path).ok()?;
    Some(String::from_utf8_lossy(&text).trim().to_string())
}

pub fn take_request_age(protocol: &str) -> Option<std::time::Duration> {
    let path = dir().join(format!("{protocol}.request"));
    let age = std::fs::metadata(&path).ok()?.modified().ok()
        .and_then(|time| time.elapsed().ok()).unwrap_or_default();
    std::fs::remove_file(path).ok()?;
    Some(age)
}

/// The configured features, a bit each for Matter, Zigbee, Doom, Themes
/// and Update, in hex.
fn features(matter: bool, zigbee: bool, doom: bool, theme: bool, update: bool) -> String {
    let bits = (matter as u8) | ((zigbee as u8) << 1) | ((doom as u8) << 2) | ((theme as u8) << 3) | ((update as u8) << 4);
    format!("{bits:x}")
}

pub fn init(matter: bool, zigbee: bool, doom: bool, theme: bool, update: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir())?;
    std::fs::set_permissions(dir(), std::fs::Permissions::from_mode(0o700))?;
    // Publish configured features independently of service/radio readiness.
    publish("features", features(matter, zigbee, doom, theme, update).as_bytes())?;
    publish_activity_icons()?;
    for name in ["display.request", "display.taken", "matter.request", "matter-reset.request", "zigbee.request", "doom.request",
                 "theme.request", "update.request", "update-trust.request", "update-forget.request", "update-rollback.request",
                 "matter.code", "matter.png", "update.code"] {
        remove(name);
    }
    status("matter", if matter { "Starting Matter..." } else { "Matter not enabled" });
    status("zigbee", if zigbee { "Starting Zigbee..." } else { "Zigbee not enabled" });
    status("doom", if doom { "Ready" } else { "Unavailable" });
    status("update", if update { crate::wifi_update::READY } else { crate::wifi_update::NOT_ENABLED });
    Ok(())
}

pub fn matter_codes(comm: &rs_matter::BasicCommData, capabilities: rs_matter::pairing::DiscoveryCapabilities) -> Result<(), String> {
    let (png_bytes, code) = encode_matter_codes(comm, capabilities)?;
    publish("matter.png", &png_bytes).map_err(|e| e.to_string())?;
    publish("matter.code", code.as_bytes()).map_err(|e| e.to_string())
}

/// A length of the gen 3's 480-pixel settings screen, on this unit's display.
fn scaled(length: usize) -> usize {
    length * crate::device::current().display / 480
}

// Eight small frames rendered through nlclient's native PNG API. Keeping the
// animation in the view avoids coupling UI refresh to radio/commissioning waits.
fn publish_activity_icons() -> std::io::Result<()> {
    let size = scaled(56);
    let (centre, orbit, dot_radius) = (size as f64 / 2.0, size as f64 * 20.0 / 56.0, size as f64 * 4.0 / 56.0);
    for frame in 0..8 {
        let mut rgba = vec![0u8; size * size * 4];
        for dot in 0..8 {
            let angle = dot as f64 * std::f64::consts::TAU / 8.0;
            let cx = centre + angle.sin() * orbit;
            let cy = centre - angle.cos() * orbit;
            let alpha = 64 + ((dot + 8 - frame) % 8) * 27;
            for y in 0..size { for x in 0..size {
                if (x as f64 - cx).powi(2) + (y as f64 - cy).powi(2) <= dot_radius.powi(2) {
                    rgba[(y * size + x) * 4..(y * size + x) * 4 + 4]
                        .copy_from_slice(&[0, 174, 239, alpha as u8]);
                }
            } }
        }
        let mut data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut data, size as u32, size as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&rgba)?;
        }
        publish(&format!("activity-{frame}.png"), &data)?;
    }
    Ok(())
}

fn encode_matter_codes(comm: &rs_matter::BasicCommData, capabilities: rs_matter::pairing::DiscoveryCapabilities) -> Result<(Vec<u8>, String), String> {
    use rs_matter::pairing::qr::{Qr, QrPayload, CommFlowType, no_optional_data};
    use rs_matter::dm::devices::test::{TEST_VID, TEST_PID};
    let payload = QrPayload::new(capabilities, CommFlowType::Standard, comm.clone(), TEST_VID, TEST_PID, "", no_optional_data);
    let mut text_buf = [0u8; 256];
    let (text, _) = payload.as_str(&mut text_buf).map_err(|e| e.to_string())?;
    let mut temporary = [0u8; 4096];
    let mut encoded = [0u8; 4096];
    let qr = Qr::compute(text, &mut temporary, &mut encoded).map_err(|e| e.to_string())?;
    // Four modules of quiet zone, integral scaling, no interpolation, in
    // the slot the gen 3's 192 pixels are on this display.
    let modules = qr.size() + 8;
    let scale = scaled(192) as u32 / modules;
    if scale == 0 { return Err("Matter QR exceeds display slot".into()); }
    let size = modules * scale;
    let pixels: Vec<u8> = (0..size).flat_map(|y| {
        let qr = &qr;
        (0..size).map(move |x| {
            let mx = (x / scale) as i32 - 4;
            let my = (y / scale) as i32 - 4;
            if mx >= 0 && my >= 0 && mx < qr.size() as i32 && my < qr.size() as i32 && qr.get_module(mx, my) { 0 } else { 255 }
        })
    }).collect();
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, size, size);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
    }
    Ok((png_bytes, comm.compute_pretty_pairing_code().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_matter::{BasicCommData, sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef}};

    #[test]
    fn features_are_a_bit_each_in_hex() {
        assert_eq!(features(true, true, true, true, true), "1f");
        assert_eq!(features(false, false, false, false, true), "10");
        assert_eq!(features(false, false, false, true, false), "8");
        assert_eq!(features(true, false, true, false, false), "5");
        assert_eq!(features(false, false, false, false, false), "0");
    }

    #[test]
    fn qr_png_and_manual_code_follow_configured_credentials() {
        for (pin, discriminator) in [(20202021u32, 0u16), (34567890, 3840)] {
            let comm = BasicCommData {
                password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&pin.to_le_bytes())),
                discriminator,
            };
            let (png, code) = encode_matter_codes(&comm, rs_matter::pairing::DiscoveryCapabilities::IP).unwrap();
            assert_eq!(code, comm.compute_pretty_pairing_code().as_str());
            let mut reader = png::Decoder::new(png.as_slice()).read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut pixels).unwrap();
            assert_eq!(info.width, info.height);
            assert!(info.width <= 192);
            assert_eq!(info.color_type, png::ColorType::Grayscale);
            assert!(pixels.contains(&0));
            assert!(pixels[..info.width as usize * 4].iter().all(|p| *p == 255));
            // Finder patterns seven modules wide in three corners, inside a
            // quiet zone of four modules on every side.
            let width = info.width as usize;
            let dark = |x: usize, y: usize| pixels[y * width + x] == 0;
            let top = (0..width).find(|&y| (0..width).any(|x| dark(x, y))).unwrap();
            let bottom = (0..width).rev().find(|&y| (0..width).any(|x| dark(x, y))).unwrap();
            let left = (0..width).find(|&x| dark(x, top)).unwrap();
            let right = (0..width).rev().find(|&x| dark(x, top)).unwrap();
            let finder = (left..width).take_while(|&x| dark(x, top)).count();
            assert_eq!(top, left);
            assert_eq!(finder * 4, left * 7, "finder {finder} px, quiet zone {left} px");
            assert_eq!((width - 1 - right, width - 1 - bottom), (left, left));
            assert_eq!(width % (left / 4), 0);
            if let Ok(dir) = std::env::var("NLCLIENT_TEST_ARTIFACTS") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(Path::new(&dir).join("matter.png"), &png).unwrap();
                std::fs::write(Path::new(&dir).join("matter.code"), &code).unwrap();
            }
        }
    }
}
