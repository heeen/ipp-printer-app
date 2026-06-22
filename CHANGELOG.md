# Changelog

## 0.6.1 — 2026-06-22

Restrict mDNS advertising to real interfaces. `enable_addr_auto()` published the
service on *every* host interface, including Docker `veth*`/`br-*` links. A
co-resident `cups-browsed` then resolved us over those links, and the racy /
duplicate A-record answers made avahi hand it a *null* host name for some
resolves — which fails its `is_local_hostname()` check, bypasses the `UUID=`
dedup, and makes it build a spurious `implicitclass://` duplicate queue. The
advertiser now enumerates interfaces and skips loopback, link-local, down, and
container/VM virtual bridges (`veth`, `docker`, `br-`, `virbr`, `vnet`, `vmnet`,
`vboxnet`), advertising an explicit address list instead. Falls back to the old
auto-detection if enumeration fails or filters everything out. No API change.

## 0.6.0 — 2026-06-20

Surface the request's document format to the print callback and make the
advertised formats configurable — the groundwork for a consumer to accept
`image/jpeg` (IPP Everywhere's last required format) by decoding it in its own
backend.

### Breaking changes

- **`JobContext` gains `document_format: String`** — the `document-format` the
  client sent (default `application/octet-stream`). The print callback branches
  on it to pick a decoder. Only the framework constructs `JobContext`, so
  consumers that merely read it are unaffected.
- **`PrinterConfig` gains `document_formats: Vec<String>`** (`#[serde(default)]`,
  so old persisted state loads). When non-empty it drives
  `document-format-supported`; empty keeps the raster defaults
  (`image/pwg-raster`, `application/vnd.cups-raster`, `application/octet-stream`).
  `document-format-default` stays `image/pwg-raster`.

## 0.5.0 — 2026-06-19

Device-fed dynamic media & supply (IPP Everywhere Tier 4). Backends can now
publish the *currently loaded* media and remaining-supply level each poll, so
`media-ready` / `media-col-ready` / `printer-supply` reflect the real device
instead of the static config fallback.

### Breaking changes

- **`DeviceBackend::poll_status` now returns `Option<PollStatus>`** instead of
  `Option<PrinterReason>`. `PollStatus { reasons, ready_media, supply_percent }`
  carries the reasons plus the optional dynamic fields. Migrate a reasons-only
  backend with `PollStatus::from_reasons(reasons)`.

### New

- **`PollStatus`** and **`ReadyMedia { name, size_hmm, media_type }`** exported
  from the crate root.
- **`PrinterRecord` gains `ready_media: Option<ReadyMedia>` and
  `supply_percent: Option<u8>`**, written by the status loop (last-known value
  is carried forward when a poll omits them).
- `media-ready` / `media-col-ready` use the live `ready_media` when present;
  `printer-supply` reports the live `supply_percent` (0–100), else full.

## 0.4.0 — 2026-06-19

IPP Everywhere conformance pass. Validated against the canonical
`ipptool ipp-everywhere.test` suite (which pulls in `ipp-1.1.test` and
`ipp-2.0.test`): the RFC 8011 / PWG 5100.12 / PWG 5100.14 sections now pass
end to end. The only remaining required item is `image/jpeg` decode (see
below), which is a real capability gap rather than an encoding bug.

### Conformance bug fixes (encoding / behaviour)

- **`operations-supported` is now `1setOf enum`** carrying operation *codes*
  (`0x0002`, `0x0004`, …) instead of keyword names. Strict clients and
  conformance tools require the enum form.
- **`finishings-supported` is now `1setOf enum`** (`3` = none), was a keyword.
- **`media-col-supported` is now `1setOf keyword`** naming the settable
  `media-col` member attributes (`media-size`, `media-*-margin`, …), not the
  collections themselves. Real size collections remain in `media-col-database`.
- **`requested-attributes` is honoured** for Get-Printer-Attributes, Get-Jobs,
  and Get-Job-Attributes (incl. the `all` magic value and the Get-Jobs default
  of `job-uri` + `job-id`).
- **Request validation (RFC 8011 §4.1.1 / §4.1.4 / §4.1.8 / §4.2)**: reject
  `request-id` 0, a missing / misordered `attributes-charset` +
  `attributes-natural-language` pair, unsupported IPP versions, and requests
  lacking a `printer-uri` / `job-uri` — each with the correct IPP status.
- **`printer-up-time` is always > 0** (floored at 1 for first-second requests).

### New required operations (PWG 5100.14 §5.1)

- **Identify-Printer** (`0x3c`) + `identify-actions-{supported,default}`,
  dispatched to the new `DeviceBackend::identify` hook (default no-op).
- **Create-Job** (`0x05`) + **Send-Document** (`0x06`) multi-document flow
  (Send-Document requires `last-document`), **Close-Job** (`0x3b`),
  **Cancel-My-Jobs** (`0x39`). Jobs now record their `requesting-user-name`
  owner so `Get-Jobs my-jobs=true` scopes correctly.

### New required descriptor / job attributes

- Media geometry: `media-size-supported`,
  `media-{top,bottom,left,right}-margin-supported`, `media-ready` /
  `media-col-ready` (static fallback from config; a backend can override
  per-poll).
- Job/limits: `multiple-document-jobs-supported`, `multiple-operation-time-out`
  (+`-action`), `which-jobs-supported`, `job-ids-supported`,
  `preferred-attributes-supported`, `overrides-supported`,
  `printer-get-attributes-supported`, `orientation-requested-supported`.
- Rendering: `print-rendering-intent-{default,supported}`,
  `pwg-raster-document-sheet-back`.
- Identity/admin: `printer-geo-location` (out-of-band `unknown`),
  `printer-organization`, `printer-organizational-unit`, `printer-icons`
  (served from a new `GET /icon.png` route), `pages-per-minute`,
  `printer-supply` / `-description` / `-info-uri` (static fallback),
  `printer-{config,state}-change-{date-time,time}`.
- Per-job: `job-originating-user-name`, `time-at-processing`,
  `job-printer-up-time`.

### Known gap

- `document-format-supported` does not yet include `image/jpeg`; IPP Everywhere
  requires JPEG decode, which is not implemented. PWG/CUPS raster only.

### Breaking changes

- `DeviceBackend` gains an `identify` method (defaulted, so existing impls
  compile unchanged).
- `JobRegistry::create` now takes an `owner: String`.
- `build_get_jobs_response` / `build_job_attrs_response` /
  `get_printer_attributes` take an extra `requested` filter argument.

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
