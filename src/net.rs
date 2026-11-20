//! Network helpers: host reachability, LAN discovery and Wake-on-LAN.
//!
//! Kyber has no discovery protocol, so "online" means the controller's TCP
//! port accepts a connection, and discovery is a sweep of the local /24
//! networks for machines that answer like a Kyber controller.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::Host;

const PROBE_TIMEOUT: Duration = Duration::from_millis(900);
const PROBE_INTERVAL: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reachability {
    Unknown,
    /// TCP connect time in milliseconds.
    Online(u32),
    Offline,
}

/// Connects to `address:port` and reports how long the handshake took.
pub fn probe(address: &str, port: u16) -> Reachability {
    let Some(target) = resolve(address, port) else {
        return Reachability::Offline;
    };
    let started = Instant::now();
    match TcpStream::connect_timeout(&target, PROBE_TIMEOUT) {
        Ok(_) => Reachability::Online(started.elapsed().as_millis().max(1) as u32),
        Err(_) => Reachability::Offline,
    }
}

fn resolve(address: &str, port: u16) -> Option<SocketAddr> {
    let address = address.trim();
    if let Ok(ip) = address.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, port));
    }
    (address, port).to_socket_addrs().ok()?.next()
}

#[derive(Clone, Copy)]
pub struct HostStatus {
    pub reachability: Reachability,
    /// MAC address seen in the ARP table, as six bytes.
    pub mac: Option<[u8; 6]>,
}

/// Background thread that keeps the reachability of the saved hosts current.
pub struct Monitor {
    targets: Arc<Mutex<Vec<(u64, String, u16)>>>,
    statuses: Arc<Mutex<HashMap<u64, HostStatus>>>,
    paused: Arc<Mutex<bool>>,
}

impl Monitor {
    pub fn start(wake: impl Fn() + Send + 'static) -> Self {
        let targets: Arc<Mutex<Vec<(u64, String, u16)>>> = Arc::default();
        let statuses: Arc<Mutex<HashMap<u64, HostStatus>>> = Arc::default();
        let paused = Arc::new(Mutex::new(false));

        let (t, s, p) = (targets.clone(), statuses.clone(), paused.clone());
        std::thread::spawn(move || loop {
            // No probing while a stream runs: keep the network quiet.
            if !*p.lock().unwrap() {
                let snapshot = t.lock().unwrap().clone();
                let handles: Vec<_> = snapshot
                    .into_iter()
                    .map(|(id, address, port)| {
                        // The ARP table is only read until a MAC is known.
                        let known_mac = s.lock().unwrap().get(&id).and_then(|st| st.mac);
                        std::thread::spawn(move || {
                            let reachability = probe(&address, port);
                            let mac = match (known_mac, reachability) {
                                (Some(mac), _) => Some(mac),
                                (None, Reachability::Online(_)) => lookup_mac(&address),
                                (None, _) => None,
                            };
                            (id, HostStatus { reachability, mac })
                        })
                    })
                    .collect();
                let mut changed = false;
                for handle in handles {
                    if let Ok((id, status)) = handle.join() {
                        let mut statuses = s.lock().unwrap();
                        let previous = statuses.insert(id, status);
                        changed |= previous.map(|p| p.reachability) != Some(status.reachability);
                    }
                }
                if changed {
                    wake();
                }
            }
            std::thread::sleep(PROBE_INTERVAL);
        });

        Self {
            targets,
            statuses,
            paused,
        }
    }

    pub fn set_hosts(&self, hosts: &[Host]) {
        *self.targets.lock().unwrap() = hosts
            .iter()
            .filter(|h| !h.address.trim().is_empty())
            .map(|h| (h.id, h.address.clone(), h.port))
            .collect();
        self.statuses
            .lock()
            .unwrap()
            .retain(|id, _| hosts.iter().any(|h| h.id == *id));
    }

    pub fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap() = paused;
    }

    pub fn status(&self, id: u64) -> HostStatus {
        self.statuses
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .unwrap_or(HostStatus {
                reachability: Reachability::Unknown,
                mac: None,
            })
    }
}

// ---------------------------------------------------------------- discovery

/// A machine found by the LAN sweep.
#[derive(Clone, Debug)]
pub struct Found {
    pub ip: Ipv4Addr,
    pub port: u16,
}

pub enum ScanEvent {
    Found(Found),
    Done { probed: usize },
}

/// IPv4 addresses of this machine's private-network interfaces.
pub fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    // Connecting a UDP socket sends nothing; it only makes the OS pick the
    // outgoing interface, whose address we then read back.
    for target in [
        "192.168.255.255:9",
        "10.255.255.255:9",
        "172.31.255.255:9",
        "1.1.1.1:9",
    ] {
        let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if socket.connect(target).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(addr)) = socket.local_addr() {
            let ip = *addr.ip();
            if ip.is_private() && !found.contains(&ip) {
                found.push(ip);
            }
        }
    }
    found
}

/// True if `ip:port` answers like a Kyber controller: an HTTPS server whose
/// `POST /session/login` without credentials is refused with 400 or 401. No
/// credentials are sent, so the (usually self-signed) certificate is not
/// verified here; kyclient does its own verification when connecting.
fn looks_like_kyber(ip: Ipv4Addr, port: u16) -> bool {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .disable_verification(true)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .into();
    match agent
        .post(format!("https://{ip}:{port}/session/login"))
        .send_empty()
    {
        Ok(response) => matches!(response.status().as_u16(), 400 | 401),
        Err(_) => false,
    }
}

/// Sweeps the local /24 networks for Kyber controllers on `port`.
pub fn scan_lan(port: u16, wake: impl Fn() + Send + Sync + 'static) -> Receiver<ScanEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let wake = Arc::new(wake);
        let locals = local_ipv4s();
        let mut candidates: Vec<Ipv4Addr> = Vec::new();
        for local in &locals {
            let [a, b, c, _] = local.octets();
            for last in 1..=254u8 {
                let ip = Ipv4Addr::new(a, b, c, last);
                if !candidates.contains(&ip) {
                    candidates.push(ip);
                }
            }
        }
        let probed = candidates.len();

        // 32 workers pulling from a shared queue keeps the sweep around ten
        // seconds per /24 without opening hundreds of sockets at once.
        let queue = Arc::new(Mutex::new(candidates));
        let workers: Vec<_> = (0..32)
            .map(|_| {
                let queue = queue.clone();
                let tx = tx.clone();
                let wake = wake.clone();
                std::thread::spawn(move || loop {
                    let Some(ip) = queue.lock().unwrap().pop() else {
                        break;
                    };
                    let target = SocketAddr::new(IpAddr::V4(ip), port);
                    if TcpStream::connect_timeout(&target, PROBE_TIMEOUT).is_ok()
                        && looks_like_kyber(ip, port)
                    {
                        let _ = tx.send(ScanEvent::Found(Found { ip, port }));
                        wake();
                    }
                })
            })
            .collect();
        for worker in workers {
            let _ = worker.join();
        }
        let _ = tx.send(ScanEvent::Done { probed });
        wake();
    });
    rx
}

// -------------------------------------------------------------- wake-on-lan

pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(|c| c == ':' || c == '-');
    for byte in &mut mac {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.map(|b| format!("{b:02X}")).join(":")
}

/// Looks the address up in the OS neighbour table (`arp -a`).
pub fn lookup_mac(address: &str) -> Option<[u8; 6]> {
    let ip = resolve(address, 0)?.ip();
    if !matches!(ip, IpAddr::V4(v4) if v4.is_private()) {
        return None;
    }
    let mut command = Command::new("arp");
    command.arg("-a").stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().ok()?;
    mac_from_arp_table(&String::from_utf8_lossy(&output.stdout), &ip.to_string())
}

fn mac_from_arp_table(table: &str, ip: &str) -> Option<[u8; 6]> {
    table.lines().find_map(|line| {
        // Windows: "  192.168.1.20   aa-bb-cc-dd-ee-ff   dynamic"
        // Unix:    "? (192.168.1.20) at aa:bb:cc:dd:ee:ff on en0"
        let mut words = line.split_whitespace();
        if !words.any(|w| w.trim_matches(|c| c == '(' || c == ')') == ip) {
            return None;
        }
        let mac = line.split_whitespace().find_map(parse_mac)?;
        // Skip broadcast and incomplete entries.
        (mac != [0xFF; 6] && mac != [0; 6]).then_some(mac)
    })
}

/// Sends the Wake-on-LAN magic packet for `mac` to the local broadcast address.
pub fn wake_on_lan(mac: [u8; 6]) -> Result<(), String> {
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket.set_broadcast(true).map_err(|e| e.to_string())?;
    let mut sent = false;
    let mut last_error = String::new();
    // Port 9 is the convention; some firmware listens on 7 instead.
    for port in [9, 7] {
        match socket.send_to(&packet, (Ipv4Addr::BROADCAST, port)) {
            Ok(_) => sent = true,
            Err(e) => last_error = e.to_string(),
        }
    }
    if sent {
        Ok(())
    } else {
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn mac_addresses_parse_in_both_notations() {
        let expected = [0xAA, 0xBB, 0xCC, 0x0D, 0xEE, 0xFF];
        assert_eq!(parse_mac("aa-bb-cc-0d-ee-ff"), Some(expected));
        assert_eq!(parse_mac("AA:BB:CC:0D:EE:FF"), Some(expected));
        assert_eq!(format_mac(expected), "AA:BB:CC:0D:EE:FF");
        assert_eq!(parse_mac("aa-bb-cc-0d-ee"), None);
        assert_eq!(parse_mac("aa-bb-cc-0d-ee-ff-11"), None);
        assert_eq!(parse_mac("192.168.1.20"), None);
        assert_eq!(parse_mac("dynamic"), None);
    }

    #[test]
    fn arp_tables_from_windows_and_unix_are_understood() {
        let windows = "\
Interface: 192.168.1.5 --- 0x7
  Internet Address      Physical Address      Type
  192.168.1.1           11-22-33-44-55-66     dynamic
  192.168.1.20          aa-bb-cc-dd-ee-ff     dynamic
  192.168.1.255         ff-ff-ff-ff-ff-ff     static
";
        assert_eq!(
            mac_from_arp_table(windows, "192.168.1.20"),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
        // Must not match 192.168.1.2 against the 192.168.1.20 row.
        assert_eq!(mac_from_arp_table(windows, "192.168.1.2"), None);
        assert_eq!(mac_from_arp_table(windows, "192.168.1.255"), None);

        let unix = "? (192.168.1.20) at aa:bb:cc:dd:ee:ff on en0 ifscope [ethernet]\n";
        assert_eq!(
            mac_from_arp_table(unix, "192.168.1.20"),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
    }

    #[test]
    fn probe_sees_open_and_closed_ports() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(matches!(probe("127.0.0.1", port), Reachability::Online(_)));
        drop(listener);
        assert_eq!(probe("127.0.0.1", port), Reachability::Offline);
        assert_eq!(probe("", 8080), Reachability::Offline);
    }
}
