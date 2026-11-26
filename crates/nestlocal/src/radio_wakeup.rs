//! The radio's nHOST_INT as a wake source. The unit suspends to RAM whenever
//! nothing holds it awake. zbwake.ko marks the board's nHOST_INT interrupt as
//! a wake source and briefly holds it awake on SPI responses. The gen3 and
//! Onyx builds use different GPIOs; the kernel rejects other ABIs via symbol
//! CRCs, so only builds for this board are tried.

/// The distinct zbwake.ko builds (build.rs).
static MODULES: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/zbwake.rs"));

#[cfg(any(target_os = "linux", test))]
fn modules_for<'a>(modules: &'a [(&'a str, &'a [u8])], board: &'a str) -> impl Iterator<Item = &'a [u8]> + 'a {
    modules.iter().filter_map(move |(target, image)| (*target == board).then_some(*image))
}

/// Loads the zbwake.ko build that fits the kernel, so that nHOST_INT wakes
/// the unit.
#[cfg(target_os = "linux")]
pub fn arm() -> Result<(), String> {
    let board = crate::device::current().name;
    let version = if board == "onyx" { "2-onyx" } else { "2" };
    for module in modules_for(MODULES, board) {
        // SAFETY: the image and the empty parameter string outlive the call.
        let loaded = unsafe { libc::syscall(libc::SYS_init_module, module.as_ptr(), module.len(), c"".as_ptr()) } == 0;
        let e = std::io::Error::last_os_error();
        if loaded || e.raw_os_error() == Some(libc::EEXIST) {
            if std::fs::read_to_string("/sys/module/zbwake/version").is_ok_and(|v| v.trim() == version) {
                return Ok(());
            }
            return Err(
                "zbwake.ko has the wrong board or lacks transfer protection; rebuild its modules and reboot".into()
            );
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
    crate::spi::host_int_low()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_units_board_can_supply_a_wake_module() {
        let modules: &[(&str, &[u8])] = &[("diamond3", b"gen3"), ("onyx", b"6.3"), ("j49", b"gen2"), ("onyx", b"6.5")];
        assert_eq!(modules_for(modules, "onyx").collect::<Vec<_>>(), [b"6.3", b"6.5"]);
        assert_eq!(modules_for(modules, "diamond3").collect::<Vec<_>>(), [b"gen3"]);
        assert_eq!(modules_for(modules, "j49").collect::<Vec<_>>(), [b"gen2"]);
        assert_eq!(modules_for(modules, "diamond").count(), 0);
    }
}
