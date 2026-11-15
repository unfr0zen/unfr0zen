//! Embeds the radio's firmware (platform/common/radio/, which utils/build-firmware.sh
//! builds first) from the files NESTLOCAL_RADIO (the gen3's EM3581 image)
//! and NESTLOCAL_RADIO_EM357 (the gen2's Zigbee image) name. Without one,
//! -zigbee runs a radio that already has the firmware but cannot install it.
//!
//! Embeds the zbwake.ko builds (platform/diamond3/zbwake/, one per gen3 release) found in the
//! directory NESTLOCAL_ZBWAKE names, each distinct one once. Without them,
//! -zigbee cannot wake the sleeping unit.

use std::fmt::Write;

fn main() {
    // rustc gives musl's linker -nostartfiles, which zig cc reports as unused.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        println!("cargo:rustc-link-arg=-Wno-unused-command-line-argument");
    }

    // The version wifi_update/mod.rs reports and marks the binary with: the
    // build's (UNFR0ZEN_BUILD_VERSION), else the repository's tag.
    println!("cargo:rerun-if-env-changed=UNFR0ZEN_BUILD_VERSION");
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&root).output().ok()
        .filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for file in ["HEAD", "packed-refs", "refs"] {
            println!("cargo:rerun-if-changed={}", std::path::Path::new(&dir).join(file).display());
        }
    }
    let version = std::env::var("UNFR0ZEN_BUILD_VERSION").ok().filter(|v| !v.is_empty())
        .or_else(|| git(&["describe", "--tags", "--always"]))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=NESTLOCAL_VERSION={version}");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for (variable, name) in [("NESTLOCAL_RADIO", "radio-em3581.bin"), ("NESTLOCAL_RADIO_EM357", "radio-em357.bin")] {
        println!("cargo:rerun-if-env-changed={variable}");
        let image = match std::env::var(variable) {
            Ok(path) if !path.is_empty() => {
                println!("cargo:rerun-if-changed={path}");
                std::fs::read(&path).unwrap_or_else(|e| panic!("{variable} {path}: {e}"))
            }
            _ => Vec::new(),
        };
        std::fs::write(out.join(name), image).unwrap();
    }

    println!("cargo:rerun-if-env-changed=NESTLOCAL_ZBWAKE");
    let mut modules: Vec<Vec<u8>> = Vec::new();
    if let Ok(dir) = std::env::var("NESTLOCAL_ZBWAKE").map(std::path::PathBuf::from) {
        println!("cargo:rerun-if-changed={}", dir.display());
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("NESTLOCAL_ZBWAKE {}: {e}", dir.display()));
        let mut paths: Vec<_> = entries.map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "ko")).collect();
        paths.sort();
        for path in paths {
            println!("cargo:rerun-if-changed={}", path.display());
            let module = std::fs::read(&path).unwrap();
            if !modules.contains(&module) {
                modules.push(module);
            }
        }
    }
    let mut list = String::from("&[");
    for (i, module) in modules.iter().enumerate() {
        let name = format!("zbwake-{i}.ko");
        std::fs::write(out.join(&name), module).unwrap();
        write!(list, "include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{name}\")),").unwrap();
    }
    list.push(']');
    std::fs::write(out.join("zbwake.rs"), list).unwrap();
}
