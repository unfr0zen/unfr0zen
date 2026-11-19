//! The WiFi chip's answers to ARP while the unit sleeps. Asleep, the host
//! answers nothing: the gen 3's chip (brcmfmac) answers ARP requests for the
//! addresses in its host table itself (ARP offload), and the packet filters
//! nlsleepd sets before each sleep pass no ARP to the host. A router that has
//! forgotten the unit's address asks for it again; unanswered, it drops what
//! it has for the unit, the broker's messages among them, until the unit
//! sends something itself.
//!
//! The driver fills the table from the interface's IPv4 addresses, and when
//! one goes it adds back a stray entry: it reads the table into a buffer that
//! still holds the request's name, and takes the "ip" past the table's end for
//! an address (105.112.0.0). So before each sleep nestlocal reads the chip's
//! offload, logs it when it changes, and sets it to answer for the
//! interface's addresses alone.

use std::sync::Mutex;

use crate::logf;

/// arp_ol's offload agent, and its answering the peers' requests.
const ARP_OL_AGENT: u32 = 0x1;
const ARP_OL_PEER_AUTO_REPLY: u32 = 0x8;
const ARP_OL_NEEDED: u32 = ARP_OL_AGENT | ARP_OL_PEER_AUTO_REPLY;
/// Entries in the chip's host table (the driver's ARPOL_MAX_ENTRIES).
const HOST_ENTRIES: usize = 8;

/// What was last logged, so that a state is logged once.
static LOGGED: Mutex<String> = Mutex::new(String::new());

/// The chip's ARP offload.
#[derive(Debug, PartialEq)]
struct Offload {
    /// arpoe.
    enabled: bool,
    /// arp_ol.
    mode: u32,
    /// arp_hostip.
    hosts: Vec<[u8; 4]>,
}

impl Offload {
    fn answers_for(&self, addrs: &[[u8; 4]]) -> bool {
        self.enabled && self.mode & ARP_OL_NEEDED == ARP_OL_NEEDED && self.hosts == addrs
    }

    fn describe(&self) -> String {
        let hosts: Vec<String> = self.hosts.iter().map(|h| std::net::Ipv4Addr::from(*h).to_string()).collect();
        format!(
            "{}, mode {:#x}, answering for {}",
            if self.enabled { "on" } else { "off" },
            self.mode,
            if hosts.is_empty() { "nothing".into() } else { hosts.join(", ") }
        )
    }
}

/// arp_hostip's reply: the addresses up to the first empty entry.
fn hosts(table: &[u8]) -> Vec<[u8; 4]> {
    table.as_chunks::<4>().0.iter().copied().take(HOST_ENTRIES).take_while(|e| *e != [0; 4]).collect()
}

/// Has the chip of `iface`, if brcmfmac's, answer ARP for the interface's
/// IPv4 addresses while the unit sleeps. Blocks for the chip's answers.
pub fn check(iface: &str) {
    let driver = std::fs::read_link(format!("/sys/class/net/{iface}/device/driver")).unwrap_or_default();
    if !driver.ends_with("brcmfmac") {
        return;
    }
    let addrs = chip::addresses(iface);
    if addrs.is_empty() {
        return;
    }
    let (state, repair) = match chip::Chip::open(iface).and_then(|chip| ensure(&chip, &addrs)) {
        Ok(found) => found,
        Err(e) => (e, None),
    };
    let mut logged = LOGGED.lock().unwrap();
    match repair {
        Some(repair) => logf!("wifi: the chip's ARP offload {repair}"),
        None if *logged != state => logf!("wifi: the chip's ARP offload: {state}"),
        None => {}
    }
    *logged = state;
}

/// Reads the offload and sets what differs from answering for `addrs`
/// alone: the offload then, and what was found and set if anything was.
fn ensure(chip: &impl Iovars, addrs: &[[u8; 4]]) -> Result<(String, Option<String>), String> {
    let found = read(chip)?;
    if found.answers_for(addrs) {
        return Ok((found.describe(), None));
    }
    if !found.enabled {
        chip.set("arpoe", &1u32.to_ne_bytes())?;
    }
    if found.mode & ARP_OL_NEEDED != ARP_OL_NEEDED {
        chip.set("arp_ol", &(found.mode | ARP_OL_NEEDED).to_ne_bytes())?;
    }
    if found.hosts != addrs {
        chip.set("arp_hostip_clear", &[])?;
        for addr in addrs {
            chip.set("arp_hostip", addr)?;
        }
    }
    let now = read(chip)?;
    let repair = format!(
        "was {}; {}{}",
        found.describe(),
        if now.answers_for(addrs) { "now " } else { "setting it left it " },
        now.describe()
    );
    Ok((now.describe(), Some(repair)))
}

fn read(chip: &impl Iovars) -> Result<Offload, String> {
    let int = |name| chip.get(name, 4).map(|v| u32::from_ne_bytes([v[0], v[1], v[2], v[3]]));
    Ok(Offload {
        enabled: int("arpoe")? != 0,
        mode: int("arp_ol")?,
        hosts: hosts(&chip.get("arp_hostip", HOST_ENTRIES * 4)?),
    })
}

/// The firmware's variables.
trait Iovars {
    /// At least `len` bytes of `name`.
    fn get(&self, name: &str, len: usize) -> Result<Vec<u8>, String>;
    fn set(&self, name: &str, value: &[u8]) -> Result<(), String>;
}

/// brcmfmac's firmware commands, through nl80211's vendor command.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod nl {
    pub const NLMSG_ERROR: u16 = 2;
    pub const NLM_F_REQUEST: u16 = 0x1;
    pub const NLM_F_ACK: u16 = 0x4;
    pub const GENL_ID_CTRL: u16 = 0x10;
    pub const CTRL_CMD_GETFAMILY: u8 = 3;
    pub const CTRL_ATTR_FAMILY_ID: u16 = 1;
    pub const CTRL_ATTR_FAMILY_NAME: u16 = 2;
    pub const NL80211_CMD_VENDOR: u8 = 103;
    pub const NL80211_ATTR_IFINDEX: u16 = 3;
    pub const NL80211_ATTR_VENDOR_ID: u16 = 195;
    pub const NL80211_ATTR_VENDOR_SUBCMD: u16 = 196;
    pub const NL80211_ATTR_VENDOR_DATA: u16 = 197;
    /// Broadcom's OUI, and brcmfmac's command passing a firmware ioctl
    /// through (BRCMF_VNDR_CMDS_DCMD), its reply in BRCMF_NLATTR_DATA.
    pub const BRCM_OUI: u32 = 0x001018;
    pub const BRCMF_VNDR_CMDS_DCMD: u32 = 1;
    pub const BRCMF_NLATTR_DATA: u16 = 2;
    pub const WLC_GET_VAR: u32 = 262;
    pub const WLC_SET_VAR: u32 = 263;
    /// struct brcmf_vndr_dcmd_hdr: cmd, len, offset, set, magic.
    const DCMD_HDR: u32 = 20;

    const HDR: usize = 16;
    const GENL_HDR: usize = 4;

    /// A generic netlink request.
    pub fn request(family: u16, flags: u16, cmd: u8, version: u8, attrs: &[(u16, &[u8])]) -> Vec<u8> {
        let mut m = vec![0u8; HDR + GENL_HDR];
        m[HDR] = cmd;
        m[HDR + 1] = version;
        for (kind, data) in attrs {
            m.extend_from_slice(&(4 + data.len() as u16).to_ne_bytes());
            m.extend_from_slice(&kind.to_ne_bytes());
            m.extend_from_slice(data);
            m.resize(m.len().next_multiple_of(4), 0);
        }
        let len = m.len() as u32;
        m[0..4].copy_from_slice(&len.to_ne_bytes());
        m[4..6].copy_from_slice(&family.to_ne_bytes());
        m[6..8].copy_from_slice(&(NLM_F_REQUEST | flags).to_ne_bytes());
        m
    }

    /// The messages in what one read returned: (type, payload).
    pub fn messages(mut buf: &[u8]) -> Vec<(u16, &[u8])> {
        let mut out = Vec::new();
        while buf.len() >= HDR {
            let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            if len < HDR || len > buf.len() {
                break;
            }
            out.push((u16::from_ne_bytes([buf[4], buf[5]]), &buf[HDR..len]));
            buf = &buf[len.next_multiple_of(4).min(buf.len())..];
        }
        out
    }

    /// A generic netlink message's attributes.
    pub fn attrs(payload: &[u8]) -> &[u8] {
        payload.get(GENL_HDR..).unwrap_or_default()
    }

    /// The attribute `kind` among `attrs`.
    pub fn attr(mut attrs: &[u8], kind: u16) -> Option<&[u8]> {
        while attrs.len() >= 4 {
            let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
            if len < 4 || len > attrs.len() {
                return None;
            }
            // Less the nested and byte order flags.
            if u16::from_ne_bytes([attrs[2], attrs[3]]) & 0x3fff == kind {
                return Some(&attrs[4..len]);
            }
            attrs = &attrs[len.next_multiple_of(4).min(attrs.len())..];
        }
        None
    }

    /// The vendor command's data: the header, then the iovar's name, its NUL
    /// and the value to set, or room for the value read.
    pub fn dcmd(name: &str, set: Option<&[u8]>, len: usize) -> Vec<u8> {
        let mut buf = name.as_bytes().to_vec();
        buf.push(0);
        match set {
            Some(value) => buf.extend_from_slice(value),
            None => buf.resize(buf.len() + len, 0),
        }
        let cmd = if set.is_some() { WLC_SET_VAR } else { WLC_GET_VAR };
        let mut d = Vec::with_capacity(DCMD_HDR as usize + buf.len());
        for v in [cmd, buf.len() as u32, DCMD_HDR, set.is_some() as u32, 0] {
            d.extend_from_slice(&v.to_ne_bytes());
        }
        d.extend(buf);
        d
    }
}

#[cfg(target_os = "linux")]
mod chip {
    use std::time::Duration;

    use super::{nl, Iovars};

    /// The chip's answers come at once; one that does not must not hold the
    /// unit's sleep.
    const TIMEOUT: Duration = Duration::from_secs(1);

    /// The IPv4 addresses of `iface`.
    pub fn addresses(iface: &str) -> Vec<[u8; 4]> {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if unsafe { libc::getifaddrs(&mut list) } != 0 {
            return Vec::new();
        }
        let mut addrs = Vec::new();
        let mut entry = list;
        while let Some(ifa) = unsafe { entry.as_ref() } {
            let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) };
            let addr = unsafe { ifa.ifa_addr.as_ref() };
            if name.to_bytes() == iface.as_bytes() && addr.is_some_and(|a| a.sa_family == libc::AF_INET as u16) {
                let addr = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                addrs.push(addr.sin_addr.s_addr.to_ne_bytes());
            }
            entry = ifa.ifa_next;
        }
        unsafe { libc::freeifaddrs(list) };
        addrs
    }

    /// A generic netlink socket talking to nl80211 about `iface`.
    pub struct Chip {
        fd: libc::c_int,
        family: u16,
        ifindex: u32,
    }

    impl Drop for Chip {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }

    impl Chip {
        pub fn open(iface: &str) -> Result<Chip, String> {
            let name = std::ffi::CString::new(iface).map_err(|e| e.to_string())?;
            let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if ifindex == 0 {
                return Err(format!("{iface}: {}", std::io::Error::last_os_error()));
            }
            let fd =
                unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_GENERIC) };
            if fd < 0 {
                return Err(format!("netlink: {}", std::io::Error::last_os_error()));
            }
            let mut chip = Chip { fd, family: 0, ifindex };
            let timeout = libc::timeval { tv_sec: TIMEOUT.as_secs() as _, tv_usec: 0 };
            let size = std::mem::size_of::<libc::timeval>() as u32;
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    (&timeout as *const libc::timeval).cast(),
                    size,
                )
            };
            let request = nl::request(
                nl::GENL_ID_CTRL,
                0,
                nl::CTRL_CMD_GETFAMILY,
                1,
                &[(nl::CTRL_ATTR_FAMILY_NAME, b"nl80211\0")],
            );
            let reply = chip.talk(&request, nl::GENL_ID_CTRL)?;
            chip.family = nl::attr(&reply, nl::CTRL_ATTR_FAMILY_ID)
                .filter(|id| id.len() == 2)
                .map(|id| u16::from_ne_bytes([id[0], id[1]]))
                .ok_or("no nl80211")?;
            Ok(chip)
        }

        /// Sends `request` and returns the attributes of the reply of type
        /// `kind`, empty for a command that only acknowledges.
        fn talk(&self, request: &[u8], kind: u16) -> Result<Vec<u8>, String> {
            if unsafe { libc::send(self.fd, request.as_ptr().cast(), request.len(), 0) } < 0 {
                return Err(format!("netlink: {}", std::io::Error::last_os_error()));
            }
            let mut reply = Vec::new();
            let mut buf = vec![0u8; 8192];
            loop {
                let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
                if n < 0 {
                    return Err(format!("netlink: {}", std::io::Error::last_os_error()));
                }
                for (t, payload) in nl::messages(&buf[..n as usize]) {
                    if t == nl::NLMSG_ERROR {
                        let error = payload.get(..4).map_or(0, |e| i32::from_ne_bytes([e[0], e[1], e[2], e[3]]));
                        if error != 0 {
                            return Err(std::io::Error::from_raw_os_error(-error).to_string());
                        }
                        return Ok(reply);
                    }
                    if t == kind {
                        reply = nl::attrs(payload).to_vec();
                        // nl80211's own requests end with their reply.
                        if kind == nl::GENL_ID_CTRL {
                            return Ok(reply);
                        }
                    }
                }
            }
        }

        fn dcmd(&self, data: &[u8]) -> Result<Vec<u8>, String> {
            let ifindex = self.ifindex.to_ne_bytes();
            let oui = nl::BRCM_OUI.to_ne_bytes();
            let subcmd = nl::BRCMF_VNDR_CMDS_DCMD.to_ne_bytes();
            let request = nl::request(
                self.family,
                nl::NLM_F_ACK,
                nl::NL80211_CMD_VENDOR,
                0,
                &[
                    (nl::NL80211_ATTR_IFINDEX, &ifindex),
                    (nl::NL80211_ATTR_VENDOR_ID, &oui),
                    (nl::NL80211_ATTR_VENDOR_SUBCMD, &subcmd),
                    (nl::NL80211_ATTR_VENDOR_DATA, data),
                ],
            );
            self.talk(&request, self.family)
        }
    }

    impl Iovars for Chip {
        fn get(&self, name: &str, len: usize) -> Result<Vec<u8>, String> {
            let reply = self.dcmd(&nl::dcmd(name, None, len)).map_err(|e| format!("reading {name}: {e}"))?;
            nl::attr(&reply, nl::NL80211_ATTR_VENDOR_DATA)
                .and_then(|data| nl::attr(data, nl::BRCMF_NLATTR_DATA))
                .filter(|value| value.len() >= len)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| format!("reading {name}: no value"))
        }

        fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
            self.dcmd(&nl::dcmd(name, Some(value), 0)).map(|_| ()).map_err(|e| format!("setting {name}: {e}"))
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod chip {
    use super::Iovars;

    /// The chip is Linux's: elsewhere there is none.
    pub fn addresses(_iface: &str) -> Vec<[u8; 4]> {
        Vec::new()
    }

    pub struct Chip;

    impl Chip {
        pub fn open(_iface: &str) -> Result<Chip, String> {
            Err("no chip".into())
        }
    }

    impl Iovars for Chip {
        fn get(&self, _name: &str, _len: usize) -> Result<Vec<u8>, String> {
            Err("no chip".into())
        }

        fn set(&self, _name: &str, _value: &[u8]) -> Result<(), String> {
            Err("no chip".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    const UNIT: [u8; 4] = [10, 0, 101, 44];
    /// The driver's stray entry: "ip" read as an address.
    const STRAY: [u8; 4] = [0x69, 0x70, 0, 0];

    /// A chip's variables, as the firmware keeps them.
    struct Fake {
        vars: RefCell<HashMap<String, Vec<u8>>>,
        sets: RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(enabled: u32, mode: u32, hosts: &[[u8; 4]]) -> Fake {
            let fake = Fake { vars: RefCell::default(), sets: RefCell::default() };
            let mut vars = fake.vars.borrow_mut();
            vars.insert("arpoe".into(), enabled.to_ne_bytes().to_vec());
            vars.insert("arp_ol".into(), mode.to_ne_bytes().to_vec());
            vars.insert("arp_hostip".into(), hosts.concat());
            drop(vars);
            fake
        }
    }

    impl Iovars for Fake {
        /// The table and its empty entry over what the buffer held: the name.
        fn get(&self, name: &str, len: usize) -> Result<Vec<u8>, String> {
            let mut buf = name.as_bytes().to_vec();
            buf.resize(buf.len() + 1 + len, 0);
            let value = &self.vars.borrow()[name];
            buf[..value.len()].copy_from_slice(value);
            if name == "arp_hostip" {
                buf[value.len()..value.len() + 4].fill(0);
            }
            Ok(buf)
        }

        fn set(&self, name: &str, value: &[u8]) -> Result<(), String> {
            self.sets.borrow_mut().push(name.to_string());
            let mut vars = self.vars.borrow_mut();
            match name {
                "arp_hostip_clear" => vars.get_mut("arp_hostip").unwrap().clear(),
                "arp_hostip" => vars.get_mut("arp_hostip").unwrap().extend_from_slice(value),
                _ => *vars.get_mut(name).unwrap() = value.to_vec(),
            }
            Ok(())
        }
    }

    #[test]
    fn the_host_table_ends_at_its_first_empty_entry() {
        // One address, its empty entry, then the "ip\0" of "arp_hostip".
        let read = [10, 0, 101, 44, 0, 0, 0, 0, 0x69, 0x70, 0, 0, 0, 0, 0, 0];
        assert_eq!(hosts(&read), vec![UNIT]);
        assert_eq!(hosts(&[STRAY, UNIT, [0; 4]].concat()), vec![STRAY, UNIT]);
        assert_eq!(hosts(&[0; 32]), Vec::<[u8; 4]>::new());
        assert_eq!(hosts(&[[1, 2, 3, 4]; 9].concat()).len(), HOST_ENTRIES);
    }

    #[test]
    fn an_offload_answering_for_the_unit_alone_is_left_as_it_is() {
        let chip = Fake::new(1, 9, &[UNIT]);
        let found = ensure(&chip, &[UNIT]).unwrap();
        assert_eq!(found, ("on, mode 0x9, answering for 10.0.101.44".into(), None));
        assert!(chip.sets.borrow().is_empty());
    }

    #[test]
    fn the_drivers_stray_entry_is_cleared() {
        let chip = Fake::new(1, 9, &[STRAY, UNIT]);
        let (state, repair) = ensure(&chip, &[UNIT]).unwrap();
        assert_eq!(*chip.sets.borrow(), ["arp_hostip_clear", "arp_hostip"]);
        assert_eq!(state, "on, mode 0x9, answering for 10.0.101.44");
        assert_eq!(
            repair.as_deref(),
            Some(
                "was on, mode 0x9, answering for 105.112.0.0, 10.0.101.44; \
                 now on, mode 0x9, answering for 10.0.101.44"
            )
        );
    }

    #[test]
    fn an_offload_turned_off_or_not_answering_peers_is_turned_on() {
        let chip = Fake::new(0, 0x2, &[]);
        let (state, repair) = ensure(&chip, &[UNIT]).unwrap();
        assert_eq!(*chip.sets.borrow(), ["arpoe", "arp_ol", "arp_hostip_clear", "arp_hostip"]);
        assert_eq!(state, "on, mode 0xb, answering for 10.0.101.44");
        assert_eq!(
            repair.as_deref(),
            Some("was off, mode 0x2, answering for nothing; now on, mode 0xb, answering for 10.0.101.44")
        );
    }

    #[test]
    fn a_vendor_command_carries_the_iovar_after_its_header() {
        let get = nl::dcmd("arpoe", None, 4);
        assert_eq!(get[..20], [[6, 1, 0, 0], [10, 0, 0, 0], [20, 0, 0, 0], [0; 4], [0; 4]].concat()[..]);
        assert_eq!(&get[20..], b"arpoe\0\0\0\0\0");
        let set = nl::dcmd("arp_hostip", Some(&UNIT), 0);
        assert_eq!(set[..20], [[7, 1, 0, 0], [15, 0, 0, 0], [20, 0, 0, 0], [1, 0, 0, 0], [0; 4]].concat()[..]);
        assert_eq!(&set[20..], b"arp_hostip\0\x0a\x00\x65\x2c");
        assert_eq!(nl::dcmd("arp_hostip_clear", Some(&[]), 0).len(), 20 + 17);
    }

    #[test]
    fn netlink_attributes_are_padded_and_found_again() {
        let request =
            nl::request(0x1c, nl::NLM_F_ACK, nl::NL80211_CMD_VENDOR, 0, &[(3, &[7, 0, 0, 0]), (197, b"abcde")]);
        assert_eq!(request.len(), 16 + 4 + 8 + 12);
        let messages = nl::messages(&request);
        assert_eq!(messages.len(), 1);
        let (kind, payload) = messages[0];
        assert_eq!(kind, 0x1c);
        let attrs = nl::attrs(payload);
        assert_eq!(nl::attr(attrs, 3), Some(&[7, 0, 0, 0][..]));
        assert_eq!(nl::attr(attrs, 197), Some(&b"abcde"[..]));
        assert_eq!(nl::attr(attrs, 196), None);
        // A truncated attribute ends the search.
        assert_eq!(nl::attr(&attrs[..10], 197), None);
    }
}
