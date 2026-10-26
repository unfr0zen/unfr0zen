//! The version --version reports: UNFR0ZEN_BUILD_VERSION, else `git describe`
//! of the tree at build time, as the GUI has it.

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!("cargo:rerun-if-env-changed=UNFR0ZEN_BUILD_VERSION");
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&root).output().ok()
        .filter(|out| out.status.success()).map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let version = std::env::var("UNFR0ZEN_BUILD_VERSION").ok()
        .or_else(|| git(&["describe", "--tag", "--always", "--dirty"]))
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo:rustc-env=UNFR0ZEN_BUILD_VERSION={version}");
    // Track all version inputs, including changes outside the CLI crate.
    if let Some(files) = git(&["ls-files"]) {
        for file in files.lines() { println!("cargo:rerun-if-changed={}", root.join(file).display()); }
    }
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for file in ["HEAD", "index", "packed-refs", "refs"] {
            println!("cargo:rerun-if-changed={}", std::path::Path::new(&dir).join(file).display());
        }
    }
}
