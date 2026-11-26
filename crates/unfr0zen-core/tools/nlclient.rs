//! Developer harness for the same in-memory builder recovery uses.
//! cargo run -p unfr0zen-core --bin nlclient -- STOCK OBJECT OUTPUT
//! --report STOCK prints what discovery finds, and any objection to the build.
use std::{io::Read, path::PathBuf};
use unfr0zen_core::nlclient::{builder, Patch};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
fn sections(source: &[u8]) -> &'static [unfr0zen_core::nlclient::menu::Section] {
    use unfr0zen_core::nlclient::{legacy_abi, menu::Section};
    if legacy_abi(source) {
        &Section::LEGACY
    } else {
        &Section::ALL
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "--report" {
        let source = std::fs::read(&args[1])?;
        let (found, objection) = unfr0zen_core::nlclient::discovery::survey(&source)?;
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"discovery": found, "objection": objection}))?);
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: nlclient STOCK OBJECT OUTPUT | --report STOCK".into());
    }
    let source = std::fs::read(&args[0])?;
    let object = std::fs::read(&args[1])?;
    let found = builder::build(&source, &object, sections(&source))?;
    let out = PathBuf::from(&args[2]);
    std::fs::write(&out, &found.output)?;
    std::fs::write(out.with_extension("json"), serde_json::to_vec_pretty(&found.manifest)?)?;
    std::fs::write(out.with_extension("undo.gz"), &found.journal)?;
    let patch = Patch::parse(&found.journal)?;
    let mut journal = Vec::new();
    flate2::read::GzDecoder::new(found.journal.as_slice()).read_to_end(&mut journal)?;
    println!(
        "{} -> {} ({} bytes); journal {}",
        patch.stock_sha,
        patch.output_sha,
        found.output.len(),
        unfr0zen_core::shell::sha256_hex(&journal)
    );
    Ok(())
}
