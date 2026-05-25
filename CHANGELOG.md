# Changelog

## 0.1.0 — 2026-05-25

Initial release. A pure-Rust IPP Everywhere framework for building
CUPS-driverless printer applications.

### Public API

- [`Server`] / [`ServerOptions`] — axum HTTP server speaking IPP on
  `/ipp/print/<name>`.
- [`DeviceBackend`] trait — enumerate devices, resolve driver names,
  poll live status.
- [`RasterDriver`] trait — turn PWG raster scanlines into device bytes.
- [`JobRegistry`] + [`JobRecord`] / [`JobState`] / [`JobId`] — monotonic
  job-id allocation, `Get-Jobs` / `Get-Job-Attributes` / `Cancel-Job`.
- [`JobContext`] / [`JobFailure`] — failure propagation surface so
  drivers can set `printer-state-reasons` and `job-state-message`.
- [`PrinterConfig`] / [`PrinterRecord`] / [`PrinterRegistry`] —
  per-printer config + runtime state.
- [`PrinterReason`] bitflags with all 17 PWG 5101.1 `printer-state-reasons`
  keywords.
- [`PersistedState`] / `default_state_path` — JSON state persistence
  under `$XDG_STATE_HOME`.
- `status::spawn` — background `printer-state-reasons` poller.
- `mdns::Advertiser` (feature `mdns`, default-on) — `_ipp._tcp.local.`
  advertising via `mdns-sd`.

### Supported IPP operations

`Print-Job`, `Validate-Job`, `Get-Printer-Attributes`, `Get-Jobs`,
`Get-Job-Attributes`, `Cancel-Job`. Other operations return `400 Bad
Request`.

### Supported document formats

`image/pwg-raster`, `application/vnd.cups-raster`, and
`application/octet-stream` (auto-detected) — all read through
`print_raster`'s unified CUPS reader.

### MSRV

Rust 1.74.

### Known gaps

- No HTTPS / IPP-over-TLS.
- No URF format support.
- No on-disk job persistence (`JobRegistry` is in-memory; survives only
  the process lifetime).
- mDNS uses `hostname` shell-out for the local hostname; falls back to
  `localhost` if that fails. No IPv6-only configurations tested.
