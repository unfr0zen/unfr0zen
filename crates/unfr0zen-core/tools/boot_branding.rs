//! Developer harness for boot artwork, including experimental kernel recoloring.
//! cargo run -p unfr0zen-core --bin boot_branding -- DEVICE_PATH INPUT OUTPUT
use std::{env, fs};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: boot_branding DEVICE_PATH INPUT OUTPUT".into());
    }
    match unfr0zen_core::boot_branding::recolor(&args[0], &fs::read(&args[1])?)? {
        Some(output) => {
            fs::write(&args[2], output)?;
            println!("Recolored {}", args[0]);
        }
        None => {
            println!("No supported logo in {}; left untouched", args[0]);
        }
    }
    Ok(())
}
