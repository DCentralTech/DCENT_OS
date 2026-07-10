//! Phase 1 Python subprocess wrapper.
//!
//! Wraps existing Python diagnostic tools (asic_enumerator.py, psu_probe.py,
//! register_scanner.py, etc.) as subprocess invocations with --json output.
//! This enables shipping diagnostics immediately by reusing proven tools
//! that have been tested on live S9 hardware.
//!
//! Phase 1 Subprocess Flow:
//!   1. DiagnosticService receives test request via API
//!   2. Spawns Python tool as tokio::process::Command with --json flag
//!   3. Captures stdout, parses JSON into Rust structs
//!   4. Pushes progress updates via WebSocket
//!   5. Stores result in completed_tests deque
//!
//! Advantages:
//!   - Reuses proven tools tested on live S9 hardware
//!   - No new HAL code needed for Phase 1
//!   - Can ship diagnostics immediately
//!
//! Disadvantages:
//!   - Requires Python3 in rootfs (~3 MB size cost)
//!   - Process spawn overhead (~100ms per tool)
//!   - Cannot do fine-grained progress tracking within Python tools
//!
//! Phase 2 will rewrite all diagnostic logic in native Rust using
//! dcentrald-hal and dcentrald-asic crates.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::DiagnosticError;

/// Default directory containing Python diagnostic tools.
pub const TOOLS_DIR: &str = "/usr/share/dcentrald/tools";

/// Default Python interpreter path.
pub const PYTHON_PATH: &str = "/usr/bin/python3";

/// Maximum time a subprocess is allowed to run before being killed.
pub const DEFAULT_TIMEOUT_S: u64 = 300; // 5 minutes

/// Known Python diagnostic tool identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PythonTool {
    /// ASIC chip enumerator — detects chips on all chains.
    AsicEnumerator,
    /// PSU PMBus probe — reads power supply data.
    PsuProbe,
    /// FPGA register scanner — dumps FPGA registers.
    RegisterScanner,
    /// I2C bus scanner — probes I2C devices.
    I2cScanner,
    /// Network connectivity test.
    NetworkTest,
    /// Chain status monitor.
    ChainStatus,
    /// Fan control diagnostic.
    FanDiag,
}

impl PythonTool {
    /// Get the script filename for this tool.
    pub fn script_name(&self) -> &'static str {
        match self {
            PythonTool::AsicEnumerator => "asic_enumerator.py",
            PythonTool::PsuProbe => "psu_probe.py",
            PythonTool::RegisterScanner => "register_scanner.py",
            PythonTool::I2cScanner => "i2c_scanner.py",
            PythonTool::NetworkTest => "network_test.py",
            PythonTool::ChainStatus => "chain_status.py",
            PythonTool::FanDiag => "fan_diag.py",
        }
    }

    /// Get the expected timeout for this tool.
    pub fn timeout(&self) -> Duration {
        match self {
            PythonTool::AsicEnumerator => Duration::from_secs(60),
            PythonTool::PsuProbe => Duration::from_secs(30),
            PythonTool::RegisterScanner => Duration::from_secs(30),
            PythonTool::I2cScanner => Duration::from_secs(30),
            PythonTool::NetworkTest => Duration::from_secs(30),
            PythonTool::ChainStatus => Duration::from_secs(60),
            PythonTool::FanDiag => Duration::from_secs(30),
        }
    }
}

/// Result from a subprocess invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubprocessResult {
    /// Tool that was run.
    pub tool: String,
    /// Exit code (0 = success).
    pub exit_code: i32,
    /// Parsed JSON output (if successful).
    pub data: Option<serde_json::Value>,
    /// Raw stdout (for debugging).
    pub stdout: String,
    /// Raw stderr (for debugging).
    pub stderr: String,
    /// Execution time in milliseconds.
    pub duration_ms: u64,
}

/// Subprocess runner for Python diagnostic tools.
///
/// Manages the lifecycle of Python tool invocations, including:
/// - Path resolution for tool scripts
/// - Argument construction with --json flag
/// - Timeout enforcement
/// - Output capture and JSON parsing
/// - Error handling for missing tools, parse failures, timeouts
pub struct SubprocessRunner {
    /// Directory containing Python tool scripts.
    tools_dir: PathBuf,
    /// Path to Python interpreter.
    python_path: PathBuf,
}

impl SubprocessRunner {
    /// Create a new subprocess runner with default paths.
    pub fn new() -> Self {
        Self {
            tools_dir: PathBuf::from(TOOLS_DIR),
            python_path: PathBuf::from(PYTHON_PATH),
        }
    }

    /// Create a subprocess runner with custom paths.
    pub fn with_paths(tools_dir: PathBuf, python_path: PathBuf) -> Self {
        Self {
            tools_dir,
            python_path,
        }
    }

    /// Run a Python diagnostic tool and capture its JSON output.
    ///
    /// The tool is invoked with the --json flag to produce machine-readable
    /// output on stdout. Stderr is captured for error diagnostics.
    ///
    /// Returns parsed JSON on success, or a DiagnosticError on failure.
    pub async fn run_tool(
        &self,
        tool: PythonTool,
        args: &[&str],
    ) -> crate::Result<SubprocessResult> {
        let script_path = self.tools_dir.join(tool.script_name());

        if !script_path.exists() {
            return Err(DiagnosticError::Subprocess(format!(
                "Tool script not found: {}",
                script_path.display()
            )));
        }

        let start = std::time::Instant::now();

        // Build command: python3 /path/to/tool.py --json [extra args]
        let mut cmd = Command::new(&self.python_path);
        cmd.arg(&script_path)
            .arg("--json")
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Spawn with timeout
        let timeout = tool.timeout();
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| {
                DiagnosticError::Subprocess(format!(
                    "Tool {} timed out after {:?}",
                    tool.script_name(),
                    timeout
                ))
            })?
            .map_err(|e| {
                DiagnosticError::Subprocess(format!(
                    "Failed to spawn {}: {}",
                    tool.script_name(),
                    e
                ))
            })?;

        let duration_ms = start.elapsed().as_millis() as u64;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // Parse JSON from stdout
        let data = if exit_code == 0 {
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(json) => Some(json),
                Err(e) => {
                    tracing::warn!(
                        tool = tool.script_name(),
                        "Failed to parse JSON output: {}",
                        e
                    );
                    None
                }
            }
        } else {
            tracing::warn!(
                tool = tool.script_name(),
                exit_code,
                stderr = %stderr,
                "Tool exited with non-zero status"
            );
            None
        };

        Ok(SubprocessResult {
            tool: tool.script_name().to_string(),
            exit_code,
            data,
            stdout,
            stderr,
            duration_ms,
        })
    }

    /// Run a tool and extract the JSON data, returning an error if
    /// the tool failed or produced invalid JSON.
    pub async fn run_tool_json(
        &self,
        tool: PythonTool,
        args: &[&str],
    ) -> crate::Result<serde_json::Value> {
        let result = self.run_tool(tool, args).await?;

        if result.exit_code != 0 {
            return Err(DiagnosticError::Subprocess(format!(
                "Tool {} failed with exit code {}: {}",
                result.tool, result.exit_code, result.stderr
            )));
        }

        result.data.ok_or_else(|| {
            DiagnosticError::Subprocess(format!(
                "Tool {} produced no valid JSON output",
                result.tool
            ))
        })
    }

    /// Check if Python3 and the tools directory exist.
    pub fn is_available(&self) -> bool {
        self.python_path.exists() && self.tools_dir.exists()
    }

    /// List available tool scripts in the tools directory.
    pub fn list_available_tools(&self) -> Vec<PythonTool> {
        let tools = [
            PythonTool::AsicEnumerator,
            PythonTool::PsuProbe,
            PythonTool::RegisterScanner,
            PythonTool::I2cScanner,
            PythonTool::NetworkTest,
            PythonTool::ChainStatus,
            PythonTool::FanDiag,
        ];

        tools
            .iter()
            .filter(|t| self.tools_dir.join(t.script_name()).exists())
            .copied()
            .collect()
    }

    /// Get the tools directory path.
    pub fn tools_dir(&self) -> &Path {
        &self.tools_dir
    }
}

impl Default for SubprocessRunner {
    fn default() -> Self {
        Self::new()
    }
}
