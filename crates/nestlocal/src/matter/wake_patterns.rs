//! What reaches a sleeping unit from a Matter controller. Asleep, the Wi-Fi
//! chip wakes the unit for the frames its patterns match, and nlsleepd sets
//! them before each sleep:
//! - a controller's messages, unicast UDP to Matter's port. The gen 3's
//!   nlsleepd takes the port (wake.rs registers it); the gen 2's has no such
//!   call, and these patterns are listed here.
//! - a controller that has lost the unit's address asks for it first, by
//!   multicast, which no pattern of nlsleepd's matches: a DNS-SD query for
//!   the unit's operational instance (mDNS, UDP 5353), and an IPv6 neighbour
//!   solicitation for one of its addresses. The chips answer ARP themselves.
//!   The gen 2's WL1271 passes no multicast frame while the unit sleeps,
//!   whatever its patterns: it answers those on its own wakes.
//!
//! nestlocal lists a pattern for each in `LIST`, and the library preloaded
//! into nlsleepd (platform/common/wakefilter, which the app installs
//! and names in nlsleepd's init script with Matter) adds them to
//! nlsleepd's before each sleep: a line a pattern, `OFFSET MASK PATTERN`, the
//! offset into the frame in decimal, nl80211's mask (a bit a pattern byte,
//! least significant first) and the bytes in hex.
//!
//! A pattern is bytes at fixed offsets, so a query wakes the unit when the
//! instance is its first question. Each generation's chip takes so many
//! patterns of so many bytes, nlsleepd's own first (`Budget`); the list goes
//! by priority, Matter's port, then the solicitations, then the queries: when
//! the instances of every fabric do not fit, one pattern for each IP version
//! (IPv6's alone, without room for both) matches any operational instance's
//! query instead, another node's included. A pattern longer than the chip
//! takes is cut, which matches more.

use std::net::Ipv6Addr;

pub const LIST: &str = "/var/run/unfr0zen/wake-patterns";
/// Where the app installs the library.
pub const LIBRARY: &str = "/nestlabs/lib/libwakefilter.so";

/// What a generation's chip leaves for the list, after nlsleepd's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    patterns: usize,
    length: usize,
    /// The list carries Matter's port: nlsleepd cannot.
    matter_port: bool,
    /// The chip passes multicast frames to its patterns while the unit sleeps.
    multicast: bool,
}

impl Budget {
    pub fn of(generation: u8) -> Budget {
        match generation {
            // The gen 2's WL1271 takes 7 patterns of up to 91 bytes; nlsleepd
            // sets 3: the Wi-Fi rekey and the broker's connection twice.
            2 => Budget { patterns: 4, length: 91, matter_port: true, multicast: false },
            // The gen 3's BCM43340 takes 8 of up to 128; nlsleepd sets 4: the
            // rekey, the broker's connection, Matter's port over IPv4 and IPv6.
            _ => Budget { patterns: 4, length: 128, matter_port: false, multicast: true },
        }
    }
}

const ETHERTYPE: usize = 12;
/// An operational instance is the fabric's and the node's identifiers,
/// sixteen hex digits each, around a hyphen.
const INSTANCE_LEN: usize = 33;

/// Bytes of a frame from its start; the others match anything.
#[derive(Default)]
struct Pattern(Vec<Option<u8>>);

impl Pattern {
    fn at(mut self, offset: usize, bytes: &[u8]) -> Pattern {
        if self.0.len() < offset + bytes.len() {
            self.0.resize(offset + bytes.len(), None);
        }
        for (i, byte) in bytes.iter().enumerate() {
            self.0[offset + i] = Some(*byte);
        }
        self
    }

    fn line(&self) -> String {
        let mut mask = vec![0u8; self.0.len().div_ceil(8)];
        for (i, byte) in self.0.iter().enumerate() {
            if byte.is_some() {
                mask[i / 8] |= 1 << (i % 8);
            }
        }
        let hex = |bytes: &mut dyn Iterator<Item = u8>| bytes.map(|b| format!("{b:02x}")).collect::<String>();
        format!("0 {} {}\n", hex(&mut mask.into_iter()), hex(&mut self.0.iter().map(|b| b.unwrap_or(0))))
    }
}

/// Unicast UDP to Matter's port on the unit, over IPv6 or IPv4.
fn matter_port(mac: [u8; 6], ipv6: bool) -> Pattern {
    let port = rs_matter::MATTER_PORT.to_be_bytes();
    if ipv6 {
        Pattern::default().at(0, &mac).at(ETHERTYPE, &[0x86, 0xdd]).at(20, &[17]).at(56, &port)
    } else {
        Pattern::default().at(0, &mac).at(ETHERTYPE, &[0x08, 0x00]).at(23, &[17]).at(36, &port)
    }
}

/// A neighbour solicitation for `address`: ICMPv6 type 135 to the address's
/// solicited-node group, which only its last three bytes tell apart.
fn neighbour_solicitation(address: &Ipv6Addr) -> Pattern {
    let a = address.octets();
    Pattern::default().at(0, &[0x33, 0x33, 0xff, a[13], a[14], a[15]]).at(ETHERTYPE, &[0x86, 0xdd]).at(20, &[58]).at(54, &[135])
}

/// An mDNS packet whose first name starts with `instance`, or with any
/// operational instance.
fn mdns_query(ipv6: bool, instance: Option<&str>) -> Pattern {
    // The first question's name follows the Ethernet, IP, UDP and DNS headers.
    let (pattern, name) = if ipv6 {
        (Pattern::default().at(0, &[0x33, 0x33, 0, 0, 0, 0xfb]).at(ETHERTYPE, &[0x86, 0xdd]).at(20, &[17]).at(56, &[0x14, 0xe9]), 74)
    } else {
        (Pattern::default().at(0, &[0x01, 0x00, 0x5e, 0, 0, 0xfb]).at(ETHERTYPE, &[0x08, 0x00]).at(23, &[17]).at(36, &[0x14, 0xe9]), 54)
    };
    let pattern = pattern.at(name, &[INSTANCE_LEN as u8]);
    match instance {
        Some(instance) => pattern.at(name + 1, instance.as_bytes()),
        None => pattern.at(name + 1 + 16, b"-"),
    }
}

/// The list for a unit with this budget, Ethernet address, IPv6 addresses
/// and operational instances.
pub fn list(budget: Budget, mac: [u8; 6], addresses: &[Ipv6Addr], instances: &[String]) -> String {
    let mut patterns = Vec::new();
    if budget.matter_port {
        patterns.push(matter_port(mac, false));
        patterns.push(matter_port(mac, true));
    }
    if !budget.multicast {
        return patterns.iter().map(Pattern::line).collect();
    }
    let mut groups = Vec::new();
    for address in addresses {
        let group = address.octets()[13..].to_vec();
        if !groups.contains(&group) {
            groups.push(group);
            patterns.push(neighbour_solicitation(address));
        }
    }
    let instances: Vec<&String> = instances.iter().filter(|i| i.len() == INSTANCE_LEN).collect();
    let free = budget.patterns.saturating_sub(patterns.len());
    if !instances.is_empty() && instances.len() * 2 <= free {
        for instance in instances {
            patterns.push(mdns_query(true, Some(instance)));
            patterns.push(mdns_query(false, Some(instance)));
        }
    } else if !instances.is_empty() {
        patterns.push(mdns_query(true, None));
        patterns.push(mdns_query(false, None));
    }
    patterns.truncate(budget.patterns);
    for pattern in &mut patterns {
        pattern.0.truncate(budget.length);
    }
    patterns.iter().map(Pattern::line).collect()
}

/// `LIST` holds Matter's port for an Ethernet address: what a gen 2 needs
/// before it sleeps.
pub fn matter_port_listed() -> bool {
    let port = format!("{:04x}", rs_matter::MATTER_PORT);
    std::fs::read_to_string(LIST).unwrap_or_default().lines().any(|line| {
        line.split(' ').nth(2).is_some_and(|pattern| pattern.ends_with(&port) && !pattern.starts_with("000000000000"))
    })
}

/// The library is in the running nlsleepd: the list's patterns are set.
pub fn in_nlsleepd() -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|process| {
        std::fs::read_to_string(process.path().join("comm")).is_ok_and(|name| name.trim() == "nlsleepd")
            && std::fs::read_to_string(process.path().join("maps")).is_ok_and(|maps| maps.contains(LIBRARY))
    })
}

/// Puts `list` in place whole: the library may be reading.
pub fn write(list: &str) -> std::io::Result<()> {
    let temporary = format!("{LIST}.{}", std::process::id());
    std::fs::write(&temporary, list)?;
    std::fs::rename(&temporary, LIST)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK_LOCAL: &str = "fe80::1ab4:30ff:fe7d:829a";
    const INSTANCE: &str = "C14271473AA1CBEC-0000000000000044";

    const MAC: [u8; 6] = [0x18, 0xb4, 0x30, 0x7d, 0x82, 0x9a];

    fn lines_for(generation: u8, addresses: &[&str], instances: &[&str]) -> Vec<String> {
        let addresses: Vec<Ipv6Addr> = addresses.iter().map(|a| a.parse().unwrap()).collect();
        let instances: Vec<String> = instances.iter().map(|i| i.to_string()).collect();
        list(Budget::of(generation), MAC, &addresses, &instances).lines().map(str::to_string).collect()
    }

    fn lines(addresses: &[&str], instances: &[&str]) -> Vec<String> {
        lines_for(3, addresses, instances)
    }

    /// The frame bytes a line asks for, by offset.
    fn bytes(line: &str) -> Vec<Option<u8>> {
        let fields: Vec<&str> = line.split(' ').collect();
        assert_eq!(fields[0], "0");
        let decode = |hex: &str| (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let (mask, pattern) = (decode(fields[1]), decode(fields[2]));
        assert_eq!(mask.len(), pattern.len().div_ceil(8));
        pattern.iter().enumerate().map(|(i, b)| (mask[i / 8] >> (i % 8) & 1 == 1).then_some(*b)).collect()
    }

    #[test]
    fn a_neighbour_solicitation_is_matched_by_its_group_and_icmpv6_type() {
        // 55 bytes: the type is the last.
        let pattern = format!("3333ff7d829a{}86dd{}3a{}87", "00".repeat(6), "00".repeat(6), "00".repeat(33));
        assert_eq!(lines(&[LINK_LOCAL], &[]), [format!("0 3f301000000040 {pattern}")]);
    }

    #[test]
    fn addresses_of_one_group_share_a_pattern() {
        assert_eq!(lines(&[LINK_LOCAL, "fd00::1ab4:30ff:fe7d:829a"], &[]).len(), 1);
        assert_eq!(lines(&[LINK_LOCAL, "fd00::1234:5678"], &[]).len(), 2);
    }

    #[test]
    fn a_fabric_is_matched_by_its_instance_in_the_first_question() {
        let all = lines(&[LINK_LOCAL], &[INSTANCE]);
        assert_eq!(all.len(), 3);
        let (v6, v4) = (bytes(&all[1]), bytes(&all[2]));
        assert_eq!(v6[..6], [0x33, 0x33, 0, 0, 0, 0xfb].map(Some));
        assert_eq!(v6[56..58], [Some(0x14), Some(0xe9)]);
        assert_eq!(v6[74], Some(33));
        assert_eq!(v6[75..].iter().map(|b| b.unwrap()).collect::<Vec<u8>>(), INSTANCE.as_bytes());
        assert_eq!(v4[..6], [0x01, 0x00, 0x5e, 0, 0, 0xfb].map(Some));
        assert_eq!((v4[23], v4[36], v4[37], v4[54]), (Some(17), Some(0x14), Some(0xe9), Some(33)));
        assert_eq!(v4[55..].iter().map(|b| b.unwrap()).collect::<Vec<u8>>(), INSTANCE.as_bytes());
        // The DNS header between the port and the name matches anything.
        assert!(v4[38..54].iter().all(Option::is_none));
    }

    #[test]
    fn fabrics_that_do_not_fit_share_a_pattern_for_any_operational_instance() {
        let all = lines(&[LINK_LOCAL], &[INSTANCE, "047D9326F7DE4A02-00000000F86EB4E1"]);
        assert_eq!(all.len(), 3);
        let v4 = bytes(&all[2]);
        assert_eq!((v4[54], v4[71]), (Some(33), Some(b'-')));
        assert!(v4[55..71].iter().all(Option::is_none) && v4.len() == 72);
        // Nothing commissioned: nothing to look up.
        assert_eq!(lines(&[LINK_LOCAL], &[]).len(), 1);
    }

    #[test]
    fn a_gen_2_lists_matter_s_port_alone_its_chip_passing_no_multicast_asleep() {
        let all = lines_for(2, &[LINK_LOCAL], &[INSTANCE]);
        assert_eq!(all.len(), 2);
        let (v4, v6) = (bytes(&all[0]), bytes(&all[1]));
        assert_eq!(v4[..6], MAC.map(Some));
        assert_eq!((v4[12], v4[13], v4[23]), (Some(0x08), Some(0x00), Some(17)));
        assert_eq!(v4[36..], [Some(0x15), Some(0xa4)]);
        assert_eq!((v6[12], v6[13], v6[20]), (Some(0x86), Some(0xdd), Some(17)));
        assert_eq!(v6[56..], [Some(0x15), Some(0xa4)]);
        assert!(all.iter().all(|line| bytes(line).len() <= 91));
        // The gen 3's nlsleepd takes the port.
        assert!(lines(&[LINK_LOCAL], &[INSTANCE]).iter().all(|line| bytes(line)[..6] != MAC.map(Some)));
    }
}
