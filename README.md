# ipp-printer-app

[![CI](https://github.com/heeen/ipp-printer-app/actions/workflows/ci.yml/badge.svg)](https://github.com/heeen/ipp-printer-app/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/ipp-printer-app.svg)](https://crates.io/crates/ipp-printer-app)
[![Docs](https://docs.rs/ipp-printer-app/badge.svg)](https://docs.rs/ipp-printer-app)

Pure-Rust IPP Everywhere framework for building CUPS-driverless **printer
applications** — the modern replacement for the PPD-driven CUPS
filter+backend model.

The framework speaks IPP over HTTP on `/ipp/print/<name>`, runs the job
state machine, tracks `printer-state-reasons`, and (optionally)
advertises itself over mDNS. You supply two things:

- a [`DeviceBackend`] — how to enumerate physical devices and poll their
  status, and
- a [`RasterDriver`] — how to turn a PWG-raster page into device bytes.

That's enough to land in CUPS via `lpadmin -m everywhere` and have
cups-browsed pick the printer up automatically.

## Quick start

```sh
cargo add ipp-printer-app
```

Minimal server (no real device, just shows the wiring):

```rust,no_run
use std::sync::Arc;
use ipp_printer_app::{
    default_state_path, DeviceBackend, PrinterConfig, PrinterRecord,
    PrinterRegistry, Server, ServerOptions,
};
use parking_lot::RwLock;

struct MyBackend;
impl DeviceBackend for MyBackend {
    fn list(&self, emit: &mut dyn FnMut(&str, &str, &str) -> bool) {
        let _ = emit("My Printer", "mydev://demo", "MFG:Demo;MDL:Test;");
    }
    fn driver_for_device(&self, _: &str, _: &str) -> Option<String> {
        Some("my_driver".into())
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let registry: PrinterRegistry = Arc::new(RwLock::new(Vec::new()));
    let state_path = default_state_path("my-printer-app");
    let backend = Arc::new(MyBackend);

    Server::bootstrap_printers(&registry, backend.as_ref(), &state_path,
        |name, info, driver, uri, device_id| Some(PrinterConfig {
            name: name.into(),
            display_name: info.into(),
            driver_name: driver.into(),
            make_and_model: "My Printer".into(),
            device_id: device_id.into(),
            device_uri: uri.into(),
            dpi: 203,
            printhead_width_dots: 384,
            media_names: vec!["oe_30x20mm_30x20mm".into()],
            media_sizes: vec![[3000, 2000]],
            darkness: 50,
        }),
    );

    Server::run(ServerOptions {
        host: "127.0.0.1".into(),
        port: 8631,
        printers: registry,
        device_backend: backend,
        print_job: Arc::new(|_ctx, _raster, _copies| Ok(())),
        state_path,
    }).await
}
```

See [`examples/minimal_server.rs`](examples/minimal_server.rs) for a
runnable version.

## What's in the box

| Concern | What you get |
|---|---|
| IPP wire format | [`ipp`](https://crates.io/crates/ipp) crate (parser + types) |
| HTTP server | `axum` 0.8 on `tokio` |
| Job lifecycle | `JobRegistry` with monotonic ids, `Get-Jobs` / `Get-Job-Attributes` / `Cancel-Job` |
| Live status | Background poll loop calling `DeviceBackend::poll_status`, configurable cadence (`IPP_PRINTER_APP_POLL_SECS`, default 30 s) |
| Discovery | mDNS `_ipp._tcp.local.` via `mdns-sd`, default-on `mdns` feature |
| State | JSON registry persisted at `$XDG_STATE_HOME/<app-id>.state.json` |
| Raster format | PWG raster + legacy CUPS raster v1/v2 via [`print_raster`](https://crates.io/crates/print_raster) |

What's **not** here:
- A device-side raster transform (dither, column-major, compression) —
  that's `RasterDriver` territory.
- A real-time job spooler with on-disk job persistence (in-memory only).
- HTTPS / IPP-over-TLS.
- URF format support (only PWG raster + CUPS raster).
- Color / multi-bit raster — the surface is 1bpp monochrome by default,
  drivers can extend.

## Features

| Feature | Default | What it pulls in |
|---|---|---|
| `mdns` | ✓ | `mdns-sd` (~200 KB) for `_ipp._tcp.local.` advertising |

For embedded targets without mDNS, build with `--no-default-features` to
drop ~200 KB of dependencies.

## CUPS integration

Once a printer application is running on `localhost:8631`:

```sh
sudo lpadmin -p MY_PRINTER -E -v ipp://localhost:8631/ipp/print/NAME -m everywhere
```

If you have `cups-browsed` running and built the framework with the
default `mdns` feature, the queue auto-appears within ~10 seconds and
no manual `lpadmin` is needed.

## Reference consumer

[`supvan-cups`](https://github.com/heeen/supvan-cups) (Supvan label
printers) is the reference real-world consumer. The full chain there:
`supvan-app` (binary) → `supvan-proto` (Bluetooth / USB HID transport)
→ `ipp-printer-app` (this crate) → axum / ipp / print_raster.

## MSRV

Rust 1.74.

## License

[MIT](LICENSE-MIT).
