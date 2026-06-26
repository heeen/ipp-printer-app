#![deny(missing_docs)]

//! Pure-Rust IPP Everywhere framework for building CUPS-driverless
//! "printer applications" — the modern replacement for the PPD-driven
//! filter+backend model.
//!
//! The framework runs an axum HTTP listener that speaks IPP over POST on
//! `/ipp/print/<printer-name>`. It's device-agnostic: a consumer crate
//! supplies a [`DeviceBackend`] (enumerate physical devices, poll their
//! status) plus a [`RasterDriver`] (turn a PWG raster page into device
//! bytes), and the framework handles the rest — job registry with
//! monotonic ids, `Get-Jobs`/`Get-Job-Attributes`/`Cancel-Job`, background
//! status polling, optional mDNS / DNS-SD advertising (enabled by the
//! default `mdns` feature), JSON state persistence.
//!
//! See `examples/minimal_server.rs` for the smallest end-to-end usage.

pub mod attributes;
pub mod device;
pub mod flags;
pub mod job;
pub mod printer;
#[cfg(feature = "mdns")]
pub mod mdns;
pub mod raster;
pub mod server;
pub mod state;
pub mod status;

pub use device::{DeviceBackend, DiscoveredDevice, PollStatus, ReadyMedia};
pub use flags::PrinterReason;
pub use job::{JobId, JobRecord, JobRegistry, JobState};
pub use printer::{PrinterConfig, PrinterHandle, PrinterRecord, PrinterRegistry};
pub use raster::{JobFailure, JobOptions, JobOutcome, RasterDriver};
pub use server::{JobContext, PrintJobFn, PrintJobFuture, Server, ServerOptions};
pub use state::{default_state_path, PersistedState};
