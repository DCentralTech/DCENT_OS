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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::board_health::BoardHealthResult;
use crate::chip_health::{ChipHealthSnapshot, ChipMap};
use crate::hashreport::HashReport;

/// Default report storage directory.
pub const REPORT_DIR: &str = "/data/reports";

/// Maximum number of stored reports before oldest is auto-deleted.
pub const MAX_STORED_REPORTS: usize = 20;

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
                "<section class=\"card\"><h3>Chain {}</h3><p>Grade <strong style=\"color:{}\">{}</strong> | {}/{} chips responding | {:.2} TH/s | {:.1} C | {:.2} V | CRC {}</p><table><thead><tr><th>Chip</th><th>Addr</th><th>Score</th><th>MHz</th><th>Grade</th></tr></thead><tbody>{}</tbody></table></section>",
                board.chain_id,
                grade_color(board.grade),
                board.grade,
                board.chips_responding,
                board.chips_expected,
                board.hashrate_ghs as f64 / 1000.0,
                board.temp_c,
                board.voltage_v,
                board.crc_errors,
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
                    "<tr><td>Chain {}</td><td><strong style=\"color:{}\">{}</strong></td><td>{}</td><td>{}/{}</td><td>{:.2} V</td><td>{:.1} C</td><td>{}</td></tr>",
                    result.chain_id,
                    grade_color(result.grade),
                    result.grade,
                    escape_html(&result.data_source),
                    result.chips_responding,
                    result.chips_expected,
                    result.voltage_readback_v,
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
                let voltage_cell = if result.measurement_type == "live_snapshot" {
                    format!(
                        "{:.2} V commanded setpoint &middot; rail NOT measured in snapshot mode &middot; {}",
                        result.voltage_setpoint_v,
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
                    "<section class=\"card\"><h3>Chain {}</h3><p>Grade <strong style=\"color:{}\">{}</strong> | {} | Source: {} | Measurement: {} | Status: {} | Estimated hashrate: {:.2} TH/s</p><table><tbody><tr><th>Chip Enumeration</th><td>{}/{} responding</td></tr><tr><th>Dead Chip Addresses</th><td>{}</td></tr><tr><th>Voltage</th><td>{}</td></tr><tr><th>CRC</th><td>{} commands, {} errors, {:.2}% rate, {}</td></tr><tr><th>Temperature</th><td>{:.1} C, {}</td></tr><tr><th>EEPROM</th><td>present={} valid={} model={} serial={}</td></tr></tbody></table><h4>Notes</h4>{}</section>",
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
                    if result.crc_ok { "OK" } else { "CHECK" },
                    result.temperature_c,
                    if result.temperature_ok { "OK" } else { "CHECK" },
                    result.eeprom_present,
                    result.eeprom_valid,
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
        std::fs::create_dir_all(&self.report_dir).map_err(crate::DiagnosticError::Io)?;

        let json_path = self.report_dir.join(format!("{}.json", test_id));
        let json_bytes = serde_json::to_vec_pretty(json_data).map_err(|e| {
            crate::DiagnosticError::ReportGeneration(format!("JSON serialization error: {e}"))
        })?;
        std::fs::write(&json_path, &json_bytes).map_err(crate::DiagnosticError::Io)?;

        let html_size_bytes = if let Some(html) = html {
            let html_path = self.report_dir.join(format!("{}.html", test_id));
            std::fs::write(&html_path, html).map_err(crate::DiagnosticError::Io)?;
            html.len() as u64
        } else {
            0
        };

        self.enforce_max_reports()?;

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
        let path = self.report_dir.join(format!("{}.html", test_id));
        std::fs::read_to_string(&path).map_err(crate::DiagnosticError::Io)
    }

    /// Load previously saved JSON data from disk.
    pub fn load_report_json(&self, test_id: &Uuid) -> crate::Result<serde_json::Value> {
        let path = self.report_dir.join(format!("{}.json", test_id));
        let data = std::fs::read_to_string(&path).map_err(crate::DiagnosticError::Io)?;
        serde_json::from_str(&data).map_err(|e| {
            crate::DiagnosticError::ReportGeneration(format!("JSON parse error: {}", e))
        })
    }

    /// List all stored report metadata.
    pub fn list_reports(&self) -> crate::Result<Vec<ReportMetadata>> {
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

            let json_text = std::fs::read_to_string(&path).map_err(crate::DiagnosticError::Io)?;
            let json_value: serde_json::Value = serde_json::from_str(&json_text).map_err(|e| {
                crate::DiagnosticError::ReportGeneration(format!("JSON parse error: {e}"))
            })?;

            let html_path = self.report_dir.join(format!("{}.html", report_id));
            let html_size_bytes = std::fs::metadata(&html_path)
                .map(|meta| meta.len())
                .unwrap_or(0);

            reports.push(ReportMetadata {
                report_id,
                test_type: infer_test_type(&json_value),
                generated_at: infer_generated_at(&json_value),
                firmware_version: infer_firmware_version(&json_value),
                html_size_bytes,
                json_size_bytes: json_text.len() as u64,
                grade: infer_grade(&json_value),
            });
        }

        reports.sort_by(|a, b| b.generated_at.cmp(&a.generated_at));
        Ok(reports)
    }

    /// Delete a stored report (both HTML and JSON).
    pub fn delete_report(&self, test_id: &Uuid) -> crate::Result<()> {
        let html_path = self.report_dir.join(format!("{}.html", test_id));
        let json_path = self.report_dir.join(format!("{}.json", test_id));

        if html_path.exists() {
            std::fs::remove_file(&html_path).map_err(crate::DiagnosticError::Io)?;
        }
        if json_path.exists() {
            std::fs::remove_file(&json_path).map_err(crate::DiagnosticError::Io)?;
        }

        Ok(())
    }

    /// Enforce the maximum report count by deleting the oldest reports.
    fn enforce_max_reports(&self) -> crate::Result<()> {
        if !self.report_dir.exists() {
            return Ok(());
        }

        let mut json_entries = Vec::new();
        for entry in std::fs::read_dir(&self.report_dir).map_err(crate::DiagnosticError::Io)? {
            let entry = entry.map_err(crate::DiagnosticError::Io)?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            json_entries.push(entry);
        }

        if json_entries.len() <= self.max_reports {
            return Ok(());
        }

        json_entries.sort_by_key(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        let delete_count = json_entries.len() - self.max_reports;

        for entry in json_entries.into_iter().take(delete_count) {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(report_id) = Uuid::parse_str(stem) else {
                continue;
            };
            self.delete_report(&report_id)?;
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
