//! [`DeviceBackend`] trait: enumerate physical devices, resolve driver names,
//! poll live status.

use crate::flags::PrinterReason;
use crate::printer::PrinterConfig;

/// Enumerate physical printers and report their live health.
///
/// Implementations describe how to discover devices (e.g. via sysfs, BlueZ,
/// USB enumeration) and how to map their identifying strings to a driver
/// name registered with the framework.
pub trait DeviceBackend: Send + Sync {
    /// Call `emit(info, uri, device_id)` for each discovered device. The
    /// closure returns `true` to continue enumeration, `false` to stop early.
    fn list(&self, emit: &mut dyn FnMut(&str, &str, &str) -> bool);

    /// Map a device's IEEE 1284 `device-id` string and URI to a driver name
    /// that this backend recognises. Return `None` for "this isn't one of
    /// mine, skip it".
    fn driver_for_device(&self, device_id: &str, device_uri: &str) -> Option<String>;

    /// Query live `printer-state-reasons` for a registered printer. The
    /// background status loop calls this on each registered printer and
    /// updates the IPP attribute when the value changes. Returning `None`
    /// means "no update" (keep whatever the registry already holds).
    fn poll_status(&self, _config: &PrinterConfig) -> Option<PrinterReason> {
        None
    }

    /// Handle an `Identify-Printer` request (PWG 5100.14 §5.1) — make the
    /// physical device announce itself (beep, flash an LED, …). `actions`
    /// holds the requested `identify-actions` keywords (`display`, `sound`,
    /// `flash`, `speak`); an empty slice means "use the default action".
    /// Default: no-op.
    fn identify(&self, _config: &PrinterConfig, _actions: &[String]) {}
}
