//! The diamond3's part of the root filesystem patch: the USB root shell
//! (inittab, usb-gadget and `unlocked`), its OpenSSH, and root's shell profile.

use crate::patch::{check_mode, patch_shell, put, text, Options, Rootfs, Tree, PROFILE};
use crate::{device, Result};

pub(crate) const OLD_GETTY: &str = "null::respawn:/usr/sbin/unlocked-getty -L ttyGS0 115200";
const NEW_SHELL: &str = "ttyGS0::respawn:/bin/sh";
/// inittab's root shell line, and the same commented out with the USB root
/// shell off. The newline before each keeps one from matching inside the other.
pub(crate) const SHELL_LINES: (&str, &str) = ("\nttyGS0::respawn:/bin/sh\n", "\n#ttyGS0::respawn:/bin/sh\n");
/// usb-gadget's choice when `unlocked` says yes: ACM, the root shell's port,
/// with mass storage; with the USB root shell off, mass storage only, as a
/// locked unit has it.
pub(crate) const GADGET_LINES: (&str, &str) = ("  USB_GADGET_MOD=g_acm_ms\n",
                                               "  USB_GADGET_MOD=g_mass_storage # unfr0zen: no USB root shell\n");
/// The files the patch requires: the root shell's and SSH's among them.
pub const FILES: [&str; 9] = ["etc/shadow", "etc/inittab", "usr/sbin/unlocked", "etc/init.d/usb-gadget", "etc/init.d/rcS",
                              "etc/init.d/rcH", "etc/ssh/sshd_config", "etc/init.d/sshd", "etc/nestlabs/client.config"];
/// (file, mode, stock text, SSH text)
const SSH_EDITS: [(&str, u32, &str, &str); 4] = [
    ("etc/ssh/sshd_config", 0o644, "PermitRootLogin no\n", "PermitRootLogin yes\n"),
    ("etc/ssh/sshd_config", 0o644, "HostKey /media/system-config/ssh/ssh_host_rsa_key\n",
     "HostKey /media/system-config/ssh/ssh_host_rsa_key\nHostKey /media/system-config/ssh/ssh_host_ecdsa_key\n"),
    ("etc/init.d/sshd", 0o750, "SSHRSAKEY=${SSHCONFDIR}/ssh_host_rsa_key\n",
     "SSHRSAKEY=${SSHCONFDIR}/ssh_host_rsa_key\nSSHECDSAKEY=${SSHCONFDIR}/ssh_host_ecdsa_key\n"),
    ("etc/init.d/sshd", 0o750, "    CreateKeyIfAbsent rsa \"${SSHRSAKEY}\"\n",
     "    CreateKeyIfAbsent rsa \"${SSHRSAKEY}\"\n    CreateKeyIfAbsent ecdsa \"${SSHECDSAKEY}\"\n"),
];
/// rcS starts sshd with this line; without SSH it is commented out. The
/// newline before it keeps the stock line from matching inside the other.
const SSHD_START: (&str, &str) = ("\n${INITDIR}/sshd start\n", "\n#${INITDIR}/sshd start\n");

pub(crate) static ROOTFS: Rootfs = Rootfs {
    files: &FILES,
    shadow_modes: &[0o640],
    optional_files: |_| Vec::new(),
    patch,
};

fn patch(tree: &mut Tree, options: &Options) -> Result<()> {
    patch_usb_shell(tree, options.usb_shell)?;
    patch_ssh(tree, options.ssh)?;
    if tree.contains_key(PROFILE) {
        patch_shell(tree, true)?;
    }
    Ok(())
}

/// `unlocked` saying yes (which sshd needs too), and the USB root shell in
/// place of the guarded getty. Without the shell, inittab's line is commented
/// out and usb-gadget loads no ACM port.
fn patch_usb_shell(tree: &mut Tree, usb_shell: bool) -> Result<()> {
    let mut inittab = text(tree, "etc/inittab")?;
    if inittab.matches(OLD_GETTY).count() == 1 {
        inittab = inittab.replace(OLD_GETTY, NEW_SHELL);
    }
    let (shell, no_shell) = SHELL_LINES;
    if inittab.matches(no_shell).count() == 1 {
        inittab = inittab.replace(no_shell, shell);
    }
    if inittab.matches(shell).count() != 1 {
        return device("expected one stock USB getty or tested USB shell line");
    }
    if !usb_shell {
        inittab = inittab.replace(shell, no_shell);
    }
    put(tree, "etc/inittab", inittab);
    let (acm, no_acm) = GADGET_LINES;
    let mut gadget = text(tree, "etc/init.d/usb-gadget")?;
    if gadget.matches(no_acm).count() == 1 {
        gadget = gadget.replace(no_acm, acm);
    }
    if gadget.matches(acm).count() != 1 {
        return device("expected one 'USB_GADGET_MOD=g_acm_ms' line in etc/init.d/usb-gadget");
    }
    if !usb_shell {
        gadget = gadget.replace(acm, no_acm);
    }
    put(tree, "etc/init.d/usb-gadget", gadget);
    if !tree.contains_key("usr/sbin/unlocked") {
        return device("missing usr/sbin/unlocked");
    }
    put(tree, "usr/sbin/unlocked", "#!/bin/sh\necho 1\n".to_string());
    for (name, mode) in [("etc/inittab", 0o644), ("usr/sbin/unlocked", 0o755), ("etc/init.d/usb-gadget", 0o750)] {
        check_mode(tree, name, mode)?;
    }
    Ok(())
}

/// An SSH server at boot, root logins with root's password, and an ECDSA host
/// key current clients accept. Without the flag rcS does not start sshd, and
/// sshd_config and its init script are put back as stock.
pub fn patch_ssh(tree: &mut Tree, enabled: bool) -> Result<()> {
    let (start, off) = SSHD_START;
    let mut rcs = text(tree, "etc/init.d/rcS")?;
    if rcs.matches(off).count() == 1 {
        rcs = rcs.replace(off, start);
    }
    if rcs.matches(start).count() != 1 {
        return device("expected one '${INITDIR}/sshd start' line in etc/init.d/rcS");
    }
    if !enabled {
        rcs = rcs.replace(start, off);
    }
    put(tree, "etc/init.d/rcS", rcs);
    for (name, mode, stock, ssh) in SSH_EDITS {
        let mut content = text(tree, name)?;
        if content.matches(ssh).count() == 1 {
            content = content.replace(ssh, stock);
        }
        if content.matches(stock).count() != 1 {
            return device(format!("expected one '{}' line in {name}", stock.lines().next().unwrap_or("")));
        }
        if enabled {
            content = content.replace(stock, ssh);
        }
        put(tree, name, content);
        check_mode(tree, name, mode)?;
    }
    Ok(())
}
