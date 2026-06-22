//! Per-printer configuration and runtime state.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::flags::PrinterReason;

/// Static printer capabilities supplied by the consumer crate (typically
/// loaded from a config file). Carries everything the framework needs to
/// build the IPP `Get-Printer-Attributes` response.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(missing_docs)]
pub struct PrinterConfig {
    /// Logical, machine-readable name: the CUPS queue name, the IPP
    /// `printer-name`, the `/ipp/print/<name>` resource path, and the mDNS
    /// `rp` TXT all use this. Keep it to `[a-z0-9_]` so it round-trips through
    /// CUPS's own DNS-SD queue-name sanitiser (`cups_queue_name`), letting a
    /// co-resident CUPS recognise its on-demand temp queue as already-served
    /// (its lookup is case-insensitive).
    pub name: String,
    /// Human-readable name shown to users: the mDNS **service instance name**
    /// (what OS print dialogs display), IPP `printer-info`, and the web UI.
    /// May contain spaces / mixed case. Empty falls back to `make_and_model`,
    /// then `name`. `#[serde(default)]` so older persisted state still loads.
    #[serde(default)]
    pub display_name: String,
    pub driver_name: String,
    pub make_and_model: String,
    pub device_id: String,
    pub device_uri: String,
    pub dpi: i32,
    pub printhead_width_dots: u32,
    pub media_names: Vec<String>,
    pub media_sizes: Vec<[i32; 2]>,
    /// Darkness 0–100 (maps to print density).
    pub darkness: i32,
    /// MIME types the consumer's print callback can decode, emitted as
    /// `document-format-supported`. Empty falls back to the framework's raster
    /// defaults (`image/pwg-raster`, `application/vnd.cups-raster`,
    /// `application/octet-stream`). Add `image/jpeg` etc. when the backend can
    /// handle them.
    #[serde(default)]
    pub document_formats: Vec<String>,
}

impl PrinterConfig {
    /// Build the canonical `ipp://<host>:<port>/ipp/print/<name>` URI. If
    /// `host` is unspecified (`0.0.0.0`, `::`, empty), advertises
    /// `localhost` so CUPS and mDNS clients get a reachable address.
    pub fn printer_uri(&self, host: &str, port: u16) -> String {
        let h = if host == "0.0.0.0" || host == "::" || host.is_empty() {
            "localhost"
        } else {
            host
        };
        format!("ipp://{h}:{port}/ipp/print/{}", self.name)
    }

    /// The effective human-readable label: `display_name` if set, else
    /// `make_and_model`, else the logical `name`. Used for the mDNS service
    /// instance name, IPP `printer-info`, and the web UI.
    pub fn display_label(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.make_and_model.is_empty() {
            &self.make_and_model
        } else {
            &self.name
        }
    }
}

/// IPP `printer-state` enum (RFC 8011 §5.4.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[allow(missing_docs)]
pub enum IppPrinterState {
    Idle = 3,
    Processing = 4,
    Stopped = 5,
}

/// Runtime printer entry in the server registry.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct PrinterRecord {
    pub config: PrinterConfig,
    pub state: IppPrinterState,
    pub reasons: PrinterReason,
    pub uuid: String,
    /// Live media loaded in the device, set by the status poller. `None` until
    /// the first successful poll — the attribute builder then falls back to the
    /// configured default for `media-ready` / `media-col-ready`.
    pub ready_media: Option<crate::device::ReadyMedia>,
    /// Live remaining-supply level 0–100 from the status poller. `None` falls
    /// back to a full static `printer-supply`.
    pub supply_percent: Option<u8>,
}

impl PrinterRecord {
    /// Wrap a config in a fresh record (state = `Idle`, no reasons set, new UUID).
    pub fn new(config: PrinterConfig) -> Self {
        Self {
            uuid: uuid::Uuid::new_v4().to_string(),
            state: IppPrinterState::Idle,
            reasons: PrinterReason::empty(),
            ready_media: None,
            supply_percent: None,
            config,
        }
    }
}

/// Borrowed view of a printer passed into [`crate::RasterDriver`] callbacks.
///
/// `record` is exposed for direct access; the helpers below are the
/// commonly-needed shortcuts.
pub struct PrinterHandle<'a> {
    /// The underlying registry entry.
    pub record: &'a PrinterRecord,
}

impl<'a> PrinterHandle<'a> {
    /// Driver name from the config (matches the value supplied by
    /// [`crate::DeviceBackend::driver_for_device`]).
    pub fn driver_name(&self) -> &str {
        &self.record.config.driver_name
    }

    /// Configured darkness, 0–100.
    pub fn darkness(&self) -> i32 {
        self.record.config.darkness
    }

    /// Printhead width in dots.
    pub fn printhead_width_dots(&self) -> u32 {
        self.record.config.printhead_width_dots
    }
}

/// Shared printer registry. Cheap to clone (it's an `Arc`).
pub type PrinterRegistry = Arc<RwLock<Vec<PrinterRecord>>>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A persisted config written before `document_formats` existed must still
    /// deserialize (the field is `#[serde(default)]` → empty).
    #[test]
    fn config_without_document_formats_loads() {
        let json = r#"{
            "name": "p", "driver_name": "d", "make_and_model": "m",
            "device_id": "", "device_uri": "mock://x", "dpi": 203,
            "printhead_width_dots": 384, "media_names": [], "media_sizes": [],
            "darkness": 50
        }"#;
        let cfg: PrinterConfig = serde_json::from_str(json).expect("back-compat load");
        assert!(cfg.document_formats.is_empty());
    }
}
