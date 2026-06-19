//! [`DeviceBackend`] trait: enumerate physical devices, resolve driver names,
//! poll live status.

use crate::flags::PrinterReason;
use crate::printer::PrinterConfig;

/// The media currently loaded in a device, as reported by a live poll. Drives
/// the dynamic `media-ready` / `media-col-ready` IPP attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyMedia {
    /// PWG self-describing media name (e.g. `om_40x30mm_40x30mm`).
    pub name: String,
    /// Width/height in hundredths of a millimetre (PWG units).
    pub size_hmm: [i32; 2],
    /// PWG media-type keyword (e.g. `labels`).
    pub media_type: String,
}

/// Live status reported by [`DeviceBackend::poll_status`]. Returning `None`
/// from the poll means "no update"; returning `Some(PollStatus)` replaces the
/// reasons and, when the optional fields are set, the dynamic media/supply
/// attributes.
#[derive(Debug, Clone)]
pub struct PollStatus {
    /// Fresh `printer-state-reasons`.
    pub reasons: PrinterReason,
    /// Currently-loaded media, if the device can report it.
    pub ready_media: Option<ReadyMedia>,
    /// Remaining-supply level 0–100 (e.g. labels left on the roll), if known.
    pub supply_percent: Option<u8>,
}

impl Default for PollStatus {
    fn default() -> Self {
        Self {
            reasons: PrinterReason::empty(),
            ready_media: None,
            supply_percent: None,
        }
    }
}

impl PollStatus {
    /// Convenience constructor for backends that only report reasons.
    pub fn from_reasons(reasons: PrinterReason) -> Self {
        Self {
            reasons,
            ..Default::default()
        }
    }
}

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

    /// Query live status for a registered printer. The background status loop
    /// calls this on each idle printer and updates the IPP attributes when the
    /// value changes. Returning `None` means "no update" (keep whatever the
    /// registry already holds); returning `Some` carries the fresh reasons and,
    /// optionally, the loaded media and remaining-supply level.
    fn poll_status(&self, _config: &PrinterConfig) -> Option<PollStatus> {
        None
    }

    /// Handle an `Identify-Printer` request (PWG 5100.14 §5.1) — make the
    /// physical device announce itself (beep, flash an LED, …). `actions`
    /// holds the requested `identify-actions` keywords (`display`, `sound`,
    /// `flash`, `speak`); an empty slice means "use the default action".
    /// Default: no-op.
    fn identify(&self, _config: &PrinterConfig, _actions: &[String]) {}
}
