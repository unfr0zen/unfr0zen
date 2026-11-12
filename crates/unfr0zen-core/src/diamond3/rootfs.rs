//! The diamond3's part of the root filesystem patch: the USB root shell
//! (inittab, usb-gadget and `unlocked`), its OpenSSH, and root's shell profile.

use crate::patch::{check_mode, patch_shell, put, text, Options, Rootfs, Tree, PROFILE};
use crate::{device, Result};

pub(crate) const OLD_GETTY: &str = "null::respawn:/usr/sbin/unlocked-getty -L ttyGS0 115200";
/// The root shell: a login shell, so it reads /etc/profile and root's
/// profile (the prompt and aliases), with HOME set, as init gives it /.
const NEW_SHELL: &str = "ttyGS0::respawn:/usr/bin/env HOME=/root /bin/sh -l";
/// inittab's root shell line, and the same commented out with the USB root
/// shell off. The newline before each keeps one from matching inside the other.
pub(crate) const SHELL_LINES: (&str, &str) = ("\nttyGS0::respawn:/usr/bin/env HOME=/root /bin/sh -l\n",
                                              "\n#ttyGS0::respawn:/usr/bin/env HOME=/root /bin/sh -l\n");
/// The plain shell earlier releases put there, on and off, replaced by the
/// login shell.
const PLAIN_SHELL_LINES: (&str, &str) = ("\nttyGS0::respawn:/bin/sh\n", "\n#ttyGS0::respawn:/bin/sh\n");
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
    patch_ssh(tree, options.ssh, !options.ssh_key_only)?;
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
    for (plain, line) in [(PLAIN_SHELL_LINES.0, shell), (PLAIN_SHELL_LINES.1, no_shell)] {
        if inittab.matches(plain).count() == 1 {
            inittab = inittab.replace(plain, line);
        }
    }
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

/// sshd_config's root login with SSH on and root's password refused: keys only.
const KEY_ONLY_LOGIN: &str = "PermitRootLogin prohibit-password\n";

/// An SSH server at boot, root logins with a key and, with `password_login`,
/// root's password, and an ECDSA host key current clients accept. Without
/// the flag rcS does not start sshd, and sshd_config and its init script are
/// put back as stock.
pub fn patch_ssh(tree: &mut Tree, enabled: bool, password_login: bool) -> Result<()> {
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
    for (i, (name, mode, stock, ssh)) in SSH_EDITS.into_iter().enumerate() {
        let mut content = text(tree, name)?;
        // The root login line has two SSH forms: with the password, or keys only.
        let forms: &[&str] = if i == 0 { &[ssh, KEY_ONLY_LOGIN] } else { &[ssh] };
        for form in forms {
            if content.matches(form).count() == 1 {
                content = content.replace(form, stock);
            }
        }
        if content.matches(stock).count() != 1 {
            return device(format!("expected one '{}' line in {name}", stock.lines().next().unwrap_or("")));
        }
        if enabled {
            content = content.replace(stock, if i == 0 && !password_login { KEY_ONLY_LOGIN } else { ssh });
        }
        put(tree, name, content);
        check_mode(tree, name, mode)?;
    }
    Ok(())
}
