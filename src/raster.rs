//! [`RasterDriver`] trait + page-header DTO for raster print jobs.

/// What a print-job callback reports back to the framework. The framework is
/// the spooler: it decides what happens to the job based on this value, so an
/// implementor only has to classify the result of one attempt — never write
/// its own retry/queue logic.
///
/// This is what makes a printer application behave like a *printer*: a device
/// that isn't ready (powered off, busy, paper being reloaded) must not drop the
/// job — it returns [`JobOutcome::DeviceUnavailable`] and the framework holds
/// the job (`job-state = processing-stopped`) and retries it until the device
/// is back, the same way a real printer holds a job through a paper jam.
#[derive(Debug, Clone)]
pub enum JobOutcome {
    /// The document was printed. The job completes.
    Completed,
    /// The device can't print right now but is expected to recover. The
    /// framework keeps the job and re-invokes the callback (with backoff) until
    /// it prints or the client cancels it, surfacing `reasons` on the printer
    /// (`printer-state-reasons`) meanwhile. Return this for transient device
    /// conditions — unreachable hardware, busy link, media being reloaded.
    DeviceUnavailable {
        /// Reasons to surface while held, e.g. [`crate::flags::PrinterReason::OFFLINE`].
        reasons: crate::flags::PrinterReason,
    },
    /// Permanent failure for *this* document (corrupt/unsupported data, a size
    /// the device can't handle, …). The job aborts; retrying wouldn't help.
    Failed(JobFailure),
}

/// Failure of a print job, carrying IPP-visible printer reasons + a message.
#[derive(Debug, Clone)]
pub struct JobFailure {
    /// Reasons OR'd into the printer's `printer-state-reasons` IPP attribute
    /// when this job aborts.
    pub printer_reasons: crate::flags::PrinterReason,
    /// Human-readable message surfaced as `job-state-message`.
    pub message: String,
}

impl JobFailure {
    /// Build a failure with explicit `printer-state-reasons`.
    pub fn new(
        printer_reasons: crate::flags::PrinterReason,
        message: impl Into<String>,
    ) -> Self {
        Self {
            printer_reasons,
            message: message.into(),
        }
    }

    /// Shorthand for a generic failure (`PrinterReason::OTHER`).
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(crate::flags::PrinterReason::OTHER, message)
    }
}

impl std::fmt::Display for JobFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for JobFailure {}

/// Page geometry parsed from a CUPS/PWG raster page header.
#[derive(Debug, Clone)]
pub struct JobOptions {
    /// Page width in pixels.
    pub width: u32,
    /// Page height in pixels.
    pub height: u32,
    /// Bits per pixel (typically 1 for monochrome, 8 for grayscale, 24 for RGB).
    pub bits_per_pixel: u32,
    /// Bytes per scanline (already pre-padded by the raster source).
    pub bytes_per_line: u32,
    /// Number of copies requested. Always ≥ 1.
    pub copies: u32,
}

impl JobOptions {
    /// Construct from a CUPS raster v1 page header. `num_copies < 1` is
    /// clamped to 1 per IPP convention.
    pub fn from_cups_v1(
        width: u32,
        height: u32,
        bits_per_pixel: u32,
        bytes_per_line: u32,
        num_copies: u32,
    ) -> Self {
        Self {
            width,
            height,
            bits_per_pixel,
            bytes_per_line,
            copies: num_copies.max(1),
        }
    }
}

/// Driver that turns a stream of raster scanlines into device bytes.
///
/// Implementations are *per-job stateful* — `start_job` returns a fresh
/// value that owns the page buffer, `write_line` accumulates scanlines,
/// `end_page` transfers the page to the device, `end_job` releases
/// resources. The framework's IPP `Print-Job` handler drives this trait;
/// you only need to provide a type that knows how to talk to your device.
pub trait RasterDriver: Sized + Send + 'static {
    /// The driver's opaque device handle (e.g. an open HID descriptor).
    type Device: Send;

    /// Allocate per-job state. Called once at the top of each job.
    fn start_job(
        printer: &crate::printer::PrinterHandle<'_>,
        options: &JobOptions,
        device: &Self::Device,
    ) -> Result<Self, JobFailure>;

    /// Called once per page before any `write_line`. Default: no-op.
    fn start_page(
        &mut self,
        _options: &JobOptions,
        _page: u32,
        _device: &Self::Device,
    ) -> Result<(), JobFailure> {
        Ok(())
    }

    /// Append one scanline to the page buffer.
    fn write_line(
        &mut self,
        options: &JobOptions,
        y: u32,
        line: &[u8],
    ) -> Result<(), JobFailure>;

    /// Transfer the completed page to the device (and repeat for copies).
    fn end_page(
        &mut self,
        options: &JobOptions,
        page: u32,
        device: &Self::Device,
    ) -> Result<(), JobFailure>;

    /// Release per-job state. Called once at the end of the job.
    fn end_job(self, device: &Self::Device);
}
