//! The machine's network interfaces, as far as routing needs to know them: which have an IPv4
//! address, which of those is wired, and which one to send on when nobody said.
//!
//! Routing is multicast, and multicast over Wi-Fi loses frames (an access point sends it once,
//! unacknowledged, often held back for sleeping stations). A machine with both a cable and
//! Wi-Fi on the same network must not end up on Wi-Fi by accident, so the choice prefers a
//! wired interface, and a wireless one is named as such wherever the bus access is shown.
//!
//! There is no portable way to list interfaces in the standard library. On Linux the
//! addresses are taken from `ip -4 -o addr show` and the kind of each interface from
//! `/sys/class/net`; elsewhere, and where that fails, the interface the system itself would
//! use for the routing group is taken and its kind is unknown.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::process::Command;

/// What an interface is made of, as far as it can be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A network card with a cable.
    Wired,
    /// Wi-Fi.
    Wireless,
    /// Something else, or nothing to tell by: a bridge, a tunnel, another system.
    Unknown,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Wired => "wired",
            Self::Wireless => "wireless",
            Self::Unknown => "kind unknown",
        })
    }
}

/// One interface with an IPv4 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// Its name, `enp3s0` or `wlan0`; empty when only the address is known.
    pub name: String,
    /// Its IPv4 address.
    pub address: Ipv4Addr,
    /// What it is.
    pub kind: Kind,
    /// Whether it is up.
    pub up: bool,
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "{} ({})", self.address, self.kind)
        } else {
            write!(f, "{} ({})", self.name, self.kind)
        }
    }
}

/// Where Linux describes its interfaces.
const SYS: &str = "/sys/class/net";

/// The interfaces and addresses in the output of `ip -4 -o addr show`: one line per address,
/// `2: enp3s0    inet 192.168.1.20/24 brd …`.
pub fn parse_addresses(output: &str) -> Vec<(String, Ipv4Addr)> {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let _index = words.next()?;
            let name = words.next()?.trim_end_matches(':');
            // A VLAN shows as `eth0.10@eth0`.
            let name = name.split('@').next()?.to_string();
            let address = words.skip_while(|word| *word != "inet").nth(1)?;
            let address = address.split('/').next()?.parse().ok()?;
            Some((name, address))
        })
        .collect()
}

/// What the interface `name` is and whether it is up, from the directory Linux keeps for it
/// under `sys`.
pub fn look(sys: &Path, name: &str) -> (Kind, bool) {
    let directory = sys.join(name);
    let kind = if directory.join("wireless").exists() || directory.join("phy80211").exists() {
        Kind::Wireless
    } else if directory.join("device").exists() {
        // A real device that is not a radio.
        Kind::Wired
    } else {
        Kind::Unknown
    };
    let state = std::fs::read_to_string(directory.join("operstate")).unwrap_or_default();
    // Virtual interfaces often say "unknown" while they work.
    (kind, matches!(state.trim(), "up" | "unknown" | ""))
}

/// Every interface with an IPv4 address that is not the loopback, as far as this system
/// tells. Empty where it does not.
pub fn list() -> Vec<Interface> {
    let Ok(output) = Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    described(
        &parse_addresses(&String::from_utf8_lossy(&output.stdout)),
        Path::new(SYS),
    )
}

/// `addresses` with what `sys` says about each interface; the loopback is left out.
pub fn described(addresses: &[(String, Ipv4Addr)], sys: &Path) -> Vec<Interface> {
    addresses
        .iter()
        .filter(|(_, address)| !address.is_loopback())
        .map(|(name, address)| {
            let (kind, up) = look(sys, name);
            Interface {
                name: name.clone(),
                address: *address,
                kind,
                up,
            }
        })
        .collect()
}

/// The address this system would send from to reach the routing group, without sending
/// anything: what it picks when nobody says.
pub fn system_default() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).ok()?;
    // Connecting a datagram socket only fixes the route; no frame leaves.
    socket
        .connect(SocketAddr::from((Ipv4Addr::new(224, 0, 23, 12), 3671)))
        .ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(address) if !address.ip().is_unspecified() => Some(*address.ip()),
        _ => None,
    }
}

/// The interface to route on when none was named: a wired one that is up if there is one
/// (the system's own choice among several), else the system's own choice, else the first
/// that is up.
pub fn choose(interfaces: &[Interface], system_default: Option<Ipv4Addr>) -> Option<Interface> {
    let preferred = |interface: &&Interface| Some(interface.address) == system_default;
    let wired = || {
        interfaces
            .iter()
            .filter(|interface| interface.up && interface.kind == Kind::Wired)
    };
    wired()
        .find(preferred)
        .or_else(|| wired().next())
        .or_else(|| interfaces.iter().find(preferred))
        .or_else(|| interfaces.iter().find(|interface| interface.up))
        .or_else(|| interfaces.first())
        .cloned()
        .or_else(|| {
            // Nothing is known about the interfaces: the system's choice, kind unknown.
            system_default.map(|address| Interface {
                name: String::new(),
                address,
                kind: Kind::Unknown,
                up: true,
            })
        })
}

/// What is known about the interface with `address`: its entry among `interfaces`, or the
/// bare address with its kind unknown.
pub fn with_address(interfaces: &[Interface], address: Ipv4Addr) -> Interface {
    interfaces
        .iter()
        .find(|interface| interface.address == address)
        .cloned()
        .unwrap_or(Interface {
            name: String::new(),
            address,
            kind: Kind::Unknown,
            up: true,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever
2: enp3s0    inet 192.168.1.20/24 brd 192.168.1.255 scope global dynamic noprefixroute enp3s0\\       valid_lft 80000sec preferred_lft 80000sec
3: wlan0    inet 192.168.1.21/24 brd 192.168.1.255 scope global dynamic noprefixroute wlan0\\       valid_lft 80000sec preferred_lft 80000sec
5: br-lab    inet 172.18.0.1/16 brd 172.18.255.255 scope global br-lab\\       valid_lft forever preferred_lft forever
7: eth1.10@eth1    inet 10.0.10.2/24 scope global eth1.10\\       valid_lft forever preferred_lft forever
not a line of it
";

    fn address(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    /// A stand-in for `/sys/class/net`: a cable, Wi-Fi, a bridge and a card without a link.
    fn sys(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "knxide-programmer-interfaces-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let make = |interface: &str, entries: &[&str], state: &str| {
            let directory = root.join(interface);
            for entry in entries {
                std::fs::create_dir_all(directory.join(entry)).unwrap();
            }
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("operstate"), state).unwrap();
        };
        make("enp3s0", &["device"], "up\n");
        make("wlan0", &["device", "wireless"], "up\n");
        make("br-lab", &[], "unknown\n");
        make("eth1.10", &["device"], "down\n");
        root
    }

    #[test]
    fn addresses_are_read_from_the_listing() {
        assert_eq!(
            parse_addresses(OUTPUT),
            [
                ("lo".to_string(), address("127.0.0.1")),
                ("enp3s0".to_string(), address("192.168.1.20")),
                ("wlan0".to_string(), address("192.168.1.21")),
                ("br-lab".to_string(), address("172.18.0.1")),
                ("eth1.10".to_string(), address("10.0.10.2")),
            ]
        );
        assert!(parse_addresses("").is_empty());
    }

    #[test]
    fn the_cable_is_preferred_over_wifi() {
        let root = sys("choice");
        let all = described(&parse_addresses(OUTPUT), &root);
        let kinds: Vec<(&str, Kind, bool)> = all
            .iter()
            .map(|interface| (interface.name.as_str(), interface.kind, interface.up))
            .collect();
        assert_eq!(
            kinds,
            [
                ("enp3s0", Kind::Wired, true),
                ("wlan0", Kind::Wireless, true),
                ("br-lab", Kind::Unknown, true),
                ("eth1.10", Kind::Wired, false),
            ]
        );
        // Both on the same network, and the system would take Wi-Fi: the cable all the same.
        let chosen = choose(&all, Some(address("192.168.1.21"))).unwrap();
        assert_eq!(chosen.name, "enp3s0");
        assert_eq!(chosen.to_string(), "enp3s0 (wired)");

        // Without the cable: what the system would take, and it is said to be wireless.
        let without: Vec<Interface> = all
            .iter()
            .filter(|interface| interface.name != "enp3s0")
            .cloned()
            .collect();
        let chosen = choose(&without, Some(address("192.168.1.21"))).unwrap();
        assert_eq!(
            (chosen.name.as_str(), chosen.kind),
            ("wlan0", Kind::Wireless)
        );
        // A card without a link is not preferred for being a card.
        let chosen = choose(&without, None).unwrap();
        assert_eq!(chosen.name, "wlan0");

        // Nothing known about the interfaces: the system's choice, and its kind is unknown.
        let chosen = choose(&[], Some(address("192.168.1.50"))).unwrap();
        assert_eq!(chosen.to_string(), "192.168.1.50 (kind unknown)");
        assert_eq!(choose(&[], None), None);

        // An address that was named is looked up, known or not.
        assert_eq!(
            with_address(&all, address("192.168.1.21")).kind,
            Kind::Wireless
        );
        assert_eq!(
            with_address(&all, address("192.168.9.9")).kind,
            Kind::Unknown
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
