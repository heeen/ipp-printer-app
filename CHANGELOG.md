# Changelog

## 0.3.0 — 2026-06-18

DNS-SD discovery now plays nicely with a co-resident `cups-browsed`.

- **Advertise `UUID=` in the `_ipp._tcp` TXT record** (was missing). The bare
  uuid (the `urn:uuid:` prefix stripped) is taken from each
  `PrinterRecord::uuid`. This is the key `cups-browsed` uses to dedupe a
  discovered *local* service against an existing CUPS queue's `printer-uuid`
  and stand down — the same mechanism CUPS's own shared queues rely on. It's
  also expected by Bonjour-for-IPP / PWG 5100.14 conformance.
- **New `ServerOptions.advertise_mdns: bool`** (breaking struct change). When
  `false`, `Server::run` does not start the advertiser itself, letting the
  caller assign each `PrinterRecord::uuid` (e.g. from the matching CUPS
  queue's `printer-uuid`) and then advertise via the already-public
  `mdns::Advertiser::register_all`, so the advertised `UUID=` matches a local
  queue. Set `true` for the previous always-advertise-at-startup behaviour.

### Migration

Add `advertise_mdns: true` to existing `ServerOptions { … }` literals to keep
0.2.x behaviour.

## 0.2.1 — 2026-06-18

Bug fix: emit `pwg-raster-document-resolution-supported` as `1setOf
resolution` (per PWG 5102.4 §6.2.1) instead of keyword. 0.2.0
shipped it as keyword because CUPS 2.4.10 (Debian trixie) looks
the attribute up via `IPP_TAG_KEYWORD`; CUPS 2.4.16+ (Ubuntu 26.04+)
correctly uses `IPP_TAG_RESOLUTION` per the spec and rejects the
keyword form, refusing to generate a PPD via `lpadmin -m everywhere`.

The spec-correct form wins. Users on CUPS 2.4.10 lose `lpadmin -m
everywhere` until they upgrade past the 2.4.x patch series that
fixed the lookup tag.

## 0.2.0 — 2026-06-16

Round out IPP Everywhere attribute coverage so CUPS' `lpadmin -m
everywhere` PPD generator can construct a queue. 0.1.0 omitted the
attributes CUPS uses to identify an IPP-Everywhere-capable printer
(`ipp-features-supported`, `pdl-override-supported`,
`printer-device-id`) and the descriptors its PPD template needs
(`media-source-supported`, `media-type-supported`,
`output-bin-supported`/`-default`, `print-content-optimize-supported`/
`-default`, `finishings-supported`/`-default`,
`job-creation-attributes-supported`). All are now emitted by
`Get-Printer-Attributes` and `Validate-Job`.

### Behaviour change

- `Get-Printer-Attributes` responses are larger (~700 extra bytes per
  printer). No existing fields changed shape, but downstream golden
  fixtures will need refreshing if they pin the full response.

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
