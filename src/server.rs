//! Axum HTTP server: IPP over POST `/ipp/print/:name`.

use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use ipp::model::Operation;
use ipp::parser::IppParser;
use ipp::model::StatusCode as IppStatus;
use ipp::prelude::*;
use ipp::reader::IppReader;
use num_traits::FromPrimitive;
use crate::attributes::{
    self, build_get_jobs_response, build_job_attrs_response, get_printer_attributes,
    print_job_accepted, validate_job,
};
use crate::device::DeviceBackend;
use crate::job::{JobId, JobRegistry, JobState};
use crate::printer::{PrinterRecord, PrinterRegistry};
use crate::raster::JobOutcome;
use crate::state::PersistedState;

/// Context passed to a print-job worker so it can observe cancellation and
/// report progress without re-querying the registry.
#[derive(Clone)]
#[allow(missing_docs)]
pub struct JobContext {
    pub id: JobId,
    pub printer_name: String,
    pub cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The `document-format` the client sent (RFC 8011), e.g.
    /// `image/pwg-raster` or `image/jpeg`. Defaults to
    /// `application/octet-stream` when the client omits it. The print
    /// callback branches on this to pick a decoder.
    pub document_format: String,
}

impl JobContext {
    /// True once the client has canceled this job. A print callback that loops
    /// (e.g. waiting on hardware) should poll this and bail promptly.
    pub fn is_canceled(&self) -> bool {
        self.cancel_flag.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Callback that prints one document to the device, returning a [`JobOutcome`]
/// that tells the framework what to do with the job.
///
/// It receives the payload by reference because the framework may call it more
/// than once: returning [`JobOutcome::DeviceUnavailable`] makes the framework
/// hold the job and re-invoke this callback (with backoff) until it prints or
/// the client cancels — so classify a transient device condition as
/// `DeviceUnavailable`, not `Failed`. Do any cheap reachability check (opening
/// the device) *first* so a held retry is cheap. The callback should also bail
/// early if [`JobContext::is_canceled`] becomes true.
/// Boxed, owned future a [`PrintJobFn`] returns. The payload is handed over as
/// an `Arc<[u8]>` (not a borrow) so the future is `'static` and can run on a
/// spawned task that outlives the call.
pub type PrintJobFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = crate::raster::JobOutcome> + Send>>;

/// The print-job callback: given a job context, the document payload, and a
/// copy count, returns a future resolving to the [`crate::raster::JobOutcome`].
/// The framework drives spooling/retry around it.
pub type PrintJobFn = Arc<dyn Fn(JobContext, Arc<[u8]>, u32) -> PrintJobFuture + Send + Sync>;

/// Server configuration. Construct in your `main`, hand to [`Server::run`].
#[allow(missing_docs)]
pub struct ServerOptions {
    pub host: String,
    pub port: u16,
    pub printers: PrinterRegistry,
    pub device_backend: Arc<dyn DeviceBackend>,
    pub print_job: PrintJobFn,
    pub state_path: std::path::PathBuf,
    /// When `true` (and the `mdns` feature is on), [`Server::run`] starts the
    /// DNS-SD advertiser itself, immediately, from the registry as it stands
    /// at bind time. Set `false` if the caller needs to advertise later —
    /// e.g. after assigning each [`crate::printer::PrinterRecord::uuid`] from
    /// an external source (a CUPS queue's `printer-uuid`) so the advertised
    /// `UUID=` matches a local queue and cups-browsed dedupes it. The caller
    /// is then responsible for calling [`crate::mdns::Advertiser::register_all`]
    /// and holding the handle.
    pub advertise_mdns: bool,
}

/// Axum-shared state. Constructed internally by [`Server::router`]; exposed
/// only so external middleware can read the printer registry.
#[derive(Clone)]
#[allow(missing_docs)]
pub struct AppState {
    pub host: String,
    pub port: u16,
    pub printers: PrinterRegistry,
    pub print_job: PrintJobFn,
    pub state_path: std::path::PathBuf,
    pub jobs: JobRegistry,
    pub device_backend: Arc<dyn DeviceBackend>,
}

/// Entry point — `Server::run(opts).await` starts the listener.
pub struct Server;

impl Server {
    /// Build the axum router with the configured state attached. Returned
    /// router can be served via [`Server::run`] or by hand.
    pub fn router(opts: ServerOptions) -> Router {
        let state = AppState {
            host: opts.host.clone(),
            port: opts.port,
            printers: opts.printers.clone(),
            print_job: opts.print_job,
            state_path: opts.state_path,
            jobs: JobRegistry::new(),
            device_backend: opts.device_backend,
        };

        Router::new()
            .route("/", get(index_handler))
            .route("/icon.png", get(icon_handler))
            .route("/ipp/print/{name}", post(ipp_handler))
            .route("/ipp/print/{name}/", post(ipp_handler))
            .with_state(state)
    }

    /// Bind to `host:port`, spawn the background status poller (and the mDNS
    /// advertiser if the `mdns` feature is enabled), and run the axum
    /// listener until it errors.
    pub async fn run(opts: ServerOptions) -> std::io::Result<()> {
        let addr = format!("{}:{}", opts.host, opts.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        log::info!("ipp-printer-app listening on http://{addr}");

        // mDNS advertising for IPP-Everywhere auto-discovery. Skipped when the
        // caller opts to advertise itself later (see ServerOptions::advertise_mdns).
        // Created before the poller so the poller can withdraw/republish each
        // printer's advert as its device goes offline / comes back online.
        #[cfg(feature = "mdns")]
        let advertiser: Option<Arc<dyn crate::status::AdvertiserControl>> = if opts.advertise_mdns {
            match crate::mdns::Advertiser::register_all(&opts.printers, opts.port) {
                Ok(adv) => Some(Arc::new(adv)),
                Err(e) => {
                    log::warn!("mdns: failed to register printers: {e}");
                    None
                }
            }
        } else {
            None
        };
        #[cfg(not(feature = "mdns"))]
        let advertiser: Option<Arc<dyn crate::status::AdvertiserControl>> = None;

        // Background status poller — refreshes printer-state-reasons and drives
        // the advertiser on offline/online transitions.
        let _status = crate::status::spawn(
            opts.device_backend.clone(),
            opts.printers.clone(),
            advertiser.clone(),
        );
        // Hold the advertiser for the server's lifetime (Drop withdraws all).
        let _advertiser = advertiser;

        axum::serve(listener, Self::router(opts)).await
    }

    /// Load printers from disk, discover devices, merge into registry.
    pub async fn bootstrap_printers(
        registry: &PrinterRegistry,
        backend: &dyn DeviceBackend,
        state_path: &std::path::Path,
        make_config: impl Fn(&str, &str, &str, &str, &str) -> Option<crate::printer::PrinterConfig>,
    ) {
        let mut records: Vec<PrinterRecord> = PersistedState::load(state_path)
            .printers
            .into_iter()
            .map(PrinterRecord::new)
            .collect();

        for d in backend.list().await {
            let Some(driver) = backend.driver_for_device(&d.device_id, &d.uri) else {
                continue;
            };
            let name = printer_name_from_uri(&d.uri, &d.info);
            if records.iter().any(|r| r.config.device_uri == d.uri) {
                continue;
            }
            let Some(cfg) = make_config(&name, &d.info, &driver, &d.uri, &d.device_id) else {
                continue;
            };
            log::info!("auto-add printer {name} -> {}", d.uri);
            records.push(PrinterRecord::new(cfg));
        }

        *registry.write() = records;
        Self::persist(registry, state_path);
    }

    /// Snapshot the registry to `state_path` as JSON. Called automatically
    /// at the end of every print job; expose for callers that want to
    /// persist after manual registry edits.
    pub fn persist(registry: &PrinterRegistry, state_path: &std::path::Path) {
        let configs: Vec<_> = registry
            .read()
            .iter()
            .map(|r| r.config.clone())
            .collect();
        let _ = PersistedState { printers: configs }.save(state_path);
    }
}

/// Logical queue name proposed during bootstrap. Lowercases and maps every
/// non-alphanumeric run to a single `_`, mirroring CUPS's own DNS-SD
/// queue-name sanitiser (`cups_queue_name`) so that — given a case-insensitive
/// CUPS name lookup — our persistent queue matches the on-demand temp queue
/// CUPS would derive from the (spaced) DNS-SD instance name, and no duplicate
/// is created. The `make_config` callback receives this as its `name` arg and
/// may override by returning a [`PrinterConfig`] with a different `name`.
fn printer_name_from_uri(uri: &str, info: &str) -> String {
    let source = if info.is_empty() { uri } else { info };
    let slug: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('_');
    let collapsed: String = trimmed
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if collapsed.is_empty() {
        "printer".to_string()
    } else {
        collapsed
    }
}

async fn index_handler(State(state): State<AppState>) -> impl IntoResponse {
    let printers = state.printers.read();
    let mut html = String::from(
        "<!DOCTYPE html><html><head><title>ipp-printer-app</title></head><body>\
         <h1>ipp-printer-app</h1><ul>",
    );
    for p in printers.iter() {
        let uri = p.config.printer_uri(&state.host, state.port);
        html.push_str(&format!(
            "<li><b>{}</b> (<code>{}</code>) — <code>{uri}</code> — device <code>{}</code></li>",
            p.config.display_label(),
            p.config.name,
            p.config.device_uri
        ));
    }
    html.push_str(&format!(
        "</ul><p>Register with CUPS: <code>lpadmin -p NAME -E -v \
         ipp://{}:{}/ipp/print/NAME -m everywhere</code></p></body></html>",
        if state.host.is_empty() || state.host == "0.0.0.0" || state.host == "::" {
            "localhost"
        } else {
            &state.host
        },
        state.port,
    ));
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html)
}

/// Serve the printer icon advertised in `printer-icons`. A 1×1 transparent
/// PNG keeps the resource valid without shipping artwork; consumers that want
/// a real icon can layer their own route ahead of this one.
async fn icon_handler() -> impl IntoResponse {
    const ICON_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/png")],
        ICON_PNG,
    )
}

async fn ipp_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    match handle_ipp(&state, &name, &body).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/ipp")],
            bytes,
        ),
        Err((status, msg)) => (
            status,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            msg.into_bytes(),
        ),
    }
}

async fn handle_ipp(
    state: &AppState,
    name: &str,
    body: &[u8],
) -> Result<Vec<u8>, (StatusCode, String)> {
    let mut req = IppParser::new(IppReader::new(Cursor::new(body.to_vec())))
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("IPP parse error: {e}")))?;

    let version = req.header().version;
    let request_id = req.header().request_id;
    let op_code = req.header().operation_or_status;

    // RFC 8011 §4.1.8: reject IPP versions outside the 1.x / 2.x families.
    let major = version.0 >> 8;
    if major != 1 && major != 2 {
        let resp = IppRequestResponse::new_response(
            IppVersion::v1_1(),
            IppStatus::ServerErrorVersionNotSupported,
            request_id,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(resp.to_bytes().to_vec());
    }

    // RFC 8011 §4.1.4: the operation-attributes group must begin with
    // `attributes-charset` followed by `attributes-natural-language`, in that
    // order. The `ipp` crate parses attributes into a hash map (losing wire
    // order), so we check the first two names against the raw request bytes.
    // Wrong order / missing → `client-error-bad-request`.
    if !operation_attributes_well_ordered(body) {
        let resp = IppRequestResponse::new_response(
            version,
            IppStatus::ClientErrorBadRequest,
            request_id,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(resp.to_bytes().to_vec());
    }

    // RFC 8011 §4.1.1: a request-id of 0 is invalid and must be rejected with
    // `client-error-bad-request`. We answer in-band (HTTP 200 + IPP status) so
    // conformant clients see the IPP error rather than a transport failure.
    if request_id == 0 {
        let resp = IppRequestResponse::new_response(
            version,
            IppStatus::ClientErrorBadRequest,
            request_id,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(resp.to_bytes().to_vec());
    }

    // RFC 8011 §4.2: an operation must target the printer (or a job) via a
    // `printer-uri` (or `job-uri`) operation attribute. We still route by the
    // request path, but a request carrying neither is malformed.
    if !has_operation_attr(&req, "printer-uri") && !has_operation_attr(&req, "job-uri") {
        let resp = IppRequestResponse::new_response(
            version,
            IppStatus::ClientErrorBadRequest,
            request_id,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(resp.to_bytes().to_vec());
    }

    let record = {
        let guard = state.printers.read();
        guard
            .iter()
            .find(|p| p.config.name == name)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, format!("printer not found: {name}")))?
    };

    // PWG 5100.14 required operations that the `ipp` crate's `Operation` enum
    // doesn't model. Dispatch them by raw code before the enum conversion.
    const OP_CANCEL_MY_JOBS: u16 = 0x0039;
    const OP_CLOSE_JOB: u16 = 0x003b;
    const OP_IDENTIFY_PRINTER: u16 = 0x003c;
    match op_code {
        OP_CLOSE_JOB => {
            // We finalize jobs eagerly on Send-Document, so Close-Job is an
            // acknowledgement — succeed if the job exists.
            let status = match extract_job_id(&req).and_then(|id| state.jobs.get(id)) {
                Some(_) => IppStatus::SuccessfulOk,
                None => IppStatus::ClientErrorNotFound,
            };
            let resp = IppRequestResponse::new_response(version, status, request_id)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Ok(resp.to_bytes().to_vec());
        }
        OP_CANCEL_MY_JOBS => {
            for j in state.jobs.jobs_for_printer(name) {
                state.jobs.cancel(j.id);
            }
            let resp =
                IppRequestResponse::new_response(version, IppStatus::SuccessfulOk, request_id)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Ok(resp.to_bytes().to_vec());
        }
        OP_IDENTIFY_PRINTER => {
            let actions = extract_identify_actions(&req);
            state
                .device_backend
                .identify(&record.config, &actions)
                .await;
            let resp =
                IppRequestResponse::new_response(version, IppStatus::SuccessfulOk, request_id)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            return Ok(resp.to_bytes().to_vec());
        }
        _ => {}
    }

    let op = Operation::from_u16(op_code)
        .ok_or((StatusCode::BAD_REQUEST, "unknown IPP operation".into()))?;

    let resp = match op {
        Operation::GetPrinterAttributes => {
            let requested = extract_requested_attributes(&req);
            get_printer_attributes(
                version,
                request_id,
                &record,
                &state.host,
                state.port,
                requested.as_ref(),
            )
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Operation::ValidateJob => validate_job(version, request_id, &record, &state.host, state.port)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        Operation::PrintJob => {
            let copies = extract_copies(&req);
            let format = extract_document_format(&req);
            let mut payload = Vec::new();
            req.payload_mut()
                .read_to_end(&mut payload)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

            let job = state.jobs.create(name.to_string(), requesting_user(&req));
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            let accepted = print_job_accepted(version, request_id, &job, &printer_uri_str)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            spawn_print_worker(state, name.to_string(), job, payload, copies, format);
            accepted
        }
        Operation::CreateJob => {
            // Document-less job creation; the document arrives via Send-Document.
            let job = state.jobs.create(name.to_string(), requesting_user(&req));
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            print_job_accepted(version, request_id, &job, &printer_uri_str)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Operation::SendDocument => {
            // Attach a document to an existing (Create-Job) job and, on the
            // last document, hand it to the print worker. We don't support
            // multi-document jobs (multiple-document-jobs-supported = false),
            // so each Send-Document carries the whole job.
            // RFC 8011 §3.3.1: Send-Document requires the `last-document`
            // boolean operation attribute.
            if !has_operation_attr(&req, "last-document") {
                let resp = IppRequestResponse::new_response(
                    version,
                    IppStatus::ClientErrorBadRequest,
                    request_id,
                )
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                return Ok(resp.to_bytes().to_vec());
            }
            let job_id = extract_job_id(&req).ok_or((
                StatusCode::BAD_REQUEST,
                "Send-Document missing job-id".to_string(),
            ))?;
            let job = state.jobs.get(job_id).ok_or((
                StatusCode::NOT_FOUND,
                format!("job not found: {job_id}"),
            ))?;
            let copies = extract_copies(&req);
            let format = extract_document_format(&req);
            let mut payload = Vec::new();
            req.payload_mut()
                .read_to_end(&mut payload)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

            if !payload.is_empty() {
                spawn_print_worker(state, name.to_string(), job.clone(), payload, copies, format);
            }
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            build_job_attrs_response(version, request_id, &job, &printer_uri_str, None)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Operation::GetJobs => {
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            let mut jobs = state.jobs.jobs_for_printer(name);
            // RFC 8011 §3.2.6.1: `my-jobs=true` scopes the listing to the
            // requesting user's own jobs.
            if my_jobs_flag(&req) {
                let user = requesting_user(&req);
                jobs.retain(|j| j.owner == user);
            }
            // When the client omits `requested-attributes`, Get-Jobs returns
            // only `job-uri` and `job-id`.
            let requested = extract_requested_attributes(&req);
            let default_set = ["job-uri".to_string(), "job-id".to_string()]
                .into_iter()
                .collect();
            let filter = effective_filter(requested.as_ref(), &default_set);
            build_get_jobs_response(version, request_id, &jobs, &printer_uri_str, filter)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Operation::GetJobAttributes => {
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            let job_id = extract_job_id(&req).ok_or((
                StatusCode::BAD_REQUEST,
                "Get-Job-Attributes missing job-id".to_string(),
            ))?;
            let job = state.jobs.get(job_id).ok_or((
                StatusCode::NOT_FOUND,
                format!("job not found: {job_id}"),
            ))?;
            // Get-Job-Attributes default is "all" — filter only when the
            // client supplies a concrete `requested-attributes` set.
            let requested = extract_requested_attributes(&req);
            let all = std::collections::BTreeSet::new();
            let filter = effective_filter(requested.as_ref(), &all);
            build_job_attrs_response(version, request_id, &job, &printer_uri_str, filter)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        Operation::CancelJob => {
            let job_id = extract_job_id(&req).ok_or((
                StatusCode::BAD_REQUEST,
                "Cancel-Job missing job-id".to_string(),
            ))?;
            let status = match state.jobs.cancel(job_id) {
                None => IppStatus::ClientErrorNotFound,
                Some(JobState::Canceled) => IppStatus::SuccessfulOk,
                Some(_) => IppStatus::ClientErrorNotPossible,
            };
            IppRequestResponse::new_response(version, status, request_id)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unsupported IPP operation: {op:?}"),
            ));
        }
    };

    Ok(resp.to_bytes().to_vec())
}

/// RFC 8011 §4.1.4: the operation-attributes group must start with
/// `attributes-charset` then `attributes-natural-language`, in that order.
/// Parses the first two attribute *names* directly from the raw IPP request
/// (header is 8 bytes; the operation group is the first delimiter group) and
/// checks them. Returns `false` on a short/malformed buffer.
fn operation_attributes_well_ordered(body: &[u8]) -> bool {
    // header: version(2) operation(2) request-id(4), then delimiter tag.
    if body.len() <= 8 || body[8] != ipp::model::DelimiterTag::OperationAttributes as u8 {
        return false;
    }
    let mut i = 9;
    let mut names: Vec<&[u8]> = Vec::new();
    while i < body.len() && names.len() < 2 {
        let tag = body[i];
        if tag <= 0x0f {
            break; // next delimiter tag → end of operation group
        }
        i += 1;
        if i + 2 > body.len() {
            return false;
        }
        let name_len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        i += 2;
        if i + name_len > body.len() {
            return false;
        }
        // name_len == 0 marks an additional value of the previous attribute.
        if name_len > 0 {
            names.push(&body[i..i + name_len]);
        }
        i += name_len;
        if i + 2 > body.len() {
            return false;
        }
        let value_len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        i += 2 + value_len;
    }
    names.first() == Some(&b"attributes-charset".as_slice())
        && names.get(1) == Some(&b"attributes-natural-language".as_slice())
}

/// True if an attribute named `name` is present in the operation-attributes
/// group of `req`.
fn has_operation_attr(req: &IppRequestResponse, name: &str) -> bool {
    req.attributes()
        .groups()
        .iter()
        .filter(|g| g.tag() == ipp::model::DelimiterTag::OperationAttributes)
        .any(|g| g.attributes().keys().any(|k| k.as_str() == name))
}

fn extract_job_id(req: &IppRequestResponse) -> Option<JobId> {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "job-id" {
                if let IppValue::Integer(n) = attr.value() {
                    return Some((*n) as JobId);
                }
            }
            if attr.name().as_str() == "job-uri" {
                if let IppValue::Uri(s) = attr.value() {
                    return s.as_str().rsplit('/').next().and_then(|s| s.parse().ok());
                }
            }
        }
    }
    None
}

fn extract_copies(req: &IppRequestResponse) -> u32 {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "copies" {
                if let IppValue::Integer(n) = attr.value() {
                    return (*n).max(1) as u32;
                }
            }
        }
    }
    0
}

/// Read the `document-format` operation attribute. Defaults to
/// `application/octet-stream` (the IPP default) when the client omits it.
fn extract_document_format(req: &IppRequestResponse) -> String {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "document-format" {
                if let IppValue::MimeMediaType(s) = attr.value() {
                    return s.as_str().to_string();
                }
            }
        }
    }
    "application/octet-stream".to_string()
}

/// Collect the client's `requested-attributes` values (RFC 8011 §4.2.5).
/// Returns `None` when the attribute is absent (caller treats that as "all").
fn extract_requested_attributes(req: &IppRequestResponse) -> Option<std::collections::BTreeSet<String>> {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "requested-attributes" {
                let mut set = std::collections::BTreeSet::new();
                for v in attr.value().into_iter() {
                    if let IppValue::Keyword(k) = v {
                        set.insert(k.as_str().to_string());
                    }
                }
                return Some(set);
            }
        }
    }
    None
}

/// Resolve the effective attribute filter for a job query. `requested` is the
/// client's `requested-attributes` (if any); `default` is the operation's
/// default set (empty = "all"). Returns `None` to mean "return everything".
fn effective_filter<'a>(
    requested: Option<&'a std::collections::BTreeSet<String>>,
    default: &'a std::collections::BTreeSet<String>,
) -> Option<&'a std::collections::BTreeSet<String>> {
    match requested {
        None => (!default.is_empty()).then_some(default),
        Some(set) if set.is_empty() || set.contains("all") => None,
        Some(set) => Some(set),
    }
}

/// Read `requesting-user-name` from the operation attributes, defaulting to
/// `anonymous` when the client omits it.
fn requesting_user(req: &IppRequestResponse) -> String {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "requesting-user-name" {
                if let IppValue::NameWithoutLanguage(s) = attr.value() {
                    return s.as_str().to_string();
                }
            }
        }
    }
    "anonymous".to_string()
}

/// Read the `my-jobs` boolean operation attribute (default `false`).
fn my_jobs_flag(req: &IppRequestResponse) -> bool {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "my-jobs" {
                if let IppValue::Boolean(b) = attr.value() {
                    return *b;
                }
            }
        }
    }
    false
}

/// Collect `identify-actions` keywords from an Identify-Printer request.
fn extract_identify_actions(req: &IppRequestResponse) -> Vec<String> {
    for group in req.attributes().groups() {
        for attr in group.attributes().values() {
            if attr.name().as_str() == "identify-actions" {
                return attr
                    .value()
                    .into_iter()
                    .filter_map(|v| match v {
                        IppValue::Keyword(k) => Some(k.as_str().to_string()),
                        _ => None,
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Spawn the background worker that runs a print job to completion, updating
/// printer/job state and persisting at the end. Shared by Print-Job and
/// Send-Document.
fn spawn_print_worker(
    state: &AppState,
    printer_name: String,
    job: crate::job::JobRecord,
    payload: Vec<u8>,
    copies: u32,
    document_format: String,
) {
    let state_clone = state.clone();
    let name_owned = printer_name;
    let job_for_worker = job;
    let payload: Arc<[u8]> = payload.into();
    // Runs on the ambient tokio runtime (spawn_print_worker is called from the
    // async IPP handler). The job future awaits the device transport directly.
    tokio::spawn(async move {
        // The printer stays `Processing` for the whole life of the job —
        // including while held waiting for the device. That keeps the status
        // poller (which only touches Idle/Stopped printers) off the device so
        // it can't contend with our retries.
        {
            let mut guard = state_clone.printers.write();
            if let Some(p) = guard.iter_mut().find(|p| p.config.name == name_owned) {
                attributes::set_printer_processing(p);
            }
        }
        state_clone
            .jobs
            .set_state(job_for_worker.id, JobState::Processing);
        let ctx = JobContext {
            id: job_for_worker.id,
            printer_name: name_owned.clone(),
            cancel_flag: job_for_worker.cancel_flag.clone(),
            document_format,
        };

        // Retry/hold loop. A `DeviceUnavailable` outcome holds the job
        // (`processing-stopped`) and retries with capped backoff until the
        // device prints it, the job is canceled, or it hits a hard failure —
        // the printer-application equivalent of holding a job through a jam.
        const BACKOFF_MAX: Duration = Duration::from_secs(30);
        // Initial retry backoff; doubles up to BACKOFF_MAX. Override for tests
        // / tuning with IPP_PRINTER_APP_RETRY_MS.
        let backoff_start = std::env::var("IPP_PRINTER_APP_RETRY_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(2));
        let mut backoff = backoff_start.min(BACKOFF_MAX);
        let mut held = false;
        loop {
            if ctx.is_canceled() {
                break;
            }
            match (state_clone.print_job)(ctx.clone(), payload.clone(), copies).await {
                JobOutcome::Completed => {
                    set_printer_reasons(&state_clone, &name_owned, crate::flags::PrinterReason::empty());
                    set_printer_idle_named(&state_clone, &name_owned);
                    // Don't clobber a Cancel that landed mid-print.
                    if !ctx.is_canceled() {
                        state_clone.jobs.set_state(job_for_worker.id, JobState::Completed);
                    }
                    break;
                }
                JobOutcome::Failed(f) => {
                    log::error!(
                        "print job {} failed: {} (reasons={:?})",
                        job_for_worker.id, f.message, f.printer_reasons
                    );
                    set_printer_reasons(&state_clone, &name_owned, f.printer_reasons);
                    set_printer_idle_named(&state_clone, &name_owned);
                    state_clone.jobs.set_failure(job_for_worker.id, f.printer_reasons, f.message);
                    break;
                }
                JobOutcome::DeviceUnavailable { reasons } => {
                    if !held {
                        held = true;
                        log::info!(
                            "print job {} held: device unavailable (reasons={:?}); will retry until it prints or is canceled",
                            job_for_worker.id, reasons
                        );
                    }
                    // Surface the condition but keep printer-state Processing.
                    set_printer_reasons(&state_clone, &name_owned, reasons);
                    state_clone.jobs.set_state(job_for_worker.id, JobState::ProcessingStopped);
                    if sleep_cancelable(&ctx.cancel_flag, backoff).await {
                        break; // canceled during the wait
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }

        if ctx.is_canceled() {
            // Reflect the cancel and clear any held condition.
            set_printer_reasons(&state_clone, &name_owned, crate::flags::PrinterReason::empty());
            set_printer_idle_named(&state_clone, &name_owned);
            state_clone.jobs.cancel(job_for_worker.id);
        }
        Server::persist(&state_clone.printers, &state_clone.state_path);
    });
}

/// Set `printer-state-reasons` for the named printer.
fn set_printer_reasons(state: &AppState, name: &str, reasons: crate::flags::PrinterReason) {
    let mut guard = state.printers.write();
    if let Some(p) = guard.iter_mut().find(|p| p.config.name == name) {
        p.reasons = reasons;
    }
}

/// Return the named printer to `idle`.
fn set_printer_idle_named(state: &AppState, name: &str) {
    let mut guard = state.printers.write();
    if let Some(p) = guard.iter_mut().find(|p| p.config.name == name) {
        attributes::set_printer_idle(p);
    }
}

/// Sleep up to `dur`, waking early (and returning `true`) if the cancel flag is
/// set. Polls in short slices so a Cancel-Job is honored promptly.
async fn sleep_cancelable(
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    dur: Duration,
) -> bool {
    const SLICE: Duration = Duration::from_millis(250);
    let mut left = dur;
    while left > Duration::ZERO {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return true;
        }
        let nap = left.min(SLICE);
        tokio::time::sleep(nap).await;
        left -= nap;
    }
    cancel.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceBackend;
    use crate::flags::PrinterReason;
    use crate::job::JobState;
    use crate::printer::{PrinterConfig, PrinterRecord, PrinterRegistry};
    use crate::raster::{JobFailure, JobOutcome};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The logical slug must match what CUPS's `cups_queue_name` derives from
    /// the same (spaced, mixed-case) DNS-SD instance name, modulo case — CUPS's
    /// printer lookup is case-insensitive. CUPS maps each non-alnum run to one
    /// `_` and trims; we additionally lowercase.
    #[test]
    fn slug_matches_cups_queue_name_modulo_case() {
        assert_eq!(
            printer_name_from_uri("supvan://x", "Supvan T50 Series t0117a2410211517"),
            "supvan_t50_series_t0117a2410211517"
        );
        // Collapses runs of separators and trims leading/trailing ones.
        assert_eq!(printer_name_from_uri("", "  Brother  HL-2270DW  "), "brother_hl_2270dw");
        // Falls back to the URI when info is empty, and never yields empty.
        assert_eq!(printer_name_from_uri("supvan://t0117", ""), "supvan_t0117");
        assert_eq!(printer_name_from_uri("", "***"), "printer");
    }

    struct NoopBackend;
    #[async_trait::async_trait]
    impl DeviceBackend for NoopBackend {
        async fn list(&self) -> Vec<crate::device::DiscoveredDevice> {
            Vec::new()
        }
        fn driver_for_device(&self, _id: &str, _uri: &str) -> Option<String> {
            None
        }
    }

    fn test_config(name: &str) -> PrinterConfig {
        PrinterConfig {
            name: name.into(),
            display_name: String::new(),
            driver_name: "test".into(),
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

    fn test_state(print_job: PrintJobFn, tag: &str) -> AppState {
        let registry: PrinterRegistry =
            Arc::new(parking_lot::RwLock::new(vec![PrinterRecord::new(test_config("p"))]));
        AppState {
            host: "127.0.0.1".into(),
            port: 0,
            printers: registry,
            print_job,
            state_path: std::env::temp_dir().join(format!("ipp-worker-test-{tag}.json")),
            jobs: crate::job::JobRegistry::new(),
            device_backend: Arc::new(NoopBackend),
        }
    }

    /// Drive a job to a terminal state, polling the registry. Returns the final
    /// job state (or panics on timeout).
    async fn run_to_terminal(state: &AppState, id: crate::job::JobId) -> JobState {
        for _ in 0..500 {
            let s = state.jobs.get(id).unwrap().state;
            if matches!(s, JobState::Completed | JobState::Aborted | JobState::Canceled) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job {id} did not reach a terminal state");
    }

    /// A `DeviceUnavailable` outcome must HOLD the job and retry it until the
    /// device prints — the paper-jam / offline behavior — not abort it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn device_unavailable_holds_and_retries_until_it_prints() {
        std::env::set_var("IPP_PRINTER_APP_RETRY_MS", "5");
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = attempts.clone();
        let print_job: PrintJobFn = Arc::new(move |_ctx, _payload, _copies| {
            let a = a.clone();
            Box::pin(async move {
                // Unavailable for the first two attempts, then it prints.
                if a.fetch_add(1, Ordering::SeqCst) < 2 {
                    JobOutcome::DeviceUnavailable { reasons: PrinterReason::OFFLINE }
                } else {
                    JobOutcome::Completed
                }
            })
        });
        let state = test_state(print_job, "hold");
        let job = state.jobs.create("p".into(), "tester".into());
        let id = job.id;
        spawn_print_worker(&state, "p".into(), job, vec![1, 2, 3], 1, "image/pwg-raster".into());

        assert_eq!(
            run_to_terminal(&state, id).await,
            JobState::Completed,
            "a held job must eventually print, not abort"
        );
        assert!(
            attempts.load(Ordering::SeqCst) >= 3,
            "the framework should have retried the held job"
        );
    }

    /// A `Failed` outcome is a permanent abort — no retry.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_outcome_aborts_without_retry() {
        std::env::set_var("IPP_PRINTER_APP_RETRY_MS", "5");
        let attempts = Arc::new(AtomicUsize::new(0));
        let a = attempts.clone();
        let print_job: PrintJobFn = Arc::new(move |_ctx, _payload, _copies| {
            let a = a.clone();
            Box::pin(async move {
                a.fetch_add(1, Ordering::SeqCst);
                JobOutcome::Failed(JobFailure::other("unsupported document"))
            })
        });
        let state = test_state(print_job, "fail");
        let job = state.jobs.create("p".into(), "tester".into());
        let id = job.id;
        spawn_print_worker(&state, "p".into(), job, vec![0], 1, "x".into());

        assert_eq!(run_to_terminal(&state, id).await, JobState::Aborted);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a Failed outcome must not be retried"
        );
    }

    /// Canceling a held job stops the retry loop promptly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn canceling_a_held_job_stops_the_retries() {
        std::env::set_var("IPP_PRINTER_APP_RETRY_MS", "5");
        let print_job: PrintJobFn = Arc::new(|_ctx, _payload, _copies| {
            // Never recovers — only a cancel can end this.
            Box::pin(async {
                JobOutcome::DeviceUnavailable { reasons: PrinterReason::MEDIA_JAM }
            })
        });
        let state = test_state(print_job, "cancel");
        let job = state.jobs.create("p".into(), "tester".into());
        let id = job.id;
        let cancel = job.cancel_flag.clone();
        spawn_print_worker(&state, "p".into(), job, vec![0], 1, "x".into());
        // Let it hold a couple of rounds, then cancel.
        tokio::time::sleep(Duration::from_millis(40)).await;
        state.jobs.cancel(id);
        let _ = cancel; // cancel() set the flag the worker polls
        assert_eq!(run_to_terminal(&state, id).await, JobState::Canceled);
    }
}
