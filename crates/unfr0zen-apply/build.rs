fn main() {
    // rustc gives musl's linker -nostartfiles, which zig cc reports as unused.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        println!("cargo:rustc-link-arg=-Wno-unused-command-line-argument");
    }
}
