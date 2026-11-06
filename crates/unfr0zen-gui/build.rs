//! On Windows, embed the app icon into the .exe so Explorer and the taskbar
//! show it, and fill in the version resource: antivirus heuristics weigh an
//! unsigned executable with no company, product or copyright as suspicious.
//! Other platforms set the window icon at runtime (with_icon).

fn main() {
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    third_party_licenses(&root);
    println!("cargo:rerun-if-env-changed=UNFR0ZEN_BUILD_VERSION");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&root).output().ok()
        .filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let version = std::env::var("UNFR0ZEN_BUILD_VERSION").ok()
        .or_else(|| git(&["describe", "--tag", "--always", "--dirty"]))
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo:rustc-env=UNFR0ZEN_BUILD_VERSION={version}");
    // Track all version inputs, including changes outside the GUI crate.
    if let Some(files) = git(&["ls-files"]) {
        for file in files.lines() { println!("cargo:rerun-if-changed={}", root.join(file).display()); }
    }
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for file in ["HEAD", "index", "packed-refs", "refs"] {
            println!("cargo:rerun-if-changed={}", std::path::Path::new(&dir).join(file).display());
        }
    }
    // The target, not the host: the .exe may be cross-built on Linux.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icon/unfr0zen.ico");
        println!("cargo:rerun-if-changed={icon}");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon);
        res.set("ProductVersion", &version);
        res.set("FileVersion", &version);
        res.set("ProductName", "unfr0zen");
        res.set("InternalName", "unfr0zen");
        res.set("OriginalFilename", "unfr0zen.exe");
        res.set("FileDescription", "unfr0zen");
        res.set("CompanyName", "unfr0zen");
        res.set("LegalCopyright", "Copyright © 2026 unfr0zen team");
        if let Err(error) = res.compile() {
            println!("cargo:warning=could not embed the Windows icon: {error}");
        }
    }
}

/// Writes `OUT_DIR/third_party.rs`: every crate the `unfr0zen` binary links,
/// with its SPDX license, read from `cargo metadata` and scoped to
/// the binary's own (non-dev, non-build) dependency graph. The About page
/// includes it, so the list stays in step with `Cargo.lock`. A metadata
/// failure leaves the list empty rather than breaking the build.
fn third_party_licenses(root: &std::path::Path) {
    use std::collections::{BTreeSet, HashMap, HashSet};
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("third_party.rs");
    println!("cargo:rerun-if-changed={}", root.join("Cargo.lock").display());
    let empty = |note: &str| {
        println!("cargo:warning=third-party license list left empty: {note}");
        std::fs::write(&dest, "pub static THIRD_PARTY: &[(&str, &str)] = &[];\n").unwrap();
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--frozen"]).current_dir(root).output();
    let json = match output {
        Ok(out) if out.status.success() => out.stdout,
        result => return empty(&format!("cargo metadata failed ({result:?})")),
    };
    let meta: serde_json::Value = match serde_json::from_slice(&json) {
        Ok(value) => value,
        Err(error) => return empty(&format!("cargo metadata is not JSON ({error})")),
    };
    // name, version and license by package id.
    let mut info: HashMap<String, (String, String, String)> = HashMap::new();
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        let (Some(id), Some(name), Some(version)) =
            (pkg["id"].as_str(), pkg["name"].as_str(), pkg["version"].as_str()) else { continue };
        let license = pkg["license"].as_str().map(str::to_string)
            .or_else(|| pkg["license_file"].as_str().map(|_| "see crate".to_string()))
            .unwrap_or_else(|| "unspecified".to_string());
        info.insert(id.into(), (name.into(), version.into(), license));
    }
    // Normal-dependency edges, and the binary's own node.
    let mut normal: HashMap<String, Vec<String>> = HashMap::new();
    let mut root_id = None;
    for node in meta["resolve"]["nodes"].as_array().into_iter().flatten() {
        let Some(id) = node["id"].as_str() else { continue };
        if info.get(id).is_some_and(|(name, ..)| name == "unfr0zen-gui") { root_id = Some(id.to_string()); }
        let deps = node["deps"].as_array().into_iter().flatten().filter(|dep| {
            dep["dep_kinds"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()))
        }).filter_map(|dep| dep["pkg"].as_str().map(str::to_string)).collect();
        normal.insert(id.into(), deps);
    }
    let Some(root_id) = root_id else { return empty("unfr0zen-gui not in metadata") };
    // Everything reachable from the binary through normal dependencies.
    let mut reached = BTreeSet::new();
    let mut stack = vec![root_id];
    while let Some(id) = stack.pop() {
        for dep in normal.get(&id).into_iter().flatten() {
            if reached.insert(dep.clone()) { stack.push(dep.clone()); }
        }
    }
    // Drop the workspace's own crates; they are the app, not third parties.
    let members: HashSet<&str> = meta["workspace_members"].as_array().into_iter().flatten()
        .filter_map(|id| id.as_str()).collect();
    // Name and license only, so a crate pulled in at several versions is one
    // entry; a genuinely different license keeps its own line.
    let mut rows: Vec<(&str, &str)> = reached.iter()
        .filter(|id| !members.contains(id.as_str())).filter_map(|id| info.get(id))
        .map(|(name, _version, license)| (name.as_str(), license.as_str())).collect();
    rows.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.1.cmp(b.1)));
    rows.dedup();
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let mut out = String::from("pub static THIRD_PARTY: &[(&str, &str)] = &[\n");
    for (name, license) in rows {
        out.push_str(&format!("    (\"{}\", \"{}\"),\n", escape(name), escape(license)));
    }
    out.push_str("];\n");
    std::fs::write(&dest, out).unwrap();
}
