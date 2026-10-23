use std::{
    collections::{BTreeSet, HashMap},
    future::pending,
    net::IpAddr,
    time::Duration,
};

use lan_mouse_ipc::DiscoveredDevice;
use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

/// DNS-SD service type used to announce and find lan-mouse instances
const SERVICE_TYPE: &str = "_lan-mouse._udp.local.";
/// TXT property: certificate sha256 fingerprint
const TXT_FINGERPRINT: &str = "fp";
/// TXT property: version of the announcement format
const TXT_VERSION: &str = "v";
const ANNOUNCEMENT_VERSION: &str = "1";

/// Announces this instance via mDNS and keeps track of other instances on the network.
///
/// If the mDNS daemon cannot be started, discovery is simply disabled.
pub(crate) struct Discovery {
    running: Option<Running>,
    /// resolved instances, by dns-sd fullname
    devices: HashMap<String, DiscoveredDevice>,
    /// fingerprint of this instance, used to ignore our own announcement
    fingerprint: String,
}

struct Running {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    instance: String,
    fullname: String,
}

impl Discovery {
    pub(crate) fn new(port: u16, fingerprint: String) -> Self {
        let running = match Running::start(port, &fingerprint) {
            Ok(running) => Some(running),
            Err(e) => {
                log::warn!("device discovery disabled: {e}");
                None
            }
        };
        Self {
            running,
            devices: Default::default(),
            fingerprint,
        }
    }

    /// all discovered devices, sorted by hostname
    pub(crate) fn devices(&self) -> Vec<DiscoveredDevice> {
        let mut devices = self.devices.values().cloned().collect::<Vec<_>>();
        devices.sort_by(|a, b| (&a.hostname, &a.fingerprint).cmp(&(&b.hostname, &b.fingerprint)));
        devices
    }

    /// waits until the list of discovered devices changes and returns the new list
    pub(crate) async fn changed(&mut self) -> Vec<DiscoveredDevice> {
        loop {
            let Some(running) = &self.running else {
                return pending().await;
            };
            match running.events.recv_async().await {
                Ok(event) => {
                    if self.handle_event(event) {
                        return self.devices();
                    }
                }
                Err(_) => {
                    log::warn!("mdns daemon stopped, device discovery disabled");
                    self.running = None;
                    if !self.devices.is_empty() {
                        self.devices.clear();
                        return Vec::new();
                    }
                }
            }
        }
    }

    /// announce the new port
    pub(crate) fn set_port(&self, port: u16) {
        let Some(running) = &self.running else {
            return;
        };
        if let Err(e) = running.register(port, &self.fingerprint) {
            log::warn!("failed to update mdns announcement: {e}");
        }
    }

    pub(crate) async fn terminate(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        // say goodbye so that peers drop us from their list immediately
        if let Ok(status) = running.daemon.unregister(&running.fullname) {
            let _ = tokio::time::timeout(Duration::from_millis(500), status.recv_async()).await;
        }
        if let Err(e) = running.daemon.shutdown() {
            log::debug!("mdns shutdown: {e}");
        }
    }

    /// returns whether the list of devices changed
    fn handle_event(&mut self, event: ServiceEvent) -> bool {
        match event {
            ServiceEvent::SearchStarted(ty) => {
                log::debug!("mdns: searching for {ty}");
                false
            }
            ServiceEvent::ServiceResolved(info) => {
                let fullname = info.get_fullname().to_owned();
                match device_from_resolved(&info, &self.fingerprint) {
                    Some(device) => {
                        log::debug!("mdns: resolved {fullname}: {device:?}");
                        let previous = self.devices.insert(fullname, device.clone());
                        previous.as_ref() != Some(&device)
                    }
                    None => {
                        log::debug!("mdns: ignoring {fullname}");
                        self.devices.remove(&fullname).is_some()
                    }
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                log::debug!("mdns: removed {fullname}");
                self.devices.remove(&fullname).is_some()
            }
            _ => false,
        }
    }
}

impl Running {
    fn start(port: u16, fingerprint: &str) -> mdns_sd::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let instance = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "lan-mouse".to_owned());
        let info = service_info(&instance, port, fingerprint)?;
        let fullname = info.get_fullname().to_owned();
        let running = Self {
            events: daemon.browse(SERVICE_TYPE)?,
            daemon,
            instance,
            fullname,
        };
        running.daemon.register(info)?;
        log::info!(
            "announcing {} via mdns (port {port}), browsing for other devices",
            running.fullname
        );
        Ok(running)
    }

    fn register(&self, port: u16, fingerprint: &str) -> mdns_sd::Result<()> {
        self.daemon
            .register(service_info(&self.instance, port, fingerprint)?)
    }
}

fn service_info(instance: &str, port: u16, fingerprint: &str) -> mdns_sd::Result<ServiceInfo> {
    // dns host label: hostnames may contain characters that are not valid in dns names
    // (e.g. spaces on macOS). It is distinct from the system hostname to not clash
    // with records published by avahi / bonjour
    let label = instance
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let host = format!("{label}-lan-mouse.local.");
    let properties = [
        (TXT_FINGERPRINT, fingerprint),
        (TXT_VERSION, ANNOUNCEMENT_VERSION),
    ];
    Ok(
        ServiceInfo::new(SERVICE_TYPE, instance, &host, (), port, &properties[..])?
            .enable_addr_auto(),
    )
}

fn device_from_resolved(info: &ResolvedService, own_fingerprint: &str) -> Option<DiscoveredDevice> {
    device_from_parts(
        info.get_fullname(),
        info.get_port(),
        info.get_addresses().iter().map(|ip| ip.to_ip_addr()),
        info.get_property_val_str(TXT_FINGERPRINT),
        own_fingerprint,
    )
}

/// Converts the parts of a resolved service to a [`DiscoveredDevice`].
///
/// Returns `None` for services without fingerprint, without usable address
/// and for our own announcement (same fingerprint).
fn device_from_parts(
    fullname: &str,
    port: u16,
    addrs: impl IntoIterator<Item = IpAddr>,
    fingerprint: Option<&str>,
    own_fingerprint: &str,
) -> Option<DiscoveredDevice> {
    let fingerprint = fingerprint.filter(|fp| !fp.is_empty())?;
    if fingerprint == own_fingerprint {
        return None;
    }
    let hostname = instance_name(fullname).filter(|h| !h.is_empty())?;
    // ordered set: ipv4 addresses first
    let ips = addrs
        .into_iter()
        .filter(is_usable_addr)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return None;
    }
    Some(DiscoveredDevice {
        hostname,
        ips,
        port,
        fingerprint: fingerprint.to_owned(),
    })
}

fn is_usable_addr(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !(ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()),
        IpAddr::V6(ip) => !(ip.is_loopback() || ip.is_unicast_link_local() || ip.is_unspecified()),
    }
}

/// extracts the (unescaped) instance name from a dns-sd fullname
fn instance_name(fullname: &str) -> Option<String> {
    let escaped = fullname.strip_suffix(SERVICE_TYPE)?.strip_suffix('.')?;
    let mut name = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => name.extend(chars.next()),
            c => name.push(c),
        }
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "aa:aa";
    const OTHER: &str = "bb:bb";

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("valid ip")
    }

    fn parse(
        fullname: &str,
        addrs: &[&str],
        fingerprint: Option<&str>,
    ) -> Option<DiscoveredDevice> {
        device_from_parts(
            fullname,
            4242,
            addrs.iter().map(|a| ip(a)),
            fingerprint,
            OWN,
        )
    }

    #[test]
    fn resolves_device() {
        let device =
            parse("desk._lan-mouse._udp.local.", &["192.168.1.5"], Some(OTHER)).expect("device");
        assert_eq!(
            device,
            DiscoveredDevice {
                hostname: "desk".into(),
                ips: vec![ip("192.168.1.5")],
                port: 4242,
                fingerprint: OTHER.into(),
            }
        );
    }

    #[test]
    fn ignores_own_instance() {
        let fullname = "desk._lan-mouse._udp.local.";
        assert_eq!(parse(fullname, &["192.168.1.5"], Some(OWN)), None);
    }

    #[test]
    fn requires_fingerprint() {
        let fullname = "desk._lan-mouse._udp.local.";
        assert_eq!(parse(fullname, &["192.168.1.5"], None), None);
        assert_eq!(parse(fullname, &["192.168.1.5"], Some("")), None);
    }

    #[test]
    fn filters_addresses_and_orders_ipv4_first() {
        let device = parse(
            "desk._lan-mouse._udp.local.",
            &[
                "fd00::5",
                "fe80::1",
                "::1",
                "127.0.0.1",
                "169.254.3.4",
                "0.0.0.0",
                "10.0.0.7",
                "192.168.1.5",
                "192.168.1.5",
            ],
            Some(OTHER),
        )
        .expect("device");
        assert_eq!(
            device.ips,
            vec![ip("10.0.0.7"), ip("192.168.1.5"), ip("fd00::5")]
        );
    }

    #[test]
    fn requires_usable_address() {
        let fullname = "desk._lan-mouse._udp.local.";
        assert_eq!(
            parse(fullname, &["127.0.0.1", "fe80::1"], Some(OTHER)),
            None
        );
        assert_eq!(parse(fullname, &[], Some(OTHER)), None);
    }

    #[test]
    fn unescapes_instance_name() {
        let device = parse(
            r"My\.Mac\\book's mac._lan-mouse._udp.local.",
            &["192.168.1.5"],
            Some(OTHER),
        )
        .expect("device");
        assert_eq!(device.hostname, r"My.Mac\book's mac");
    }

    #[test]
    fn ignores_foreign_service_type() {
        assert_eq!(
            parse("desk._other._udp.local.", &["192.168.1.5"], Some(OTHER)),
            None
        );
    }

    #[test]
    fn builds_service_info() {
        let info = service_info("My Mac.book", 4242, OTHER).expect("service info");
        assert_eq!(info.get_fullname(), r"My Mac\.book._lan-mouse._udp.local.");
        assert_eq!(info.get_hostname(), "My-Mac-lan-mouse.local.");
        assert_eq!(info.get_port(), 4242);
        assert_eq!(info.get_property_val_str(TXT_FINGERPRINT), Some(OTHER));
        assert_eq!(info.get_property_val_str(TXT_VERSION), Some("1"));
        assert!(info.is_addr_auto());
        assert_eq!(
            instance_name(info.get_fullname()).as_deref(),
            Some("My Mac.book")
        );
    }

    /// needs a network interface with multicast: `cargo test -- --ignored discovers_other_instance`
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "uses the real network"]
    async fn discovers_other_instance() {
        let mut discovery = Discovery::new(4343, OWN.to_owned());
        let other = ServiceDaemon::new().expect("daemon");
        let info = service_info("lan-mouse-test-peer", 4344, OTHER).expect("service info");
        other.register(info).expect("register");

        let devices = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let devices = discovery.changed().await;
                if !devices.is_empty() {
                    break devices;
                }
            }
        })
        .await
        .expect("timeout");

        // our own announcement is filtered out, the peer is found
        assert_eq!(devices.len(), 1, "{devices:?}");
        assert_eq!(devices[0].hostname, "lan-mouse-test-peer");
        assert_eq!(devices[0].port, 4344);
        assert_eq!(devices[0].fingerprint, OTHER);
        assert_eq!(devices, discovery.devices());
        discovery.terminate().await;
        let _ = other.shutdown();
    }
}
