//! LAN discovery over mDNS/DNS-SD.
//!
//! With a fleet of robots, typing an IP and a 32-byte fingerprint per machine
//! does not scale — and DHCP reassigns those IPs anyway. Each host advertises
//! itself as `_telekin._udp.local`, publishing the one thing a viewer cannot
//! guess: its certificate fingerprint. The viewer can then list what is on the
//! network and connect by name.
//!
//! The fingerprint in an advertisement is a *convenience, not an authority*.
//! Anything on the LAN can publish an mDNS record, so trusting a discovered
//! fingerprint blindly would defeat pinning. Treat it as trust-on-first-use:
//! record it the first time, and be suspicious if it ever changes.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

/// DNS-SD service type. QUIC is UDP, so the transport label matches.
pub const SERVICE_TYPE: &str = "_telekin._udp.local.";

/// TXT key carrying the host's certificate fingerprint.
const TXT_FINGERPRINT: &str = "fp";
/// TXT key carrying the protocol version, so mismatched builds are visible
/// before you try to connect rather than after.
const TXT_VERSION: &str = "v";
/// TXT key carrying the account to sign in with.
///
/// Separate from the instance name since the name became the hostname: across
/// a fleet imaged from one card every robot runs the same account, so the
/// account identifies nobody, but it is still what the operator has to type.
const TXT_ACCOUNT: &str = "user";

/// TXT key carrying the machine's hostname. The instance name is the account
/// to sign in as, which is the useful label; the hostname disambiguates two
/// robots that share an operator account.
const TXT_HOSTNAME: &str = "host";

/// A host found on the network.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// Instance name, i.e. what `--name` was set to on the host.
    pub name: String,
    /// Every address the host advertised, best candidate first. A robot with
    /// WiFi and Ethernet up publishes both, and a host bound to `0.0.0.0`
    /// still advertises its IPv6 addresses even though it cannot serve them —
    /// so callers should try these in order rather than trust the first.
    pub addrs: Vec<std::net::SocketAddr>,
    /// Advertised fingerprint. Unverified — see the module docs.
    pub fingerprint: Option<String>,
    pub proto_version: Option<u16>,
    /// The machine's own hostname, when it advertised one.
    pub hostname: Option<String>,
    /// The account to sign in with, when the host advertised one. Prefills
    /// the username so picking a robot leaves only a password to type.
    pub account: Option<String>,
}

/// Keeps a host's advertisement alive. Dropping it withdraws the record.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        // Best-effort: an explicit goodbye lets viewers drop us immediately
        // instead of waiting for the record to expire.
        let _ = self.daemon.unregister(&self.fullname);
    }
}

/// Advertise this host on the local network.
pub fn advertise(
    name: &str,
    port: u16,
    fingerprint: &str,
    proto_version: u16,
    hostname: &str,
    account: &str,
) -> anyhow::Result<Advertisement> {
    let daemon = ServiceDaemon::new().context("could not start the mDNS responder")?;
    // The name mDNS uses to address this record, distinct from the machine's
    // own hostname that we publish for display.
    let mdns_host = format!("{}.local.", sanitize(name));

    let properties: HashMap<String, String> = HashMap::from([
        (TXT_FINGERPRINT.to_string(), fingerprint.to_string()),
        (TXT_VERSION.to_string(), proto_version.to_string()),
        (TXT_HOSTNAME.to_string(), hostname.to_string()),
        (TXT_ACCOUNT.to_string(), account.to_string()),
    ]);

    // An empty address list makes mdns-sd publish every routable interface
    // address it finds, which is what we want on a robot with both WiFi and
    // Ethernet up.
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &sanitize(name),
        &mdns_host,
        "",
        port,
        properties,
    )
    .context("invalid mDNS service description")?
    .enable_addr_auto();

    let fullname = service.get_fullname().to_string();
    daemon.register(service).context("mDNS registration failed")?;
    Ok(Advertisement { daemon, fullname })
}

/// Browse the network for hosts, returning what answered within `timeout`.
///
/// Results are keyed by instance name, so a host reachable on several
/// interfaces appears once.
pub fn discover(timeout: Duration) -> anyhow::Result<Vec<Discovered>> {
    let daemon = ServiceDaemon::new().context("could not start the mDNS browser")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("could not browse for Telekin hosts")?;

    let deadline = std::time::Instant::now() + timeout;
    // Keyed by *identity*, not by name.
    //
    // Names collide: a fleet imaged from one card advertises one hostname
    // until someone renames them, and `--name` is a human decision that can be
    // made the same way twice. A certificate fingerprint cannot collide — each
    // robot generates its own key pair on first run — so twenty robots calling
    // themselves the same thing still appear as twenty rows rather than
    // silently overwriting each other in this map.
    let mut found: HashMap<String, Discovered> = HashMap::new();

    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let port = info.get_port();
                let mut addrs: Vec<std::net::SocketAddr> = info
                    .get_addresses()
                    .iter()
                    .map(|ip| std::net::SocketAddr::new(ip.to_ip_addr(), port))
                    .collect();
                // Rank first, then by address so repeated runs agree.
                addrs.sort_by_key(|a| (address_rank(a), a.to_string()));
                if addrs.is_empty() {
                    continue;
                }
                let name = info
                    .get_fullname()
                    .split('.')
                    .next()
                    .unwrap_or(info.get_fullname())
                    .to_string();
                let fingerprint = info.get_property_val_str(TXT_FINGERPRINT).map(str::to_owned);
                let key = identity(&name, fingerprint.as_deref(), &addrs);
                found.insert(
                    key,
                    Discovered {
                        name,
                        addrs,
                        fingerprint,
                        proto_version: info
                            .get_property_val_str(TXT_VERSION)
                            .and_then(|v| v.parse().ok()),
                        hostname: info.get_property_val_str(TXT_HOSTNAME).map(str::to_owned),
                        account: info.get_property_val_str(TXT_ACCOUNT).map(str::to_owned),
                    },
                );
            }
            Ok(_) => {}
            Err(_) => break, // timed out: report whatever answered
        }
    }

    let _ = daemon.shutdown();
    let mut out: Vec<_> = found.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// What makes two answers the same robot.
///
/// The fingerprint when there is one: it is unique by construction and does
/// not change when the robot moves between networks, so a robot answering on
/// both WiFi and a cable is one row rather than two.
///
/// Without one — an older host, or one that never published the record — fall
/// back to the name and its first address. That can still merge two robots
/// that share a name *and* an address, which cannot happen on one network, and
/// it never merges two that only share a name.
fn identity(name: &str, fingerprint: Option<&str>, addrs: &[std::net::SocketAddr]) -> String {
    match fingerprint {
        Some(fp) => format!("fp:{fp}"),
        None => match addrs.first() {
            Some(addr) => format!("addr:{name}@{addr}"),
            None => format!("name:{name}"),
        },
    }
}

/// Ordering preference for advertised addresses, lowest tried first.
///
/// This is a LAN tool and hosts commonly bind `0.0.0.0`, so a private IPv4
/// address is the most likely to actually accept a connection. Global IPv6
/// goes last: it is advertised even by a host that cannot serve it.
fn address_rank(addr: &std::net::SocketAddr) -> u8 {
    use std::net::IpAddr;
    match addr.ip() {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(v4) if v4.is_link_local() => 2,
        IpAddr::V4(_) => 1,
        IpAddr::V6(v6) if v6.is_loopback() => 4,
        IpAddr::V6(_) => 3,
    }
}

/// DNS-SD instance names cannot contain dots; they split the label.
fn sanitize(name: &str) -> String {
    name.replace('.', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> std::net::SocketAddr {
        s.parse().expect("address")
    }

    #[test]
    fn robots_sharing_a_name_stay_separate() {
        // The failure this replaced: a fleet imaged from one card advertises
        // the same name, and keying by name collapsed all of them into one.
        let a = identity("tangox", Some("79:20:bd"), &[addr("192.168.1.10:9631")]);
        let b = identity("tangox", Some("3f:a1:88"), &[addr("192.168.1.11:9631")]);
        assert_ne!(a, b);
    }

    #[test]
    fn one_robot_on_two_networks_is_one_row() {
        // A robot with WiFi and a cable answers twice with the same key.
        let wifi = identity("tangox", Some("79:20:bd"), &[addr("192.168.1.10:9631")]);
        let wired = identity("tangox", Some("79:20:bd"), &[addr("192.168.2.10:9631")]);
        assert_eq!(wifi, wired);
    }

    #[test]
    fn a_host_without_a_fingerprint_still_gets_told_apart() {
        // Older hosts publish no fingerprint; the address has to carry it.
        let a = identity("robot", None, &[addr("192.168.1.10:9631")]);
        let b = identity("robot", None, &[addr("192.168.1.11:9631")]);
        assert_ne!(a, b);
        let same = identity("robot", None, &[addr("192.168.1.10:9631")]);
        assert_eq!(a, same);
    }

    #[test]
    fn a_fingerprint_beats_an_address() {
        // The same robot after DHCP moved it: still one robot.
        let before = identity("robot", Some("79:20:bd"), &[addr("192.168.1.10:9631")]);
        let after = identity("robot", Some("79:20:bd"), &[addr("192.168.1.99:9631")]);
        assert_eq!(before, after);
    }
}
