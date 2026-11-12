//! The patch bundle as a release publishes it.
//!
//!     unfr0zen-bundle pack DEVICE VERSION OUT
//!         Writes DEVICE's bundle (diamond3) to OUT, signed with the base64
//!         Ed25519 seed in UNFR0ZEN_BUNDLE_KEY when it is set.
//!     unfr0zen-bundle keygen KEY
//!         Writes a new seed to KEY (mode 600) and prints its public key.
//!     unfr0zen-bundle verify BUNDLE PUBLIC-KEY
//!         Checks BUNDLE's files and its signature by the base64 public key.

use std::fs;
use std::process::exit;

use unfr0zen_core::bundle::{self, Bundle};
use unfr0zen_core::ed25519_dalek::SigningKey;
use unfr0zen_core::Device;

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("unfr0zen-bundle: {message}");
    exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args[..] {
        ["pack", device, version, out] => {
            let device = match device {
                "diamond3" => Device::Diamond3,
                other => fail(format!("no bundle for {other}")),
            };
            let key = std::env::var("UNFR0ZEN_BUNDLE_KEY").ok().filter(|k| !k.is_empty())
                .map(|k| bundle::signing_key(&k).unwrap_or_else(|e| fail(e)));
            let data = unfr0zen_host::bundle::pack(device, version, key.as_ref()).unwrap_or_else(|e| fail(e));
            fs::write(out, &data).unwrap_or_else(|e| fail(format!("{out}: {e}")));
            let signed = key.map_or("unsigned".to_string(), |k| format!("signed by {}", bundle::public_key_text(&k)));
            println!("{out}: {} bytes, {signed}", data.len());
        }
        ["keygen", path] => {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).unwrap_or_else(|e| fail(e));
            let key = SigningKey::from_bytes(&seed);
            use base64::Engine;
            let text = base64::engine::general_purpose::STANDARD.encode(seed);
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            use std::io::Write;
            options.open(path).and_then(|mut file| file.write_all(format!("{text}\n").as_bytes()))
                .unwrap_or_else(|e| fail(format!("{path}: {e}")));
            println!("{}", bundle::public_key_text(&key));
        }
        ["verify", path, public] => {
            let bundle = Bundle::unpack(&fs::read(path).unwrap_or_else(|e| fail(format!("{path}: {e}")))).unwrap_or_else(|e| fail(e));
            bundle.verify(&bundle::verifying_key(public).unwrap_or_else(|e| fail(e))).unwrap_or_else(|e| fail(e));
            println!("{path}: {} {} verified", bundle.manifest.device, bundle.manifest.version);
        }
        _ => fail("usage: unfr0zen-bundle pack DEVICE VERSION OUT | keygen KEY | verify BUNDLE PUBLIC-KEY"),
    }
}
