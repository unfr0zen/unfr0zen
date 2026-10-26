//! The j49's part of the root filesystem patch. Its USB gadget is built into
//! the kernel and it has no SSH server: the root shell is a script inittab
//! starts, SSH a server the run brings, and its busybox 1.19 ash takes none of
//! root's profile additions (no alias, no prompt substitution).

use crate::patch::{check_mode, patch_shell, put, text, Options, Rootfs, Tree, PROFILE};
use crate::{device, Result};

/// The files the patch requires.
pub const FILES: [&str; 5] = ["etc/shadow", "etc/inittab", "etc/init.d/rcS", "etc/init.d/rcH", "etc/nestlabs/client.config"];
/// A root shell on the USB serial port (ttyGS0, which only a kernel built
/// with CONFIG_USB_DYNAMIC_ACM has): a script started at sysinit, as busybox
/// init starts respawn entries only once rcS is done and NoLongerEvil's never
/// is. It waits for ttyGS0, so under a kernel without it it stays idle. Its
/// inittab entry is a plain command: busybox 1.19's init splits entries at
/// every colon.
pub(crate) const SHELL_LINE: &str = "::sysinit:/etc/init.d/unfr0zen-usb-shell\n";
pub(crate) const SHELL_SCRIPT: &str = "etc/init.d/unfr0zen-usb-shell";
pub(crate) const SHELL: &str = "#!/bin/sh\n# A root shell on the USB serial port, from inittab at sysinit (unfr0zen).\n\
                                while :; do\n    [ -e /dev/ttyGS0 ] && /sbin/getty -i -n -l /bin/sh -L ttyGS0 115200 vt100\n\
                                \x20   sleep 5\ndone &\n";
/// The login unfr0zen-gen2.py put there, which this shell replaces.
pub(crate) const GEN2_GETTY: &str = "# A login on the USB serial port (unfr0zen-gen2.py)\n::sysinit:/bin/sh -c \"while :; do \
                                     [ -e /dev/ttyGS0 ] && /sbin/getty -L ttyGS0 115200 vt100; sleep 5; done &\"\n";
/// rcS's stock sshd line, and the start of the server --ssh adds.
const SSH_START: (&str, &str) = ("\n${INITDIR}/sshd start\n", "${INITDIR}/unfr0zen-ssh start\n");

pub(crate) static ROOTFS: Rootfs = Rootfs {
    files: &FILES,
    // NoLongerEvil's installer leaves etc/shadow readable by all.
    shadow_modes: &[0o640, 0o644],
    optional_files,
    patch,
};

fn optional_files(options: &Options) -> Vec<String> {
    let server = options.ssh_server.iter().flat_map(|s| s.files.iter().map(|(name, _, _)| name.clone()));
    std::iter::once(SHELL_SCRIPT.to_string()).chain(server).collect()
}

fn patch(tree: &mut Tree, options: &Options) -> Result<()> {
    patch_usb_shell(tree, options.usb_shell)?;
    let server = options.ssh_server.as_ref().filter(|_| options.ssh);
    if options.ssh && server.is_none() {
        return device("--ssh on a j49 needs its SSH server's files");
    }
    patch_ssh(tree, server)?;
    if tree.contains_key(PROFILE) {
        patch_shell(tree, false)?;
    }
    Ok(())
}

/// The root shell's script and its entry at the top of inittab, or no entry.
pub(crate) fn patch_usb_shell(tree: &mut Tree, usb_shell: bool) -> Result<()> {
    let mut inittab = text(tree, "etc/inittab")?.replace(SHELL_LINE, "").replace(GEN2_GETTY, "");
    if inittab.contains("ttyGS0") {
        return device("etc/inittab: a ttyGS0 line that is not unfr0zen's");
    }
    if usb_shell {
        inittab.insert_str(0, SHELL_LINE);
        tree.insert(SHELL_SCRIPT.to_string(), (SHELL.as_bytes().to_vec(), 0o755));
    }
    put(tree, "etc/inittab", inittab);
    check_mode(tree, "etc/inittab", 0o644)
}

/// With a server given, its files and their start in rcS, after stock's sshd
/// line (whose sshd is absent); without, no start.
pub(crate) fn patch_ssh(tree: &mut Tree, server: Option<&crate::nestlocal::Overlay>) -> Result<()> {
    let (anchor, start) = SSH_START;
    let mut rcs = text(tree, "etc/init.d/rcS")?.replace(&format!("{anchor}{start}"), anchor);
    if rcs.contains(start) || rcs.matches(anchor).count() != 1 {
        return device("etc/init.d/rcS: expected one '${INITDIR}/sshd start' line, and unfr0zen's SSH start only after it");
    }
    if let Some(server) = server {
        rcs = rcs.replace(anchor, &format!("{anchor}{start}"));
        for (name, mode, data) in &server.files {
            tree.insert(name.clone(), (data.clone(), *mode));
        }
    }
    put(tree, "etc/init.d/rcS", rcs);
    check_mode(tree, "etc/init.d/rcS", 0o750)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nestlocal::Overlay;
    use crate::patch::patch_root_password;

    #[test]
    fn a_j49_gets_its_root_shell_at_sysinit_and_loses_it_again() {
        let stock = "::sysinit:/etc/init.d/rcS\n::shutdown:/etc/init.d/rcH\n";
        let mut tree = Tree::from([
            ("etc/shadow".to_string(), (b"root:*:1:0:99999:7:::\n".to_vec(), 0o644)),
            ("etc/inittab".to_string(), (stock.as_bytes().to_vec(), 0o644)),
        ]);
        patch_root_password(&mut tree, "$6$s$h", ROOTFS.shadow_modes).unwrap();
        patch_usb_shell(&mut tree, true).unwrap();
        let inittab = String::from_utf8(tree["etc/inittab"].0.clone()).unwrap();
        assert_eq!(inittab, format!("{SHELL_LINE}{stock}"));
        assert_eq!(tree[SHELL_SCRIPT], (SHELL.as_bytes().to_vec(), 0o755));
        assert!(SHELL.contains("\n    sleep 5\ndone &\n"));
        assert!(String::from_utf8_lossy(&tree["etc/shadow"].0).starts_with("root:$6$s$h:"));
        patch_usb_shell(&mut tree, true).unwrap();
        assert_eq!(tree["etc/inittab"].0, inittab.as_bytes(), "a second run adds no second line");
        // unfr0zen-gen2.py's login gives way to the shell, or to nothing.
        tree.insert("etc/inittab".into(), (format!("{GEN2_GETTY}{stock}").into_bytes(), 0o644));
        patch_usb_shell(&mut tree, false).unwrap();
        assert_eq!(tree["etc/inittab"].0, stock.as_bytes());
        tree.insert("etc/inittab".into(), (format!("ttyGS0::respawn:/bin/sh\n{stock}").into_bytes(), 0o644));
        assert!(patch_usb_shell(&mut tree, true).is_err());
    }

    #[test]
    fn a_j49_starts_the_ssh_server_it_is_given_after_stock_sshd() {
        let stock = "a\n${INITDIR}/sshd start\nb\n";
        let mut tree = Tree::from([("etc/init.d/rcS".to_string(), (stock.as_bytes().to_vec(), 0o750))]);
        let server = Overlay { directories: vec![], files: vec![("usr/sbin/dropbearmulti".into(), 0o755, b"ELF".to_vec())] };
        patch_ssh(&mut tree, Some(&server)).unwrap();
        let started = "a\n${INITDIR}/sshd start\n${INITDIR}/unfr0zen-ssh start\nb\n";
        assert_eq!(tree["etc/init.d/rcS"].0, started.as_bytes());
        assert_eq!(tree["usr/sbin/dropbearmulti"], (b"ELF".to_vec(), 0o755));
        patch_ssh(&mut tree, Some(&server)).unwrap();
        assert_eq!(tree["etc/init.d/rcS"].0, started.as_bytes(), "once");
        patch_ssh(&mut tree, None).unwrap();
        assert_eq!(tree["etc/init.d/rcS"].0, stock.as_bytes());
    }
}
