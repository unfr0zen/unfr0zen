//! Onyx-specific rootfs setup: the Wi-Fi host wake pad's mux and the USB
//! shell started from the gadget script instead of init.

use crate::common::rootfs;
use crate::setup::{check_mode, put, Options, Rootfs, Tree};
use crate::{device, Result};

const GADGET_START: &str = "    ${MODPROBE} ${USB_GADGET_MOD}.ko iSerialNumber=${SERIAL_NO}\n";
const GADGET_STOP: &str = "    ${MODPROBE} -r ${USB_GADGET_MOD}.ko\n";

// Onyx starts the gadget during sysinit. Attach the shell there so services
// later in rcS cannot delay it. A new session owns the tty after each hangup.
const ONYX_SHELL_START: &str = r#"    ${MODPROBE} ${USB_GADGET_MOD}.ko iSerialNumber=${SERIAL_NO} || exit $?
    if [ "$USB_GADGET_MOD" = g_acm_ms ]; then
        ${STARTDAEMON} -q -o -b -m -p /var/lock/unfr0zen-usb-shell.pid -x /bin/sh -- -c '
            export HOME=/root
            while :; do
                if [ -c /dev/ttyGS0 ]; then
                    /usr/bin/setsid /bin/sh -c "exec /bin/sh -l <>/dev/ttyGS0 1>&0 2>&0"
                fi
                sleep 1
            done'
    fi
"#;
const ONYX_SHELL_STOP: &str = r#"    ${STOPDAEMON} -q -o -R TERM/2 -p /var/lock/unfr0zen-usb-shell.pid -x /bin/sh || exit $?
    rm -f /var/lock/unfr0zen-usb-shell.pid
    ${MODPROBE} -r ${USB_GADGET_MOD}.ko
"#;

const WLAN: &str = "etc/init.d/wlan";

/// The common files and the Wi-Fi init script, read whole.
static FILES: [&str; rootfs::FILES.len() + 1] = {
    let mut files = [WLAN; rootfs::FILES.len() + 1];
    let mut i = 0;
    while i < rootfs::FILES.len() {
        files[i] = rootfs::FILES[i];
        i += 1;
    }
    files
};

pub(crate) static ROOTFS: Rootfs =
    Rootfs { files: &FILES, shadow_modes: &[0o640], set_up: set_up_onyx, optional_files: |_| Vec::new() };

fn set_up_onyx(tree: &mut Tree, options: &mut Options) -> Result<()> {
    rootfs::set_up_access(tree, options, false, &[(GADGET_START, ONYX_SHELL_START), (GADGET_STOP, ONYX_SHELL_STOP)])?;
    set_up_wlan_gpio(tree)
}

/// The Wi-Fi chip's host wake (INT_WIFI_TO_AP) is SD1_CLK as GPIO2_IO17,
/// which no stock device tree node muxes: it keeps whatever the boot left,
/// and the boot ROM's USB download path leaves USDHC1_CLK, where the driver
/// never sees the chip's interrupts. The init script muxes it before power on.
fn set_up_wlan_gpio(tree: &mut Tree) -> Result<()> {
    const MARKER: &str = "wlan_bcm_power_on()";
    const PATCH: &str = "    /sbin/devmem 0x020E044C 4 0x1008\n    /sbin/devmem 0x020E01C0 4 0x15\n";
    let Some((content, _)) = tree.get(WLAN) else {
        return device(format!("{WLAN} was not read"));
    };
    let content = String::from_utf8_lossy(content).into_owned();
    if content.contains("devmem 0x020E044C") {
        return Ok(());
    }
    let Some(at) = content.find(MARKER) else {
        return device(format!("{WLAN} has no {MARKER} function"));
    };
    let body = at + content[at..].find('\n').unwrap_or(content.len() - at) + 1;
    let patched = format!("{}{PATCH}{}", &content[..body], &content[body..]);
    put(tree, WLAN, patched);
    check_mode(tree, WLAN, 0o750)
}
