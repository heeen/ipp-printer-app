//! IPP `printer-state-reasons` bit flags (PWG 5101.1 keywords).

use bitflags::bitflags;

/// Underlying integer representation for [`PrinterReason`].
pub type PrinterReasonRaw = u32;

bitflags! {
    /// `printer-state-reasons` flags. Use [`PrinterReason::empty`] for
    /// "no reasons"; do NOT define a `NONE = 0` constant — bitflags
    /// `.contains(zero)` is always `true`, making it a footgun.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[allow(missing_docs)]
    pub struct PrinterReason: PrinterReasonRaw {
        const OTHER = 0x0001;
        const COVER_OPEN = 0x0002;
        const INPUT_TRAY_MISSING = 0x0004;
        const MARKER_SUPPLY_EMPTY = 0x0008;
        const MARKER_SUPPLY_LOW = 0x0010;
        const MARKER_WASTE_ALMOST_FULL = 0x0020;
        const MARKER_WASTE_FULL = 0x0040;
        const MEDIA_EMPTY = 0x0080;
        const MEDIA_JAM = 0x0100;
        const MEDIA_LOW = 0x0200;
        const MEDIA_NEEDED = 0x0400;
        const OFFLINE = 0x0800;
        const SPOOL_AREA_FULL = 0x1000;
        const TONER_EMPTY = 0x2000;
        const TONER_LOW = 0x4000;
        const DOOR_OPEN = 0x8000;
        const IDENTIFY_PRINTER_REQUESTED = 0x10000;
    }
}

impl PrinterReason {
    /// PWG keyword tokens for this flag set, in the order CUPS expects.
    /// An empty set yields `["none"]`.
    pub fn ipp_keywords(&self) -> Vec<&'static str> {
        if self.is_empty() {
            return vec!["none"];
        }
        let table = [
            (Self::OTHER, "other"),
            (Self::COVER_OPEN, "cover-open"),
            (Self::DOOR_OPEN, "door-open"),
            (Self::INPUT_TRAY_MISSING, "input-tray-missing"),
            (Self::MARKER_SUPPLY_EMPTY, "marker-supply-empty"),
            (Self::MARKER_SUPPLY_LOW, "marker-supply-low"),
            (Self::MARKER_WASTE_ALMOST_FULL, "marker-waste-almost-full"),
            (Self::MARKER_WASTE_FULL, "marker-waste-full"),
            (Self::MEDIA_EMPTY, "media-empty"),
            (Self::MEDIA_JAM, "media-jam"),
            (Self::MEDIA_LOW, "media-low"),
            (Self::MEDIA_NEEDED, "media-needed"),
            (Self::OFFLINE, "offline-report"),
            (Self::SPOOL_AREA_FULL, "spool-area-full"),
            (Self::TONER_EMPTY, "toner-empty"),
            (Self::TONER_LOW, "toner-low"),
            (Self::IDENTIFY_PRINTER_REQUESTED, "identify-printer-requested"),
        ];
        table
            .into_iter()
            .filter(|(bit, _)| self.contains(*bit))
            .map(|(_, kw)| kw)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_is_none() {
        assert_eq!(PrinterReason::empty().ipp_keywords(), vec!["none"]);
    }

    #[test]
    fn single_flag_surfaces() {
        assert_eq!(PrinterReason::COVER_OPEN.ipp_keywords(), vec!["cover-open"]);
    }

    #[test]
    fn multi_flag_surfaces_all() {
        let r = PrinterReason::COVER_OPEN | PrinterReason::MEDIA_EMPTY;
        let kws = r.ipp_keywords();
        assert!(kws.contains(&"cover-open"));
        assert!(kws.contains(&"media-empty"));
        assert!(!kws.contains(&"none"));
    }
}
