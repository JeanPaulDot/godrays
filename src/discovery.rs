//! Finding Godrays hosts on the local network with mDNS / DNS-SD.
//!
//! A hosting PC (while it hosts or has pairing open) advertises
//! `_godrays._tcp.local.` with its agent port, its Kyber port, its
//! certificate fingerprint and whether pairing is open. Clients browse for
//! it continuously, so hosts appear within a second, no scan needed.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE: &str = "_godrays._tcp.local.";

#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub name: String,
    pub address: IpAddr,
    pub agent_port: u16,
    pub kyber_port: u16,
    pub fingerprint: String,
    pub pairing: bool,
    pub hosting: bool,
}

/// What this PC advertises.
#[derive(Clone, Debug, PartialEq)]
pub struct Advert {
    pub name: String,
    pub agent_port: u16,
    pub kyber_port: u16,
    pub fingerprint: String,
    pub pairing: bool,
    pub hosting: bool,
}

pub struct Discovery {
    daemon: Option<ServiceDaemon>,
    found: Arc<Mutex<HashMap<String, Found>>>,
    advertised: Mutex<Option<(Advert, String)>>,
}

fn host_label(name: &str) -> String {
    let label: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{}.local.", label.trim_matches('-').to_lowercase())
}

impl Discovery {
    /// Starts browsing. Without a usable network the list simply stays empty.
    pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let found: Arc<Mutex<HashMap<String, Found>>> = Arc::default();
        let daemon = ServiceDaemon::new().ok();
        if let Some(receiver) = daemon.as_ref().and_then(|d| d.browse(SERVICE).ok()) {
            let found = found.clone();
            std::thread::spawn(move || {
                while let Ok(event) = receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let address = info
                                .get_addresses()
                                .iter()
                                .map(|ip| ip.to_ip_addr())
                                .find(IpAddr::is_ipv4)
                                .or_else(|| {
                                    info.get_addresses().iter().next().map(|ip| ip.to_ip_addr())
                                });
                            let Some(address) = address else { continue };
                            let property = |key| info.get_property_val_str(key).unwrap_or_default();
                            let name = info
                                .get_fullname()
                                .strip_suffix(&format!(".{SERVICE}"))
                                .unwrap_or(info.get_fullname())
                                .replace("\\.", ".");
                            let entry = Found {
                                name,
                                address,
                                agent_port: info.get_port(),
                                kyber_port: property("kyber").parse().unwrap_or(8080),
                                fingerprint: property("fp").to_owned(),
                                pairing: property("pairing") == "1",
                                hosting: property("hosting") == "1",
                            };
                            found
                                .lock()
                                .unwrap()
                                .insert(info.get_fullname().to_owned(), entry);
                            wake();
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            found.lock().unwrap().remove(&fullname);
                            wake();
                        }
                        _ => {}
                    }
                }
            });
        }
        Self {
            daemon,
            found,
            advertised: Mutex::new(None),
        }
    }

    /// Hosts seen on the network, other than this PC, sorted by name.
    pub fn list(&self, own_fingerprint: &str) -> Vec<Found> {
        let mut list: Vec<Found> = self
            .found
            .lock()
            .unwrap()
            .values()
            .filter(|f| own_fingerprint.is_empty() || f.fingerprint != own_fingerprint)
            .cloned()
            .collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Advertises this PC (or updates the advert). `None` withdraws it.
    pub fn advertise(&self, advert: Option<Advert>) {
        let Some(daemon) = &self.daemon else { return };
        let mut current = self.advertised.lock().unwrap();
        if current.as_ref().map(|(a, _)| a) == advert.as_ref() {
            return;
        }
        if let Some((_, fullname)) = current.take() {
            let _ = daemon.unregister(&fullname);
        }
        let Some(advert) = advert else { return };
        let flag = |on: bool| if on { "1" } else { "0" };
        let properties = [
            ("v", "1".to_owned()),
            ("kyber", advert.kyber_port.to_string()),
            ("fp", advert.fingerprint.clone()),
            ("pairing", flag(advert.pairing).to_owned()),
            ("hosting", flag(advert.hosting).to_owned()),
        ];
        let info = ServiceInfo::new(
            SERVICE,
            &advert.name,
            &host_label(&advert.name),
            "",
            advert.agent_port,
            &properties[..],
        )
        .map(ServiceInfo::enable_addr_auto);
        if let Ok(info) = info {
            let fullname = info.get_fullname().to_owned();
            if daemon.register(info).is_ok() {
                *current = Some((advert, fullname));
            }
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.advertise(None);
        if let Some(daemon) = &self.daemon {
            let _ = daemon.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn host_labels_are_dns_safe() {
        assert_eq!(host_label("GAMING-RIG"), "gaming-rig.local.");
        assert_eq!(host_label("Jean's PC"), "jean-s-pc.local.");
    }

    /// Advertises and finds itself through the real mDNS stack. Skipped
    /// quietly when this machine has no multicast-capable network.
    #[test]
    fn an_advert_is_found_by_a_browser() {
        let host = Discovery::start(|| {});
        let browser = Discovery::start(|| {});
        if host.daemon.is_none() {
            return;
        }
        let name = format!("godrays-test-{}", std::process::id());
        host.advertise(Some(Advert {
            name: name.clone(),
            agent_port: 18081,
            kyber_port: 18080,
            fingerprint: "sha256:test".into(),
            pairing: true,
            hosting: false,
        }));
        let deadline = Instant::now() + Duration::from_secs(8);
        let found = loop {
            if let Some(found) = browser.list("").into_iter().find(|f| f.name == name) {
                break Some(found);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let Some(found) = found else {
            eprintln!("no mDNS on this network; skipping");
            return;
        };
        assert_eq!(found.agent_port, 18081);
        assert_eq!(found.kyber_port, 18080);
        assert!(found.pairing && !found.hosting);
        // Its own fingerprint filters it out.
        assert!(browser.list("sha256:test").iter().all(|f| f.name != name));
    }
}
