//! Background `printer-state-reasons` poller.
//!
//! A single tokio task per server walks the printer registry every poll
//! interval (default 30 s, override with `IPP_PRINTER_APP_POLL_SECS`) and
//! asks the backend for fresh status. Backends that don't override
//! [`DeviceBackend::poll_status`] return `None` and leave the registry
//! untouched.

use std::sync::Arc;
use std::time::Duration;

use crate::device::DeviceBackend;
use crate::printer::{PrinterRecord, PrinterRegistry};

/// Lets the status poller pull a printer out of DNS-SD discovery when its
/// device goes offline, and republish it when the device returns — so a
/// powered-off printer stops showing up in print dialogs. Implemented by
/// [`crate::mdns::Advertiser`]; the poller holds it as a trait object so the
/// poller stays independent of the optional `mdns` feature.
pub trait AdvertiserControl: Send + Sync {
    /// Republish the printer's discovery advert (its device came back online).
    /// Idempotent: a no-op if already advertised.
    fn publish(&self, rec: &PrinterRecord);
    /// Withdraw the printer's discovery advert (its device went offline).
    /// Idempotent: a no-op if not currently advertised.
    fn withdraw(&self, name: &str);
    /// Whether this printer is currently advertised.
    fn is_advertised(&self, name: &str) -> bool;
}

/// Default cadence (configurable via `IPP_PRINTER_APP_POLL_SECS`).
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Spawn the polling task. Returns immediately. Drop the returned
/// [`tokio::task::JoinHandle`] to abort the loop on server shutdown.
pub fn spawn(
    backend: Arc<dyn DeviceBackend>,
    registry: PrinterRegistry,
    advertiser: Option<Arc<dyn AdvertiserControl>>,
) -> tokio::task::JoinHandle<()> {
    let interval = std::env::var("IPP_PRINTER_APP_POLL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(POLL_INTERVAL);

    tokio::spawn(async move {
        // First poll happens after `interval` so the server has time to
        // bootstrap printers before the first status query lands on a device.
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // immediate first tick
        loop {
            ticker.tick().await;
            poll_once(backend.as_ref(), &registry, advertiser.as_deref()).await;
        }
    })
}

async fn poll_once(
    backend: &dyn DeviceBackend,
    registry: &PrinterRegistry,
    advertiser: Option<&dyn AdvertiserControl>,
) {
    use crate::flags::PrinterReason;
    use crate::printer::IppPrinterState;
    // Snapshot printers that aren't actively printing. We poll Idle AND Stopped
    // ones: a Stopped (offline) printer must keep being polled so it recovers
    // to Idle when the device answers again. Processing printers are skipped so
    // we don't contend with the device mid-job.
    let configs: Vec<_> = {
        let g = registry.read();
        g.iter()
            .filter(|r| matches!(r.state, IppPrinterState::Idle | IppPrinterState::Stopped))
            .map(|r| r.config.clone())
            .collect()
    };

    for cfg in configs {
        // `poll_status` may block on a HID read; let it run in the blocking
        // section so other tasks (Get-Printer-Attributes, etc.) stay responsive.
        let status = tokio::task::block_in_place(|| backend.poll_status(&cfg));
        let Some(status) = status else { continue };

        // `reachable` is the source of truth for both printer-state and the
        // discovery advert. Snapshot the record (for a possible republish)
        // while holding the lock, then reconcile the advert after releasing it.
        let reachable = !status.reasons.contains(PrinterReason::OFFLINE);
        let mut rec_snapshot: Option<PrinterRecord> = None;
        {
            let mut g = registry.write();
            if let Some(rec) = g.iter_mut().find(|r| r.config.name == cfg.name) {
                if rec.reasons != status.reasons {
                    log::debug!(
                        "status: {} reasons {:?} -> {:?}",
                        cfg.name,
                        rec.reasons,
                        status.reasons
                    );
                    rec.reasons = status.reasons;
                }
                // Reflect reachability in printer-state: an unreachable device
                // reports OFFLINE, which we surface as printer-state=stopped so
                // CUPS *holds* queued jobs until the device is back (then idle
                // again releases them). Only Idle/Stopped printers are in this
                // set, so a Processing job is never disturbed.
                let want = if reachable {
                    IppPrinterState::Idle
                } else {
                    IppPrinterState::Stopped
                };
                if rec.state != want {
                    log::info!("status: {} state {:?} -> {:?}", cfg.name, rec.state, want);
                    rec.state = want;
                }
                // Carry forward last-known media/supply when a poll omits them.
                if status.ready_media.is_some() {
                    rec.ready_media = status.ready_media;
                }
                if status.supply_percent.is_some() {
                    rec.supply_percent = status.supply_percent;
                }
                if advertiser.is_some() {
                    rec_snapshot = Some(rec.clone());
                }
            }
        }

        // Reconcile the discovery advert against reachability — idempotent, not
        // edge-triggered: a reachable printer must be advertised, an offline one
        // withdrawn. Reconciling (rather than firing on a state edge) means a
        // held job that drove the printer Stopped→Processing→Idle as it finally
        // printed still gets its advert restored on the next poll.
        if let Some(adv) = advertiser {
            if reachable {
                if let Some(rec) = rec_snapshot {
                    if !adv.is_advertised(&cfg.name) {
                        adv.publish(&rec);
                    }
                }
            } else if adv.is_advertised(&cfg.name) {
                adv.withdraw(&cfg.name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::PollStatus;
    use crate::flags::PrinterReason;
    use crate::printer::{IppPrinterState, PrinterConfig, PrinterRecord};
    use parking_lot::Mutex;
    use std::collections::HashSet;

    /// Backend whose reported reasons the test flips to simulate the device
    /// going offline and coming back.
    struct FakeBackend {
        reasons: Mutex<PrinterReason>,
    }
    impl DeviceBackend for FakeBackend {
        fn list(&self, _emit: &mut dyn FnMut(&str, &str, &str) -> bool) {}
        fn driver_for_device(&self, _id: &str, _uri: &str) -> Option<String> {
            None
        }
        fn poll_status(&self, _config: &PrinterConfig) -> Option<PollStatus> {
            Some(PollStatus {
                reasons: *self.reasons.lock(),
                ready_media: None,
                supply_percent: None,
            })
        }
    }

    /// Records the advert calls the poller makes and the live advertised set.
    #[derive(Default)]
    struct FakeAdvertiser {
        advertised: Mutex<HashSet<String>>,
    }
    impl AdvertiserControl for FakeAdvertiser {
        fn publish(&self, rec: &PrinterRecord) {
            self.advertised.lock().insert(rec.config.name.clone());
        }
        fn withdraw(&self, name: &str) {
            self.advertised.lock().remove(name);
        }
        fn is_advertised(&self, name: &str) -> bool {
            self.advertised.lock().contains(name)
        }
    }

    fn config(name: &str) -> PrinterConfig {
        PrinterConfig {
            name: name.into(),
            display_name: String::new(),
            driver_name: "t".into(),
            make_and_model: "Test".into(),
            device_id: String::new(),
            device_uri: "mock://x".into(),
            dpi: 203,
            printhead_width_dots: 384,
            media_names: vec![],
            media_sizes: vec![],
            darkness: 50,
            document_formats: vec![],
        }
    }

    // block_in_place inside poll_once needs a multi-threaded runtime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn offline_stops_and_withdraws_then_recovery_republishes() {
        let backend = FakeBackend { reasons: Mutex::new(PrinterReason::OFFLINE) };
        let registry: PrinterRegistry =
            Arc::new(parking_lot::RwLock::new(vec![PrinterRecord::new(config("p"))]));
        let adv = Arc::new(FakeAdvertiser::default());
        // Simulate startup register_all having advertised it.
        adv.publish(&registry.read()[0].clone());
        let advref: &dyn AdvertiserControl = adv.as_ref();

        // Device offline -> printer stopped + advert withdrawn.
        poll_once(&backend, &registry, Some(advref)).await;
        assert_eq!(registry.read()[0].state, IppPrinterState::Stopped);
        assert!(!adv.is_advertised("p"), "offline printer must be withdrawn");

        // Device back -> printer idle + advert republished.
        *backend.reasons.lock() = PrinterReason::empty();
        poll_once(&backend, &registry, Some(advref)).await;
        assert_eq!(registry.read()[0].state, IppPrinterState::Idle);
        assert!(adv.is_advertised("p"), "recovered printer must be republished");
    }

    /// The advert is reconciled, not edge-triggered: a held job that drove the
    /// printer Stopped -> Processing -> Idle leaves it Idle but un-advertised;
    /// the next poll must republish it even though the poller saw no Stopped->
    /// Idle edge.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_republishes_without_a_state_edge() {
        let backend = FakeBackend { reasons: Mutex::new(PrinterReason::empty()) };
        let registry: PrinterRegistry =
            Arc::new(parking_lot::RwLock::new(vec![PrinterRecord::new(config("p"))]));
        // Already Idle (the worker reset it after a held job finished printing),
        // but the advert was never restored.
        let adv = Arc::new(FakeAdvertiser::default());
        assert!(!adv.is_advertised("p"));
        let advref: &dyn AdvertiserControl = adv.as_ref();

        poll_once(&backend, &registry, Some(advref)).await;
        assert!(
            adv.is_advertised("p"),
            "reconcile must restore the advert with no state transition"
        );
    }
}
