//! The root-filesystem patch, applied to a mounted slot from host copies of
//! the files it edits: only the differences reach the unit. Port of
//! scripts/patch_root0.py's edits and unfr0zen.py's patch_on_device.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use base64::{engine::general_purpose::{STANDARD, STANDARD_NO_PAD}, Engine};
use regex::Regex;

use crate::nestlocal::{self, Matter, Mqtt, Overlay, Pki};
use crate::shell::{sha256_hex, Volume};
use crate::{device, Device, Result};

pub const DEFAULT_ROOT_PASSWORD: &str = "unfr0zen";
pub const DEFAULT_NTP_SERVER: &str = "pool.ntp.org";
/// ntpd and connman's fallback name the time server; 6.4-8 has no ntpd.
const NTP_LINES: [(&str, &str); 2] = [("etc/ntp.conf", "server "), ("etc/connman/main.conf", "FallbackTimeservers = ")];
/// nlclient's backplate simulator: run it as 1H1C with made-up temperature and humidity.
const BACKPLATE_SIM: [&str; 3] = ["backplatesimulationrun", "backplatesimulationhvactype", "backplatesimulatetempandhum"];
/// A Heat Link model (Display-3.x, Display-5.x) whose radio nestlocal holds
/// has no Heat Link any more: nlclient would block on its loss. Asked for,
/// rcS sets using_wwn_boiler in nlclient's settings before it starts, so it
/// runs as heating a Works With Nest boiler through W1 instead: on, every
/// boot; off, once, if the marker says an earlier run set it. nlclient keeps
/// every key it read when it writes its settings back, so a run without it
/// has to take the key out itself; a unit that never had the block keeps its
/// stock rcS. The block stands before
/// nestlocal's start line when there is one: nestlocal's rcS edit wants its
/// line right before nestlabs's.
const WWN_BEGIN: &str = "# unfr0zen: Works With Nest boiler\n";
const WWN_END: &str = "# unfr0zen: end\n";
const WWN_SETTINGS: &str = "/media/user-config/settings.config";
const WWN_MARKER: &str = "/media/user-config/.unfr0zen-wwn-boiler";
const WWN_ON: &str = r#"case "$(fw_printenv -n nlmodel 2>/dev/null)" in Display-3.*|Display-5.*)
  [ -f SETTINGS ] || printf '%s\n' '<?xml version="1.0" encoding="ISO-8859-1"?>' '<settings>' '</settings>' > SETTINGS
  touch MARKER
  sed -i -e '/<s key="using_wwn_boiler" /d' -e 's|^</settings>|  <s key="using_wwn_boiler" value="1"/>\n</settings>|' SETTINGS
  ;;
esac
"#;
const WWN_OFF: &str = r#"if [ -f MARKER ]; then
  sed -i '/<s key="using_wwn_boiler" /d' SETTINGS
  rm -f MARKER
fi
"#;
/// (file, the line to follow, the line read-write root adds after it)
const ROOT_RW_LINES: [(&str, &str, &str); 2] = [
    ("etc/init.d/rcS", "${INITDIR}/mountfs start\n", "${MOUNT} -n -o remount,rw /\n"),
    ("etc/init.d/rcH", "${INITDIR}/umountfs stop\n", "${INITDIR}/umountroot stop\n"),
];
/// Names the firmware reaches Nest and Google by: frontdoor hands out the rest
/// (transport, logs, weather), so with it gone nlclient learns no others.
const CLOUD_HOSTS: [&str; 7] = ["frontdoor.nest.com", "home.nest.com", "www.nest.com", "time.nest.com", "tvd.nest.com",
                                "clients3.google.com", "embeddedassistant.googleapis.com"];
const HOSTS_BEGIN: &str = "# unfr0zen: block cloud\n";
const HOSTS_END: &str = "# unfr0zen: end\n";
pub(crate) const PROFILE: &str = "root/.profile";
const SHELL_BEGIN: &str = "# unfr0zen: shell\n";
const SHELL_END: &str = "# unfr0zen: end\n";
/// root's prompt as dash takes it: no bash escapes, so ESC comes from printf
/// and prompt_pwd stands in for \w, run again at each prompt. Then the usual
/// aliases, in options toybox's ls, grep, df, du and free take.
const SHELL: &str = r#"E=$(printf "\033")
prompt_pwd() { case "$PWD/" in "$HOME"/*) echo "~${PWD#"$HOME"}" ;; *) echo "$PWD" ;; esac; }
PS1="${E}[38;2;0;172;193m$(id -un) ${E}[38;2;155;215;232m\$(prompt_pwd) ${E}[1;37m\$ ${E}[0m"
alias ls='ls --color=auto'
alias ll='ls -lah --color=auto'
alias la='ls -A --color=auto'
alias grep='grep --color=auto'
alias df='df -h'
alias du='du -h'
alias free='free -h'
"#;
const CONNMAND: &str = "usr/sbin/connmand";
const CONNMAND_ORIGINAL: &str = "etc/unfr0zen/connmand.original";
/// connmand's and nlclient's internet check: Google's, and with the cloud
/// blocked Cloudflare's, as connmand resolves it itself, past etc/hosts. Each
/// is NUL-terminated and padded to the same length, to be swapped in place.
const CHECK_URLS: (&[u8], &[u8]) = (b"http://clients3.google.com/generate_204\0",
                                    b"http://cp.cloudflare.com/generate_204\0\0\0");
/// A password, CA or device name from an earlier build must not outlive it.
const STALE_SECRETS: [&str; 3] = [nestlocal::PASSWORD_FILE, nestlocal::CA_FILE, nestlocal::NAME_FILE];

/// Host copies of the volume's files: name, (contents, mode).
pub type Tree = BTreeMap<String, (Vec<u8>, u32)>;

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

pub(crate) fn text(tree: &Tree, name: &str) -> Result<String> {
    let (data, _) = tree.get(name).map_or_else(|| device(format!("missing {name}")), Ok)?;
    String::from_utf8(data.clone()).or_else(|_| device(format!("{name} is not text")))
}

pub(crate) fn put(tree: &mut Tree, name: &str, text: String) {
    let mode = tree.get(name).map_or(0o644, |(_, mode)| *mode);
    tree.insert(name.to_string(), (text.into_bytes(), mode));
}

pub(crate) fn check_mode(tree: &Tree, name: &str, mode: u32) -> Result<()> {
    match tree.get(name) {
        Some((_, actual)) if actual & 0o7777 == mode => Ok(()),
        _ => device(format!("patched {name} must be root-owned, mode {mode:o}")),
    }
}

/// nlsleepd's init script, the same on the gen 2 and the gen 3.
pub(crate) const NLSLEEP: &str = "etc/init.d/nlsleep";
/// Where the start line below preloads the library from.
const WAKE_FILTER: &str = "nestlabs/lib/libwakefilter.so";
/// How the script starts nlsleepd, and the same with the library that adds
/// nestlocal's Wi-Fi wake patterns to nlsleepd's preloaded (nestlocal puts it
/// there: crates/nestlocal/src/matter/wake_patterns.rs). The loader goes on
/// without a library that is missing.
const NLSLEEPD_START: (&str, &str) = (
    "\n ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" -a ${NLSLEEPD_APP} ",
    "\n LD_PRELOAD=/nestlabs/lib/libwakefilter.so ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" -a ${NLSLEEPD_APP} ");

/// nlclient's init script, the same on the gen 2 and the gen 3.
const NESTLABS: &str = "etc/init.d/nestlabs";
/// Where the start line below preloads the library from.
const LOCAL_CLOUD: &str = "nestlabs/lib/liblocalcloud.so";
/// How the script starts nlclient, and the same with the library that has
/// nlclient's network count as connected (platform/common/localcloud), so it
/// reaches nestlocal, its cloud on 127.0.0.1, with or without Wi-Fi.
const NLCLIENT_START: (&str, &str) = (
    "\n ${STARTDAEMON} -q -b -p \"${NLCLIENTPIDFILE}\" -a \"${NLCLIENTAPP}\" ",
    "\n LD_PRELOAD=/nestlabs/lib/liblocalcloud.so ${STARTDAEMON} -q -b -p \"${NLCLIENTPIDFILE}\" -a \"${NLCLIENTAPP}\" ");

/// nlclient started with the local cloud library on a unit running
/// nestlocal; as stock without.
fn patch_local_cloud(tree: &mut Tree, enabled: bool) -> Result<()> {
    let (stock, preloaded) = NLCLIENT_START;
    let mut script = text(tree, NESTLABS)?.replace(preloaded, stock);
    if enabled {
        if script.matches(stock).count() != 1 {
            return device(format!("expected one line starting nlclient in {NESTLABS}"));
        }
        script = script.replace(stock, preloaded);
    }
    put(tree, NESTLABS, script);
    Ok(())
}

/// nlsleepd started with the wake pattern library for a unit with Matter, so
/// that a controller looking the sleeping unit up wakes it; as stock without.
fn patch_wake_patterns(tree: &mut Tree, enabled: bool) -> Result<()> {
    let (stock, preloaded) = NLSLEEPD_START;
    let mut script = text(tree, NLSLEEP)?.replace(preloaded, stock);
    if enabled {
        if script.matches(stock).count() != 1 {
            return device(format!("expected one line starting nlsleepd in {NLSLEEP}"));
        }
        script = script.replace(stock, preloaded);
    }
    put(tree, NLSLEEP, script);
    Ok(())
}

/// SHA-512 crypt of password, as etc/shadow takes it ($6$salt$hash, 5000 rounds).
pub fn root_hash(password: &str) -> Result<String> {
    const SALT_CHARS: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    if password.is_empty() || password.contains([':', '\n']) {
        return device("root password must be one line, not empty, without ':'");
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| crate::Error::Io(e.to_string()))?;
    let salt: String = random.iter().map(|b| SALT_CHARS[(*b as usize) % SALT_CHARS.len()] as char).collect();
    let params = sha_crypt::Sha512Params::new(sha_crypt::ROUNDS_DEFAULT).expect("default rounds");
    let hash = sha_crypt::sha512_crypt_b64(password.as_bytes(), salt.as_bytes(), &params)
        .or_else(|e| device(format!("root password hash: {e:?}")))?;
    Ok(format!("$6${salt}${hash}"))
}

const AUTHORIZED_KEYS: &str = "root/.ssh/authorized_keys";

/// Checks one OpenSSH public key and normalizes its type and base64 encoding.
pub fn ssh_public_key(data: &[u8]) -> Result<String> {
    let invalid = || crate::Error::Device("--ssh-public-key: expected one valid OpenSSH public key (Ed25519, ECDSA or RSA)".into());
    let line = std::str::from_utf8(data).map_err(|_| invalid())?.trim();
    if line.contains(['\n', '\r', '\0']) { return Err(invalid()); }
    let mut fields = line.split_whitespace();
    let kind = fields.next().ok_or_else(invalid)?;
    let encoded = fields.next().ok_or_else(invalid)?;
    let blob = STANDARD.decode(encoded).or_else(|_| STANDARD_NO_PAD.decode(encoded)).map_err(|_| invalid())?;
    let mut remaining = blob.as_slice();
    fn field<'a>(data: &mut &'a [u8]) -> Option<&'a [u8]> {
        let length = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
        let value = data.get(4..)?.get(..length)?;
        *data = data.get(4..)?.get(length..)?;
        Some(value)
    }
    if field(&mut remaining) != Some(kind.as_bytes()) { return Err(invalid()); }
    let valid = match kind {
        "ssh-ed25519" => field(&mut remaining).is_some_and(|key| key.len() == 32),
        "ssh-rsa" => {
            let positive = |value: &[u8]| !value.is_empty() && value[0] & 0x80 == 0
                && value.iter().any(|b| *b != 0) && (value[0] != 0 || value.get(1).is_some_and(|b| b & 0x80 != 0));
            field(&mut remaining).is_some_and(positive) && field(&mut remaining).is_some_and(positive)
        }
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => {
            let curve = kind.strip_prefix("ecdsa-sha2-").unwrap();
            let size = match curve { "nistp256" => 65, "nistp384" => 97, _ => 133 };
            field(&mut remaining) == Some(curve.as_bytes())
                && field(&mut remaining).is_some_and(|point| point.len() == size && point[0] == 4)
        }
        _ => false,
    };
    if !valid || !remaining.is_empty() { return Err(invalid()); }
    Ok(format!("{kind} {}", STANDARD.encode(blob)))
}

fn patch_authorized_key(tree: &mut Tree, key: &str) -> Result<()> {
    let mut contents = if tree.contains_key(AUTHORIZED_KEYS) { text(tree, AUTHORIZED_KEYS)? } else { String::new() };
    let present = contents.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let pair = fields.next().zip(fields.next());
        pair.is_some_and(|(kind, encoded)| ssh_public_key(format!("{kind} {encoded}").as_bytes()).is_ok_and(|existing| existing == key))
    });
    if !present {
        if !contents.is_empty() && !contents.ends_with('\n') { contents.push('\n'); }
        contents.push_str(key);
        contents.push('\n');
    }
    tree.insert(AUTHORIZED_KEYS.into(), (contents.into_bytes(), 0o600));
    Ok(())
}

pub fn check_ntp_server(host: &str) -> Result<()> {
    static HOST: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$"));
    if HOST.is_match(host) { Ok(()) } else { device(format!("not a host name: {host}")) }
}

pub(crate) fn patch_root_password(tree: &mut Tree, password_hash: &str, modes: &[u32]) -> Result<()> {
    let shadow = text(tree, "etc/shadow")?;
    let roots = shadow.split_inclusive('\n').filter(|line| line.starts_with("root:")).count();
    if roots != 1 {
        return device("expected exactly one root account in etc/shadow");
    }
    let shadow: String = shadow.split_inclusive('\n').map(|line| {
        if !line.starts_with("root:") {
            return line.to_string();
        }
        let mut fields: Vec<&str> = line.split(':').collect();
        fields[1] = password_hash;
        fields.join(":")
    }).collect();
    put(tree, "etc/shadow", shadow);
    match tree.get("etc/shadow") {
        Some((_, mode)) if modes.contains(&(mode & 0o7777)) => Ok(()),
        _ => device("patched etc/shadow must be root-owned, mode 640"),
    }
}

pub fn patch_ntp(tree: &mut Tree, server: &str) -> Result<()> {
    let present: Vec<_> = NTP_LINES.iter().filter(|(name, _)| tree.contains_key(*name)).collect();
    if present.is_empty() {
        return device("neither etc/ntp.conf nor etc/connman/main.conf is in the image");
    }
    for (name, prefix) in present {
        let content = text(tree, name)?;
        let line = regex(&format!(r"(?m)^{}\S+\n", regex::escape(prefix)));
        if line.find_iter(&content).count() != 1 {
            return device(format!("expected one '{prefix}HOST' line in {name}"));
        }
        put(tree, name, line.replace(&content, regex::NoExpand(&format!("{prefix}{server}\n"))).into_owned());
        check_mode(tree, name, 0o644)?;
    }
    Ok(())
}

/// Without the flag the simulator is set off, as in stock, so a run without
/// it undoes an earlier one.
pub fn patch_backplate_sim(tree: &mut Tree, enabled: bool) -> Result<()> {
    let mut content = text(tree, "etc/nestlabs/client.config")?;
    let value = if enabled { "1" } else { "0" };
    for key in BACKPLATE_SIM {
        let setting = regex(&format!(r#"<a key="{key}" value="[^"]*"/>"#));
        if setting.find_iter(&content).count() != 1 {
            return device(format!("expected one {key} setting in client.config"));
        }
        content = setting.replace(&content, regex::NoExpand(&format!(r#"<a key="{key}" value="{value}"/>"#))).into_owned();
    }
    put(tree, "etc/nestlabs/client.config", content);
    Ok(())
}

/// Remounts root read-write at boot, and adds umountroot at shutdown so a
/// reboot leaves it clean. Without the flag both lines are taken out.
pub fn patch_root_rw(tree: &mut Tree, enabled: bool) -> Result<()> {
    for (name, anchor, line) in ROOT_RW_LINES {
        let mut content = text(tree, name)?;
        if content.matches(anchor).count() != 1 {
            return device(format!("expected one '{}' line in {name}", anchor.trim()));
        }
        content = content.replace(&format!("{anchor}{line}"), anchor);
        if content.contains(line) {
            return device(format!("{name}: '{}' somewhere other than after '{}'", line.trim(), anchor.trim()));
        }
        if enabled {
            content = content.replace(anchor, &format!("{anchor}{line}"));
        }
        put(tree, name, content);
        check_mode(tree, name, 0o750)?;
    }
    Ok(())
}

/// Puts the Works With Nest boiler block in rcS, setting using_wwn_boiler
/// when asked and taking it back when not (WWN_ON, WWN_OFF).
pub fn patch_wwn_boiler(tree: &mut Tree, enabled: bool) -> Result<()> {
    let mut content = text(tree, "etc/init.d/rcS")?;
    let Some(start) = content.find(WWN_BEGIN) else {
        if !enabled {
            return Ok(());
        }
        return install_wwn_boiler(tree, content, enabled);
    };
    let end = content[start..].find(WWN_END)
        .map_or_else(|| device("etc/init.d/rcS: unfr0zen boiler block without its end line"), |end| Ok(start + end + WWN_END.len()))?;
    content.replace_range(start..end, "");
    install_wwn_boiler(tree, content, enabled)
}

fn install_wwn_boiler(tree: &mut Tree, mut content: String, enabled: bool) -> Result<()> {
    let anchor = ["${INITDIR}/nestlocal start\n${INITDIR}/nestlabs start\n", "${INITDIR}/nestlabs start\n"]
        .into_iter().find(|anchor| content.contains(anchor))
        .ok_or_else(|| crate::Error::Device("etc/init.d/rcS: expected a nestlabs start line".into()))?;
    if content.matches(anchor).count() != 1 {
        return device("etc/init.d/rcS: expected one nestlabs start line");
    }
    let script = if enabled { WWN_ON } else { WWN_OFF };
    let block = format!("{WWN_BEGIN}{}{WWN_END}", script.replace("SETTINGS", WWN_SETTINGS).replace("MARKER", WWN_MARKER));
    content = content.replace(anchor, &format!("{block}{anchor}"));
    put(tree, "etc/init.d/rcS", content);
    check_mode(tree, "etc/init.d/rcS", 0o750)
}

/// Resolves the cloud's names to nowhere in etc/hosts, which the resolver reads
/// before DNS. Without the flag the block is taken out.
pub fn patch_block_cloud(tree: &mut Tree, enabled: bool) -> Result<()> {
    let mut content = text(tree, "etc/hosts")?;
    if let Some(start) = content.find(HOSTS_BEGIN) {
        let end = content[start..].find(HOSTS_END)
            .map_or_else(|| device("etc/hosts: unfr0zen block without its end line"), |end| Ok(start + end + HOSTS_END.len()))?;
        content.replace_range(start..end, "");
    }
    if enabled {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content += HOSTS_BEGIN;
        for host in CLOUD_HOSTS {
            content += &format!("0.0.0.0 {host}\n:: {host}\n");
        }
        content += HOSTS_END;
    }
    put(tree, "etc/hosts", content);
    check_mode(tree, "etc/hosts", 0o644)
}

/// root's shell: a coloured prompt with the user and working directory, and
/// the usual aliases, at the end of root/.profile. Disabled, the block is
/// taken out.
pub fn patch_shell(tree: &mut Tree, enabled: bool) -> Result<()> {
    let mut content = text(tree, PROFILE)?;
    if let Some(start) = content.find(SHELL_BEGIN) {
        let end = content[start..].find(SHELL_END)
            .map_or_else(|| device(format!("{PROFILE}: unfr0zen block without its end line")), |end| Ok(start + end + SHELL_END.len()))?;
        content.replace_range(start..end, "");
    }
    if enabled {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content += SHELL_BEGIN;
        content += SHELL;
        content += SHELL_END;
    }
    put(tree, PROFILE, content);
    check_mode(tree, PROFILE, 0o644)
}

/// Points a binary's internet check at Cloudflare with the cloud blocked, and
/// back at Google without. A binary with neither URL is left as it is.
fn patch_check_url(data: &mut [u8], name: &str, block_cloud: bool) -> Result<()> {
    let (google, cloudflare) = CHECK_URLS;
    let (from, to) = if block_cloud { (google, cloudflare) } else { (cloudflare, google) };
    let at: Vec<usize> = data.windows(from.len()).enumerate().filter(|(_, w)| *w == from).map(|(i, _)| i).collect();
    let present = data.windows(to.len()).filter(|w| *w == to).count();
    match (at.as_slice(), present) {
        ([at], 0) => data[*at..*at + to.len()].copy_from_slice(to),
        ([], 0) => log::warn!("{name}: no internet check URL; left as it is"),
        ([], 1) => {}
        _ => return device(format!("expected one internet check URL in {name}")),
    }
    Ok(())
}

/// A device's part of the root filesystem patch.
pub(crate) struct Rootfs {
    /// The files the patch requires.
    pub files: &'static [&'static str],
    /// The modes etc/shadow may have.
    pub shadow_modes: &'static [u32],
    /// Files the patch may add, for the run's options.
    pub optional_files: fn(&Options) -> Vec<String>,
    /// The USB root shell, SSH and root's shell profile, as the device has them.
    pub patch: fn(&mut Tree, &Options) -> Result<()>,
}

fn rootfs(target: Device) -> Result<&'static Rootfs> {
    match target {
        Device::Diamond3 => Ok(&crate::diamond3::rootfs::ROOTFS),
        Device::Diamond | Device::J49 => Ok(&crate::j49::rootfs::ROOTFS),
        Device::Onyx => device("Nest Thermostat E (onyx) is not supported"),
    }
}

/// What a run asks for, beyond the root shell, read-write root and SSH,
/// which it always applies.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// From root_hash.
    pub root_hash: String,
    /// The app's version, for the UNFR0ZEN screen to show.
    pub version: String,
    /// An empty name means DEFAULT_NTP_SERVER.
    pub ntp_server: Option<String>,
    pub backplate_sim: bool,
    /// Nest's and Google's names resolve to nowhere, and connmand and
    /// nlclient check for the internet at Cloudflare instead.
    pub block_cloud: bool,
    pub mqtt: Option<Mqtt>,
    pub matter: Option<Matter>,
    /// The name MQTT and Matter give the unit; None keeps the unit's.
    pub device_name: Option<String>,
    /// The library nlsleepd preloads for Matter (platform/common/wakefilter):
    /// installed with Matter on a gen 2 or gen 3, removed without.
    pub wake_filter: Vec<u8>,
    /// The library preloaded into nlclient on a unit running nestlocal
    /// (platform/common/localcloud), so nlclient reaches it without Wi-Fi.
    pub local_cloud: Vec<u8>,
    /// Files for the optional onboard Thread transport.
    pub thread: Option<Overlay>,
    /// nestlocal joins a Zigbee network through the unit's radio, which it
    /// then has: wpantund stays down.
    pub zigbee: bool,
    /// Run a Heat Link model as heating a Works With Nest boiler through W1,
    /// for a radio nestlocal holds.
    pub wwn_boiler: bool,
    /// Doom's files: the UNFR0ZEN screen offers it and nestlocal launches it.
    pub doom: Option<Overlay>,
    /// Regulate from nestlocal's virtual sensor: BLE on gen 3, or the
    /// reviewed native RCS input and Sensors selector patch on gen 2.
    pub external_temperature: bool,
    /// The relocatable native Sensors view helper, linked only on j49.
    pub external_temperature_screen: Vec<u8>,
    /// Gen 2's settings screens in the gen 3's dark palette, through the
    /// reviewed nlclient 5.9.4-5 patch; other generations ignore it.
    pub dark_theme: bool,
    /// An SSH server root logs in to with its password; without it rcS does
    /// not start sshd and sshd's files are stock.
    pub ssh: bool,
    /// A validated OpenSSH public key to merge into root's authorized_keys; needs SSH.
    pub ssh_public_key: Option<String>,
    /// SSH refuses root's password: only a key logs in. Off, root logs in
    /// with its password too (DEFAULT_ROOT_PASSWORD unless one is given).
    pub ssh_key_only: bool,
    /// The SSH server's files for a device without one of its own (the j49).
    pub ssh_server: Option<Overlay>,
    /// The USB root shell: an ACM port with a root shell on it. Without it
    /// the unit shows only mass storage over USB.
    pub usb_shell: bool,
}

/// One change to the volume: action, SHA-256 before and after.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub action: &'static str,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// The changes as initramfs-changes.txt lists them.
pub fn changes_text(changes: &[Change]) -> String {
    changes.iter().map(|c| format!("{} {}: {} -> {}\n", c.action, c.name,
                                   c.old.as_deref().unwrap_or("-"), c.new.as_deref().unwrap_or("-"))).collect()
}

type Originals = BTreeMap<String, Option<(String, u32, String)>>;

/// Records the unit's files: contents into the tree, or only their SHA-256.
fn fetch(volume: &mut impl Volume, tree: &mut Tree, originals: &mut Originals, names: &[&str],
         required: bool, contents: bool) -> Result<()> {
    for &name in names {
        if originals.contains_key(name) {
            continue;
        }
        let found = if contents {
            volume.read(name)?.map(|(owner, mode, data)| {
                let sha = sha256_hex(&data);
                tree.insert(name.to_string(), (data, mode));
                (owner, mode, sha)
            })
        } else {
            volume.inspect(name)?
        };
        match &found {
            None if required => return device(format!("{name} is missing from the device root volume")),
            Some((owner, _, _)) if !owner.starts_with("0:") => {
                return device(format!("{name} on the device is not owned by root"));
            }
            _ => {}
        }
        originals.insert(name.to_string(), found);
    }
    Ok(())
}

/// Patches host copies of the device's files in patch_root0.py's order, with
/// every option it takes, then (unless dry_run) applies the differences on
/// the unit and verifies them on flash. The host never parses UBIFS.
///
/// nestlocal: the ARMv7 binary --mqtt and --matter install. new_pki: a key set for a
/// unit without its own.
/// build_screen: the settings screen's undo journal for the unit's stock
/// nlclient, as nlclient::build makes it.
#[allow(clippy::too_many_arguments)]
pub fn patch_on_device(volume: &mut impl Volume, target: Device, options: &Options, nestlocal_binary: &[u8],
                       new_pki: &mut dyn FnMut() -> Result<Pki>,
                       dry_run: bool, build_screen: &dyn Fn(&[u8]) -> Result<Vec<u8>>) -> Result<Vec<Change>> {
    let rootfs = rootfs(target)?;
    if options.thread.is_some() && (options.matter.is_none() || options.zigbee) {
        return device("Matter over Thread requires Matter and exclusive radio access");
    }
    if options.ssh_key_only && (!options.ssh || options.ssh_public_key.is_none()) {
        return device("SSH without password login needs SSH and a public key to log in with");
    }
    if options.ssh_public_key.is_some() && !options.ssh {
        return device("--ssh-public-key needs --ssh");
    }
    let public_key = options.ssh_public_key.as_deref().map(|key| ssh_public_key(key.as_bytes())).transpose()?;
    // What patch_root0.check_source asks of an extracted tree: here, that the
    // mount is a real application root before anything is written into it.
    if volume.shell_binary()? != "0 755" {
        return device("the mounted volume has no root-owned mode-755 bin/busybox or bin/toybox");
    }
    // nlclient's journal carries its internet check URL too, so undo and
    // upgrades take it back with the screen.
    let build_screen = &|source: &[u8]| -> Result<Vec<u8>> {
        let journal = build_screen(source)?;
        let external_temperature = matches!(target, Device::Diamond | Device::J49) && options.external_temperature;
        let dark_theme = target == Device::J49 && options.dark_theme;
        if !options.block_cloud && !external_temperature && !dark_theme {
            return Ok(journal);
        }
        let mut output = crate::nlclient::Patch::parse(&journal)?.apply(source, false)?;
        if external_temperature {
            crate::nlclient::external_temperature::patch(source, &mut output, &options.external_temperature_screen)?;
        }
        if dark_theme {
            crate::nlclient::dark_theme::patch(source, &mut output)?;
        }
        if options.block_cloud {
            patch_check_url(&mut output, crate::nlclient::BINARY, true)?;
        }
        crate::nlclient::Patch::journal(source, &output)
    };
    let (mut tree, mut originals) = (Tree::new(), Originals::new());
    fetch(volume, &mut tree, &mut originals, rootfs.files, true, true)?;
    let optional = (rootfs.optional_files)(options);
    fetch(volume, &mut tree, &mut originals, &optional.iter().map(String::as_str).collect::<Vec<_>>(), false, false)?;
    patch_root_password(&mut tree, &options.root_hash, rootfs.shadow_modes)?;
    if public_key.is_some() {
        fetch(volume, &mut tree, &mut originals, &[AUTHORIZED_KEYS], false, true)?;
    }
    if let Some(server) = &options.ntp_server {
        let names: Vec<&str> = NTP_LINES.iter().map(|(name, _)| *name).collect();
        fetch(volume, &mut tree, &mut originals, &names, false, true)?;
        patch_ntp(&mut tree, if server.is_empty() { DEFAULT_NTP_SERVER } else { server })?;
    }
    let mut overlay = None;
    // An installed nestlocal is the unit's cloud, so it stays; a run with no
    // integration rewrites its files with them all off, which also puts
    // Nest's Thread radio set-up back after Zigbee.
    let turn_off = volume.inspect(nestlocal::OPTIONS)?.is_some();
    if options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some() || options.external_temperature || turn_off {
        fetch(volume, &mut tree, &mut originals, &nestlocal::STOCK_FILES, true, true)?;
        fetch(volume, &mut tree, &mut originals, &[nestlocal::UNLOCK_BUNDLE], false, true)?;
        if options.matter.is_some() {
            fetch(volume, &mut tree, &mut originals, &[nestlocal::BLE_FIRMWARE], false, true)?;
        }
        fetch(volume, &mut tree, &mut originals, &["etc/nestlocal/server.crt", "etc/nestlocal/server.key", nestlocal::RANDOM_SEED], false, true)?;
        let own = |name: &str| tree.get(name).map(|(data, _)| data.as_slice());
        let pki = match nestlocal::unit_pki(own("etc/nestlocal/server.crt"), own("etc/nestlocal/server.key"),
                                            own("etc/ssl/certs/ca-bundle.pem")) {
            Some(pki) => {
                log::info!("nestlocal keeps the unit's own certificates");
                pki
            }
            None => new_pki()?,
        };
        overlay = Some(nestlocal::overlay(|name| tree.get(name).cloned(), options.mqtt.as_ref(),
                                          options.matter.as_ref(), options.zigbee || options.thread.is_some(),
                                          options.device_name.as_deref(), nestlocal_binary, &pki)?);
        if let Some(thread) = &options.thread {
            let overlay = overlay.as_mut().unwrap();
            let (_, _, line) = overlay.files.iter_mut().find(|(name, _, _)| name == nestlocal::OPTIONS).unwrap();
            *line = nestlocal::options(options.mqtt.as_ref(), options.matter.as_ref(), false, options.device_name.as_deref())?.0;
            line.pop();
            line.extend_from_slice(b" -matter-thread\n");
            overlay.files.extend(thread.files.clone());
            overlay.directories.extend(thread.directories.clone());
            let (_, _, monit) = overlay.files.iter_mut().find(|(name, _, _)| name == "etc/monit.d/nestlocal.monitrc").unwrap();
            *monit = String::from_utf8(monit.clone()).unwrap()
                .replace("start program = \"/etc/init.d/nestlocal start\"",
                         "start program = \"/etc/init.d/nestlocal start\" with timeout 180 seconds").into_bytes();
            monit.extend_from_slice(include_bytes!("../../nestlocal/rootfs/thread.monitrc"));
        }
        if let Some(doom) = &options.doom {
            let overlay = overlay.as_mut().unwrap();
            let (_, _, line) = overlay.files.iter_mut().find(|(name, _, _)| name == nestlocal::OPTIONS).unwrap();
            line.pop();
            line.extend_from_slice(b" -doom\n");
            overlay.files.extend(doom.files.clone());
            overlay.directories.extend(doom.directories.clone());
        }
        if options.external_temperature {
            let overlay = overlay.as_mut().unwrap();
            let (_, _, line) = overlay.files.iter_mut().find(|(name, _, _)| name == nestlocal::OPTIONS).unwrap();
            line.pop();
            line.extend_from_slice(b" -external-temperature\n");
            if target == Device::Diamond3 {
                let (_, _, product) = overlay.files.iter_mut().find(|(name, _, _)| name == "etc/nestlabs/product.config").unwrap();
                *product = nestlocal::stock_ble_on(&String::from_utf8_lossy(product)).into_bytes();
            }
        }
    }
    let mut directories = Vec::new();
    if let Some(overlay) = &overlay {
        fetch(volume, &mut tree, &mut originals, &STALE_SECRETS, false, false)?;
        for name in STALE_SECRETS {
            tree.remove(name);
        }
        let names: Vec<&str> = overlay.files.iter().map(|(name, _, _)| name.as_str()).collect();
        fetch(volume, &mut tree, &mut originals, &names, false, false)?;
        for (name, mode, data) in &overlay.files {
            tree.insert(name.clone(), (data.clone(), *mode));
        }
        directories = overlay.directories.clone();
    }
    patch_backplate_sim(&mut tree, options.backplate_sim)?;
    fetch(volume, &mut tree, &mut originals, &["etc/hosts"], options.block_cloud, true)?;
    if tree.contains_key("etc/hosts") {
        patch_block_cloud(&mut tree, options.block_cloud)?;
    }
    fetch(volume, &mut tree, &mut originals, &[PROFILE], false, true)?;
    if matches!(target, Device::Diamond3 | Device::J49) {
        fetch(volume, &mut tree, &mut originals, &[NLSLEEP], false, true)?;
        fetch(volume, &mut tree, &mut originals, &[WAKE_FILTER], false, false)?;
        let wake = options.matter.is_some() && !options.wake_filter.is_empty();
        if wake {
            tree.insert(WAKE_FILTER.into(), (options.wake_filter.clone(), 0o755));
        }
        if tree.contains_key(NLSLEEP) {
            patch_wake_patterns(&mut tree, wake)?;
        }
    }
    // nestlocal is nlclient's cloud: nlclient has to reach it whatever its network.
    fetch(volume, &mut tree, &mut originals, &[NESTLABS], false, true)?;
    fetch(volume, &mut tree, &mut originals, &[LOCAL_CLOUD], false, false)?;
    if overlay.is_some() {
        if options.local_cloud.is_empty() {
            return device("nestlocal needs the local cloud library for nlclient");
        }
        tree.insert(LOCAL_CLOUD.into(), (options.local_cloud.clone(), 0o755));
    }
    if tree.contains_key(NESTLABS) {
        patch_local_cloud(&mut tree, overlay.is_some())?;
    } else if overlay.is_some() {
        return device(format!("{NESTLABS} is missing: nlclient could not reach nestlocal"));
    }
    patch_root_rw(&mut tree, true)?;
    patch_wwn_boiler(&mut tree, options.wwn_boiler)?;
    (rootfs.patch)(&mut tree, options)?;
    if let Some(key) = &public_key {
        patch_authorized_key(&mut tree, key)?;
        directories.retain(|(name, _)| name != "root/.ssh");
        directories.push(("root/.ssh".into(), 0o700));
    }

    let screen = volume.inspect(crate::nlclient::BINARY)?;
    if matches!(target, Device::Diamond | Device::J49) && options.external_temperature && screen.is_none() {
        return device("j49 external temperature requires nlclient");
    }
    if target == Device::J49 && options.dark_theme && screen.is_none() {
        return device("j49 dark theme requires nlclient");
    }
    let mut screen_patch = None;
    if let Some((owner, mode, sha)) = &screen {
        if !owner.starts_with("0:") || mode & 0o111 == 0 {
            return device("nlclient must be a root-owned executable");
        }
        let (_, _, mut source) = volume.read(crate::nlclient::BINARY)?
            .ok_or_else(|| crate::Error::Device("nlclient disappeared during inspection".into()))?;
        if sha256_hex(&source) != *sha { return device("nlclient changed during inspection"); }
        fetch(volume, &mut tree, &mut originals, &[crate::nlclient::JOURNAL], false, true)?;
        let mut previous = tree.get(crate::nlclient::JOURNAL)
            .map(|(data, _)| crate::nlclient::Patch::parse(data)).transpose()?;
        // nlclient replaced but its journal never written: the original backup
        // is the source, if it builds to exactly the executable on the unit.
        let mut rebuilt = None;
        if previous.as_ref().is_none_or(|p| p.output_sha != *sha && p.stock_sha != *sha) {
            fetch(volume, &mut tree, &mut originals, &[crate::nlclient::ORIGINAL], false, false)?;
            if let Some((_, _, backup_sha)) = originals.get(crate::nlclient::ORIGINAL).and_then(Option::as_ref)
                .filter(|(_, _, backup_sha)| backup_sha != sha && previous.as_ref().is_none_or(|p| p.stock_sha == *backup_sha)).cloned() {
                let (_, _, original) = volume.read(crate::nlclient::ORIGINAL)?
                    .ok_or_else(|| crate::Error::Device("nlclient original backup disappeared during inspection".into()))?;
                if sha256_hex(&original) != backup_sha { return device("nlclient original backup changed during inspection"); }
                let journal = if original.starts_with(b"\x7fELF") { build_screen(&original).ok() } else { None };
                if journal.as_deref().and_then(|j| crate::nlclient::Patch::parse(j).ok()).is_some_and(|p| p.output_sha == *sha) {
                    log::warn!("nlclient has no undo journal for it; rebuilding it from the original backup");
                    (source, previous, rebuilt) = (original, None, journal);
                }
            }
        }
        if let Some(previous) = &previous {
            if previous.output_sha == *sha { source = previous.apply(&source, true)?; }
            else if previous.stock_sha != *sha { return device("nlclient undo journal does not match the executable"); }
        }
        if matches!(target, Device::Diamond | Device::J49) && options.external_temperature && !source.starts_with(b"\x7fELF") {
            return device("j49 external temperature requires an ARM ELF nlclient");
        }
        if target == Device::J49 && options.dark_theme && !source.starts_with(b"\x7fELF") {
            return device("j49 dark theme requires an ARM ELF nlclient");
        }
        if source.starts_with(b"\x7fELF") {
            // Preserve the stock executable once, including on devices patched
            // by older versions where only the undo journal was retained.
            fetch(volume, &mut tree, &mut originals, &[crate::nlclient::ORIGINAL], false, false)?;
            if let Some((_, _, backup_sha)) = originals.get(crate::nlclient::ORIGINAL).and_then(Option::as_ref) {
                if *backup_sha != sha256_hex(&source) {
                    return device("nlclient original backup does not match this firmware; leaving it untouched");
                }
            }
            let journal = match rebuilt { Some(journal) => journal, None => build_screen(&source)? };
            let current = crate::nlclient::Patch::parse(&journal)?;
            let patch = if let Some(previous) = previous.filter(|p| p.output_sha == *sha) {
                crate::nlclient::Patch::upgrade(&previous, current)?
            } else { current };
            fetch(volume, &mut tree, &mut originals, &[crate::nlclient::ICON], false, false)?;
            fetch(volume, &mut tree, &mut originals, &[crate::nlclient::GLYPH], false, false)?;
            fetch(volume, &mut tree, &mut originals, &[crate::nlclient::VERSION], false, false)?;
            tree.insert(crate::nlclient::JOURNAL.into(), (journal, 0o600));
            tree.insert(crate::nlclient::ICON.into(), (crate::nlclient::ICON_BYTES.to_vec(), 0o644));
            tree.insert(crate::nlclient::GLYPH.into(), (crate::nlclient::GLYPH_BYTES.to_vec(), 0o644));
            tree.insert(crate::nlclient::VERSION.into(), (format!("unfr0zen {}\n", options.version).into_bytes(), 0o644));
            if originals.get(crate::nlclient::ORIGINAL).is_some_and(Option::is_none) {
                tree.insert(crate::nlclient::ORIGINAL.into(), (source, 0o600));
            }
            directories.push(("etc/unfr0zen".into(), 0o755));
            screen_patch = Some(patch);
        } else {
            tree.remove(crate::nlclient::JOURNAL);
            log::warn!("UNFR0ZEN screen skipped: nlclient is not an ELF executable");
        }
    } else { log::warn!("UNFR0ZEN screen skipped: nlclient is absent"); }
    patch_connmand(volume, &mut tree, &mut originals, &mut directories, options.block_cloud)?;
    patch_boot_artwork(volume, &mut tree, &mut originals, &mut directories)?;
    let patch_screen = screen_patch.as_ref().is_some_and(|patch|
        screen.as_ref().unwrap().2 != patch.output_sha);

    let mut writes: Vec<(String, &[u8], String, u32)> = Vec::new();
    for (name, (data, mode)) in &tree {
        let old = originals.get(name).cloned().flatten();
        let key_file = name == AUTHORIZED_KEYS && public_key.is_some();
        if old.as_ref().is_none_or(|(owner, old_mode, sha)| (*old_mode, sha.as_str()) != (*mode, sha256_hex(data).as_str())
            || (key_file && owner != "0:0")) {
            let owner = if key_file { "0:0".to_string() } else { old.map_or("0:0".to_string(), |(owner, _, _)| owner) };
            writes.push((name.clone(), data, owner, *mode));
        }
    }
    let removals: Vec<&str> = STALE_SECRETS.iter().copied().chain([WAKE_FILTER, LOCAL_CLOUD])
        .filter(|name| originals.get(*name).is_some_and(Option::is_some) && !tree.contains_key(*name)).collect();
    let mut folders = Vec::new();
    for (name, mode) in directories {
        if volume.directory(&name)? != Some(format!("0:0 {mode:o}")) {
            folders.push((name, mode));
        }
    }

    let mut changes: Vec<Change> = folders.iter().map(|(name, mode)| Change {
        name: format!("{name}/"), action: "directory", old: None, new: Some(format!("{mode:o}")),
    }).collect();
    for (name, data, _, _) in &writes {
        let old = originals.get(name).cloned().flatten().map(|(_, _, sha)| sha);
        changes.push(Change { name: name.clone(), action: if old.is_some() { "write" } else { "create" },
                              old, new: Some(sha256_hex(data)) });
    }
    for name in &removals {
        let old = originals.get(*name).cloned().flatten().map(|(_, _, sha)| sha);
        changes.push(Change { name: name.to_string(), action: "remove", old, new: None });
    }
    if patch_screen {
        changes.push(Change { name: crate::nlclient::BINARY.into(), action: "patch",
                              old: Some(screen_patch.as_ref().unwrap().stock_sha.clone()), new: Some(screen_patch.as_ref().unwrap().output_sha.clone()) });
    }
    // Match the write order: backups, other files, staged nlclient, undo journal.
    let mut sizes = Vec::new();
    for (name, data, _, _) in writes.iter().filter(|(name, _, _, _)| is_boot_backup(name))
        .chain(writes.iter().filter(|(name, _, _, _)| !is_boot_backup(name) && name != crate::nlclient::JOURNAL)) {
        let old = if originals.get(name).is_some_and(Option::is_some) { volume.file_size(name)? } else { 0 };
        sizes.push((data.len() as u64, old));
    }
    if patch_screen {
        let patch = screen_patch.as_ref().unwrap();
        sizes.push((patch.stock_len.max(patch.output_len) as u64, patch.stock_len as u64));
    }
    for (name, data, _, _) in writes.iter().filter(|(name, _, _, _)| name == crate::nlclient::JOURNAL) {
        let old = if originals.get(name).is_some_and(Option::is_some) { volume.file_size(name)? } else { 0 };
        sizes.push((data.len() as u64, old));
    }
    let needed = staging_space(&sizes) + (1 << 20);
    if (!writes.is_empty() || patch_screen) && volume.checks_space() {
        let available = volume.free_bytes()?;
        log::info!("Root volume free space: {available} bytes; peak staging requirement: {needed} bytes");
        if available < needed {
            return device(format!("the volume has too little free space: {available} bytes available, {needed} bytes required including staging and reserve"));
        }
    }
    if dry_run || changes.is_empty() {
        return Ok(changes);
    }

    volume.remount(true)?;
    for (name, mode) in &folders {
        volume.make_directory(name, *mode)?;
    }
    // Boot originals must reach flash before any artwork or kernel replacement.
    for (name, data, owner, mode) in writes.iter().filter(|(name, _, _, _)| is_boot_backup(name)) {
        check_staging_space(volume, data.len() as u64)?;
        volume.write(name, data, owner, *mode)?;
        if volume.inspect(name)? != Some((owner.clone(), *mode, sha256_hex(data))) {
            return device(format!("{name}: boot artwork backup did not read back correctly from flash"));
        }
    }
    for (name, data, owner, mode) in &writes {
        if name != crate::nlclient::JOURNAL && !is_boot_backup(name) {
            check_staging_space(volume, data.len() as u64)?;
            volume.write(name, data, owner, *mode)?;
            // Confirm the full backup reached flash before replacing nlclient.
            if name == crate::nlclient::ORIGINAL && volume.inspect(name)? !=
                Some((owner.clone(), *mode, sha256_hex(data))) {
                return device("nlclient original backup did not read back correctly from flash");
            }
            // The same for connmand, whose backup sorts before it.
            if name == CONNMAND_ORIGINAL && volume.inspect(name)? !=
                Some((owner.clone(), *mode, sha256_hex(data))) {
                return device("connmand original backup did not read back correctly from flash");
            }
        }
    }
    for name in &removals {
        volume.remove(name)?;
    }
    if patch_screen {
        let patch = screen_patch.as_ref().unwrap();
        check_staging_space(volume, patch.stock_len.max(patch.output_len) as u64)?;
        volume.patch_nlclient(patch)?;
    }
    // Keep the prior undo journal until the binary replacement succeeds.
    for (name, data, owner, mode) in &writes {
        if name == crate::nlclient::JOURNAL {
            check_staging_space(volume, data.len() as u64)?;
            volume.write(name, data, owner, *mode)?;
        }
    }
    volume.remount(false)?;
    if patch_screen {
        let result = volume.inspect(crate::nlclient::BINARY)?;
        if result.as_ref().map(|(owner, mode, sha)| (owner, mode, sha)) !=
           screen.as_ref().map(|(owner, mode, _)| (owner, mode, &screen_patch.as_ref().unwrap().output_sha)) {
            return device("nlclient patch did not read back correctly from flash");
        }
    }
    if !writes.is_empty() {
        let names: Vec<String> = writes.iter().map(|(name, _, _, _)| name.clone()).collect();
        for (name, (sha, owner_mode)) in volume.digests(&names)? {
            let (_, data, owner, mode) = writes.iter().find(|(n, _, _, _)| *n == name).expect("asked for");
            if sha.as_deref() != Some(sha256_hex(data).as_str()) || owner_mode.as_deref() != Some(format!("{owner} {mode:o}").as_str()) {
                return device(format!("{name} readback from flash does not match: {sha:?} {owner_mode:?}"));
            }
        }
    }
    for name in &removals {
        if volume.inspect(name)?.is_some() {
            return device(format!("{name} is still on the volume after removal"));
        }
    }
    for (name, mode) in &folders {
        if volume.directory(name)? != Some(format!("0:0 {mode:o}")) {
            return device(format!("directory {name} does not read back as 0:0 {mode:o}"));
        }
    }
    log::info!("Verified {} changes on flash", changes.len());
    Ok(changes)
}

fn is_boot_backup(name: &str) -> bool {
    crate::boot_branding::FILES.iter().any(|&(_, backup)| name == backup)
}

/// Peak extra bytes while staging replacements, reclaiming each old file after rename.
fn staging_space(sizes: &[(u64, u64)]) -> u64 {
    let (mut retained, mut peak) = (0u64, 0u64);
    for &(new, old) in sizes {
        peak = peak.max(retained + new);
        retained = (retained + new).saturating_sub(old);
    }
    peak
}

// UBIFS compression and garbage collection affect the space actually reclaimed.
fn check_staging_space(volume: &mut impl Volume, size: u64) -> Result<()> {
    if !volume.checks_space() {
        return Ok(());
    }
    let available = volume.free_bytes()?;
    let needed = size + (1 << 20);
    if available < needed {
        return device(format!("the volume has too little free space: {available} bytes available, {needed} bytes required including staging and reserve"));
    }
    Ok(())
}

/// connmand's internet check, as patch_check_url sets it, with the stock
/// connmand kept once in etc/unfr0zen before it is first changed.
fn patch_connmand(volume: &mut impl Volume, tree: &mut Tree, originals: &mut Originals,
                  directories: &mut Vec<(String, u32)>, block_cloud: bool) -> Result<()> {
    fetch(volume, tree, originals, &[CONNMAND], false, true)?;
    let Some((data, _)) = tree.get(CONNMAND) else { return Ok(()); };
    let mut stock = data.clone();
    patch_check_url(&mut stock, CONNMAND, false)?;
    let mut output = stock.clone();
    patch_check_url(&mut output, CONNMAND, block_cloud)?;
    if output != stock {
        fetch(volume, tree, originals, &[CONNMAND_ORIGINAL], false, false)?;
        match originals.get(CONNMAND_ORIGINAL).and_then(Option::as_ref) {
            Some((_, _, sha)) if *sha != sha256_hex(&stock) => {
                return device("connmand original backup does not match this firmware; leaving it untouched");
            }
            Some(_) => {}
            None => {
                tree.insert(CONNMAND_ORIGINAL.into(), (stock, 0o600));
                if !directories.iter().any(|(name, _)| name == "etc/unfr0zen") {
                    directories.push(("etc/unfr0zen".into(), 0o755));
                }
            }
        }
    }
    tree.get_mut(CONNMAND).expect("fetched").0 = output;
    Ok(())
}

fn patch_boot_artwork(volume: &mut impl Volume, tree: &mut Tree, originals: &mut Originals,
                      directories: &mut Vec<(String, u32)>) -> Result<()> {
    for (name, backup) in crate::boot_branding::FILES {
        fetch(volume, tree, originals, &[name], false, true)?;
        let Some((source, mode)) = tree.get(name) else { continue; };
        let (source, mode) = (source.clone(), *mode);
        if name == "boot/kernel_fdt.itb" {
            // Vendor FIT signatures cannot be regenerated by the patcher.
            fetch(volume, tree, originals, &[backup], false, true)?;
            if let Some((original, _)) = tree.get(backup) {
                if crate::boot_branding::recolor(name, original)?.is_some_and(|branded| branded == source) {
                    tree.insert(name.into(), (original.clone(), mode));
                    log::info!("Restoring the signed original kernel FIT from {backup}");
                }
            }
            continue;
        }
        let Some(output) = crate::boot_branding::recolor(name, &source)? else { continue; };
        fetch(volume, tree, originals, &[backup], false, false)?;
        if let Some((_, _, sha)) = originals.get(backup).and_then(Option::as_ref) {
            if *sha != sha256_hex(&source) {
                log::warn!("Boot artwork skipped: {backup} belongs to different artwork; preserving it");
                continue;
            }
        } else {
            tree.insert(backup.into(), (source, 0o600));
        }
        tree.insert(name.into(), (output, mode));
        for (directory, mode) in [("etc/unfr0zen", 0o755), ("etc/unfr0zen/boot", 0o700)] {
            if !directories.iter().any(|(name, _)| name == directory) {
                directories.push((directory.into(), mode));
            }
        }
        log::info!("Recoloring recognized boot artwork: {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use crate::diamond3::rootfs::{FILES as ROOT_SHELL_FILES, GADGET_LINES, OLD_GETTY, SHELL_LINES};
    use crate::shell::{Digests, File};
    use std::io::Read;

    /// The mounted slot, in memory.
    struct FakeVolume {
        files: BTreeMap<String, (String, u32, Vec<u8>)>,
        directories: BTreeMap<String, String>,
        free: u64,
        binary: String,
        commands: Vec<String>,
        readback: Option<String>,
        /// A file whose write fails, as a full or failing flash would.
        failing: Option<&'static str>,
    }

    fn stock() -> Vec<(&'static str, u32, String)> {
        vec![
            ("etc/shadow", 0o640, "root:*:16000:0:99999:7:::\ndaemon:*:16000:0:99999:7:::\n".into()),
            ("etc/inittab", 0o644, format!("::sysinit:/etc/init.d/rcS\n{OLD_GETTY}\n")),
            ("usr/sbin/unlocked", 0o755, "#!/bin/sh\necho 0\n".into()),
            ("etc/init.d/rcS", 0o750, "a\n${INITDIR}/mountfs start\n${INITDIR}/sshd start\n${INITDIR}/nestlabs start\nb\n".into()),
            ("etc/init.d/rcH", 0o750, "a\n${INITDIR}/umountfs stop\nb\n".into()),
            ("etc/ssh/sshd_config", 0o644, "PermitRootLogin no\nHostKey /media/system-config/ssh/ssh_host_rsa_key\n".into()),
            ("etc/init.d/sshd", 0o750, "SSHRSAKEY=${SSHCONFDIR}/ssh_host_rsa_key\n    CreateKeyIfAbsent rsa \"${SSHRSAKEY}\"\n".into()),
            ("etc/nestlabs/client.config", 0o644, BACKPLATE_SIM.iter().map(|k| format!("<a key=\"{k}\" value=\"0\"/>\n")).collect()),
            ("etc/init.d/usb-gadget", 0o750, format!("if [ `unlocked` -eq \"1\" ]; then\n{}else\n  USB_GADGET_MOD=g_mass_storage\nfi\n", GADGET_LINES.0)),
            ("etc/init.d/nestlabs", 0o755, NESTLABS_SCRIPT.into()),
        ]
    }

    const NESTLABS_SCRIPT: &str = "'monit_start')\n log_info \"Service initiation\"\n ${STARTDAEMON} -q -b -p \"${NLCLIENTPIDFILE}\" \
                                   -a \"${NLCLIENTAPP}\" -- `find_configs ${NESTLABSCONFDIR} ${CONFFILENAMES}`\n ;;\n";

    impl FakeVolume {
        fn new(files: Vec<(&str, u32, String)>) -> Self {
            FakeVolume {
                files: files.into_iter().map(|(n, m, t)| (n.to_string(), ("0:0".to_string(), m, t.into_bytes()))).collect(),
                directories: BTreeMap::new(), free: 1 << 30, binary: "0 755".into(), commands: Vec::new(), readback: None,
                failing: None,
            }
        }

        fn text(&self, name: &str) -> String {
            String::from_utf8(self.files[name].2.clone()).unwrap()
        }
    }

    impl Volume for FakeVolume {
        fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
            Ok(self.files.get(name).cloned())
        }
        fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
            Ok(self.files.get(name).map(|(o, m, d)| (o.clone(), *m, sha256_hex(d))))
        }
        fn directory(&mut self, name: &str) -> Result<Option<String>> {
            Ok(self.directories.get(name).cloned())
        }
        fn free_bytes(&mut self) -> Result<u64> {
            Ok(self.free)
        }
        fn shell_binary(&mut self) -> Result<String> {
            Ok(self.binary.clone())
        }
        fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32) -> Result<()> {
            self.commands.push(format!("write {name} {mode:o}"));
            if self.failing == Some(name) {
                return device(format!("{name}: no space left on device"));
            }
            self.files.insert(name.to_string(), (owner.to_string(), mode, data.to_vec()));
            Ok(())
        }
        fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
            self.commands.push(format!("mkdir {name}"));
            self.directories.insert(name.to_string(), format!("0:0 {mode:o}"));
            Ok(())
        }
        fn remove(&mut self, name: &str) -> Result<()> {
            self.commands.push(format!("rm {name}"));
            self.files.remove(name);
            Ok(())
        }
        fn remount(&mut self, writable: bool) -> Result<()> {
            self.commands.push(if writable { "remount rw".into() } else { "remount ro".into() });
            Ok(())
        }
        fn digests(&mut self, names: &[String]) -> Result<Digests> {
            Ok(names.iter().map(|name| {
                let (owner, mode, data) = &self.files[name];
                (name.clone(), (Some(self.readback.clone().unwrap_or_else(|| sha256_hex(data))), Some(format!("{owner} {mode:o}"))))
            }).collect())
        }
    }

    #[test]
    fn onyx_is_rejected_before_patching() {
        let mut volume = FakeVolume::new(Vec::new());
        let error = patch_on_device(&mut volume, Device::Onyx, &Options::default(), b"",
            &mut || panic!("unsupported device must not create keys"), false,
            &|_| panic!("unsupported device must not build a screen")).unwrap_err();
        assert!(error.to_string().contains("Nest Thermostat E (onyx) is not supported"));
        assert!(volume.commands.is_empty());
    }

    fn public_key(kind: &str, fields: &[&[u8]]) -> String {
        let mut blob = Vec::new();
        for value in std::iter::once(kind.as_bytes()).chain(fields.iter().copied()) {
            blob.extend_from_slice(&(value.len() as u32).to_be_bytes());
            blob.extend_from_slice(value);
        }
        format!("{kind} {}", STANDARD.encode(blob))
    }

    #[test]
    fn ssh_public_keys_validate_the_type_and_complete_wire_format() {
        let ed25519 = public_key("ssh-ed25519", &[&[1; 32]]);
        let rsa = public_key("ssh-rsa", &[&[1, 0, 1], &[0, 0x80, 1]]);
        for (curve, size) in [("nistp256", 65), ("nistp384", 97), ("nistp521", 133)] {
            let mut point = vec![1; size];
            point[0] = 4;
            let key = public_key(&format!("ecdsa-sha2-{curve}"), &[curve.as_bytes(), &point]);
            assert_eq!(ssh_public_key(key.as_bytes()).unwrap(), key);
        }
        assert_eq!(ssh_public_key(format!("  {ed25519} user@host\n").as_bytes()).unwrap(), ed25519);
        assert_eq!(ssh_public_key(rsa.as_bytes()).unwrap(), rsa);
        for key in [String::new(), "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret".into(),
                    "ssh-ed25519 !!!!".into(), "ssh-ed25519 AAAA".into(),
                    ed25519.replacen("ssh-ed25519", "ssh-rsa", 1),
                    public_key("ssh-ed25519", &[&[1; 31]]),
                    public_key("ssh-ed25519", &[&[1; 32], b"trailing"]),
                    public_key("ssh-rsa", &[&[0x80], &[1]]),
                    public_key("ssh-rsa", &[&[1]]),
                    public_key("ecdsa-sha2-nistp256", &[b"nistp384", &[4; 65]]),
                    public_key("ecdsa-sha2-nistp256", &[b"nistp256", &[4; 64]]),
                    format!("{ed25519}\n{rsa}")] {
            assert!(ssh_public_key(key.as_bytes()).is_err(), "accepted {key:?}");
        }
    }

    #[test]
    fn ssh_public_keys_merge_once_with_safe_permissions_on_both_devices() {
        for target in [Device::Diamond3, Device::J49] {
            for existing in [false, true] {
                let mut volume = FakeVolume::new(stock());
                if target == Device::J49 {
                    volume.files.get_mut("etc/inittab").unwrap().2 = b"::sysinit:/etc/init.d/rcS\n".to_vec();
                }
                let key = public_key("ssh-ed25519", &[&[1; 32]]);
                let other = format!("# keep this key\n{} other@host", public_key("ssh-ed25519", &[&[2; 32]]));
                if existing {
                    volume.files.insert(AUTHORIZED_KEYS.into(), ("0:12".into(), 0o644, other.as_bytes().to_vec()));
                    volume.directories.insert("root/.ssh".into(), "0:12 755".into());
                }
                let options = Options { ssh: true, ssh_public_key: Some(format!("{key} user@host")),
                    ssh_server: Some(Overlay { directories: vec![], files: vec![
                        ("usr/sbin/dropbearmulti".into(), 0o755, b"ELF".to_vec())] }), ..options() };
                let run = |volume: &mut FakeVolume, options: &Options, dry_run| patch_on_device(
                    volume, target, options, b"", &mut no_pki, dry_run,
                    &|source| crate::nlclient::build(source, b""));
                let before = volume.files.clone();
                let before_directories = volume.directories.clone();
                let planned = run(&mut volume, &options, true).unwrap();
                assert!(names(&planned).contains(AUTHORIZED_KEYS));
                assert!(volume.commands.is_empty());
                assert_eq!(volume.files, before);
                assert_eq!(volume.directories, before_directories);
                assert_eq!(run(&mut volume, &options, false).unwrap(), planned);
                let expected = if existing { format!("{other}\n{key}\n") } else { format!("{key}\n") };
                assert_eq!(volume.files[AUTHORIZED_KEYS], ("0:0".into(), 0o600, expected.as_bytes().to_vec()));
                assert_eq!(volume.directories["root/.ssh"], "0:0 700");
                volume.commands.clear();
                let options = Options { ssh_public_key: Some(format!("{key} different-comment")), ..options };
                assert!(run(&mut volume, &options, false).unwrap().is_empty());
                assert!(volume.commands.is_empty());
                assert!(run(&mut volume, &Options { ssh_public_key: None, ..options }, false).unwrap().is_empty());
                assert_eq!(volume.text(AUTHORIZED_KEYS), expected);
            }
        }
    }

    #[test]
    fn ssh_public_key_installation_requires_ssh_and_rejects_non_root_files() {
        let key = public_key("ssh-ed25519", &[&[1; 32]]);
        let mut volume = FakeVolume::new(stock());
        let options = Options { ssh_public_key: Some(key), ..options() };
        assert!(patch(&mut volume, &options, false).unwrap_err().to_string().contains("needs --ssh"));
        volume.files.insert(AUTHORIZED_KEYS.into(), ("1000:1000".into(), 0o600, b"keep".to_vec()));
        let options = Options { ssh: true, ..options };
        assert!(patch(&mut volume, &options, false).unwrap_err().to_string().contains("not owned by root"));
        assert!(volume.commands.is_empty());
    }

    /// The lines around nlsleepd's start in the script of every release seen (5.9.4-5 to 6.5-14).
    const SCRIPT: &str = "start_nlsleepd () {\n log_info \"Service initiation\"\n ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" \
                          -a ${NLSLEEPD_APP} -- `${CAT} ${CONFDIR}/${NLSLEEPD_NAME}.options 2> ${NULL}`\n}\n";

    fn script(tree: &Tree) -> String {
        String::from_utf8(tree[NLSLEEP].0.clone()).unwrap()
    }

    #[test]
    fn matter_preloads_the_wake_pattern_library_into_nlsleepd_and_off_again_takes_it_out() {
        let mut tree = Tree::new();
        tree.insert(NLSLEEP.into(), (SCRIPT.as_bytes().to_vec(), 0o755));
        patch_wake_patterns(&mut tree, true).unwrap();
        let preloaded = script(&tree);
        assert!(preloaded.contains("\n LD_PRELOAD=/nestlabs/lib/libwakefilter.so ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" -a ${NLSLEEPD_APP} -- "));
        assert_eq!(tree[NLSLEEP].1, 0o755);
        // Again changes nothing.
        patch_wake_patterns(&mut tree, true).unwrap();
        assert_eq!(script(&tree), preloaded);
        patch_wake_patterns(&mut tree, false).unwrap();
        assert_eq!(script(&tree), SCRIPT);
    }

    #[test]
    fn a_script_that_starts_nlsleepd_another_way_is_refused_for_matter_and_left_alone_without() {
        let mut tree = Tree::new();
        tree.insert(NLSLEEP.into(), (b"start_nlsleepd () {\n nlsleepd &\n}\n".to_vec(), 0o755));
        assert!(patch_wake_patterns(&mut tree, true).unwrap_err().to_string().contains("starting nlsleepd"));
        patch_wake_patterns(&mut tree, false).unwrap();
        assert_eq!(script(&tree), "start_nlsleepd () {\n nlsleepd &\n}\n");
    }

    fn options() -> Options {
        Options { root_hash: "$6$salt$hash".into(), local_cloud: b"\x7fELF local cloud".to_vec(), ..Options::default() }
    }

    fn no_pki() -> Result<Pki> {
        panic!("a key set was made for a unit that has its own")
    }

    fn patch(volume: &mut FakeVolume, options: &Options, dry_run: bool) -> Result<Vec<Change>> {
        patch_on_device(volume, Device::Diamond3, options, b"ARMV7", &mut no_pki, dry_run, &|source| {
            let object = std::fs::read(std::env::var_os("NLCLIENT_OBJECT").map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/custom-screen.o")))?;
            crate::nlclient::build(source, &object)
        })
    }

    fn names(changes: &[Change]) -> BTreeSet<&str> {
        changes.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn thread_installation_owns_the_radio_and_is_removed_from_options_when_disabled() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        let thread = Overlay { files: vec![("nestlabs/sbin/ot-daemon".into(), 0o755, b"DAEMON".to_vec())],
                               directories: Vec::new() };
        let options = Options { matter: Some(Matter::from_pairing_code(nestlocal::DEFAULT_MATTER_CODE).unwrap()),
                                thread: Some(thread), ..options() };
        patch(&mut volume, &options, false).unwrap();
        let line = volume.text(nestlocal::OPTIONS);
        assert!(line.contains("-matter-thread") && !line.contains("-zigbee"));
        assert!(!volume.text("etc/monitrc").contains("include /etc/monit.d/wpantund.monitrc"));
        assert!(volume.text("etc/monit.d/nestlocal.monitrc").contains("nestlocal-thread-daemon"));
        patch(&mut volume, &Options { thread: None, ..options }, false).unwrap();
        assert!(!volume.text(nestlocal::OPTIONS).contains("-matter-thread"));
        assert!(!volume.text("etc/monit.d/nestlocal.monitrc").contains("nestlocal-thread-daemon"));
    }

    #[test]
    fn matter_installs_the_wake_pattern_library_for_nlsleepd_and_off_again_removes_it() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        let script = "start_nlsleepd () {\n ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" -a ${NLSLEEPD_APP} -- `${CAT} x`\n}\n";
        volume.files.insert(NLSLEEP.into(), ("0:0".into(), 0o755, script.as_bytes().to_vec()));
        let matter = Some(Matter::from_pairing_code(nestlocal::DEFAULT_MATTER_CODE).unwrap());
        let options = Options { matter, wake_filter: b"\x7fELF library".to_vec(), ..options() };
        let changes = patch(&mut volume, &options, false).unwrap();
        assert!(names(&changes).contains(WAKE_FILTER));
        assert_eq!((volume.files[WAKE_FILTER].1, volume.files[WAKE_FILTER].2.as_slice()), (0o755, &b"\x7fELF library"[..]));
        assert!(volume.text(NLSLEEP).contains(" LD_PRELOAD=/nestlabs/lib/libwakefilter.so ${STARTDAEMON} "));
        // Again changes neither.
        assert!(!names(&patch(&mut volume, &options, false).unwrap()).contains(WAKE_FILTER));
        let changes = patch(&mut volume, &Options { matter: None, mqtt: Some(Mqtt { url: "tcp://b:1883".into(), user: None,
            password: None, tls: false, ca: None, entity_root: None }), ..options }, false).unwrap();
        assert!(changes.iter().any(|c| c.name == WAKE_FILTER && c.action == "remove"));
        assert!(!volume.files.contains_key(WAKE_FILTER));
        assert_eq!(volume.text(NLSLEEP), script);
    }

    #[test]
    fn nestlocal_has_nlclient_reach_it_without_wifi_and_a_unit_without_it_starts_nlclient_as_stock() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        let zigbee = Options { zigbee: true, ..options() };
        let changes = patch(&mut volume, &zigbee, false).unwrap();
        assert!(names(&changes).contains(LOCAL_CLOUD) && names(&changes).contains(NESTLABS));
        assert_eq!((volume.files[LOCAL_CLOUD].1, volume.files[LOCAL_CLOUD].2.as_slice()), (0o755, &b"\x7fELF local cloud"[..]));
        assert!(volume.text(NESTLABS).contains("\n LD_PRELOAD=/nestlabs/lib/liblocalcloud.so ${STARTDAEMON} -q -b -p \"${NLCLIENTPIDFILE}\" -a "));
        // Again changes neither, and turning every integration off keeps nestlocal, nlclient's cloud.
        let again = names(&patch(&mut volume, &zigbee, false).unwrap()).into_iter().map(String::from).collect::<Vec<_>>();
        assert!(!again.iter().any(|name| name == LOCAL_CLOUD || name == NESTLABS), "{again:?}");
        patch(&mut volume, &Options { zigbee: false, ..zigbee.clone() }, false).unwrap();
        assert!(volume.text(NESTLABS).contains("LD_PRELOAD=/nestlabs/lib/liblocalcloud.so"));
        let error = patch(&mut volume, &Options { local_cloud: Vec::new(), ..zigbee }, false).unwrap_err();
        assert!(error.to_string().contains("local cloud library"), "{error}");

        // A unit without nestlocal: the library goes and nlclient starts as stock.
        let mut volume = FakeVolume::new(stock());
        volume.files.insert(NESTLABS.into(), ("0:0".into(), 0o755,
            NESTLABS_SCRIPT.replace(" ${STARTDAEMON} -q -b -p", " LD_PRELOAD=/nestlabs/lib/liblocalcloud.so ${STARTDAEMON} -q -b -p").into_bytes()));
        volume.files.insert(LOCAL_CLOUD.into(), ("0:0".into(), 0o755, b"\x7fELF old".to_vec()));
        let changes = patch(&mut volume, &options(), false).unwrap();
        assert!(changes.iter().any(|c| c.name == LOCAL_CLOUD && c.action == "remove"));
        assert_eq!(volume.text(NESTLABS), NESTLABS_SCRIPT);
    }

    #[test]
    fn the_external_temperature_is_an_option_of_nestlocal_with_nlclients_ble_on() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        let options = Options { external_temperature: true, ..options() };
        patch(&mut volume, &options, false).unwrap();
        assert!(volume.text(nestlocal::OPTIONS).trim_end().ends_with(" -external-temperature"));
        assert!(!volume.text("etc/nestlabs/product.config").contains("key=\"ble_disabled\" value=\"1\""));
        patch(&mut volume, &Options { external_temperature: false, ..options }, false).unwrap();
        assert!(!volume.text(nestlocal::OPTIONS).contains("external-temperature"));
    }

    #[test]
    fn j49_external_temperature_requires_nlclient_before_any_writes() {
        let pki = nestlocal::generate_pki().unwrap();
        let binaries = [None, Some(b"not an ELF".to_vec()), Some(b"\x7fELF unreviewed".to_vec())];
        for (target, binary) in [Device::Diamond, Device::J49].into_iter().flat_map(|t| binaries.iter().cloned().map(move |b| (t, b))) {
            let mut volume = nestlocal_unit(Some(&pki));
            volume.files.get_mut("etc/inittab").unwrap().2 = b"::sysinit:/etc/init.d/rcS\n".to_vec();
            if let Some(binary) = binary {
                volume.files.insert(crate::nlclient::BINARY.into(), ("0:0".into(), 0o755, binary));
            }
            let before = volume.files.clone();
            let error = patch_on_device(&mut volume, target,
                &Options { external_temperature: true, ..options() }, b"ARMV7", &mut no_pki,
                false, &|source| crate::nlclient::Patch::journal(source, source)).unwrap_err();
            assert!(error.to_string().contains("j49 external temperature"), "{error}");
            assert_eq!(volume.files, before);
            assert!(volume.commands.is_empty());
        }
    }

    #[test]
    fn j49_external_temperature_is_conditional_idempotent_and_reversible() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join(".work/firmware"));
        let object = std::env::var_os("NLCLIENT_OBJECT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("build/custom-screen.o"));
        let (Ok(source), Ok(object)) = (std::fs::read(firmware.join("nlclient-gen2-2019")), std::fs::read(object)) else {
            assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need the j49 sample and screen object");
            return;
        };
        let rcs_object = std::fs::read(std::env::var_os("NLCLIENT_RCS_OBJECT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("build/j49-rcs-screen.o"))).expect("build the j49 RCS screen object");
        let pki = nestlocal::generate_pki().unwrap();
        for block_cloud in [false, true] {
            let mut volume = nestlocal_unit(Some(&pki));
            volume.files.insert("etc/hosts".into(), ("0:0".into(), 0o644, b"127.0.0.1 localhost\n".to_vec()));
            volume.files.get_mut("etc/inittab").unwrap().2 = b"::sysinit:/etc/init.d/rcS\n".to_vec();
            volume.files.insert(crate::nlclient::BINARY.into(), ("0:0".into(), 0o755, source.clone()));
            let run = |volume: &mut FakeVolume, options: &Options, dry_run| patch_on_device(
                volume, Device::J49, options, b"ARMV7", &mut no_pki, dry_run,
                &|source| crate::nlclient::build(source, &object));
            let off = Options { block_cloud, external_temperature_screen: rcs_object.clone(),
                mqtt: Some(Mqtt { url: "broker".into(), ..Mqtt::default() }), ..options() };
            run(&mut volume, &off, false).unwrap();
            let screen = volume.files[crate::nlclient::BINARY].2.clone();
            volume.commands.clear();
            let on = Options { external_temperature: true, ..off.clone() };
            let before = volume.files.clone();
            let planned = run(&mut volume, &on, true).unwrap();
            assert!(names(&planned).contains(crate::nlclient::BINARY));
            assert_eq!(volume.files, before);
            assert!(volume.commands.is_empty());
            assert_eq!(run(&mut volume, &on, false).unwrap(), planned);
            assert_ne!(volume.files[crate::nlclient::BINARY].2, screen);
            assert!(volume.text(nestlocal::OPTIONS).contains("-external-temperature"));
            volume.commands.clear();
            assert!(run(&mut volume, &on, false).unwrap().is_empty());
            assert!(volume.commands.is_empty());
            run(&mut volume, &off, false).unwrap();
            assert_eq!(volume.files[crate::nlclient::BINARY].2, screen);
            assert!(!volume.text(nestlocal::OPTIONS).contains("-external-temperature"));
            assert_eq!(volume.files[crate::nlclient::ORIGINAL].2, source);
        }
    }

    #[test]
    fn leaving_the_external_temperature_out_puts_the_units_ble_setting_back() {
        let config = |value: u8, rest: &str| format!("<a key=\"usephoenix\" value=\"0\"/>\n<a key=\"ble_disabled\" value=\"{value}\"/>\n{rest}");
        let matter = "\n<!-- unfr0zen owns BLE for Matter; original ble_disabled=0 -->";
        let mqtt = || Mqtt { url: "tcp://b:1883".into(), ..Mqtt::default() };
        for (installed, original) in [(config(0, ""), config(0, "")), (config(1, ""), config(1, "")), (config(1, matter), config(0, ""))] {
            for left_on in [None, Some(mqtt())] {
                let pki = nestlocal::generate_pki().unwrap();
                let mut volume = nestlocal_unit(Some(&pki));
                volume.files.get_mut("etc/nestlabs/product.config").unwrap().2 = installed.clone().into_bytes();
                patch(&mut volume, &Options { external_temperature: true, mqtt: Some(mqtt()), ..options() }, false).unwrap();
                assert!(volume.text("etc/nestlabs/product.config").contains("key=\"ble_disabled\" value=\"0\""));
                patch(&mut volume, &Options { external_temperature: true, mqtt: Some(mqtt()), ..options() }, false).unwrap();
                assert!(volume.text("etc/nestlabs/product.config").contains("key=\"ble_disabled\" value=\"0\""));
                patch(&mut volume, &Options { mqtt: left_on, ..options() }, false).unwrap();
                assert_eq!(volume.text("etc/nestlabs/product.config"), original);
            }
        }
    }

    #[test]
    fn doom_installs_nestlocal_offering_it_and_is_withdrawn_from_options_when_disabled() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        let doom = Overlay { files: vec![("nestlabs/sbin/doom".into(), 0o755, b"DOOM".to_vec())],
                             directories: vec![("usr/share/doom".into(), 0o755)] };
        let options = Options { doom: Some(doom), ..options() };
        patch(&mut volume, &options, false).unwrap();
        assert!(volume.text(nestlocal::OPTIONS).trim_end().ends_with(" -doom"));
        assert_eq!(volume.text("nestlabs/sbin/doom"), "DOOM");
        patch(&mut volume, &Options { doom: None, ..options }, false).unwrap();
        assert!(!volume.text(nestlocal::OPTIONS).contains("-doom"));
    }

    #[test]
    fn dry_run_writes_nothing() {
        let mut volume = FakeVolume::new(stock());
        let changes = patch(&mut volume, &options(), true).unwrap();
        // Without SSH, sshd's own files stay stock and are not written.
        let unchanged = ["etc/nestlabs/client.config", "etc/ssh/sshd_config", "etc/init.d/sshd"];
        let expected: BTreeSet<&str> = ROOT_SHELL_FILES.iter().copied().filter(|n| !unchanged.contains(n)).collect();
        assert_eq!(names(&changes), expected);
        assert!(changes.iter().all(|c| c.action == "write"));
        assert!(volume.commands.is_empty());
        assert!(changes_text(&changes).contains("write etc/shadow: "));
    }

    #[test]
    fn writes_patched_files_and_keeps_owner_and_mode() {
        let mut volume = FakeVolume::new(stock());
        volume.files.get_mut("etc/shadow").unwrap().0 = "0:42".into();
        patch(&mut volume, &options(), false).unwrap();
        assert_eq!(volume.commands.first().unwrap(), "remount rw");
        assert_eq!(volume.commands.last().unwrap(), "remount ro");
        assert!(volume.text("etc/shadow").starts_with("root:$6$salt$hash:16000"));
        assert_eq!((volume.files["etc/shadow"].0.as_str(), volume.files["etc/shadow"].1), ("0:42", 0o640));
        assert_eq!(volume.text("usr/sbin/unlocked"), "#!/bin/sh\necho 1\n");
        // The USB root shell is opt-in: a default run gives no ACM port.
        assert!(volume.text("etc/inittab").contains(SHELL_LINES.1));
        assert!(volume.text("etc/init.d/usb-gadget").contains(GADGET_LINES.1));
        assert!(volume.text("etc/init.d/rcS").contains("${INITDIR}/mountfs start\n${MOUNT} -n -o remount,rw /\n"));
        assert!(volume.text("etc/init.d/rcH").contains("${INITDIR}/umountroot stop\n"));
        // SSH is opt-in: a default run starts no sshd.
        assert!(volume.text("etc/init.d/rcS").contains("\n#${INITDIR}/sshd start\n"));
        assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin no\n"));
    }

    #[test]
    fn a_patched_volume_is_left_alone() {
        let mut volume = FakeVolume::new(stock());
        patch(&mut volume, &options(), false).unwrap();
        volume.commands.clear();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert!(volume.commands.is_empty());
    }

    #[test]
    fn refuses_files_not_owned_by_root_and_a_volume_without_its_shell() {
        let mut volume = FakeVolume::new(stock());
        volume.files.get_mut("etc/inittab").unwrap().0 = "1000:1000".into();
        assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("not owned by root"));
        for binary in ["", "0 644", "1000 755"] {
            let mut volume = FakeVolume::new(stock());
            volume.binary = binary.into();
            assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("bin/busybox or bin/toybox"));
            assert!(volume.commands.is_empty());
        }
    }

    #[test]
    fn staging_space_counts_backups_and_one_replacement_at_a_time() {
        let kernel = 4_199_955;
        let nestlocal = 6_366_424;
        let nlclient = 7_286_049;
        let sizes = [(kernel, 0), (kernel, kernel), (nestlocal, nestlocal),
                     (nlclient, nlclient), (10_000, 8_000)];
        assert_eq!(staging_space(&sizes), kernel + nlclient);
        assert!(staging_space(&sizes) + (1 << 20) < 22_096 * 1024);
        assert!(sizes.iter().map(|(new, _)| new).sum::<u64>() + (1 << 20) > 22_096 * 1024);
        assert_eq!(staging_space(&[(100, 0), (200, 100), (300, 200)]), 500);
        assert_eq!(staging_space(&[(100, 200), (300, 0)]), 300);
    }

    #[test]
    fn readback_mismatch_and_too_little_space_fail() {
        let mut volume = FakeVolume::new(stock());
        volume.readback = Some("0".repeat(64));
        assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("readback from flash"));
        let mut volume = FakeVolume::new(stock());
        volume.free = 4096;
        assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("free space"));
        assert!(volume.commands.is_empty());
    }

    #[test]
    fn unexpected_stock_files_stop_the_patch_before_any_write() {
        type Mutation = fn(&mut File<Vec<u8>>);
        let cases: Vec<(&str, Mutation, &str)> = vec![
            ("etc/shadow", |f| f.2.extend_from_slice(b"root:*:1:0:99999:7:::\n"), "one root account"),
            ("etc/inittab", |f| f.2 = b"::sysinit:/etc/init.d/rcS\n".to_vec(), "USB getty"),
            ("etc/shadow", |f| f.1 = 0o644, "mode 640"),
            ("etc/init.d/rcS", |f| f.2.extend_from_slice(b"${MOUNT} -n -o remount,rw /\n"), "somewhere other than"),
            ("etc/ssh/sshd_config", |f| f.2 = b"PermitRootLogin without-password\n".to_vec(), "PermitRootLogin no"),
            ("etc/hosts", |f| f.2.extend_from_slice(HOSTS_BEGIN.as_bytes()), "without its end line"),
        ];
        for (name, change, error) in cases {
            let mut files = stock();
            files.push(("etc/hosts", 0o644, "127.0.0.1 localhost\n".into()));
            let mut volume = FakeVolume::new(files);
            change(volume.files.get_mut(name).unwrap());
            let failure = patch(&mut volume, &Options { block_cloud: true, ..options() }, false).unwrap_err();
            assert!(failure.to_string().contains(error), "{name}: {failure}");
            assert!(volume.commands.is_empty(), "{name}");
        }
    }

    #[test]
    fn ntp_server_edits_the_files_the_firmware_has() {
        let mut files = stock();
        files.push(("etc/connman/main.conf", 0o644, "[General]\nFallbackTimeservers = time.nest.com\n".into()));
        let mut volume = FakeVolume::new(files);
        let changes = patch(&mut volume, &Options { ntp_server: Some(String::new()), ..options() }, false).unwrap();
        assert!(names(&changes).contains("etc/connman/main.conf"));
        assert!(volume.text("etc/connman/main.conf").contains("FallbackTimeservers = pool.ntp.org\n"));
        assert!(!volume.files.contains_key("etc/ntp.conf"));
    }

    #[test]
    fn block_cloud_sinks_the_cloud_in_etc_hosts_and_comes_back_out() {
        const HOSTS: &str = "127.0.0.1 localhost.localdomain localhost\n::1 ip6-localhost ip6-loopback\n";
        let mut files = stock();
        files.push(("etc/hosts", 0o644, HOSTS.into()));
        let mut volume = FakeVolume::new(files);
        let blocked = Options { block_cloud: true, ..options() };
        assert!(names(&patch(&mut volume, &blocked, false).unwrap()).contains("etc/hosts"));
        let hosts = volume.text("etc/hosts");
        assert!(hosts.starts_with(HOSTS));
        assert!(hosts.contains("0.0.0.0 frontdoor.nest.com\n:: frontdoor.nest.com\n"));
        assert!(!names(&patch(&mut volume, &blocked, false).unwrap()).contains("etc/hosts"));
        patch(&mut volume, &options(), false).unwrap();
        assert_eq!(volume.text("etc/hosts"), HOSTS);
        // A unit without etc/hosts is fine until the block is asked for.
        let mut bare = FakeVolume::new(stock());
        patch(&mut bare, &options(), false).unwrap();
        assert!(patch(&mut bare, &blocked, false).unwrap_err().to_string().contains("etc/hosts"));
    }

    #[test]
    fn shell_setup_goes_at_the_end_of_roots_profile_once_and_comes_back_out() {
        const PROFILE_TEXT: &str = "PATH=\"${PATH}:/nestlabs/sbin\"\nexport PATH\n";
        let mut files = stock();
        files.push((PROFILE, 0o644, PROFILE_TEXT.into()));
        let mut volume = FakeVolume::new(files);
        assert!(names(&patch(&mut volume, &options(), false).unwrap()).contains(PROFILE));
        let patched = format!("{PROFILE_TEXT}{SHELL_BEGIN}{SHELL}{SHELL_END}");
        assert_eq!(volume.text(PROFILE), patched);
        assert!(!names(&patch(&mut volume, &options(), false).unwrap()).contains(PROFILE));
        let mut tree = Tree::from([(PROFILE.to_string(), (patched.into_bytes(), 0o644))]);
        patch_shell(&mut tree, false).unwrap();
        assert_eq!(text(&tree, PROFILE).unwrap(), PROFILE_TEXT);
        // A unit without root/.profile keeps its stock shell.
        let mut bare = FakeVolume::new(stock());
        assert!(!names(&patch(&mut bare, &options(), false).unwrap()).contains(PROFILE));
    }

    fn nestlocal_unit(pki: Option<&Pki>) -> FakeVolume {
        let mut files = stock();
        files[7].2 += &format!("<a key=\"cloudregisterurl\" value=\"{}\"/>\n", nestlocal::STOCK_URL);
        files.push(("etc/monitrc", 0o600, "set daemon 30\ninclude /etc/monit.d/wpantund.monitrc\n".into()));
        files.push(("etc/init.d/networking-defines", 0o644, "NETWORKING_TECHNOLOGIES=\"common wifi wpan\"\n".into()));
        files.push(("etc/nestlabs/product.config", 0o644, "<a key=\"usephoenix\" value=\"0\"/>\n".into()));
        let bundle = pki.map_or(String::new(), |p| String::from_utf8(p.ca.clone()).unwrap());
        files.push(("etc/ssl/certs/ca-bundle.pem", 0o644, bundle.clone()));
        files.push(("etc/ssl/certs/ca-unlock-bundle.pem", 0o644, bundle));
        if let Some(pki) = pki {
            files.push(("etc/nestlocal/server.crt", 0o644, String::from_utf8(pki.server_crt.clone()).unwrap()));
            files.push(("etc/nestlocal/server.key", 0o600, String::from_utf8(pki.server_key.clone()).unwrap()));
        }
        FakeVolume::new(files)
    }

    #[test]
    fn mqtt_builds_the_overlay_from_the_units_files_and_keeps_its_certificates() {
        let pki = nestlocal::generate_pki().unwrap();
        let mqtt = Mqtt { url: "broker".into(), user: Some("u".into()), password: Some("p".into()), ..Mqtt::default() };
        let mut volume = nestlocal_unit(Some(&pki));
        let changes = patch(&mut volume, &Options { mqtt: Some(mqtt.clone()), ..options() }, false).unwrap();
        assert!(!names(&changes).contains("etc/nestlocal/server.crt"));
        assert_eq!(volume.files["etc/nestlocal/server.key"].2, pki.server_key);
        assert_eq!(volume.text("etc/nestlocal/mqtt.password"), "p\n");
        assert!(volume.text("etc/nestlocal/options").starts_with("-mqtt tcp://broker:1883 -mqtt-user u"));
        assert!(volume.text("etc/init.d/rcS").contains("${INITDIR}/nestlocal start\n${INITDIR}/nestlabs start\n"));
        // A unit without nestlocal gets the key set it is given.
        let mut fresh = nestlocal_unit(None);
        let mut made = 0;
        patch_on_device(&mut fresh, Device::Diamond3, &Options { mqtt: Some(mqtt), ..options() }, b"ARMV7",
                        &mut || { made += 1; Ok(pki.clone()) }, false,
                        &|source| crate::nlclient::build(source, b"")).unwrap();
        assert_eq!(made, 1);
        // As Python's with_ca: the empty bundle's newline, then the CA.
        assert_eq!(fresh.files["etc/ssl/certs/ca-bundle.pem"].2, [b"\n", pki.ca.as_slice()].concat());
    }

    #[test]
    fn wwn_boiler_is_set_from_rcs_when_asked_and_taken_back_after() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        // Nest's radio: rcS has no block.
        patch(&mut volume, &Options { mqtt: Some(Mqtt { url: "tcp://b:1883".into(), ..Mqtt::default() }), ..options() }, false).unwrap();
        let without = volume.text("etc/init.d/rcS");
        assert!(!without.contains(WWN_BEGIN));
        // Zigbee alone does not ask for it.
        patch(&mut volume, &Options { zigbee: true, ..options() }, false).unwrap();
        assert!(!volume.text("etc/init.d/rcS").contains(WWN_BEGIN));
        patch(&mut volume, &Options { zigbee: true, wwn_boiler: true, ..options() }, false).unwrap();
        let rcs = volume.text("etc/init.d/rcS");
        let block = &rcs[rcs.find(WWN_BEGIN).unwrap()..rcs.find(WWN_END).unwrap() + WWN_END.len()];
        assert_eq!(rcs.replace(block, ""), without);
        assert!(block.contains("case \"$(fw_printenv -n nlmodel 2>/dev/null)\" in Display-3.*|Display-5.*)\n"));
        assert!(block.contains("  touch /media/user-config/.unfr0zen-wwn-boiler\n"));
        assert!(block.contains(r#"<s key="using_wwn_boiler" value="1"/>"#) && !block.contains("SETTINGS") && !block.contains("MARKER"));
        // Before nestlocal's line, which keeps its place right before
        // nestlabs's on the next run; the same run changes nothing.
        assert!(rcs.contains(&format!("{WWN_END}${{INITDIR}}/nestlocal start\n${{INITDIR}}/nestlabs start\n")), "{rcs}");
        assert_eq!(volume.files["etc/init.d/rcS"].1, 0o750);
        patch(&mut volume, &Options { zigbee: true, wwn_boiler: true, ..options() }, false).unwrap();
        assert_eq!(volume.text("etc/init.d/rcS"), rcs);
        // Not asked for any more, the radio still ours: the block takes the
        // key back once, then the marker, and stays the only change around
        // nestlabs's line.
        patch(&mut volume, &Options { zigbee: true, ..options() }, false).unwrap();
        let rcs = volume.text("etc/init.d/rcS");
        assert_eq!(rcs.matches(WWN_BEGIN).count(), 1);
        assert!(rcs.contains("if [ -f /media/user-config/.unfr0zen-wwn-boiler ]; then\n  sed -i '/<s key=\"using_wwn_boiler\" /d' /media/user-config/settings.config\n  rm -f /media/user-config/.unfr0zen-wwn-boiler\nfi\n"));
        assert!(!rcs.contains("value=\"1\""));
        assert!(rcs.contains(&format!("{WWN_END}${{INITDIR}}/nestlocal start\n${{INITDIR}}/nestlabs start\n")), "{rcs}");
    }

    #[test]
    fn a_run_with_no_integration_turns_an_installed_nestlocals_off() {
        let pki = nestlocal::generate_pki().unwrap();
        let mut volume = nestlocal_unit(Some(&pki));
        patch(&mut volume, &Options { zigbee: true, ..options() }, false).unwrap();
        assert!(!volume.text("etc/monitrc").contains("wpantund"));
        assert!(volume.text("etc/nestlocal/options").contains("-zigbee"));
        // Nothing ticked: nestlocal stays, as the unit's cloud, with Nest's
        // Thread radio set-up back and no integration of its own.
        patch(&mut volume, &options(), false).unwrap();
        assert!(volume.text("etc/monitrc").contains("include /etc/monit.d/wpantund.monitrc\n"));
        assert!(volume.text("etc/init.d/networking-defines").contains("wpan"));
        let line = volume.text("etc/nestlocal/options");
        assert!(!line.contains("-zigbee") && !line.contains("-matter"), "{line}");
        assert!(volume.text("etc/init.d/rcS").contains("${INITDIR}/nestlocal start\n"));
        // A unit that never had nestlocal does not get it.
        let mut stock_unit = nestlocal_unit(None);
        patch(&mut stock_unit, &options(), false).unwrap();
        assert!(!stock_unit.files.contains_key("etc/nestlocal/options"));
    }

    #[test]
    fn usb_shell_runs_only_when_asked_and_off_again_drops_the_acm_port() {
        let mut volume = FakeVolume::new(stock());
        let stock_gadget = volume.text("etc/init.d/usb-gadget");
        let off = |volume: &FakeVolume| {
            let inittab = volume.text("etc/inittab");
            assert!(inittab.contains(SHELL_LINES.1) && !inittab.contains(SHELL_LINES.0), "{inittab}");
            let gadget = volume.text("etc/init.d/usb-gadget");
            assert!(gadget.contains(GADGET_LINES.1) && !gadget.contains("g_acm_ms"), "{gadget}");
        };
        let on = |volume: &FakeVolume| {
            let inittab = volume.text("etc/inittab");
            assert!(inittab.contains(SHELL_LINES.0) && !inittab.contains('#'), "{inittab}");
            assert_eq!(volume.text("etc/init.d/usb-gadget"), stock_gadget);
        };
        patch(&mut volume, &options(), false).unwrap();
        off(&volume);
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        patch(&mut volume, &Options { usb_shell: true, ..options() }, false).unwrap();
        on(&volume);
        patch(&mut volume, &options(), false).unwrap();
        off(&volume);
        patch(&mut volume, &Options { usb_shell: true, ..options() }, false).unwrap();
        on(&volume);
        // SSH does not depend on it: unlocked still says yes.
        assert_eq!(volume.text("usr/sbin/unlocked"), "#!/bin/sh\necho 1\n");
        // A unit patched before the login shell gets it, on or off.
        for (plain, line) in [("ttyGS0::respawn:/bin/sh\n", SHELL_LINES.0), ("#ttyGS0::respawn:/bin/sh\n", SHELL_LINES.1)] {
            let mut earlier = FakeVolume::new(stock());
            earlier.files.get_mut("etc/inittab").unwrap().2 = format!("::sysinit:/etc/init.d/rcS\n{plain}").into_bytes();
            patch(&mut earlier, &Options { usb_shell: true, ..options() }, false).unwrap();
            on(&earlier);
            patch(&mut earlier, &options(), false).unwrap();
            off(&earlier);
            assert!(!earlier.text("etc/inittab").contains(plain.trim_start_matches('#')), "{line}");
        }
        // A usb-gadget without the ACM line is not one this edit knows.
        let mut odd = FakeVolume::new(stock());
        odd.files.get_mut("etc/init.d/usb-gadget").unwrap().2 = b"USB_GADGET_MOD=g_mass_storage\n".to_vec();
        assert!(patch(&mut odd, &options(), false).unwrap_err().to_string().contains("g_acm_ms"));
    }

    #[test]
    fn ssh_without_password_login_takes_keys_only_and_needs_a_key() {
        let mut volume = FakeVolume::new(stock());
        let key = Some(public_key("ssh-ed25519", &[&[1; 32]]));
        assert!(patch(&mut volume, &Options { ssh: true, ssh_key_only: true, ..options() }, false).is_err(), "no key: no way in");
        patch(&mut volume, &Options { ssh: true, ssh_key_only: true, ssh_public_key: key.clone(), ..options() }, false).unwrap();
        assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin prohibit-password\n"));
        patch(&mut volume, &Options { ssh: true, ssh_public_key: key, ..options() }, false).unwrap();
        assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin yes\n"), "password login allowed again");
        patch(&mut volume, &options(), false).unwrap();
        assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin no\n"), "SSH off: stock");
    }

    #[test]
    fn ssh_runs_only_when_asked_and_off_again_stops_sshd() {
        let mut volume = FakeVolume::new(stock());
        let stock_rcs = volume.text("etc/init.d/rcS");
        let off = |volume: &FakeVolume| {
            let rcs = volume.text("etc/init.d/rcS");
            assert!(rcs.contains("\n#${INITDIR}/sshd start\n") && !rcs.contains("\n${INITDIR}/sshd start\n"), "{rcs}");
            assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin no\n"));
            assert!(!volume.text("etc/init.d/sshd").contains("ecdsa"));
        };
        let on = |volume: &FakeVolume| {
            let rcs = volume.text("etc/init.d/rcS");
            assert_eq!(rcs.replace("${MOUNT} -n -o remount,rw /\n", ""), stock_rcs);
            assert!(volume.text("etc/ssh/sshd_config").contains("PermitRootLogin yes\n"));
            assert!(volume.text("etc/init.d/sshd").contains("CreateKeyIfAbsent ecdsa"));
        };
        patch(&mut volume, &options(), false).unwrap();
        off(&volume);
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        patch(&mut volume, &Options { ssh: true, ..options() }, false).unwrap();
        on(&volume);
        patch(&mut volume, &options(), false).unwrap();
        off(&volume);
        patch(&mut volume, &Options { ssh: true, ..options() }, false).unwrap();
        on(&volume);
        // An rcS that does not start sshd is not one this edit knows.
        let mut odd = FakeVolume::new(stock());
        odd.files.get_mut("etc/init.d/rcS").unwrap().2 = b"a\n${INITDIR}/mountfs start\n${INITDIR}/nestlabs start\n".to_vec();
        assert!(patch(&mut odd, &options(), false).unwrap_err().to_string().contains("sshd start"));
    }

    #[test]
    fn unsupported_nlclient_is_left_unchanged() {
        let mut volume = FakeVolume::new(stock());
        volume.files.insert(crate::nlclient::BINARY.into(), ("0:0".into(), 0o755, b"other firmware".to_vec()));
        let changes = patch(&mut volume, &options(), false).unwrap();
        assert!(!names(&changes).contains(crate::nlclient::BINARY));
        assert!(!volume.files.contains_key(crate::nlclient::JOURNAL));
        assert_eq!(volume.files[crate::nlclient::BINARY].2, b"other firmware");
    }

    #[test]
    #[ignore = "requires NLCLIENT_STOCK pointing to the researched firmware"]
    fn native_screen_patch_is_automatic_dry_run_safe_and_idempotent() {
        let source = std::fs::read(std::env::var("NLCLIENT_STOCK").expect("NLCLIENT_STOCK")).unwrap();
        let mut volume = FakeVolume::new(stock());
        volume.files.insert(crate::nlclient::BINARY.into(), ("0:0".into(), 0o755, source));
        let changes = patch(&mut volume, &options(), true).unwrap();
        assert!(volume.commands.is_empty());
        assert!(names(&changes).contains(crate::nlclient::BINARY));
        assert!(names(&changes).contains(crate::nlclient::ORIGINAL));
        assert!(!volume.files.contains_key(crate::nlclient::ORIGINAL));
        let changes = patch(&mut volume, &options(), false).unwrap();
        assert!(names(&changes).contains(crate::nlclient::ICON));
        assert_eq!(volume.files[crate::nlclient::GLYPH].2, crate::nlclient::GLYPH_BYTES);
        let journal = &volume.files[crate::nlclient::JOURNAL].2;
        let restored = crate::nlclient::Patch::parse(journal).unwrap().apply(&volume.files[crate::nlclient::BINARY].2, true).unwrap();
        assert_eq!(sha256_hex(&restored), crate::nlclient::Patch::parse(journal).unwrap().stock_sha);
        assert_eq!(volume.files[crate::nlclient::ORIGINAL], ("0:0".into(), 0o600, restored.clone()));
        let backup_write = volume.commands.iter().position(|c| c.starts_with(&format!("write {} ", crate::nlclient::ORIGINAL))).unwrap();
        let binary_write = volume.commands.iter().position(|c| c.starts_with(&format!("write {} ", crate::nlclient::BINARY))).unwrap();
        assert!(backup_write < binary_write);
        volume.commands.clear();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert!(volume.commands.is_empty());
        // Upgrade older installations that only have the undo journal.
        volume.files.remove(crate::nlclient::ORIGINAL);
        let changes = patch(&mut volume, &options(), false).unwrap();
        assert_eq!(names(&changes), BTreeSet::from([crate::nlclient::ORIGINAL]));
        assert_eq!(volume.files[crate::nlclient::ORIGINAL].2, restored);
        // Even existing backup permissions are preserved on later runs.
        volume.files.get_mut(crate::nlclient::ORIGINAL).unwrap().1 = 0o400;
        volume.commands.clear();
        let current_journal = volume.files[crate::nlclient::JOURNAL].2.clone();
        // Simulate a prior payload revision and its matching on-device journal.
        use std::io::Write;
        let mut old_binary = volume.files[crate::nlclient::BINARY].2.clone();
        *old_binary.last_mut().unwrap() ^= 1;
        let mut old_journal = Vec::new();
        flate2::read::GzDecoder::new(current_journal.as_slice()).read_to_end(&mut old_journal).unwrap();
        use sha2::Digest;
        old_journal[44..76].copy_from_slice(&sha2::Sha256::digest(&old_binary));
        *old_journal.last_mut().unwrap() ^= 1;
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zip.write_all(&old_journal).unwrap();
        volume.files.get_mut(crate::nlclient::JOURNAL).unwrap().2 = zip.finish().unwrap();
        volume.files.get_mut(crate::nlclient::BINARY).unwrap().2 = old_binary;
        let changes = patch(&mut volume, &options(), false).unwrap();
        assert!(names(&changes).contains(crate::nlclient::BINARY));
        assert_eq!(volume.files[crate::nlclient::JOURNAL].2, current_journal);
        assert_eq!(volume.files[crate::nlclient::ORIGINAL], ("0:0".into(), 0o400, restored));
        assert!(!volume.commands.iter().any(|c| c.starts_with(&format!("write {} ", crate::nlclient::ORIGINAL))));
        assert!(patch(&mut volume, &options(), true).unwrap().is_empty());
        // An unexpected existing backup is never replaced.
        volume.files.get_mut(crate::nlclient::ORIGINAL).unwrap().2 = b"keep me".to_vec();
        volume.commands.clear();
        assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("original backup does not match"));
        assert!(volume.commands.is_empty());
        assert_eq!(volume.files[crate::nlclient::ORIGINAL].2, b"keep me");
    }

    #[test]
    fn unknown_boot_artwork_is_left_untouched_without_backups() {
        let mut volume = FakeVolume::new(stock());
        for (name, _) in crate::boot_branding::FILES {
            volume.files.insert(name.into(), ("0:0".into(), 0o644, b"custom artwork".to_vec()));
        }
        let changes = patch(&mut volume, &options(), false).unwrap();
        for (name, backup) in crate::boot_branding::FILES {
            assert!(!names(&changes).contains(name));
            assert_eq!(volume.files[name].2, b"custom artwork");
            assert!(!volume.files.contains_key(backup));
        }
    }

    #[test]
    fn recognized_boot_artwork_is_automatic_dry_run_safe_and_backed_up_once() {
        let Some(dir) = std::env::var_os("BOOT_BRANDING_FIXTURES").map(std::path::PathBuf::from) else { return; };
        let mut volume = FakeVolume::new(stock());
        let mut stock_art = BTreeMap::new();
        for ((name, _), file) in crate::boot_branding::FILES.iter().zip(["splash-stock.png", "animation-stock.png", "kernel-stock.itb"]) {
            let source = std::fs::read(dir.join(file)).unwrap();
            stock_art.insert(*name, source.clone());
            volume.files.insert((*name).into(), ("0:0".into(), 0o644, source));
        }
        let dry = patch(&mut volume, &options(), true).unwrap();
        assert!(volume.commands.is_empty());
        for &(name, backup) in &crate::boot_branding::FILES[..2] {
            assert!(names(&dry).contains(name) && names(&dry).contains(backup));
            assert!(!volume.files.contains_key(backup));
        }
        assert_eq!(patch(&mut volume, &options(), false).unwrap(), dry);
        let (kernel, kernel_backup) = crate::boot_branding::FILES[2];
        assert_eq!(volume.files[kernel].2, stock_art[kernel]);
        assert!(!volume.files.contains_key(kernel_backup));
        for &(name, backup) in &crate::boot_branding::FILES[..2] {
            assert_ne!(volume.files[name].2, stock_art[name]);
            assert_eq!(volume.files[backup], ("0:0".into(), 0o600, stock_art[name].clone()));
            let before = volume.commands.iter().position(|c| c.starts_with(&format!("write {backup} "))).unwrap();
            let after = volume.commands.iter().position(|c| c.starts_with(&format!("write {name} "))).unwrap();
            assert!(before < after);
        }
        volume.commands.clear();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert!(volume.commands.is_empty());
        // A restored original can be recolored using its existing untouched backup.
        let (name, backup) = crate::boot_branding::FILES[0];
        volume.files.get_mut(name).unwrap().2 = stock_art[name].clone();
        volume.files.get_mut(backup).unwrap().1 = 0o400;
        let changed = patch(&mut volume, &options(), false).unwrap();
        assert_eq!(names(&changed), BTreeSet::from([name]));
        assert_eq!(volume.files[backup].1, 0o400);
        assert!(!volume.commands.iter().any(|c| c.starts_with(&format!("write {backup} "))));
        // A backup from another firmware is preserved along with its live artwork.
        volume.files.get_mut(name).unwrap().2 = stock_art[name].clone();
        volume.files.get_mut(backup).unwrap().2 = b"keep this backup".to_vec();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert_eq!(volume.files[name].2, stock_art[name]);
        assert_eq!(volume.files[backup].2, b"keep this backup");
    }

    #[test]
    fn kernel_branding_is_restored_only_when_it_matches_its_original_backup() {
        let Some(dir) = std::env::var_os("BOOT_BRANDING_FIXTURES").map(std::path::PathBuf::from) else { return; };
        let (name, backup) = crate::boot_branding::FILES[2];
        let original = std::fs::read(dir.join("kernel-stock.itb")).unwrap();
        let branded = crate::boot_branding::recolor(name, &original).unwrap().unwrap();
        let mut volume = FakeVolume::new(stock());
        patch(&mut volume, &options(), false).unwrap();
        volume.commands.clear();
        volume.files.insert(name.into(), ("0:0".into(), 0o644, branded.clone()));
        volume.files.insert(backup.into(), ("0:0".into(), 0o400, original.clone()));
        let dry = patch(&mut volume, &options(), true).unwrap();
        assert_eq!(names(&dry), BTreeSet::from([name]));
        assert!(volume.commands.is_empty());
        assert_eq!(volume.files[name].2, branded);
        assert_eq!(patch(&mut volume, &options(), false).unwrap(), dry);
        assert_eq!(volume.files[name].2, original);
        assert_eq!(volume.files[backup], ("0:0".into(), 0o400, original));
        volume.commands.clear();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert!(volume.commands.is_empty());
        volume.files.get_mut(name).unwrap().2 = b"custom kernel".to_vec();
        assert!(patch(&mut volume, &options(), false).unwrap().is_empty());
        assert_eq!(volume.files[name].2, b"custom kernel");
    }

    #[test]
    fn failed_boot_backup_never_replaces_live_artwork() {
        let Some(dir) = std::env::var_os("BOOT_BRANDING_FIXTURES").map(std::path::PathBuf::from) else { return; };
        let (name, backup) = crate::boot_branding::FILES[0];
        let source = std::fs::read(dir.join("splash-stock.png")).unwrap();
        let mut volume = FakeVolume::new(stock());
        volume.files.insert(name.into(), ("0:0".into(), 0o644, source.clone()));
        volume.failing = Some(backup);
        assert!(patch(&mut volume, &options(), false).is_err());
        assert_eq!(volume.files[name].2, source);
        assert!(!volume.files.contains_key(backup));
    }

    /// Supply deterministic in-memory screen revisions to exercise recovery,
    /// including interrupted writes. No shell scripts or process-wide env vars.
    struct FakeBuild(std::cell::RefCell<Vec<u8>>);
    impl FakeBuild {
        fn new() -> Self { Self(std::cell::RefCell::new(Vec::new())) }
        fn stock(&self) -> Vec<u8> { [&b"\x7fELF\nstock nlclient\n"[..], CHECK_URLS.0].concat() }
        fn makes(&self, output: &[u8]) -> Vec<u8> {
            let journal = crate::nlclient::tests::journal(&self.stock(), output);
            *self.0.borrow_mut() = journal.clone();
            journal
        }
        fn patch(&self, volume: &mut FakeVolume, options: &Options, dry_run: bool) -> Result<Vec<Change>> {
            patch_on_device(volume, Device::Diamond3, options, b"ARMV7", &mut no_pki, dry_run,
                &|source| {
                    if source != self.stock() { return device("fake screen builder received different firmware"); }
                    Ok(self.0.borrow().clone())
                })
        }
    }

    fn with_nlclient(binary: Vec<u8>) -> FakeVolume {
        let mut volume = FakeVolume::new(stock());
        volume.files.insert(crate::nlclient::BINARY.into(), ("0:0".into(), 0o755, binary));
        volume
    }

    fn position(volume: &FakeVolume, name: &str) -> usize {
        volume.commands.iter().position(|c| c.starts_with(&format!("write {name} ")))
            .unwrap_or_else(|| panic!("{name} not written: {:?}", volume.commands))
    }

    #[test]
    fn native_screen_backs_up_the_original_first_and_keeps_the_journal_last() {
        use crate::nlclient::{BINARY, GLYPH, ICON, JOURNAL, ORIGINAL};
        let build = FakeBuild::new();
        let output = [&build.stock()[..], b"UNFR0ZEN screen"].concat();
        let journal = build.makes(&output);
        let mut volume = with_nlclient(build.stock());
        let changes = build.patch(&mut volume, &options(), true).unwrap();
        for name in [BINARY, ORIGINAL, JOURNAL, ICON, GLYPH, "etc/unfr0zen/"] {
            assert!(names(&changes).contains(name), "{name}");
        }
        assert!(volume.commands.is_empty());
        build.patch(&mut volume, &options(), false).unwrap();
        assert_eq!(volume.files[BINARY], ("0:0".into(), 0o755, output));
        assert_eq!(volume.files[ORIGINAL], ("0:0".into(), 0o600, build.stock()));
        assert_eq!(volume.files[JOURNAL].2, journal);
        assert!(position(&volume, ORIGINAL) < position(&volume, BINARY));
        assert!(position(&volume, BINARY) < position(&volume, JOURNAL));
        volume.commands.clear();
        assert!(build.patch(&mut volume, &options(), false).unwrap().is_empty());
        assert!(volume.commands.is_empty());
    }

    #[test]
    fn native_screen_names_the_app_version_and_a_newer_app_rewrites_only_that() {
        use crate::nlclient::VERSION;
        let build = FakeBuild::new();
        build.makes(&[&build.stock()[..], b"UNFR0ZEN screen"].concat());
        let mut volume = with_nlclient(build.stock());
        let first = Options { version: "v1.2-3-gabc1234".into(), ..options() };
        build.patch(&mut volume, &first, false).unwrap();
        assert_eq!(volume.files[VERSION], ("0:0".into(), 0o644, b"unfr0zen v1.2-3-gabc1234\n".to_vec()));
        let changes = build.patch(&mut volume, &Options { version: "v1.3".into(), ..first }, false).unwrap();
        assert_eq!(changes.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), [VERSION]);
        assert_eq!(volume.files[VERSION].2, b"unfr0zen v1.3\n");
    }

    #[test]
    fn block_cloud_moves_the_internet_check_to_cloudflare_and_back() {
        use crate::nlclient::{BINARY, JOURNAL, Patch};
        let (google, cloudflare) = CHECK_URLS;
        let connmand = [&b"\x7fELF connmand\0"[..], google, b"%s"].concat();
        let build = FakeBuild::new();
        let output = [&build.stock()[..], b"UNFR0ZEN screen"].concat();
        build.makes(&output);
        let mut volume = with_nlclient(build.stock());
        volume.files.insert(CONNMAND.into(), ("0:0".into(), 0o755, connmand.clone()));
        volume.files.insert("etc/hosts".into(), ("0:0".into(), 0o644, b"127.0.0.1 localhost\n".to_vec()));
        let contains = |data: &[u8], url: &[u8]| data.windows(url.len()).any(|w| w == url);
        let blocked = Options { block_cloud: true, ..options() };
        build.patch(&mut volume, &blocked, false).unwrap();
        for name in [CONNMAND, BINARY] {
            let data = &volume.files[name].2;
            assert!(contains(data, cloudflare) && !contains(data, google), "{name}");
        }
        assert_eq!(volume.files[CONNMAND].2.len(), connmand.len());
        assert_eq!(volume.files[CONNMAND_ORIGINAL], ("0:0".into(), 0o600, connmand.clone()));
        assert!(position(&volume, CONNMAND_ORIGINAL) < position(&volume, CONNMAND));
        let journal = Patch::parse(&volume.files[JOURNAL].2).unwrap();
        assert_eq!(journal.apply(&volume.files[BINARY].2, true).unwrap(), build.stock());
        assert!(build.patch(&mut volume, &blocked, false).unwrap().is_empty());
        build.patch(&mut volume, &options(), false).unwrap();
        assert_eq!(volume.files[CONNMAND].2, connmand);
        assert_eq!(volume.files[CONNMAND_ORIGINAL].2, connmand);
        assert_eq!(volume.files[BINARY].2, output);
        // A backup of some other connmand is never overwritten, nor patched over.
        volume.files.get_mut(CONNMAND_ORIGINAL).unwrap().2 = b"other connmand".to_vec();
        assert!(build.patch(&mut volume, &blocked, false).unwrap_err().to_string().contains("connmand original backup"));
        assert_eq!(volume.files[CONNMAND].2, connmand);
        volume.files.remove(CONNMAND_ORIGINAL);
        // A connmand without the URL is left alone; one with it twice is refused.
        volume.files.insert(CONNMAND.into(), ("0:0".into(), 0o755, b"\x7fELF connmand\0".to_vec()));
        build.patch(&mut volume, &blocked, false).unwrap();
        assert_eq!(volume.files[CONNMAND].2, b"\x7fELF connmand\0");
        volume.files.insert(CONNMAND.into(), ("0:0".into(), 0o755, [google, google].concat()));
        assert!(build.patch(&mut volume, &blocked, false).unwrap_err().to_string().contains(CONNMAND));
    }

    #[test]
    fn a_failed_nlclient_write_keeps_the_previous_journal() {
        use crate::nlclient::{BINARY, JOURNAL, ORIGINAL};
        let build = FakeBuild::new();
        let first = [&build.stock()[..], b"first screen"].concat();
        let old_journal = build.makes(&first);
        let mut volume = with_nlclient(build.stock());
        build.patch(&mut volume, &options(), false).unwrap();
        build.makes(&[&build.stock()[..], b"second screen"].concat());
        volume.failing = Some(BINARY);
        assert!(build.patch(&mut volume, &options(), false).unwrap_err().to_string().contains("no space left"));
        assert_eq!(volume.files[BINARY].2, first);
        assert_eq!(volume.files[JOURNAL].2, old_journal);
        assert_eq!(volume.files[ORIGINAL].2, build.stock());
    }

    #[test]
    fn a_failed_journal_write_is_rebuilt_from_the_original_on_the_next_run() {
        use crate::nlclient::{BINARY, JOURNAL, ORIGINAL};
        let build = FakeBuild::new();
        let first = [&build.stock()[..], b"first screen"].concat();
        let first_journal = build.makes(&first);
        let mut volume = with_nlclient(build.stock());
        volume.failing = Some(JOURNAL);
        assert!(build.patch(&mut volume, &options(), false).unwrap_err().to_string().contains("no space left"));
        assert_eq!(volume.files[BINARY].2, first);
        assert!(!volume.files.contains_key(JOURNAL));
        volume.failing = None;
        volume.commands.clear();
        let changes = build.patch(&mut volume, &options(), false).unwrap();
        assert_eq!(names(&changes), BTreeSet::from([JOURNAL]));
        assert_eq!(volume.files[JOURNAL].2, first_journal);
        // The same after an upgrade whose new journal did not reach flash.
        let second = [&build.stock()[..], b"second"].concat();
        let second_journal = build.makes(&second);
        volume.failing = Some(JOURNAL);
        assert!(build.patch(&mut volume, &options(), false).is_err());
        assert_eq!((&volume.files[BINARY].2, &volume.files[JOURNAL].2), (&second, &first_journal));
        volume.failing = None;
        volume.commands.clear();
        assert_eq!(names(&build.patch(&mut volume, &options(), false).unwrap()), BTreeSet::from([JOURNAL]));
        assert_eq!(volume.files[JOURNAL].2, second_journal);
        assert_eq!(volume.files[ORIGINAL], ("0:0".into(), 0o600, build.stock()));
        assert!(!volume.commands.iter().any(|c| c.starts_with(&format!("write {ORIGINAL} ")) || c.starts_with(&format!("write {BINARY} "))));
        // A build that no longer gives the executable on the unit is refused.
        build.makes(&[&build.stock()[..], b"third"].concat());
        volume.files.remove(JOURNAL);
        volume.commands.clear();
        assert!(build.patch(&mut volume, &options(), false).unwrap_err().to_string().contains("original backup does not match"));
        assert!(volume.commands.is_empty());
    }

    #[test]
    fn a_new_screen_upgrades_through_the_old_journal_and_keeps_the_backup() {
        use crate::nlclient::{BINARY, JOURNAL, ORIGINAL};
        let build = FakeBuild::new();
        // The first build changes stock bytes the second leaves alone.
        let mut first = build.stock();
        first[1] = b'e';
        first.extend_from_slice(b"first screen, longer than the second");
        build.makes(&first);
        let mut volume = with_nlclient(build.stock());
        build.patch(&mut volume, &options(), false).unwrap();
        volume.files.get_mut(ORIGINAL).unwrap().1 = 0o400;
        let second = [&build.stock()[..], b"second"].concat();
        let journal = build.makes(&second);
        volume.commands.clear();
        let changes = build.patch(&mut volume, &options(), false).unwrap();
        let screen = changes.iter().find(|c| c.name == BINARY).unwrap();
        assert_eq!((screen.old.clone(), screen.new.clone()), (Some(sha256_hex(&first)), Some(sha256_hex(&second))));
        assert_eq!(volume.files[BINARY].2, second);
        assert_eq!(volume.files[JOURNAL].2, journal);
        assert_eq!(volume.files[ORIGINAL], ("0:0".into(), 0o400, build.stock()));
        assert!(!volume.commands.iter().any(|c| c.starts_with(&format!("write {ORIGINAL} "))));
    }

    #[test]
    fn an_unrecognised_journal_or_backup_stops_the_patch_before_any_write() {
        use crate::nlclient::{JOURNAL, ORIGINAL};
        let build = FakeBuild::new();
        build.makes(&[&build.stock()[..], b"screen"].concat());
        let mut foreign = with_nlclient(build.stock());
        let other = crate::nlclient::tests::journal(b"\x7fELF other firmware", b"\x7fELF other firmware+");
        foreign.files.insert(JOURNAL.into(), ("0:0".into(), 0o600, other));
        assert!(build.patch(&mut foreign, &options(), false).unwrap_err().to_string().contains("undo journal does not match"));
        assert!(foreign.commands.is_empty());
        let mut kept = with_nlclient(build.stock());
        kept.files.insert(ORIGINAL.into(), ("0:0".into(), 0o600, b"keep me".to_vec()));
        assert!(build.patch(&mut kept, &options(), false).unwrap_err().to_string().contains("original backup does not match"));
        assert!(kept.commands.is_empty());
        assert_eq!(kept.files[ORIGINAL].2, b"keep me");
    }

    #[test]
    fn nlclient_must_be_a_root_owned_executable() {
        for (owner, mode) in [("1000:0", 0o755), ("0:0", 0o644)] {
            let mut volume = FakeVolume::new(stock());
            volume.files.insert(crate::nlclient::BINARY.into(), (owner.into(), mode, b"\x7fELF".to_vec()));
            assert!(patch(&mut volume, &options(), false).unwrap_err().to_string().contains("root-owned executable"));
            assert!(volume.commands.is_empty());
        }
    }

    #[test]
    fn root_hash_is_sha512_crypt() {
        let hash = root_hash("unfr0zen").unwrap();
        assert!(hash.starts_with("$6$") && hash.split('$').count() == 4, "{hash}");
        assert!(root_hash("a:b").is_err() && root_hash("").is_err());
        assert!(check_ntp_server("-bad").is_err() && check_ntp_server("pool.ntp.org").is_ok());
    }
}
