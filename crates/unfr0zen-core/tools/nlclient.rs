//! Developer harness for the same in-memory builder recovery uses.
//! cargo run -p unfr0zen-core --bin nlclient -- STOCK OBJECT OUTPUT
//! --report STOCK prints what discovery finds, and any objection to the build.
//! --golden STOCK OBJECT TOOLCHAIN writes the sample's golden under
//! platform/common/nlclient/tests/golden, for an intentional screen revision.
use std::{io::Read, path::PathBuf};
use unfr0zen_core::nlclient::{builder, Patch};
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
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
    if args.len() == 4 && args[0] == "--golden" {
        let source = std::fs::read(&args[1])?;
        let object = std::fs::read(&args[2])?;
        let found = builder::build(&source, &object)?;
        let manifest = &found.manifest;
        let offset = manifest["payload_offset"].as_u64().unwrap() as usize;
        let size = manifest["payload_size"].as_u64().unwrap() as usize;
        let mut journal = Vec::new();
        flate2::read::GzDecoder::new(found.journal.as_slice()).read_to_end(&mut journal)?;
        let patch: serde_json::Map<String, serde_json::Value> = [
            "factory", "factory_file_offset", "image", "original_factory_bytes", "original_program_headers",
            "payload_address", "payload_offset", "program_header_offset", "replaced_screen", "runtime_directory",
            "settings_menu", "source_size",
        ].into_iter().map(|key| (key.to_owned(), manifest[key].clone())).collect();
        let golden = serde_json::json!({
            "discovery": manifest["discovery"],
            "native_api": manifest["native_api"],
            "output": {"journal_sha256": unfr0zen_core::shell::sha256_hex(&journal),
                       "sha256": unfr0zen_core::shell::sha256_hex(&found.output), "size": found.output.len()},
            "patch": patch,
            "payload": {"entry": manifest["entry"], "sha256": unfr0zen_core::shell::sha256_hex(&found.output[offset..offset + size]),
                        "size": size, "symbols": manifest["symbols"]},
            "toolchain": args[3].to_string_lossy(),
        });
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../platform/common/nlclient/tests/golden")
            .join(format!("{}.json", unfr0zen_core::shell::sha256_hex(&source)));
        std::fs::write(&path, serde_json::to_vec_pretty(&golden)?)?;
        println!("{}", path.display());
        return Ok(());
    }
    if args.len() != 3 {
        return Err("usage: nlclient STOCK OBJECT OUTPUT | --report STOCK | --golden STOCK OBJECT TOOLCHAIN".into());
    }
    let source = std::fs::read(&args[0])?;
    let object = std::fs::read(&args[1])?;
    let found = builder::build(&source, &object)?;
    let out = PathBuf::from(&args[2]);
    std::fs::write(&out, &found.output)?;
    std::fs::write(
        out.with_extension("json"),
        serde_json::to_vec_pretty(&found.manifest)?,
    )?;
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
