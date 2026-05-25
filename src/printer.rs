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
    pub name: String,
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
}

impl PrinterRecord {
    /// Wrap a config in a fresh record (state = `Idle`, no reasons set, new UUID).
    pub fn new(config: PrinterConfig) -> Self {
        Self {
            uuid: uuid::Uuid::new_v4().to_string(),
            state: IppPrinterState::Idle,
            reasons: PrinterReason::empty(),
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
