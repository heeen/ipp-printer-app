//! Smallest possible printer application — a no-op `DeviceBackend` reports a
//! single fake device, the `print_job` callback discards the raster.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example minimal_server --no-default-features
//! ```
//!
//! `--no-default-features` skips mDNS so the example doesn't try to grab a
//! multicast socket. Add it back if you want to actually advertise.
//!
//! Once it's running, point CUPS at it:
//!
//! ```sh
//! sudo lpadmin -p ipp-demo -E \
//!     -v ipp://localhost:8631/ipp/print/demo -m everywhere
//! ```

use std::sync::Arc;

use ipp_printer_app::{
    default_state_path, DeviceBackend, PrinterConfig, PrinterRegistry, Server, ServerOptions,
};
use parking_lot::RwLock;

struct DemoBackend;

impl DeviceBackend for DemoBackend {
    fn list(&self, emit: &mut dyn FnMut(&str, &str, &str) -> bool) {
        let _ = emit("Demo Printer", "demo://printer-1", "MFG:Demo;MDL:Test;");
    }

    fn driver_for_device(&self, _device_id: &str, device_uri: &str) -> Option<String> {
        device_uri.starts_with("demo://").then(|| "demo".to_string())
    }
}

fn make_config(
    name: &str,
    info: &str,
    driver: &str,
    uri: &str,
    device_id: &str,
) -> Option<PrinterConfig> {
    Some(PrinterConfig {
        name: name.to_string(),
        display_name: info.to_string(),
        driver_name: driver.to_string(),
        make_and_model: "Demo Printer".into(),
        device_id: device_id.to_string(),
        device_uri: uri.to_string(),
        dpi: 203,
        printhead_width_dots: 384,
        media_names: vec!["om_30x20mm_30x20mm".into()],
        media_sizes: vec![[3000, 2000]],
        darkness: 50,
        document_formats: Vec::new(),
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .try_init()
        .ok();

    let registry: PrinterRegistry = Arc::new(RwLock::new(Vec::new()));
    let state_path = default_state_path("ipp-printer-app-demo");
    let backend = Arc::new(DemoBackend);

    Server::bootstrap_printers(&registry, backend.as_ref(), &state_path, make_config);

    Server::run(ServerOptions {
        host: "127.0.0.1".into(),
        port: 8631,
        printers: registry,
        device_backend: backend,
        print_job: Arc::new(|ctx, raster, copies| {
            log::info!(
                "demo: pretending to print job {} on {} ({} bytes, {} copies)",
                ctx.id,
                ctx.printer_name,
                raster.len(),
                copies
            );
            // A real device backend would return DeviceUnavailable when the
            // hardware can't be reached (the framework then holds + retries the
            // job) or Failed for a bad document. The demo always succeeds.
            ipp_printer_app::JobOutcome::Completed
        }),
        state_path,
        advertise_mdns: true,
    })
    .await
}
