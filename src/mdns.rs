//! mDNS / DNS-SD advertising for IPP printers (`_ipp._tcp.local.`).
//!
//! Gated by the default-on `mdns` feature. [`Advertiser::register_all`]
//! publishes one service instance per printer in the registry, with the TXT
//! records CUPS / cups-browsed need for IPP-Everywhere auto-discovery
//! (RFC 8011 + Bonjour for IPP + PWG 5100.14).

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceInfo};
use parking_lot::Mutex;

use crate::printer::{PrinterRecord, PrinterRegistry};
use crate::status::AdvertiserControl;

const IPP_SERVICE: &str = "_ipp._tcp.local.";

/// Interface name prefixes for container / VM virtual bridges and veth pairs.
///
/// Advertising on these is actively harmful next to a co-resident
/// `cups-browsed`: it resolves our service over every Docker `veth*` /
/// `br-*` link, and the duplicate / racy A-record answers on those links make
/// avahi hand `cups-browsed` a *null* host name for some resolves. A null host
/// name fails its `is_local_hostname()` check, so that resolve bypasses the
/// `UUID=` dedup and `cups-browsed` builds a spurious `implicitclass://`
/// duplicate queue. Restricting the advert to real interfaces removes those
/// resolves at the source. (Plain `br0`/`tun0` are *not* matched — only the
/// `br-`/container-style names — so genuine LAN bridges still advertise.)
const VIRTUAL_IFACE_PREFIXES: &[&str] = &[
    "veth", "docker", "br-", "virbr", "vnet", "vmnet", "vboxnet",
];

/// Owns the [`ServiceDaemon`] and the bind-time host/addresses, and tracks the
/// DNS-SD fullname registered for each printer so individual services can be
/// withdrawn and republished as devices go offline / come back.
pub struct Advertiser {
    daemon: ServiceDaemon,
    port: u16,
    host: String,
    addrs: Vec<IpAddr>,
    /// printer logical name -> registered DNS-SD fullname.
    registered: Mutex<HashMap<String, String>>,
}

impl Advertiser {
    /// Start a daemon and register every printer in the registry.
    pub fn register_all(registry: &PrinterRegistry, port: u16) -> mdns_sd::Result<Self> {
        let me = Self {
            daemon: ServiceDaemon::new()?,
            port,
            host: hostname(),
            addrs: advertise_addrs(),
            registered: Mutex::new(HashMap::new()),
        };
        for rec in registry.read().iter() {
            me.register_one(rec)?;
        }
        Ok(me)
    }

    /// Build + register one printer's service, recording its fullname.
    /// Replaces any existing registration for the same logical name.
    fn register_one(&self, rec: &PrinterRecord) -> mdns_sd::Result<()> {
        let info = service_info(
            &self.host,
            &self.addrs,
            self.port,
            rec.config.display_label(),
            &rec.config.name,
            &rec.config.make_and_model,
            &rec.uuid,
        )?;
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        log::info!("mdns: registered {fullname}");
        self.registered.lock().insert(rec.config.name.clone(), fullname);
        Ok(())
    }
}

impl AdvertiserControl for Advertiser {
    fn publish(&self, rec: &PrinterRecord) {
        if !self.registered.lock().contains_key(&rec.config.name) {
            if let Err(e) = self.register_one(rec) {
                log::warn!("mdns: republish of {} failed: {e}", rec.config.name);
            }
        }
    }

    fn withdraw(&self, name: &str) {
        if let Some(fullname) = self.registered.lock().remove(name) {
            log::info!("mdns: withdrawing {fullname} (device offline)");
            let _ = self.daemon.unregister(&fullname);
        }
    }

    fn is_advertised(&self, name: &str) -> bool {
        self.registered.lock().contains_key(name)
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        for fullname in self.registered.lock().values() {
            let _ = self.daemon.unregister(fullname);
        }
        let _ = self.daemon.shutdown();
    }
}

/// Whether `name` looks like a container/VM virtual bridge or veth interface
/// we must not advertise on (see [`VIRTUAL_IFACE_PREFIXES`]).
fn is_virtual_iface(name: &str) -> bool {
    VIRTUAL_IFACE_PREFIXES
        .iter()
        .any(|p| name.starts_with(p))
}

/// The host addresses to advertise on: every up, non-loopback, non-link-local
/// interface that isn't a container/VM virtual bridge. Replaces mdns-sd's
/// `enable_addr_auto()` (which advertises on *all* interfaces, including the
/// `veth*`/`br-*` links that defeat `cups-browsed` dedup — see
/// [`VIRTUAL_IFACE_PREFIXES`]). Returns empty if enumeration fails or filters
/// everything out, in which case the caller falls back to `enable_addr_auto()`.
fn advertise_addrs() -> Vec<IpAddr> {
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(i) => i,
        Err(e) => {
            log::warn!("mdns: interface enumeration failed ({e}); advertising on all interfaces");
            return Vec::new();
        }
    };
    let mut addrs = Vec::new();
    for iface in ifaces {
        if iface.is_loopback() || iface.is_link_local() || !iface.is_oper_up() {
            continue;
        }
        if is_virtual_iface(&iface.name) {
            log::debug!("mdns: skipping virtual interface {} ({})", iface.name, iface.ip());
            continue;
        }
        log::debug!("mdns: advertising on {} ({})", iface.name, iface.ip());
        addrs.push(iface.ip());
    }
    addrs
}

fn hostname() -> String {
    let h = std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string());
    // mdns-sd normalises trailing ".local." — pass bare hostname.
    h
}

fn service_info(
    host: &str,
    addrs: &[IpAddr],
    port: u16,
    instance_name: &str,
    logical_name: &str,
    make_and_model: &str,
    uuid: &str,
) -> mdns_sd::Result<ServiceInfo> {
    let mut txt: HashMap<String, String> = HashMap::new();
    // `rp` is the IPP resource path on our server — always the logical name.
    // The DNS-SD *instance* name (below) carries the human-readable label.
    txt.insert("rp".into(), format!("ipp/print/{logical_name}"));
    // `UUID=` lets a local cups-browsed dedupe this advert against a CUPS
    // queue with the same `printer-uuid` and stand down (it's the same
    // mechanism CUPS's own shared queues use). Advertise the bare value —
    // cups-browsed strips `urn:uuid:` on the CUPS side before comparing.
    let bare_uuid = uuid.strip_prefix("urn:uuid:").unwrap_or(uuid);
    if !bare_uuid.is_empty() {
        txt.insert("UUID".into(), bare_uuid.to_string());
    }
    txt.insert("ty".into(), make_and_model.to_string());
    txt.insert("note".into(), make_and_model.to_string());
    txt.insert("product".into(), format!("({make_and_model})"));
    // Document formats CUPS asks for during driverless setup.
    txt.insert(
        "pdl".into(),
        "image/pwg-raster,application/vnd.cups-raster,application/octet-stream".into(),
    );
    // IPP Everywhere advertises URF=…; CUPS reads this for the everywhere driver.
    txt.insert("URF".into(), "W8,SRGB24,CP1,RS203".into());
    txt.insert("Color".into(), "F".into());
    txt.insert("Duplex".into(), "F".into());
    txt.insert("adminurl".into(), format!("http://{host}.local:{port}/"));
    txt.insert("priority".into(), "0".into());
    txt.insert("qtotal".into(), "1".into());
    // TXT version per PWG 5100.14.
    txt.insert("txtvers".into(), "1".into());

    // Advertise an explicit, filtered address list when we have one; otherwise
    // fall back to mdns-sd's auto-detection (all interfaces). The filtered list
    // excludes container/VM virtual bridges so a co-resident `cups-browsed`
    // doesn't see us over `veth*`/`br-*` links (see `advertise_addrs`).
    let info = if addrs.is_empty() {
        ServiceInfo::new(
            IPP_SERVICE,
            instance_name,
            &format!("{host}.local."),
            "", // IPs filled by enable_addr_auto
            port,
            txt,
        )?
        .enable_addr_auto()
    } else {
        ServiceInfo::new(
            IPP_SERVICE,
            instance_name,
            &format!("{host}.local."),
            addrs,
            port,
            txt,
        )?
    };
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::is_virtual_iface;

    #[test]
    fn flags_container_and_vm_interfaces() {
        for name in [
            "veth1a2b3c",   // Docker container veth pair (host side)
            "docker0",      // Docker default bridge
            "br-9f3c1d20a", // Docker user-defined bridge
            "virbr0",       // libvirt bridge
            "vnet3",        // libvirt VM tap
            "vmnet8",       // VMware
            "vboxnet0",     // VirtualBox
        ] {
            assert!(is_virtual_iface(name), "{name} should be filtered out");
        }
    }

    #[test]
    fn keeps_real_interfaces() {
        // Real NICs and genuine LAN bridges/tunnels (no `-` / container prefix)
        // must still be advertised.
        for name in ["eth0", "enp3s0", "wlan0", "wlp2s0", "br0", "tun0", "lo"] {
            assert!(!is_virtual_iface(name), "{name} should be kept");
        }
    }
}
