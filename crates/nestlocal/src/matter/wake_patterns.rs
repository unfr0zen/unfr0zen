//! What reaches a sleeping unit before a Matter message can. Asleep, the
//! Wi-Fi chip wakes the unit for the frames its patterns match, and nlsleepd
//! gives it one kind: a frame to the unit's address and a port, which covers
//! a controller's messages (wake.rs). A controller that has lost the unit's
//! address asks for it first, by multicast, which those never match:
//! - a DNS-SD query for the unit's operational instance (mDNS, UDP 5353);
//! - an IPv6 neighbour solicitation for one of its addresses. The chip
//!   answers ARP itself, and not this.
//!
//! nestlocal lists a pattern for each in `LIST`, and the shim in nlsleepd
//! (platform/diamond3/wakefilter) adds them to nlsleepd's before each sleep:
//! a line a pattern, `OFFSET MASK PATTERN`, the offset into the frame in
//! decimal, nl80211's mask (a bit a pattern byte, least significant first)
//! and the bytes in hex.
//!
//! A pattern is bytes at fixed offsets, so a query wakes the unit when the
//! instance is its first question. The chip holds `CHIP_PATTERNS` and
//! nlsleepd uses `NLSLEEPD_PATTERNS` of them: when the instances of every
//! fabric do not fit, one pattern for each IP version matches any operational
//! instance's query instead, another node's included.

use std::net::Ipv6Addr;

pub const LIST: &str = "/var/run/unfr0zen/wake-patterns";

/// What the BCM43340 takes.
const CHIP_PATTERNS: usize = 8;
/// nlsleepd's own: the Wi-Fi rekey, the broker's connection, and the Matter
/// port over IPv4 and IPv6.
const NLSLEEPD_PATTERNS: usize = 4;

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

/// The list for a unit with these addresses and operational instances.
pub fn list(addresses: &[Ipv6Addr], instances: &[String]) -> String {
    let mut patterns = Vec::new();
    let mut groups = Vec::new();
    for address in addresses {
        let group = address.octets()[13..].to_vec();
        if !groups.contains(&group) {
            groups.push(group);
            patterns.push(neighbour_solicitation(address));
        }
    }
    let instances: Vec<&String> = instances.iter().filter(|i| i.len() == INSTANCE_LEN).collect();
    let free = (CHIP_PATTERNS - NLSLEEPD_PATTERNS).saturating_sub(patterns.len());
    if !instances.is_empty() && instances.len() * 2 <= free {
        for instance in instances {
            patterns.push(mdns_query(true, Some(instance)));
            patterns.push(mdns_query(false, Some(instance)));
        }
    } else if !instances.is_empty() {
        patterns.push(mdns_query(true, None));
        patterns.push(mdns_query(false, None));
    }
    patterns.truncate(CHIP_PATTERNS - NLSLEEPD_PATTERNS);
    patterns.iter().map(Pattern::line).collect()
}

/// Puts `list` in place whole: the shim may be reading.
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

    fn lines(addresses: &[&str], instances: &[&str]) -> Vec<String> {
        let addresses: Vec<Ipv6Addr> = addresses.iter().map(|a| a.parse().unwrap()).collect();
        let instances: Vec<String> = instances.iter().map(|i| i.to_string()).collect();
        list(&addresses, &instances).lines().map(str::to_string).collect()
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
}
