//! Build `Get-Printer-Attributes` / `Validate-Job` IPP responses.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use ipp::attribute::{IppAttribute, IppAttributes};
use ipp::model::DelimiterTag;
use ipp::prelude::*;
use ipp::request::IppRequestResponse;
use ipp::value::IppValue;

use crate::printer::{IppPrinterState, PrinterRecord};

fn kw(s: &str) -> IppValue {
    IppValue::Keyword(s.try_into().expect("keyword"))
}

fn mime(s: &str) -> IppValue {
    IppValue::MimeMediaType(s.try_into().expect("mime"))
}

fn uri(s: &str) -> IppValue {
    IppValue::Uri(s.try_into().expect("uri"))
}

fn charset(s: &str) -> IppValue {
    IppValue::Charset(s.try_into().expect("charset"))
}

fn lang(s: &str) -> IppValue {
    IppValue::NaturalLanguage(s.try_into().expect("language"))
}

fn attr(name: &str, value: IppValue) -> IppAttribute {
    IppAttribute::new(name.try_into().expect("attr name"), value)
}

fn add(attrs: &mut IppAttributes, tag: DelimiterTag, name: &str, value: IppValue) {
    attrs.add(tag, attr(name, value));
}

fn add_array_keyword(attrs: &mut IppAttributes, tag: DelimiterTag, name: &str, items: &[&str]) {
    let values: Vec<IppValue> = items.iter().map(|s| kw(s)).collect();
    add(attrs, tag, name, IppValue::Array(values));
}

fn text(s: &str) -> IppValue {
    IppValue::TextWithoutLanguage(s.try_into().expect("text"))
}

fn add_array_enum(attrs: &mut IppAttributes, tag: DelimiterTag, name: &str, codes: &[i32]) {
    let values: Vec<IppValue> = codes.iter().map(|c| IppValue::Enum(*c)).collect();
    add(attrs, tag, name, IppValue::Array(values));
}

/// Break a Unix timestamp into a civil UTC `dateTime` value (Hinnant's
/// `civil_from_days`). IPP `dateTime` (RFC 2579 / RFC 8011 §5.1.15).
fn datetime_utc(unix_secs: i64) -> IppValue {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (hour, minutes, seconds) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    IppValue::DateTime {
        year: year as u16,
        month: month as u8,
        day: day as u8,
        hour: hour as u8,
        minutes: minutes as u8,
        seconds: seconds as u8,
        deci_seconds: 0,
        utc_dir: '+',
        utc_hours: 0,
        utc_mins: 0,
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Advertise localhost when the server bound to an unspecified address.
fn advertise_host(host: &str) -> &str {
    if host == "0.0.0.0" || host == "::" || host.is_empty() {
        "localhost"
    } else {
        host
    }
}

/// Build the advertised printer URI for a record.
fn printer_uri(record: &PrinterRecord, host: &str, port: u16) -> String {
    format!(
        "ipp://{}:{}/ipp/print/{}",
        advertise_host(host),
        port,
        record.config.name
    )
}

/// Build a successful Get-Printer-Attributes response.
///
/// `requested` is the client's `requested-attributes` set. `None` (or a set
/// containing the magic value `all`) returns the full attribute group; a
/// concrete set filters the response down to the named attributes (RFC 8011
/// §4.2.5).
pub fn get_printer_attributes(
    version: IppVersion,
    request_id: u32,
    record: &PrinterRecord,
    host: &str,
    port: u16,
    requested: Option<&BTreeSet<String>>,
) -> Result<IppRequestResponse, ipp::parser::IppParseError> {
    let mut resp =
        IppRequestResponse::new_response(version, StatusCode::SuccessfulOk, request_id)?;
    let attrs = resp.attributes_mut();
    let cfg = &record.config;
    let printer_uri_str = printer_uri(record, host, port);
    let more_info = format!(
        "http://{}:{}/",
        advertise_host(host),
        port
    );

    let p = DelimiterTag::PrinterAttributes;
    add(attrs, p, "printer-uri-supported", uri(&printer_uri_str));
    add(attrs, p, "uri-authentication-supported", kw("none"));
    add(attrs, p, "uri-security-supported", kw("none"));
    add(
        attrs,
        p,
        "printer-name",
        IppValue::NameWithoutLanguage(cfg.name.as_str().try_into().unwrap()),
    );
    add(
        attrs,
        p,
        "printer-location",
        IppValue::TextWithoutLanguage("".try_into().unwrap()),
    );
    add(
        attrs,
        p,
        "printer-info",
        IppValue::TextWithoutLanguage(cfg.make_and_model.as_str().try_into().unwrap()),
    );
    add(
        attrs,
        p,
        "printer-make-and-model",
        IppValue::TextWithoutLanguage(cfg.make_and_model.as_str().try_into().unwrap()),
    );
    add(attrs, p, "printer-more-info", uri(&more_info));
    add(
        attrs,
        p,
        "printer-uuid",
        uri(&format!("urn:uuid:{}", record.uuid)),
    );
    // RFC 8011 §5.4.29: seconds since the printer started; must be > 0 (the
    // uptime clock starts lazily, so floor it at 1 for requests in the first
    // second).
    add(
        attrs,
        p,
        "printer-up-time",
        IppValue::Integer(uptime_secs().max(1) as i32),
    );

    add(attrs, p, "printer-state", IppValue::Enum(record.state as i32));
    let reason_kws: Vec<&str> = record.reasons.ipp_keywords();
    add_array_keyword(attrs, p, "printer-state-reasons", &reason_kws);
    add(attrs, p, "printer-is-accepting-jobs", IppValue::Boolean(true));
    add(attrs, p, "queued-job-count", IppValue::Integer(0));

    add_array_keyword(attrs, p, "ipp-versions-supported", &["1.1", "2.0", "2.1"]);
    add_array_keyword(attrs, p, "ipp-features-supported", &["ipp-everywhere"]);
    add(attrs, p, "pdl-override-supported", kw("attempted"));
    if !cfg.device_id.is_empty() {
        add(
            attrs,
            p,
            "printer-device-id",
            IppValue::TextWithoutLanguage(cfg.device_id.as_str().try_into().unwrap()),
        );
    }
    // RFC 8011 §5.4.15: `1setOf enum` carrying the operation *codes*, not
    // keyword names. Order follows the numeric code.
    add_array_enum(
        attrs,
        p,
        "operations-supported",
        &[
            0x0002, // Print-Job
            0x0004, // Validate-Job
            0x0005, // Create-Job
            0x0006, // Send-Document
            0x0008, // Cancel-Job
            0x0009, // Get-Job-Attributes
            0x000a, // Get-Jobs
            0x000b, // Get-Printer-Attributes
            0x0039, // Cancel-My-Jobs
            0x003b, // Close-Job
            0x003c, // Identify-Printer
        ],
    );
    add(
        attrs,
        p,
        "charset-configured",
        charset("utf-8"),
    );
    add(
        attrs,
        p,
        "charset-supported",
        IppValue::Array(vec![charset("utf-8")]),
    );
    add(attrs, p, "natural-language-configured", lang("en"));
    add(
        attrs,
        p,
        "natural-language-supported",
        IppValue::Array(vec![lang("en")]),
    );
    add(
        attrs,
        p,
        "generated-natural-language-supported",
        IppValue::Array(vec![lang("en")]),
    );
    add_array_keyword(attrs, p, "compression-supported", &["none"]);

    // PWG raster is the IPP Everywhere required format; the unified CUPS reader
    // also handles legacy CUPS raster v1/v2 if a client picks that path.
    add(
        attrs,
        p,
        "document-format-supported",
        IppValue::Array(vec![
            mime("image/pwg-raster"),
            mime("application/vnd.cups-raster"),
            mime("application/octet-stream"),
        ]),
    );
    add(
        attrs,
        p,
        "document-format-default",
        mime("image/pwg-raster"),
    );
    // PWG raster type for the everywhere driver.
    add_array_keyword(
        attrs,
        p,
        "pwg-raster-document-type-supported",
        &["black_1"],
    );
    // PWG 5102.4 §6.2.1: `1setOf resolution`. CUPS 2.4.16+ (Ubuntu 26.04)
    // requires this typing; 2.4.10 (Debian trixie) regrettably has a bug
    // and looks it up as IPP_TAG_KEYWORD instead — the spec form wins.
    add(
        attrs,
        p,
        "pwg-raster-document-resolution-supported",
        IppValue::Array(vec![IppValue::Resolution {
            cross_feed: cfg.dpi,
            feed: cfg.dpi,
            units: 3,
        }]),
    );
    add_array_keyword(
        attrs,
        p,
        "urf-supported",
        &["W8", "SRGB24", "CP1", "RS203"],
    );

    add(attrs, p, "color-supported", IppValue::Boolean(false));
    add_array_keyword(attrs, p, "print-color-mode-supported", &["monochrome"]);
    add(attrs, p, "print-color-mode-default", kw("monochrome"));
    add_array_keyword(attrs, p, "sides-supported", &["one-sided"]);
    add(attrs, p, "sides-default", kw("one-sided"));
    add(attrs, p, "orientation-requested-default", IppValue::Enum(3));
    // portrait / landscape / reverse-landscape / reverse-portrait (RFC 8011).
    add_array_enum(attrs, p, "orientation-requested-supported", &[3, 4, 5, 6]);

    // Identify-Printer actions (PWG 5100.14 §5.1). The framework dispatches
    // the operation to `DeviceBackend::identify`; we advertise a display-type
    // action which any backend can honour (a beep/LED maps to `sound`/`flash`).
    add_array_keyword(attrs, p, "identify-actions-supported", &["display", "sound"]);
    add_array_keyword(attrs, p, "identify-actions-default", &["display"]);

    // IPP Everywhere required descriptors. We expose conservative defaults that
    // satisfy CUPS' `-m everywhere` PPD generator without claiming features
    // we don't implement (no real trays, no finishings).
    add_array_keyword(attrs, p, "media-source-supported", &["main"]);
    add_array_keyword(attrs, p, "media-type-supported", &["labels", "stationery"]);
    add_array_keyword(attrs, p, "output-bin-supported", &["face-up"]);
    add(attrs, p, "output-bin-default", kw("face-up"));
    add_array_keyword(
        attrs,
        p,
        "print-content-optimize-supported",
        &["auto", "graphic", "photo", "text", "text-and-graphic"],
    );
    add(attrs, p, "print-content-optimize-default", kw("auto"));
    // RFC 8011 §5.2.6: `1setOf enum`. `3` == `none`.
    add_array_enum(attrs, p, "finishings-supported", &[3]);
    add(attrs, p, "finishings-default", IppValue::Enum(3));
    add(attrs, p, "job-creation-attributes-supported", IppValue::Array(vec![
        kw("copies"),
        kw("media"),
        kw("media-col"),
        kw("orientation-requested"),
        kw("print-color-mode"),
        kw("print-content-optimize"),
        kw("print-quality"),
        kw("printer-resolution"),
        kw("sides"),
    ]));

    add(
        attrs,
        p,
        "printer-resolution-default",
        IppValue::Resolution {
            cross_feed: cfg.dpi,
            feed: cfg.dpi,
            units: 3,
        },
    );
    add(
        attrs,
        p,
        "printer-resolution-supported",
        IppValue::Array(vec![IppValue::Resolution {
            cross_feed: cfg.dpi,
            feed: cfg.dpi,
            units: 3,
        }]),
    );

    let media_kws: Vec<&str> = cfg.media_names.iter().map(|s| s.as_str()).collect();
    if !media_kws.is_empty() {
        add(attrs, p, "media-default", kw(media_kws[0]));
        add_array_keyword(attrs, p, "media-supported", &media_kws);

        // media-col-{default} — required by IPP Everywhere.
        let default_size = cfg.media_sizes.first().copied().unwrap_or([4000, 3000]);
        add(
            attrs,
            p,
            "media-col-default",
            media_col(media_kws[0], default_size),
        );
        let media_cols: Vec<IppValue> = media_kws
            .iter()
            .zip(cfg.media_sizes.iter().copied().chain(std::iter::repeat(default_size)))
            .map(|(name, size)| media_col(name, size))
            .collect();
        // PWG 5100.13: `media-col-supported` is `1setOf keyword` naming the
        // member attributes a client may set in a `media-col` collection — NOT
        // the collections themselves (that's `media-col-database`, which CUPS'
        // `lpadmin -m everywhere` PPD generator walks to enumerate sizes).
        add_array_keyword(
            attrs,
            p,
            "media-col-supported",
            &[
                "media-size",
                "media-size-name",
                "media-top-margin",
                "media-bottom-margin",
                "media-left-margin",
                "media-right-margin",
                "media-source",
                "media-type",
            ],
        );
        add(attrs, p, "media-col-database", IppValue::Array(media_cols.clone()));

        // `media-size-supported` (PWG 5100.12 §6.3.x): `1setOf collection` of
        // bare `media-size` (x/y only), distinct from `media-col-database`.
        let media_sizes: Vec<IppValue> = media_kws
            .iter()
            .zip(cfg.media_sizes.iter().copied().chain(std::iter::repeat(default_size)))
            .map(|(_, size)| media_size_col(size))
            .collect();
        add(attrs, p, "media-size-supported", IppValue::Array(media_sizes));

        // media-ready / media-col-ready — the loaded media. The device backend
        // can overwrite these per-poll with live roll data; absent that we fall
        // back to the configured default so the (required) attributes exist.
        add(attrs, p, "media-ready", kw(media_kws[0]));
        add(
            attrs,
            p,
            "media-col-ready",
            IppValue::Array(vec![media_col(media_kws[0], default_size)]),
        );
    }

    // Hard-margin support. A thermal label printer prints edge-to-edge: 0 on
    // all sides (hundredths of a millimetre). Required by IPP Everywhere.
    for margin in [
        "media-top-margin-supported",
        "media-bottom-margin-supported",
        "media-left-margin-supported",
        "media-right-margin-supported",
    ] {
        add(attrs, p, margin, IppValue::Integer(0));
    }

    add(
        attrs,
        p,
        "copies-supported",
        IppValue::RangeOfInteger { min: 1, max: 999 },
    );
    add(attrs, p, "copies-default", IppValue::Integer(1));
    add(
        attrs,
        p,
        "print-quality-supported",
        IppValue::Array(vec![
            IppValue::Enum(3),
            IppValue::Enum(4),
            IppValue::Enum(5),
        ]),
    );
    add(attrs, p, "print-quality-default", IppValue::Enum(4));

    // --- Job/limit descriptors (PWG 5100.14 §5.x, mostly static) ---
    add(attrs, p, "multiple-document-jobs-supported", IppValue::Boolean(false));
    add(attrs, p, "multiple-operation-time-out", IppValue::Integer(60));
    add(attrs, p, "multiple-operation-time-out-action", kw("process-job"));
    add(attrs, p, "job-ids-supported", IppValue::Boolean(true));
    add(attrs, p, "preferred-attributes-supported", IppValue::Boolean(false));
    add_array_keyword(attrs, p, "overrides-supported", &["document-number", "pages"]);
    add_array_keyword(attrs, p, "printer-get-attributes-supported", &["document-format"]);
    add_array_keyword(
        attrs,
        p,
        "which-jobs-supported",
        &[
            "completed",
            "not-completed",
            "aborted",
            "canceled",
            "pending",
            "processing",
        ],
    );

    // --- Rendering descriptors ---
    add(attrs, p, "print-rendering-intent-default", kw("auto"));
    add_array_keyword(attrs, p, "print-rendering-intent-supported", &["auto"]);
    // One-sided printer: the back side is rendered the same way as the front.
    add(attrs, p, "pwg-raster-document-sheet-back", kw("normal"));

    // --- Identity / admin descriptors ---
    // Location is not known to the framework; out-of-band `unknown` is the
    // honest value (a real `geo:` URI would be fabricated coordinates).
    add(attrs, p, "printer-geo-location", IppValue::Other { tag: 0x12, data: Vec::<u8>::new().into() });
    add(attrs, p, "printer-organization", text(""));
    add(attrs, p, "printer-organizational-unit", text(""));
    add(
        attrs,
        p,
        "printer-icons",
        IppValue::Array(vec![uri(&format!(
            "http://{}:{}/icon.png",
            advertise_host(host),
            port
        ))]),
    );
    add(attrs, p, "pages-per-minute", IppValue::Integer(20));

    // --- Supply / consumable (PWG 5100.14). The device backend can overwrite
    // these per-poll with the real labels-remaining gauge; the static fallback
    // keeps the required attributes present. ---
    add(
        attrs,
        p,
        "printer-supply",
        IppValue::Array(vec![IppValue::OctetString(
            "index=1;class=supplyThatIsConsumed;type=stoppingMaterial;\
             unit=percent;maxcapacity=100;level=100;colorantname=unknown;"
                .try_into()
                .expect("supply"),
        )]),
    );
    add(
        attrs,
        p,
        "printer-supply-description",
        IppValue::Array(vec![text("Label Stock")]),
    );
    add(
        attrs,
        p,
        "printer-supply-info-uri",
        uri(&format!("http://{}:{}/", advertise_host(host), port)),
    );

    // --- Change tracking (RFC 8011 §5.4.26-29) ---
    let now = now_unix();
    add(attrs, p, "printer-config-change-time", IppValue::Integer(uptime_secs() as i32));
    add(attrs, p, "printer-config-change-date-time", datetime_utc(now));
    add(attrs, p, "printer-state-change-time", IppValue::Integer(uptime_secs() as i32));
    add(attrs, p, "printer-state-change-date-time", datetime_utc(now));

    filter_requested(&mut resp, requested);
    Ok(resp)
}

/// Apply `requested-attributes` filtering to a freshly-built
/// Get-Printer-Attributes response. `None` or a set containing the magic
/// value `all` is a no-op (return everything). Otherwise the printer-attribute
/// group is reduced to the explicitly-named attributes (RFC 8011 §4.2.5). The
/// always-present operation attributes (charset / language) are preserved.
fn filter_requested(resp: &mut IppRequestResponse, requested: Option<&BTreeSet<String>>) {
    let Some(set) = requested else { return };
    if set.is_empty() || set.contains("all") {
        return;
    }
    for group in resp.attributes_mut().groups_mut() {
        if group.tag() != DelimiterTag::PrinterAttributes {
            continue;
        }
        group
            .attributes_mut()
            .retain(|name, _| set.contains(name.as_str()));
    }
}

/// Validate-Job: same capability surface as Get-Printer-Attributes (success).
pub fn validate_job(
    version: IppVersion,
    request_id: u32,
    record: &PrinterRecord,
    host: &str,
    port: u16,
) -> Result<IppRequestResponse, ipp::parser::IppParseError> {
    get_printer_attributes(version, request_id, record, host, port, None)
}

/// Build the `Print-Job` accepted response for a freshly-allocated job.
pub fn print_job_accepted(
    version: IppVersion,
    request_id: u32,
    job: &crate::job::JobRecord,
    printer_uri_str: &str,
) -> Result<IppRequestResponse, ipp::parser::IppParseError> {
    let mut resp =
        IppRequestResponse::new_response(version, StatusCode::SuccessfulOk, request_id)?;
    let job_uri_str = format!("{printer_uri_str}/job/{}", job.id);
    let j = DelimiterTag::JobAttributes;
    add(resp.attributes_mut(), j, "job-uri", uri(&job_uri_str));
    add(
        resp.attributes_mut(),
        j,
        "job-id",
        IppValue::Integer(job.id as i32),
    );
    add(
        resp.attributes_mut(),
        j,
        "job-state",
        IppValue::Enum(job.state as i32),
    );
    add_array_keyword(
        resp.attributes_mut(),
        j,
        "job-state-reasons",
        &job_state_reason_keywords(job),
    );
    Ok(resp)
}

/// Build a `Get-Job-Attributes` response for a single job. `requested` filters
/// the returned attributes (`None` = all, the Get-Job-Attributes default).
pub fn build_job_attrs_response(
    version: IppVersion,
    request_id: u32,
    job: &crate::job::JobRecord,
    printer_uri_str: &str,
    requested: Option<&BTreeSet<String>>,
) -> Result<IppRequestResponse, ipp::parser::IppParseError> {
    let mut resp =
        IppRequestResponse::new_response(version, StatusCode::SuccessfulOk, request_id)?;
    for a in job_attrs_for_group(job, printer_uri_str, requested) {
        resp.attributes_mut().add(DelimiterTag::JobAttributes, a);
    }
    Ok(resp)
}

/// Build a `Get-Jobs` response listing one job per group. `requested` filters
/// the per-job attributes; the Get-Jobs default (`None`) is `job-uri` +
/// `job-id` only (RFC 8011 §3.2.6.1), supplied by the caller.
pub fn build_get_jobs_response(
    version: IppVersion,
    request_id: u32,
    jobs: &[crate::job::JobRecord],
    printer_uri_str: &str,
    requested: Option<&BTreeSet<String>>,
) -> Result<IppRequestResponse, ipp::parser::IppParseError> {
    let mut resp =
        IppRequestResponse::new_response(version, StatusCode::SuccessfulOk, request_id)?;
    // Each job goes in its own JobAttributes group. The `ipp` crate's `add`
    // merges all attrs with the same DelimiterTag into one group, which is
    // wrong for multi-job responses — we push raw groups instead.
    for job in jobs {
        let mut group = ipp::attribute::IppAttributeGroup::new(DelimiterTag::JobAttributes);
        for a in job_attrs_for_group(job, printer_uri_str, requested) {
            group
                .attributes_mut()
                .insert(a.name().to_owned(), a);
        }
        resp.attributes_mut().groups_mut().push(group);
    }
    Ok(resp)
}

fn job_attrs_for_group(
    job: &crate::job::JobRecord,
    printer_uri_str: &str,
    requested: Option<&BTreeSet<String>>,
) -> Vec<IppAttribute> {
    let job_uri_str = format!("{printer_uri_str}/job/{}", job.id);
    let mut out = vec![
        attr("job-uri", uri(&job_uri_str)),
        attr("job-id", IppValue::Integer(job.id as i32)),
        attr("job-printer-uri", uri(printer_uri_str)),
        attr(
            "job-name",
            IppValue::NameWithoutLanguage(
                format!("job-{}", job.id).as_str().try_into().unwrap(),
            ),
        ),
        attr("job-state", IppValue::Enum(job.state as i32)),
        attr(
            "job-originating-user-name",
            IppValue::NameWithoutLanguage(job.owner.as_str().try_into().unwrap_or_else(|_| {
                "anonymous".try_into().expect("anonymous")
            })),
        ),
        attr("time-at-creation", IppValue::Integer(job.created_secs())),
    ];
    let reason_kws = job_state_reason_keywords(job);
    out.push(attr(
        "job-state-reasons",
        IppValue::Array(reason_kws.iter().map(|s| kw(s)).collect()),
    ));
    if !job.message.is_empty() {
        out.push(attr(
            "job-state-message",
            IppValue::TextWithoutLanguage(job.message.as_str().try_into().unwrap()),
        ));
    }
    // We don't separately track when processing began; the mock pipeline
    // starts work as soon as the job is accepted, so creation time is a faithful
    // stand-in. Required by RFC 8011 (no-value|integer).
    out.push(attr("time-at-processing", IppValue::Integer(job.created_secs())));
    out.push(attr("job-printer-up-time", IppValue::Integer(uptime_secs() as i32)));
    if let Some(s) = job.completed_secs() {
        out.push(attr("time-at-completed", IppValue::Integer(s)));
    }
    if let Some(set) = requested {
        out.retain(|a| set.contains(a.name().as_str()));
    }
    out
}

fn job_state_reason_keywords(job: &crate::job::JobRecord) -> Vec<&'static str> {
    use crate::flags::PrinterReason;
    use crate::job::JobState;
    let mut out = Vec::new();
    if job.reasons.contains(PrinterReason::MEDIA_EMPTY) {
        out.push("job-completed-with-errors");
    }
    if job.reasons.contains(PrinterReason::MEDIA_JAM) {
        out.push("aborted-by-system");
    }
    if job.reasons.contains(PrinterReason::OFFLINE) {
        out.push("connection-error");
    }
    match job.state {
        JobState::Canceled => out.push("job-canceled-by-user"),
        JobState::Completed => out.push("job-completed-successfully"),
        JobState::Aborted if out.is_empty() => out.push("aborted-by-system"),
        _ => {}
    }
    if out.is_empty() {
        out.push("none");
    }
    out
}

/// Transition the printer into `IppPrinterState::Processing`.
pub fn set_printer_processing(record: &mut PrinterRecord) {
    record.state = IppPrinterState::Processing;
}

/// Transition the printer back to `IppPrinterState::Idle`.
pub fn set_printer_idle(record: &mut PrinterRecord) {
    record.state = IppPrinterState::Idle;
}

/// Build a `media-col` collection with `media-size` (x/y in hundredths of mm)
/// and `media-size-name`. CUPS expects PWG dimensions in hundredths of mm.
fn media_col(name: &str, size_hmm: [i32; 2]) -> IppValue {
    use std::collections::BTreeMap;
    let mut size = BTreeMap::new();
    size.insert(
        "x-dimension".try_into().unwrap(),
        IppValue::Integer(size_hmm[0]),
    );
    size.insert(
        "y-dimension".try_into().unwrap(),
        IppValue::Integer(size_hmm[1]),
    );
    let mut col = BTreeMap::new();
    col.insert(
        "media-size".try_into().unwrap(),
        IppValue::Collection(size),
    );
    col.insert(
        "media-size-name".try_into().unwrap(),
        kw(name),
    );
    IppValue::Collection(col)
}

/// Build a bare `media-size` collection (x/y dimensions only) for
/// `media-size-supported`.
fn media_size_col(size_hmm: [i32; 2]) -> IppValue {
    use std::collections::BTreeMap;
    let mut size = BTreeMap::new();
    size.insert(
        "x-dimension".try_into().unwrap(),
        IppValue::Integer(size_hmm[0]),
    );
    size.insert(
        "y-dimension".try_into().unwrap(),
        IppValue::Integer(size_hmm[1]),
    );
    IppValue::Collection(size)
}

fn uptime_secs() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs()
}
