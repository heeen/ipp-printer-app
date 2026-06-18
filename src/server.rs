//! Axum HTTP server: IPP over POST `/ipp/print/:name`.

use std::io::{Cursor, Read};
use std::sync::Arc;

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
use crate::raster::JobFailure;
use crate::state::PersistedState;

/// Context passed to a print-job worker so it can observe cancellation and
/// report progress without re-querying the registry.
#[derive(Clone)]
#[allow(missing_docs)]
pub struct JobContext {
    pub id: JobId,
    pub printer_name: String,
    pub cancel_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Callback to process a CUPS raster document on a device.
///
/// Returning `Err(JobFailure)` lets the framework propagate
/// `job-state-reasons` / `job-state-message` to IPP clients.
pub type PrintJobFn = Arc<
    dyn Fn(JobContext, Vec<u8>, u32) -> Result<(), JobFailure>
        + Send
        + Sync,
>;

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

        // Background status poller — keeps printer-state-reasons fresh.
        let _status = crate::status::spawn(opts.device_backend.clone(), opts.printers.clone());

        // mDNS advertising for IPP-Everywhere auto-discovery. Skipped when the
        // caller opts to advertise itself later (see ServerOptions::advertise_mdns).
        #[cfg(feature = "mdns")]
        let _advertiser = if opts.advertise_mdns {
            match crate::mdns::Advertiser::register_all(&opts.printers, opts.port) {
                Ok(adv) => Some(adv),
                Err(e) => {
                    log::warn!("mdns: failed to register printers: {e}");
                    None
                }
            }
        } else {
            None
        };

        axum::serve(listener, Self::router(opts)).await
    }

    /// Load printers from disk, discover devices, merge into registry.
    pub fn bootstrap_printers(
        registry: &PrinterRegistry,
        backend: &dyn DeviceBackend,
        state_path: &std::path::Path,
        make_config: impl Fn(&str, &str, &str, &str) -> Option<crate::printer::PrinterConfig>,
    ) {
        let mut records: Vec<PrinterRecord> = PersistedState::load(state_path)
            .printers
            .into_iter()
            .map(PrinterRecord::new)
            .collect();

        backend.list(&mut |info, uri, device_id| {
            let driver = match backend.driver_for_device(device_id, uri) {
                Some(d) => d,
                None => return true,
            };
            let name = printer_name_from_uri(uri, info);
            if records.iter().any(|r| r.config.device_uri == uri) {
                return true;
            }
            let Some(cfg) = make_config(&name, &driver, uri, device_id) else {
                return true;
            };
            log::info!("auto-add printer {name} -> {uri}");
            records.push(PrinterRecord::new(cfg));
            true
        });

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

/// Generic slug used as the proposed printer name during bootstrap. The
/// `make_config` callback receives this as its first arg and is free to
/// override by returning a [`PrinterConfig`] with a different `name`.
fn printer_name_from_uri(uri: &str, info: &str) -> String {
    let source = if info.is_empty() { uri } else { info };
    let slug: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    let collapsed: String = trimmed
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
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
            "<li><b>{}</b> — <code>{uri}</code> — device <code>{}</code></li>",
            p.config.name, p.config.device_uri
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

async fn ipp_handler(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Bytes,
) -> impl IntoResponse {
    match handle_ipp(&state, &name, &body) {
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

fn handle_ipp(state: &AppState, name: &str, body: &[u8]) -> Result<Vec<u8>, (StatusCode, String)> {
    let mut req = IppParser::new(IppReader::new(Cursor::new(body.to_vec())))
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("IPP parse error: {e}")))?;

    let version = req.header().version;
    let request_id = req.header().request_id;
    let op = Operation::from_u16(req.header().operation_or_status)
        .ok_or((StatusCode::BAD_REQUEST, "unknown IPP operation".into()))?;

    let record = {
        let guard = state.printers.read();
        guard
            .iter()
            .find(|p| p.config.name == name)
            .cloned()
            .ok_or((StatusCode::NOT_FOUND, format!("printer not found: {name}")))?
    };

    let resp = match op {
        Operation::GetPrinterAttributes => get_printer_attributes(
            version,
            request_id,
            &record,
            &state.host,
            state.port,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        Operation::ValidateJob => validate_job(version, request_id, &record, &state.host, state.port)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        Operation::PrintJob => {
            let copies = extract_copies(&req);
            let mut payload = Vec::new();
            req.payload_mut()
                .read_to_end(&mut payload)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

            let job = state.jobs.create(name.to_string());
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            let accepted = print_job_accepted(version, request_id, &job, &printer_uri_str)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let state_clone = state.clone();
            let name_owned = name.to_string();
            let job_for_worker = job.clone();
            std::thread::spawn(move || {
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
                };
                let result = (state_clone.print_job)(ctx, payload, copies);
                {
                    let mut guard = state_clone.printers.write();
                    if let Some(p) = guard.iter_mut().find(|p| p.config.name == name_owned) {
                        attributes::set_printer_idle(p);
                        match &result {
                            Ok(()) => p.reasons = crate::flags::PrinterReason::empty(),
                            Err(f) => p.reasons = f.printer_reasons,
                        }
                    }
                }
                match result {
                    Ok(()) => {
                        // Don't clobber a Cancel that landed while the worker
                        // was running — the registry already saw it.
                        if !job_for_worker.cancel_flag.load(std::sync::atomic::Ordering::Acquire) {
                            state_clone
                                .jobs
                                .set_state(job_for_worker.id, JobState::Completed);
                        }
                    }
                    Err(f) => {
                        log::error!(
                            "print job {} failed: {} (reasons={:?})",
                            job_for_worker.id,
                            f.message,
                            f.printer_reasons,
                        );
                        state_clone
                            .jobs
                            .set_failure(job_for_worker.id, f.printer_reasons, f.message);
                    }
                }
                Server::persist(&state_clone.printers, &state_clone.state_path);
            });

            accepted
        }
        Operation::GetJobs => {
            let printer_uri_str = record.config.printer_uri(&state.host, state.port);
            let jobs = state.jobs.jobs_for_printer(name);
            build_get_jobs_response(version, request_id, &jobs, &printer_uri_str)
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
            build_job_attrs_response(version, request_id, &job, &printer_uri_str)
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
