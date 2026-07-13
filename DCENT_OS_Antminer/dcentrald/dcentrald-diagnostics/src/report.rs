//! HTML/PDF report generation using askama templates.
//!
//! Renders diagnostic test results into self-contained HTML reports
//! with embedded CSS and SVG charts. PDF export is handled by the
//! browser's native print-to-PDF — no wkhtmltopdf or other binary needed.
//!
//! Report Generation Flow:
//!   1. Test completes, TestResult struct is populated
//!   2. askama template renders HTML with embedded CSS and SVG charts
//!   3. HTML is stored in /data/reports/{test_id}.html
//!   4. Dashboard offers "Print to PDF" button (browser native)
//!   5. API serves HTML at /api/diagnostics/{type}/report?test_id=uuid
//!
//! Report Storage:
//!   /data/reports/
//!     {test_id}.json    — Raw test data (machine-readable)
//!     {test_id}.html    — Rendered report (human-readable)
//!   Max 20 reports stored (oldest auto-deleted)

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use dcentrald_common::atomic_file::{self, AtomicWriteOptions};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::board_health::BoardHealthResult;
use crate::chip_health::{ChipHealthSnapshot, ChipMap};
use crate::evidence::{DiagnosticEvidence, EvidenceKind};
use crate::hashreport::HashReport;

/// Default report storage directory.
pub const REPORT_DIR: &str = "/data/reports";

/// Maximum number of stored reports before oldest is auto-deleted.
pub const MAX_STORED_REPORTS: usize = 20;

/// Per-file ceiling for machine-readable diagnostic evidence.
pub const MAX_REPORT_JSON_BYTES: usize = 8 * 1024 * 1024;

/// Per-file ceiling for the self-contained human-readable rendering.
pub const MAX_REPORT_HTML_BYTES: usize = 8 * 1024 * 1024;

/// Serialize report-pair publication and retention within the daemon process.
///
/// The JSON file is the commit marker: HTML is published (or durably removed)
/// first, then JSON. This lock prevents in-process readers from observing the
/// interval between those operations. Cross-process coordination is outside
/// this contract; report UUIDs are expected to be immutable job identifiers.
static REPORT_MUTATION_LOCK: Mutex<()> = Mutex::new(());

fn lock_report_storage() -> MutexGuard<'static, ()> {
    match REPORT_MUTATION_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(
                target: "diagnostic_persistence",
                "diagnostic report storage lock was poisoned; retaining serialized ownership"
            );
            REPORT_MUTATION_LOCK.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn invalid_report_file(path: &Path, detail: &str) -> crate::DiagnosticError {
    crate::DiagnosticError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("diagnostic report {} {}", path.display(), detail),
    ))
}

fn validate_report_target(path: &Path) -> crate::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(invalid_report_file(
            path,
            "must be absent or an existing regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(crate::DiagnosticError::Io(error)),
    }
}

fn ensure_report_size(path: &Path, size: usize, max_bytes: usize) -> crate::Result<()> {
    if size <= max_bytes {
        return Ok(());
    }
    Err(crate::DiagnosticError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "diagnostic report {} is {} bytes; limit is {} bytes",
            path.display(),
            size,
            max_bytes
        ),
    )))
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl BoundedJsonWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        if buffer.len() > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "pretty-printed diagnostic JSON exceeds the {} byte limit",
                    self.max_bytes
                ),
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_json_bounded(
    path: &Path,
    value: &serde_json::Value,
    max_bytes: usize,
) -> crate::Result<Vec<u8>> {
    let mut writer = BoundedJsonWriter::new(max_bytes);
    serde_json::to_writer_pretty(&mut writer, value).map_err(|error| {
        if error.is_io() {
            crate::DiagnosticError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "diagnostic report {} exceeds its serialization bound: {error}",
                    path.display()
                ),
            ))
        } else {
            crate::DiagnosticError::ReportGeneration(format!(
                "JSON serialization error for {}: {error}",
                path.display()
            ))
        }
    })?;
    Ok(writer.into_inner())
}

fn require_uncommitted_report_target(path: &Path) -> crate::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            Err(crate::DiagnosticError::Io(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "diagnostic report {} already exists; report IDs are immutable",
                    path.display()
                ),
            )))
        }
        Ok(_) => Err(invalid_report_file(
            path,
            "must be absent; report IDs are immutable",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(crate::DiagnosticError::Io(error)),
    }
}

fn atomic_write_report(path: &Path, bytes: &[u8], max_bytes: usize) -> crate::Result<()> {
    atomic_file::atomic_write(path, bytes, AtomicWriteOptions::state_file(max_bytes))
        .map(|_| ())
        .map_err(|error| crate::DiagnosticError::Io(error.into_io_error()))
}

fn remove_report_file(path: &Path) -> crate::Result<()> {
    atomic_file::remove_file(path)
        .map(|_| ())
        .map_err(|error| crate::DiagnosticError::Io(error.into_io_error()))
}

fn open_report_file(path: &Path) -> crate::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).map_err(crate::DiagnosticError::Io)
}

fn read_bounded_report(path: &Path, max_bytes: usize) -> crate::Result<Vec<u8>> {
    // O_NOFOLLOW plus metadata from the opened descriptor closes the
    // check/open symlink race on the production Unix targets.
    let file = open_report_file(path)?;
    let metadata = file.metadata().map_err(crate::DiagnosticError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(invalid_report_file(path, "is not a regular file"));
    }
    if metadata.len() > max_bytes as u64 {
        return Err(crate::DiagnosticError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "diagnostic report {} declares {} bytes; limit is {} bytes",
                path.display(),
                metadata.len(),
                max_bytes
            ),
        )));
    }

    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(max_bytes));
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(crate::DiagnosticError::Io)?;
    if bytes.len() > max_bytes {
        return Err(crate::DiagnosticError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "diagnostic report {} grew beyond the {} byte limit while reading",
                path.display(),
                max_bytes
            ),
        )));
    }
    Ok(bytes)
}

fn regular_file_size_or_absent(path: &Path, max_bytes: usize) -> crate::Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > max_bytes as u64 {
                Err(crate::DiagnosticError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "diagnostic report {} declares {} bytes; limit is {} bytes",
                        path.display(),
                        metadata.len(),
                        max_bytes
                    ),
                )))
            } else {
                Ok(metadata.len())
            }
        }
        Ok(_) => Err(invalid_report_file(path, "is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(crate::DiagnosticError::Io(error)),
    }
}

/// Report format options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportFormat {
    /// Self-contained HTML with embedded CSS and SVG.
    Html,
    /// Raw JSON data.
    Json,
}

/// Report metadata stored alongside the rendered report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMetadata {
    /// Unique report identifier (matches test_id).
    pub report_id: Uuid,
    /// Type of diagnostic test that generated this report.
    pub test_type: String,
    /// ISO 8601 timestamp of report generation.
    pub generated_at: String,
    /// Firmware version at time of report.
    pub firmware_version: String,
    /// File size of HTML report in bytes.
    pub html_size_bytes: u64,
    /// File size of JSON data in bytes.
    pub json_size_bytes: u64,
    /// Overall grade (if applicable).
    pub grade: Option<String>,
}

/// Report generator for diagnostic test results.
///
/// Uses askama compile-time templates to render self-contained HTML
/// reports. Each report includes:
/// - D-Central branding and header
/// - System identification summary
/// - Test results with color-coded grades
/// - ChipMap grid (for chip/board health tests)
/// - SVG charts for performance data
/// - Recommendations and warnings
pub struct ReportGenerator {
    /// Base directory for report storage.
    report_dir: PathBuf,
    /// Maximum reports to keep.
    max_reports: usize,
}

impl ReportGenerator {
    /// Create a new report generator with the default storage directory.
    pub fn new() -> Self {
        Self {
            report_dir: PathBuf::from(REPORT_DIR),
            max_reports: MAX_STORED_REPORTS,
        }
    }

    /// Create a report generator with a custom storage directory.
    pub fn with_dir(report_dir: PathBuf) -> Self {
        Self {
            report_dir,
            max_reports: MAX_STORED_REPORTS,
        }
    }

    /// Render a HashReport to HTML.
    ///
    /// Generates a complete self-contained HTML document with:
    /// - System identification header
    /// - Baseline measurements
    /// - Per-board results with ChipMap grids
    /// - Performance charts (hashrate over 12 windows)
    /// - Unit grade with explanation
    /// - Warnings and recommendations
    pub fn render_hashreport(&self, report: &HashReport) -> crate::Result<String> {
        let mut board_sections = String::new();
        for board in &report.boards {
            let voltage_evidence = evidence_label(&board.voltage_evidence, &board.voltage_v);
            let crc_evidence = evidence_label(&board.crc_evidence, &board.crc_errors);
            let chip_rows = board
                .chips
                .iter()
                .take(24)
                .map(|chip| {
                    format!(
                        "<tr><td>{}</td><td>0x{:02X}</td><td>{:.2}</td><td>{}</td><td>{}</td></tr>",
                        chip.index, chip.address, chip.health_score, chip.frequency_mhz, chip.grade
                    )
                })
                .collect::<Vec<_>>()
                .join("");
            board_sections.push_str(&format!(
                "<section class=\"card\"><h3>Chain {}</h3><p>Grade <strong style=\"color:{}\">{}</strong> | {}/{} chips responding | {:.2} TH/s | {:.1} C | {:.2} V ({}) | CRC {} ({})</p><table><thead><tr><th>Chip</th><th>Addr</th><th>Score</th><th>MHz</th><th>Grade</th></tr></thead><tbody>{}</tbody></table></section>",
                board.chain_id,
                grade_color(board.grade),
                board.grade,
                board.chips_responding,
                board.chips_expected,
                board.hashrate_ghs as f64 / 1000.0,
                board.temp_c,
                board.voltage_v,
                voltage_evidence,
                board.crc_errors,
                crc_evidence,
                chip_rows
            ));
        }

        Ok(format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>D-Central HashReport Snapshot</title><style>body{{font-family:Arial,sans-serif;margin:24px;background:#0b1020;color:#e5e7eb}}h1,h2,h3{{margin:0 0 12px}}.card{{background:#111827;border:1px solid #1f2937;border-radius:12px;padding:16px;margin:16px 0}}table{{width:100%;border-collapse:collapse}}th,td{{padding:8px;border-bottom:1px solid #1f2937;text-align:left}}.badge{{display:inline-block;padding:6px 10px;border-radius:999px;background:{};color:#fff;font-weight:bold}}ul{{margin:8px 0 0 20px}}</style></head><body><h1>D-Central HashReport Snapshot</h1><p>This report is a <strong>{}</strong> built from current miner runtime data. It is not a timed diagnostic drive.</p><section class=\"card\"><h2>Summary</h2><p><span class=\"badge\">Unit Grade {}</span></p><p>{}</p><p>Generated: {} | Firmware: {} | Source: {} | Duration represented: {} s</p></section><section class=\"card\"><h2>System</h2><p>Serial: {} | MAC: {} | Model: {} | Control board: {} | Chip type: {} ({}) | Boards: {} | Total chips: {}</p><p>Fan PWM: {} | Fan RPM: {} | Temps: {:?} | Voltages: {:?}</p></section><section class=\"card\"><h2>Warnings</h2>{}</section><section class=\"card\"><h2>Recommendations</h2>{}</section>{}</body></html>",
            grade_color(report.unit_grade),
            escape_html(&report.report_kind),
            report.unit_grade,
            escape_html(&report.unit_grade_explanation),
            escape_html(&report.generated_at),
            escape_html(&report.firmware_version),
            escape_html(&report.source),
            report.duration_seconds,
            escape_html(&report.system.serial),
            escape_html(&report.system.mac),
            escape_html(&report.system.model),
            escape_html(&report.system.control_board),
            escape_html(&report.system.chip_type),
            escape_html(&report.system.chip_id),
            report.system.board_count,
            report.system.total_chips,
            report.baseline.fan_pwm,
            report.baseline.fan_rpm,
            report.baseline.temperatures_c,
            report.baseline.voltages_v,
            render_list(&report.warnings),
            render_list(&report.recommendations),
            board_sections,
        ))
    }

    /// Render a ChipMap to an HTML fragment (SVG grid).
    ///
    /// Produces a color-coded grid where each cell represents one ASIC chip.
    /// Colors: Green (healthy), Yellow (marginal), Orange (degraded),
    /// Red (poor), Gray (dead).
    pub fn render_chipmap(&self, chipmap: &ChipMap) -> crate::Result<String> {
        let columns = usize::from(chipmap.columns.max(1));
        let cells = chipmap
            .cells
            .iter()
            .map(|cell| {
                format!(
                    "<div class=\"chip-cell\" style=\"background:{}\" title=\"Chip {} | Addr 0x{:02X} | Grade {} | Score {:.2} | {} MHz | {} nonces | {} CRC\"><span>{}</span></div>",
                    cell.color.css_color(),
                    cell.index,
                    cell.address,
                    cell.grade,
                    cell.health_score,
                    cell.frequency_mhz,
                    cell.nonce_count,
                    cell.crc_errors,
                    cell.index,
                )
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(format!(
            "<div class=\"chip-grid\" style=\"grid-template-columns:repeat({},minmax(22px,1fr))\">{}</div>",
            columns, cells
        ))
    }

    /// Render a persisted chip-health snapshot to HTML.
    pub fn render_chip_health(&self, snapshot: &ChipHealthSnapshot) -> crate::Result<String> {
        let summary_cards = snapshot
            .chains
            .iter()
            .map(|chain| {
                format!(
                    "<tr><td>Chain {}</td><td>{}</td><td>{}/{}</td><td>{:.2}</td><td>{:.2} TH/s</td><td>{:.1} C</td><td>{} mV</td><td>{}</td></tr>",
                    chain.chain_id,
                    escape_html(&chain.source),
                    chain.responding_chips,
                    chain.chip_count,
                    chain.board_health_score,
                    chain.board_hashrate_ghs / 1000.0,
                    chain.board_temp_c,
                    chain.voltage_mv,
                    escape_html(&chain.status),
                )
            })
            .collect::<Vec<_>>()
            .join("");

        let chain_sections = snapshot
            .chains
            .iter()
            .map(|chain| {
                let chipmap = self.render_chipmap(&chain.chipmap).unwrap_or_else(|_| {
                    "<p>Chip map unavailable.</p>".to_string()
                });
                format!(
                    "<section class=\"card\"><h3>Chain {}</h3><p>Source: {} | Score: <strong>{:.2}</strong> | Responding chips: {}/{} | Hashrate: {:.2} TH/s | Temp: {:.1} C | Voltage: {} mV | Status: {}</p>{}</section>",
                    chain.chain_id,
                    escape_html(&chain.source),
                    chain.board_health_score,
                    chain.responding_chips,
                    chain.chip_count,
                    chain.board_hashrate_ghs / 1000.0,
                    chain.board_temp_c,
                    chain.voltage_mv,
                    escape_html(&chain.status),
                    chipmap,
                )
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>D-Central Chip Health Snapshot</title><style>{}</style></head><body><h1>D-Central Chip Health Snapshot</h1><p>This report is a <strong>persisted snapshot</strong> generated from the miner's current runtime state. No timed chip-health drive was launched.</p><section class=\"card\"><h2>Summary</h2><p>Generated: {} | Source: {} | Boards: {} | Chips: {}</p><h3>Warnings</h3>{}<h3>Recommendations</h3>{}</section><section class=\"card\"><h2>Board Summary</h2><table><thead><tr><th>Chain</th><th>Source</th><th>Responding</th><th>Score</th><th>Hashrate</th><th>Temp</th><th>Voltage</th><th>Status</th></tr></thead><tbody>{}</tbody></table></section>{}</body></html>",
            shared_report_css(),
            escape_html(&snapshot.generated_at),
            escape_html(&snapshot.source),
            snapshot.total_boards,
            snapshot.total_chips,
            render_list(&snapshot.warnings),
            render_list(&snapshot.recommendations),
            summary_cards,
            chain_sections,
        ))
    }

    /// Render persisted board-health snapshot results to HTML.
    pub fn render_board_health(&self, results: &[BoardHealthResult]) -> crate::Result<String> {
        let rows = results
            .iter()
            .map(|result| {
                format!(
                    "<tr><td>Chain {}</td><td><strong style=\"color:{}\">{}</strong></td><td>{}</td><td>{}/{}</td><td>{:.2} V ({})</td><td>{:.1} C</td><td>{}</td></tr>",
                    result.chain_id,
                    grade_color(result.grade),
                    result.grade,
                    escape_html(&result.data_source),
                    result.chips_responding,
                    result.chips_expected,
                    result.voltage_readback_v,
                    evidence_label(&result.voltage_evidence, &result.voltage_readback_v),
                    result.temperature_c,
                    escape_html(&result.status),
                )
            })
            .collect::<Vec<_>>()
            .join("");

        let sections = results
            .iter()
            .map(|result| {
                let dead_addresses = if result.dead_chip_addresses.is_empty() {
                    "None".to_string()
                } else {
                    result
                        .dead_chip_addresses
                        .iter()
                        .map(|address| format!("0x{address:02X}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let notes = result
                    .notes
                    .iter()
                    .map(|note| format!("<li>{}</li>", escape_html(note)))
                    .collect::<Vec<_>>()
                    .join("");
                // Snapshot mode performs NO PIC/dsPIC set/get readback, so the
                // "readback"/"deviation" fields are the echoed setpoint / a
                // hardcoded zero — render them honestly instead of implying a
                // measurement occurred (a collapsed rail can carry a non-zero
                // commanded value). A real measured test renders the full triple.
                let voltage_cell = if !result
                    .voltage_evidence
                    .is_measured_for(&result.voltage_readback_v)
                {
                    format!(
                        "{:.2} V &middot; evidence: {} from {} &middot; rail NOT measured &middot; {}",
                        result.voltage_setpoint_v,
                        evidence_label(&result.voltage_evidence, &result.voltage_readback_v),
                        escape_html(result.voltage_evidence.source()),
                        if result.voltage_ok {
                            "configured (non-zero)"
                        } else {
                            "NOT configured (0 V)"
                        }
                    )
                } else {
                    format!(
                        "{:.2} V setpoint, {:.2} V readback, {:.2}% deviation, {}",
                        result.voltage_setpoint_v,
                        result.voltage_readback_v,
                        result.voltage_deviation_pct,
                        if result.voltage_ok { "OK" } else { "CHECK" }
                    )
                };
                format!(
                    "<section class=\"card\"><h3>Chain {}</h3><p>Grade <strong style=\"color:{}\">{}</strong> | {} | Source: {} | Measurement: {} | Status: {} | Estimated hashrate: {:.2} TH/s</p><table><tbody><tr><th>Chip Enumeration</th><td>{}/{} responding</td></tr><tr><th>Dead Chip Addresses</th><td>{}</td></tr><tr><th>Voltage</th><td>{}</td></tr><tr><th>CRC</th><td>{} commands, {} errors, {:.2}% rate, {}</td></tr><tr><th>Temperature</th><td>{:.1} C, {}</td></tr><tr><th>EEPROM</th><td>present={} valid={} evidence={} model={} serial={}</td></tr></tbody></table><h4>Notes</h4>{}</section>",
                    result.chain_id,
                    grade_color(result.grade),
                    result.grade,
                    escape_html(&result.grade_explanation),
                    escape_html(&result.data_source),
                    escape_html(&result.measurement_type),
                    escape_html(&result.status),
                    result.estimated_hashrate_ghs / 1000.0,
                    result.chips_responding,
                    result.chips_expected,
                    dead_addresses,
                    voltage_cell,
                    result.crc_commands_sent,
                    result.crc_errors_received,
                    result.crc_error_rate_pct,
                    if result.crc_ok && result.crc_evidence.is_measured_for(&result.crc_errors_received) { "OK (measured)" } else if result.crc_ok { "NOT MEASURED" } else { "CHECK" },
                    result.temperature_c,
                    if result.temperature_ok { "OK" } else { "CHECK" },
                    result.eeprom_present,
                    result.eeprom_valid,
                    evidence_label(&result.eeprom_evidence, &result.eeprom_valid),
                    escape_html(result.eeprom_model.as_deref().unwrap_or("unknown")),
                    escape_html(result.eeprom_serial.as_deref().unwrap_or("unknown")),
                    if notes.is_empty() { "<p>None.</p>".to_string() } else { format!("<ul>{}</ul>", notes) },
                )
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>D-Central Board Health Snapshot</title><style>{}</style></head><body><h1>D-Central Board Health Snapshot</h1><p>This report is a <strong>persisted snapshot</strong> derived from current miner runtime values. No dedicated board stress sequence was launched.</p><section class=\"card\"><h2>Board Summary</h2><table><thead><tr><th>Chain</th><th>Grade</th><th>Source</th><th>Responding</th><th>Voltage</th><th>Temp</th><th>Status</th></tr></thead><tbody>{}</tbody></table></section>{}</body></html>",
            shared_report_css(),
            rows,
            sections,
        ))
    }

    /// Save a rendered report to disk.
    ///
    /// Stores both the HTML report and the raw JSON data:
    /// - /data/reports/{test_id}.html
    /// - /data/reports/{test_id}.json
    ///
    /// If the maximum number of reports is exceeded, the oldest report
    /// is automatically deleted.
    pub fn save_report(
        &self,
        test_id: &Uuid,
        html: Option<&str>,
        json_data: &serde_json::Value,
    ) -> crate::Result<ReportMetadata> {
        let _storage_guard = lock_report_storage();
        std::fs::create_dir_all(&self.report_dir).map_err(crate::DiagnosticError::Io)?;

        let json_path = self.report_dir.join(format!("{}.json", test_id));
        let html_path = self.report_dir.join(format!("{}.html", test_id));
        let json_bytes = serialize_json_bounded(&json_path, json_data, MAX_REPORT_JSON_BYTES)?;
        ensure_report_size(&json_path, json_bytes.len(), MAX_REPORT_JSON_BYTES)?;
        if let Some(html) = html {
            ensure_report_size(&html_path, html.len(), MAX_REPORT_HTML_BYTES)?;
        }

        // Validate both paths before the first mutation. The common primitive
        // repeats this check immediately before each rename to close ordinary
        // symlink/type substitution; the directory itself remains trusted.
        validate_report_target(&json_path)?;
        validate_report_target(&html_path)?;
        // A UUID identifies one immutable report generation. Replacing either
        // member in place cannot provide a crash-atomic two-file transaction:
        // a reboot between HTML and JSON publication could pair different
        // generations. Orphan cleanup is therefore explicit deletion, not an
        // implicit overwrite hidden inside save_report().
        require_uncommitted_report_target(&json_path)?;
        require_uncommitted_report_target(&html_path)?;

        let html_size_bytes = if let Some(html) = html {
            atomic_write_report(&html_path, html.as_bytes(), MAX_REPORT_HTML_BYTES)?;
            html.len() as u64
        } else {
            // An absent rendering must not leave a stale HTML file paired with
            // the new JSON. JSON is published only after this deletion is durable.
            remove_report_file(&html_path)?;
            0
        };

        // JSON is the pair's commit marker and is always published last.
        atomic_write_report(&json_path, &json_bytes, MAX_REPORT_JSON_BYTES)?;

        self.enforce_max_reports_unlocked(test_id)?;

        Ok(ReportMetadata {
            report_id: *test_id,
            test_type: infer_test_type(json_data),
            generated_at: infer_generated_at(json_data),
            firmware_version: infer_firmware_version(json_data),
            html_size_bytes,
            json_size_bytes: json_bytes.len() as u64,
            grade: infer_grade(json_data),
        })
    }

    /// Load a previously saved HTML report from disk.
    pub fn load_report_html(&self, test_id: &Uuid) -> crate::Result<String> {
        let _storage_guard = lock_report_storage();
        let json_path = self.report_dir.join(format!("{}.json", test_id));
        let commit = read_bounded_report(&json_path, MAX_REPORT_JSON_BYTES)?;
        serde_json::from_slice::<serde_json::Value>(&commit).map_err(|error| {
            crate::DiagnosticError::ReportGeneration(format!(
                "JSON commit marker parse error: {error}"
            ))
        })?;
        let path = self.report_dir.join(format!("{}.html", test_id));
        let bytes = read_bounded_report(&path, MAX_REPORT_HTML_BYTES)?;
        String::from_utf8(bytes).map_err(|error| {
            crate::DiagnosticError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
        })
    }

    /// Load previously saved JSON data from disk.
    pub fn load_report_json(&self, test_id: &Uuid) -> crate::Result<serde_json::Value> {
        let _storage_guard = lock_report_storage();
        let path = self.report_dir.join(format!("{}.json", test_id));
        let data = read_bounded_report(&path, MAX_REPORT_JSON_BYTES)?;
        serde_json::from_slice(&data).map_err(|e| {
            crate::DiagnosticError::ReportGeneration(format!("JSON parse error: {}", e))
        })
    }

    /// List all stored report metadata.
    pub fn list_reports(&self) -> crate::Result<Vec<ReportMetadata>> {
        let _storage_guard = lock_report_storage();
        if !self.report_dir.exists() {
            return Ok(Vec::new());
        }

        let mut reports = Vec::new();
        for entry in std::fs::read_dir(&self.report_dir).map_err(crate::DiagnosticError::Io)? {
            let entry = entry.map_err(crate::DiagnosticError::Io)?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(report_id) = Uuid::parse_str(stem) else {
                continue;
            };

            let json_bytes = read_bounded_report(&path, MAX_REPORT_JSON_BYTES)?;
            let json_value: serde_json::Value =
                serde_json::from_slice(&json_bytes).map_err(|e| {
                    crate::DiagnosticError::ReportGeneration(format!("JSON parse error: {e}"))
                })?;

            let html_path = self.report_dir.join(format!("{}.html", report_id));
            let html_size_bytes = regular_file_size_or_absent(&html_path, MAX_REPORT_HTML_BYTES)?;

            reports.push(ReportMetadata {
                report_id,
                test_type: infer_test_type(&json_value),
                generated_at: infer_generated_at(&json_value),
                firmware_version: infer_firmware_version(&json_value),
                html_size_bytes,
                json_size_bytes: json_bytes.len() as u64,
                grade: infer_grade(&json_value),
            });
        }

        reports.sort_by(|a, b| b.generated_at.cmp(&a.generated_at));
        Ok(reports)
    }

    /// Delete a stored report (both HTML and JSON).
    pub fn delete_report(&self, test_id: &Uuid) -> crate::Result<()> {
        let _storage_guard = lock_report_storage();
        self.delete_report_unlocked(test_id)
    }

    fn delete_report_unlocked(&self, test_id: &Uuid) -> crate::Result<()> {
        let html_path = self.report_dir.join(format!("{}.html", test_id));
        let json_path = self.report_dir.join(format!("{}.json", test_id));

        // Remove the commit marker first so a partially completed deletion is
        // never advertised by list_reports as a complete report pair.
        remove_report_file(&json_path)?;
        remove_report_file(&html_path)?;

        Ok(())
    }

    /// Enforce the maximum report count by deleting the oldest reports.
    fn enforce_max_reports_unlocked(&self, protected_report_id: &Uuid) -> crate::Result<()> {
        if !self.report_dir.exists() {
            return Ok(());
        }

        // A crash after HTML publication but before the JSON commit marker can
        // leave a bounded orphan. Reap such UUID-scoped artifacts on the next
        // successful save so repeated interrupted generations cannot consume
        // unbounded writable storage. Non-report names remain untouched.
        self.cleanup_orphan_html_unlocked()?;

        let mut json_entries = Vec::new();
        for entry in std::fs::read_dir(&self.report_dir).map_err(crate::DiagnosticError::Io)? {
            let entry = entry.map_err(crate::DiagnosticError::Io)?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(report_id) = Uuid::parse_str(stem) else {
                continue;
            };
            let file_type = entry.file_type().map_err(crate::DiagnosticError::Io)?;
            if !file_type.is_file() {
                return Err(invalid_report_file(&path, "is not a regular file"));
            }
            json_entries.push((report_id, entry));
        }

        if json_entries.len() <= self.max_reports {
            return Ok(());
        }

        let protected_name = std::ffi::OsString::from(format!("{protected_report_id}.json"));
        json_entries.sort_by(|(_, left), (_, right)| {
            let protected_order =
                (left.file_name() == protected_name).cmp(&(right.file_name() == protected_name));
            let left_modified = left
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let right_modified = right
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            protected_order.then_with(|| {
                left_modified
                    .cmp(&right_modified)
                    .then_with(|| left.file_name().cmp(&right.file_name()))
            })
        });

        let delete_count = json_entries.len() - self.max_reports;

        for (report_id, _) in json_entries.into_iter().take(delete_count) {
            self.delete_report_unlocked(&report_id)?;
        }

        Ok(())
    }

    fn cleanup_orphan_html_unlocked(&self) -> crate::Result<()> {
        for entry in std::fs::read_dir(&self.report_dir).map_err(crate::DiagnosticError::Io)? {
            let entry = entry.map_err(crate::DiagnosticError::Io)?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("html") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(report_id) = Uuid::parse_str(stem) else {
                continue;
            };
            let json_path = self.report_dir.join(format!("{report_id}.json"));
            match std::fs::symlink_metadata(&json_path) {
                Ok(metadata) if metadata.file_type().is_file() => {}
                Ok(_) => return Err(invalid_report_file(&json_path, "is not a regular file")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    remove_report_file(&path)?;
                }
                Err(error) => return Err(crate::DiagnosticError::Io(error)),
            }
        }
        Ok(())
    }

    /// Get the report storage directory path.
    pub fn report_dir(&self) -> &Path {
        &self.report_dir
    }
}

impl Default for ReportGenerator {
    fn default() -> Self {
        Self::new()
    }
}

fn grade_color(grade: char) -> &'static str {
    match grade {
        'A' => "#16a34a",
        'B' => "#65a30d",
        'C' => "#ca8a04",
        'D' => "#ea580c",
        _ => "#dc2626",
    }
}

fn evidence_label<T: PartialEq>(evidence: &DiagnosticEvidence<T>, value: &T) -> String {
    if evidence.is_measured_for(value) {
        "measured".to_string()
    } else if evidence.kind() == EvidenceKind::Measured {
        "invalid measured claim; not eligible".to_string()
    } else {
        format!("{}; not measured", evidence.kind().as_str())
    }
}

fn shared_report_css() -> &'static str {
    "body{font-family:Arial,sans-serif;margin:24px;background:#0b1020;color:#e5e7eb}h1,h2,h3,h4{margin:0 0 12px}.card{background:#111827;border:1px solid #1f2937;border-radius:12px;padding:16px;margin:16px 0}table{width:100%;border-collapse:collapse}th,td{padding:8px;border-bottom:1px solid #1f2937;text-align:left;vertical-align:top}.chip-grid{display:grid;gap:4px;margin-top:12px}.chip-cell{min-height:22px;border-radius:4px;display:flex;align-items:center;justify-content:center;font-size:10px;font-weight:bold;color:#fff}.chip-cell span{mix-blend-mode:screen}ul{margin:8px 0 0 20px}p{margin:0 0 12px}"
}

fn render_list(items: &[String]) -> String {
    if items.is_empty() {
        "<p>None.</p>".to_string()
    } else {
        format!(
            "<ul>{}</ul>",
            items
                .iter()
                .map(|item| format!("<li>{}</li>", escape_html(item)))
                .collect::<Vec<_>>()
                .join("")
        )
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn infer_test_type(json_data: &serde_json::Value) -> String {
    if let Some(test_type) = json_data
        .get("report_type")
        .and_then(|value| value.as_str())
    {
        return test_type.to_string();
    }
    if json_data.get("report_version").is_some() {
        return "hashreport".to_string();
    }
    if json_data.is_array() {
        return "board_health_snapshot".to_string();
    }
    "diagnostic_snapshot".to_string()
}

fn infer_generated_at(json_data: &serde_json::Value) -> String {
    json_data
        .get("generated_at")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn infer_firmware_version(json_data: &serde_json::Value) -> String {
    json_data
        .get("firmware_version")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn infer_grade(json_data: &serde_json::Value) -> Option<String> {
    json_data
        .get("unit_grade")
        .or_else(|| json_data.get("grade"))
        .and_then(|value| match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
}

#[cfg(test)]
mod evidence_rendering_tests {
    use super::*;

    #[test]
    fn report_labels_commanded_values_as_not_measured() {
        let evidence = DiagnosticEvidence::commanded(13.7f32, "runtime_setpoint", None);
        assert_eq!(evidence_label(&evidence, &13.7), "commanded; not measured");
    }

    #[test]
    fn report_does_not_render_an_incoherent_measured_claim_as_measured() {
        let evidence: DiagnosticEvidence<u32> = serde_json::from_value(serde_json::json!({
            "kind": "measured",
            "value": 0,
            "source": "",
            "quality": "observed"
        }))
        .unwrap();
        assert_eq!(
            evidence_label(&evidence, &0),
            "invalid measured claim; not eligible"
        );
    }
}

#[cfg(all(test, unix))]
mod report_storage_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn scratch_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "dcentrald-diagnostic-report-test-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn sample_json(generation: u64) -> serde_json::Value {
        serde_json::json!({
            "report_type": "storage_contract_test",
            "generated_at": format!("generation-{generation}"),
            "firmware_version": "test",
            "grade": "C"
        })
    }

    #[test]
    fn report_pair_is_private_bounded_and_durably_deletable() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();

        let metadata = generator
            .save_report(&id, Some("<html>evidence</html>"), &sample_json(1))
            .unwrap();
        assert_eq!(metadata.report_id, id);
        assert_eq!(
            generator.load_report_html(&id).unwrap(),
            "<html>evidence</html>"
        );
        assert_eq!(
            generator.load_report_json(&id).unwrap()["generated_at"],
            "generation-1"
        );
        assert_eq!(generator.list_reports().unwrap().len(), 1);

        for extension in ["html", "json"] {
            let mode = std::fs::metadata(dir.join(format!("{id}.{extension}")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        generator.delete_report(&id).unwrap();
        assert!(!dir.join(format!("{id}.html")).exists());
        assert!(!dir.join(format!("{id}.json")).exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn oversized_html_is_rejected_before_either_pair_member_is_published() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let html = "x".repeat(MAX_REPORT_HTML_BYTES + 1);

        let error = generator
            .save_report(&id, Some(&html), &sample_json(1))
            .unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::InvalidInput
        ));
        assert!(!dir.join(format!("{id}.html")).exists());
        assert!(!dir.join(format!("{id}.json")).exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn oversized_pretty_json_is_bounded_before_either_pair_member_is_published() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let json = serde_json::json!({"large": "x".repeat(MAX_REPORT_JSON_BYTES)});

        let error = generator
            .save_report(&id, Some("html must not be published"), &json)
            .unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::InvalidInput
        ));
        assert!(!dir.join(format!("{id}.html")).exists());
        assert!(!dir.join(format!("{id}.json")).exists());
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn symlink_commit_marker_is_rejected_before_html_mutation() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let referent = dir.join("outside.json");
        let json_path = dir.join(format!("{id}.json"));
        let html_path = dir.join(format!("{id}.html"));
        std::fs::write(&referent, b"outside").unwrap();
        symlink(&referent, &json_path).unwrap();

        let error = generator
            .save_report(&id, Some("new html"), &sample_json(1))
            .unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::InvalidInput
        ));
        assert_eq!(std::fs::read(&referent).unwrap(), b"outside");
        assert!(!html_path.exists());

        std::fs::remove_file(&json_path).unwrap();
        std::fs::remove_file(&referent).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn orphan_html_requires_explicit_deletion_before_uuid_reuse() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let html_path = dir.join(format!("{id}.html"));
        std::fs::write(&html_path, b"orphan html").unwrap();

        let error = generator
            .save_report(&id, None, &sample_json(2))
            .unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(std::fs::read(&html_path).unwrap(), b"orphan html");
        assert!(!dir.join(format!("{id}.json")).exists());

        generator.delete_report(&id).unwrap();
        generator.save_report(&id, None, &sample_json(2)).unwrap();
        assert!(!html_path.exists());
        assert_eq!(
            generator.load_report_json(&id).unwrap()["generated_at"],
            "generation-2"
        );
        generator.delete_report(&id).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn committed_report_id_is_immutable_and_pair_is_unchanged() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        generator
            .save_report(&id, Some("generation one"), &sample_json(1))
            .unwrap();

        let error = generator
            .save_report(&id, Some("generation two"), &sample_json(2))
            .unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(generator.load_report_html(&id).unwrap(), "generation one");
        assert_eq!(
            generator.load_report_json(&id).unwrap()["generated_at"],
            "generation-1"
        );

        generator.delete_report(&id).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn bounded_reader_rejects_sparse_oversized_report() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let json_path = dir.join(format!("{id}.json"));
        let file = std::fs::File::create(&json_path).unwrap();
        file.set_len((MAX_REPORT_JSON_BYTES + 1) as u64).unwrap();

        let error = generator.load_report_json(&id).unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::InvalidData
        ));

        std::fs::remove_file(&json_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn orphan_html_is_not_served_without_the_json_commit_marker() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let id = Uuid::new_v4();
        let html_path = dir.join(format!("{id}.html"));
        std::fs::write(&html_path, b"orphan").unwrap();

        let error = generator.load_report_html(&id).unwrap_err();
        assert!(matches!(
            error,
            crate::DiagnosticError::Io(ref io) if io.kind() == io::ErrorKind::NotFound
        ));

        std::fs::remove_file(&html_path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn retention_never_evicts_the_report_being_committed() {
        let dir = scratch_dir();
        let mut generator = ReportGenerator::with_dir(dir.clone());
        generator.max_reports = 1;
        let prior_id = Uuid::new_v4();
        let committed_id = Uuid::new_v4();
        generator
            .save_report(&prior_id, Some("prior"), &sample_json(1))
            .unwrap();
        generator
            .save_report(&committed_id, Some("committed"), &sample_json(2))
            .unwrap();

        assert!(!dir.join(format!("{prior_id}.json")).exists());
        assert!(!dir.join(format!("{prior_id}.html")).exists());
        assert!(dir.join(format!("{committed_id}.json")).is_file());
        assert_eq!(generator.list_reports().unwrap().len(), 1);

        generator.delete_report(&committed_id).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn retention_ignores_non_report_json_names() {
        let dir = scratch_dir();
        let mut generator = ReportGenerator::with_dir(dir.clone());
        generator.max_reports = 1;
        std::fs::write(dir.join("unrelated.json"), b"not a diagnostic report").unwrap();
        let prior_id = Uuid::new_v4();
        let committed_id = Uuid::new_v4();
        generator
            .save_report(&prior_id, Some("prior"), &sample_json(1))
            .unwrap();
        generator
            .save_report(&committed_id, Some("committed"), &sample_json(2))
            .unwrap();

        assert!(!dir.join(format!("{prior_id}.json")).exists());
        assert!(dir.join(format!("{committed_id}.json")).is_file());
        assert!(dir.join("unrelated.json").is_file());

        generator.delete_report(&committed_id).unwrap();
        std::fs::remove_file(dir.join("unrelated.json")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn successful_save_reaps_uuid_scoped_orphan_html() {
        let dir = scratch_dir();
        let generator = ReportGenerator::with_dir(dir.clone());
        let orphan_id = Uuid::new_v4();
        let committed_id = Uuid::new_v4();
        let orphan_path = dir.join(format!("{orphan_id}.html"));
        std::fs::write(&orphan_path, b"interrupted generation").unwrap();

        generator
            .save_report(&committed_id, Some("committed"), &sample_json(1))
            .unwrap();

        assert!(!orphan_path.exists());
        assert!(dir.join(format!("{committed_id}.json")).is_file());
        generator.delete_report(&committed_id).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
