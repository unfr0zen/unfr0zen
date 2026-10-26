//! The radio's nHOST_INT as a wake source. The unit suspends to RAM whenever
//! nothing holds it awake, and asleep the GPC masks GPIO2's lines 0-15,
//! nHOST_INT (GPIO2_IO04) among them: a message for the unit would wait for
//! the next wake. zbwake.ko (platform/diamond3/zbwake/ in the repository) marks the line's
//! interrupt as a wake source. It is built for each gen3 release's kernel,
//! and a kernel refuses a module whose symbol CRCs differ from its own, before
//! running any of it, so loading each build in turn finds the one that fits.

/// The distinct zbwake.ko builds (build.rs).
static MODULES: &[&[u8]] = include!(concat!(env!("OUT_DIR"), "/zbwake.rs"));

/// Loads the zbwake.ko build that fits the kernel, so that nHOST_INT wakes
/// the unit.
#[cfg(target_os = "linux")]
pub fn arm() -> Result<(), String> {
    for module in MODULES {
        // SAFETY: the image and the empty parameter string outlive the call.
        let loaded = unsafe { libc::syscall(libc::SYS_init_module, module.as_ptr(), module.len(), c"".as_ptr()) } == 0;
        let e = std::io::Error::last_os_error();
        if loaded || e.raw_os_error() == Some(libc::EEXIST) {
            if std::fs::read_to_string("/sys/module/zbwake/version").is_ok_and(|v| v.trim() == "2") {
                return Ok(());
            }
            return Err("zbwake.ko lacks transfer protection; rebuild its modules and reboot".into());
        }
        // Symbol CRC mismatches return EINVAL; vermagic mismatches return ENOEXEC.
        if !matches!(e.raw_os_error(), Some(libc::ENOEXEC | libc::EINVAL)) {
            return Err(format!("loading zbwake.ko: {e}"));
        }
    }
    Err("no zbwake.ko build fits this kernel".into())
}

#[cfg(not(target_os = "linux"))]
pub fn arm() -> Result<(), String> {
    let _ = MODULES;
    Err("radio wake requires Linux".into())
}

/// The radio has something to say: the unit must not go to sleep, since
/// nHOST_INT is already low and no falling edge would wake it.
pub fn pending() -> bool {
    super::spi::host_int_low()
}
