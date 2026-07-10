//! Read-only bosminer telemetry and MMIO probe for `a lab unit`.
//!
//! This is a lab helper, not production firmware code. It subscribes to
//! `/tmp/bosminer_telemetry.sock`, opportunistically extracts JSON payloads from
//! the framed stream, optionally samples `:8081/metrics`, and can take read-only
//! am2 MMIO snapshots for a target chain.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dcentrald_hal::fpga_chain::{self, DevmemFpgaChain};
// W13.B1 (2026-05-10): renamed from `uart_relay::UartRelay`. The
// `0x43D000xx` window is a Braiins-am2 diagnostic mirror, NOT control.
// See dcentrald_asic::bm1362::uart_relay for the canonical control reg.
use dcentrald_hal::glitch_monitor::BraiinsGlitchMonitor;
use flate2::read::GzDecoder;
use serde_json::{Map, Value};

const DEFAULT_SOCKET_PATH: &str = "/tmp/bosminer_telemetry.sock";
const DEFAULT_DURATION_SECS: u64 = 30;
const DEFAULT_SAMPLE_INTERVAL_MS: u64 = 1000;
const DEFAULT_FPGA_CHAIN_ID: u8 = 4;
const TELEMETRY_BUFFER_LIMIT: usize = 1 << 20;
const JSON_PREFIX_HEX_BYTES: usize = 16;
const TELEMETRY_CONFIG_JSON: &[u8] = br#"{"session":{"tuner":{"level":"iter"}}}"#;

const INTERESTING_METRIC_PREFIXES: &[&str] = &[
    "miner_state ",
    "miner_pause_state ",
    "shared_psu_enable_state ",
    "shared_psu_voltage ",
    "shared_psu_heartbeats ",
    "stratum_accepted_submits_counter ",
    "stratum_rejected_submits_counter ",
    "stratum_accepted_shares_counter ",
    "stratum_rejected_shares_counter ",
    "total_stratum_accepted_shares_counter ",
    "total_stratum_rejected_shares_counter ",
    "hashchain_state{",
    "hashboard_shares{",
    "chip_frequency{",
    "chip_shares{",
    "chip_temperature_celsius{",
];

#[derive(Debug)]
struct Config {
    socket_path: String,
    duration: Duration,
    sample_interval: Duration,
    metrics_enabled: bool,
    raw_out: Option<PathBuf>,
    print_json: bool,
    fpga_chain_id: u8,
    fpga_base: Option<u64>,
}

#[derive(Debug)]
struct ExtractedJson {
    prefix_len: usize,
    prefix_hex: String,
    value: Value,
}

fn usage() {
    println!(
        "usage: bosminer_telemetry_dump [--seconds N] [--socket PATH] [--sample-interval-ms N] \\\n  [--no-metrics] [--fpga-base HEX] [--fpga-chain-id N] [--raw-out PATH] [--print-json]"
    );
    println!();
    println!("defaults:");
    println!("  --seconds {}", DEFAULT_DURATION_SECS);
    println!("  --socket {}", DEFAULT_SOCKET_PATH);
    println!("  --sample-interval-ms {}", DEFAULT_SAMPLE_INTERVAL_MS);
    println!("  --fpga-chain-id {}", DEFAULT_FPGA_CHAIN_ID);
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|e| format!("invalid {} '{}': {}", name, value, e))
}

fn parse_u8(name: &str, value: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|e| format!("invalid {} '{}': {}", name, value, e))
}

fn parse_hex_u64(name: &str, value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).map_err(|e| format!("invalid {} '{}': {}", name, value, e))
    } else {
        trimmed
            .parse::<u64>()
            .map_err(|e| format!("invalid {} '{}': {}", name, value, e))
    }
}

fn parse_args() -> Result<Config, String> {
    let mut socket_path = DEFAULT_SOCKET_PATH.to_string();
    let mut duration = Duration::from_secs(DEFAULT_DURATION_SECS);
    let mut sample_interval = Duration::from_millis(DEFAULT_SAMPLE_INTERVAL_MS);
    let mut metrics_enabled = true;
    let mut raw_out = None;
    let mut print_json = false;
    let mut fpga_chain_id = DEFAULT_FPGA_CHAIN_ID;
    let mut fpga_base = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                usage();
                std::process::exit(0);
            }
            "--seconds" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --seconds".to_string())?;
                duration = Duration::from_secs(parse_u64("seconds", &value)?);
            }
            "--socket" => {
                socket_path = args
                    .next()
                    .ok_or_else(|| "missing value for --socket".to_string())?;
            }
            "--sample-interval-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --sample-interval-ms".to_string())?;
                let ms = parse_u64("sample interval", &value)?;
                if ms == 0 {
                    return Err("sample interval must be > 0".to_string());
                }
                sample_interval = Duration::from_millis(ms);
            }
            "--no-metrics" => {
                metrics_enabled = false;
            }
            "--fpga-base" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --fpga-base".to_string())?;
                fpga_base = Some(parse_hex_u64("fpga base", &value)?);
            }
            "--fpga-chain-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --fpga-chain-id".to_string())?;
                fpga_chain_id = parse_u8("fpga chain id", &value)?;
            }
            "--raw-out" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --raw-out".to_string())?;
                raw_out = Some(PathBuf::from(value));
            }
            "--print-json" => {
                print_json = true;
            }
            _ => {
                return Err(format!("unknown argument: {}", arg));
            }
        }
    }

    Ok(Config {
        socket_path,
        duration,
        sample_interval,
        metrics_enabled,
        raw_out,
        print_json,
        fpga_chain_id,
        fpga_base,
    })
}

fn hex_preview(bytes: &[u8], limit: usize) -> String {
    let preview = bytes
        .iter()
        .take(limit)
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join("");
    if bytes.len() > limit {
        format!("{}...", preview)
    } else {
        preview
    }
}

fn find_json_end(bytes: &[u8]) -> Option<usize> {
    let first = *bytes.first()?;
    let mut stack = match first {
        b'{' => vec![b'}'],
        b'[' => vec![b']'],
        _ => return None,
    };
    let mut in_string = false;
    let mut escape = false;

    for (idx, byte) in bytes.iter().enumerate().skip(1) {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            match *byte {
                b'\\' => escape = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match *byte {
            b'"' => in_string = true,
            b'{' => stack.push(b'}'),
            b'[' => stack.push(b']'),
            b'}' | b']' => {
                let expected = stack.pop()?;
                if *byte != expected {
                    return None;
                }
                if stack.is_empty() {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }

    None
}

fn drain_json_records(buffer: &mut Vec<u8>) -> Vec<ExtractedJson> {
    let mut records = Vec::new();

    loop {
        let start = match buffer.iter().position(|b| matches!(*b, b'{' | b'[')) {
            Some(idx) => idx,
            None => {
                if buffer.len() > TELEMETRY_BUFFER_LIMIT {
                    eprintln!(
                        "telemetry buffer grew to {} bytes without JSON start; clearing",
                        buffer.len()
                    );
                    buffer.clear();
                }
                break;
            }
        };

        let prefix_hex = hex_preview(&buffer[..start], JSON_PREFIX_HEX_BYTES);
        let prefix_len = start;

        let Some(len) = find_json_end(&buffer[start..]) else {
            if start > 0 {
                buffer.drain(..start);
            }
            if buffer.len() > TELEMETRY_BUFFER_LIMIT {
                eprintln!(
                    "telemetry partial JSON exceeded {} bytes; dropping buffer",
                    TELEMETRY_BUFFER_LIMIT
                );
                buffer.clear();
            }
            break;
        };

        let end = start + len;
        let candidate = buffer[start..end].to_vec();
        match serde_json::from_slice::<Value>(&candidate) {
            Ok(value) => {
                records.push(ExtractedJson {
                    prefix_len,
                    prefix_hex,
                    value,
                });
                buffer.drain(..end);
            }
            Err(_) => {
                buffer.drain(..start + 1);
            }
        }
    }

    records
}

fn known_event_family(key: &str) -> bool {
    matches!(key, "session_dev" | "session_hb" | "iter_hb" | "event_dev")
}

fn split_event<'a>(value: &'a Value) -> (String, &'a Map<String, Value>) {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    let empty = EMPTY.get_or_init(Map::new);

    let Some(top) = value.as_object() else {
        return ("non_object".to_string(), empty);
    };

    if top.len() == 1 {
        let (key, nested) = top.iter().next().unwrap();
        if known_event_family(key) {
            if let Some(obj) = nested.as_object() {
                return (key.clone(), obj);
            }
        }
    }

    let family = top
        .get("event_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            top.keys()
                .find(|key| known_event_family(key))
                .cloned()
                .unwrap_or_else(|| "json".to_string())
        });

    (family, top)
}

fn fmt_value(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn summarize_json(value: &Value) -> String {
    let (family, body) = split_event(value);
    let sequence = fmt_value(
        body.get("global_counter")
            .or_else(|| body.get("telemetry_global_sequence_number"))
            .or_else(|| body.get("local_counter")),
    )
    .unwrap_or_else(|| "-".to_string());
    let event_type = fmt_value(body.get("event_type")).unwrap_or_else(|| family.clone());
    let session = fmt_value(body.get("session_id")).unwrap_or_else(|| "-".to_string());
    let fingerprint = fmt_value(body.get("hashboard_fingerprint"))
        .unwrap_or_else(|| fmt_value(body.get("chip_label")).unwrap_or_else(|| "-".to_string()));
    let hr = fmt_value(body.get("measured_hr")).unwrap_or_else(|| "-".to_string());
    let calc_hr = fmt_value(body.get("calculated_hr")).unwrap_or_else(|| "-".to_string());
    let total_ghs = fmt_value(body.get("total_ghs"))
        .or_else(|| fmt_value(body.get("total_hashrate_ghs")))
        .unwrap_or_else(|| "-".to_string());
    let power = fmt_value(body.get("measured_power"))
        .or_else(|| fmt_value(body.get("est_power")))
        .unwrap_or_else(|| "-".to_string());
    let fan_pwm = fmt_value(body.get("fan_pwm")).unwrap_or_else(|| "-".to_string());
    let errors = fmt_value(body.get("errors")).unwrap_or_else(|| "-".to_string());

    format!(
        "family={} event={} seq={} session={} fp={} measured_hr={} calculated_hr={} total_ghs={} power={} fan_pwm={} errors={}",
        family, event_type, sequence, session, fingerprint, hr, calc_hr, total_ghs, power, fan_pwm, errors
    )
}

fn split_http_body(response: &[u8]) -> Option<&[u8]> {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| &response[idx + 4..])
        .or_else(|| {
            response
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|idx| &response[idx + 2..])
        })
}

fn fetch_metrics_text() -> Result<String, String> {
    let mut stream = TcpStream::connect("127.0.0.1:8081")
        .map_err(|e| format!("connect 127.0.0.1:8081 failed: {}", e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| format!("set read timeout failed: {}", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| format!("set write timeout failed: {}", e))?;
    stream
        .write_all(
            b"GET /metrics HTTP/1.1\r\nHost: 127.0.0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        )
        .map_err(|e| format!("write request failed: {}", e))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("read response failed: {}", e))?;

    let body =
        split_http_body(&response).ok_or_else(|| "HTTP body separator not found".to_string())?;
    if body.starts_with(&[0x1F, 0x8B]) {
        let mut decoder = GzDecoder::new(body);
        let mut decoded = String::new();
        decoder
            .read_to_string(&mut decoded)
            .map_err(|e| format!("gunzip metrics failed: {}", e))?;
        Ok(decoded)
    } else {
        String::from_utf8(body.to_vec()).map_err(|e| format!("metrics utf8 decode failed: {}", e))
    }
}

fn interesting_metric_lines(metrics: &str) -> Vec<String> {
    metrics
        .lines()
        .filter(|line| {
            INTERESTING_METRIC_PREFIXES
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .map(str::to_string)
        .collect()
}

fn sample_mmio(fpga: &DevmemFpgaChain) -> String {
    let ctrl = fpga.read_raw(0x00);
    let build = fpga.read_raw(0x04);
    let baud = fpga.read_baud();
    let tx_ctrl = fpga.read_work_tx_ctrl();
    let tx_thr = fpga.read_work_tx_threshold();
    let tx_stat = fpga.read_work_tx_status();
    let tx_last = fpga.read_work_tx_last();
    let rx_stat = fpga.read_work_rx_status();

    format!(
        "ctrl=0x{ctrl:08X} build=0x{build:08X} baud=0x{baud:08X} tx_ctrl=0x{tx_ctrl:08X} tx_thr=0x{tx_thr:08X} tx_stat=0x{tx_stat:08X} tx_last=0x{tx_last:08X} rx_stat=0x{rx_stat:08X} tx_full={} irq={} rx_empty={}",
        tx_stat & fpga_chain::STAT_TX_FULL != 0,
        tx_stat & fpga_chain::STAT_IRQ != 0,
        rx_stat & fpga_chain::STAT_RX_EMPTY != 0,
    )
}

fn sample_glitch_mirror(monitor: &BraiinsGlitchMonitor) -> String {
    // Read mirror words via the new diagnostic API. Each call may fail
    // (offset out of bounds, etc.) — fall back to 0 for the print.
    let r28 = monitor.read_word(0x28).unwrap_or(0);
    let r2c = monitor.read_word(0x2C).unwrap_or(0);
    let r30 = monitor.read_word(0x30).unwrap_or(0);
    let r34 = monitor.read_word(0x34).unwrap_or(0);
    format!("r28=0x{r28:08X} r2c=0x{r2c:08X} r30=0x{r30:08X} r34=0x{r34:08X}",)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = match parse_args() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("argument error: {}", e);
            usage();
            std::process::exit(2);
        }
    };

    println!("== bosminer telemetry dump ==");
    println!("socket={}", config.socket_path);
    println!("duration_s={}", config.duration.as_secs());
    println!("sample_interval_ms={}", config.sample_interval.as_millis());
    println!("metrics_enabled={}", config.metrics_enabled);
    println!("print_json={}", config.print_json);
    if let Some(path) = config.raw_out.as_ref() {
        println!("raw_out={}", path.display());
    }
    if let Some(base) = config.fpga_base {
        println!(
            "fpga_chain_id={} fpga_base=0x{:08X}",
            config.fpga_chain_id, base
        );
    }

    let mut stream = None;
    let mut last_socket_error: Option<String> = None;
    let mut next_socket_retry = Instant::now();

    let mut raw_file = if let Some(path) = config.raw_out.as_ref() {
        Some(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?,
        )
    } else {
        None
    };

    let fpga = match config.fpga_base {
        Some(base) => match DevmemFpgaChain::open_am2(config.fpga_chain_id, base) {
            Ok(chain) => Some(chain),
            Err(e) => {
                eprintln!(
                    "mmio open failed for chain {} @ 0x{:08X}: {}",
                    config.fpga_chain_id, base, e
                );
                None
            }
        },
        None => None,
    };
    // W13.B1: open the Braiins-am2 glitch monitor via UIO. Default uio18
    // matches `a lab unit` BraiinsOS device-tree assignment; override via
    // `DCENT_BRAIINS_GLITCH_UIO` for other Braiins-am2 boxes.
    let glitch_uio: u8 = env::var("DCENT_BRAIINS_GLITCH_UIO")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(18);
    let relay = BraiinsGlitchMonitor::open(glitch_uio).ok();

    let start = Instant::now();
    let deadline = start + config.duration;
    let mut next_sample = start;
    let mut socket_buf = Vec::new();
    let mut read_buf = [0u8; 4096];
    let mut last_metrics: Option<Vec<String>> = None;
    let mut last_metrics_error: Option<String> = None;
    let mut last_mmio: Option<String> = None;
    let mut last_relay: Option<String> = None;

    while Instant::now() < deadline {
        let now = Instant::now();

        if stream.is_none() && now >= next_socket_retry {
            match UnixStream::connect(&config.socket_path) {
                Ok(mut sock) => {
                    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
                    sock.write_all(TELEMETRY_CONFIG_JSON)?;
                    println!(
                        "TELEM t={}ms telemetry socket connected + config sent ({})",
                        start.elapsed().as_millis(),
                        String::from_utf8_lossy(TELEMETRY_CONFIG_JSON)
                    );
                    stream = Some(sock);
                    last_socket_error = None;
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    if last_socket_error.as_ref() != Some(&msg) {
                        eprintln!(
                            "TELEM t={}ms telemetry socket unavailable: {}",
                            start.elapsed().as_millis(),
                            msg
                        );
                        last_socket_error = Some(msg);
                    }
                    next_socket_retry = now + config.sample_interval;
                }
            }
        }

        if now >= next_sample {
            let elapsed_ms = start.elapsed().as_millis();

            if config.metrics_enabled {
                match fetch_metrics_text() {
                    Ok(metrics) => {
                        last_metrics_error = None;
                        let lines = interesting_metric_lines(&metrics);
                        if last_metrics.as_ref() != Some(&lines) {
                            println!("METRICS t={}ms lines={}", elapsed_ms, lines.len());
                            for line in &lines {
                                println!("  {}", line);
                            }
                            last_metrics = Some(lines);
                        }
                    }
                    Err(e) => {
                        if last_metrics_error.as_ref() != Some(&e) {
                            eprintln!("METRICS t={}ms error={}", elapsed_ms, e);
                            last_metrics_error = Some(e);
                        }
                    }
                }
            }

            if let Some(fpga) = fpga.as_ref() {
                let snapshot = sample_mmio(fpga);
                if last_mmio.as_ref() != Some(&snapshot) {
                    println!(
                        "MMIO t={}ms chain={} {}",
                        elapsed_ms, config.fpga_chain_id, snapshot
                    );
                    last_mmio = Some(snapshot);
                }
            }

            if let Some(monitor) = relay.as_ref() {
                let snapshot = sample_glitch_mirror(monitor);
                if last_relay.as_ref() != Some(&snapshot) {
                    println!("GLITCH_MIRROR t={}ms {}", elapsed_ms, snapshot);
                    last_relay = Some(snapshot);
                }
            }

            next_sample += config.sample_interval;
        }

        if let Some(sock) = stream.as_mut() {
            match sock.read(&mut read_buf) {
                Ok(0) => {
                    eprintln!(
                        "TELEM t={}ms telemetry socket closed by peer",
                        start.elapsed().as_millis()
                    );
                    stream = None;
                    next_socket_retry = Instant::now() + config.sample_interval;
                }
                Ok(n) => {
                    if let Some(file) = raw_file.as_mut() {
                        file.write_all(&read_buf[..n])?;
                        file.flush()?;
                    }
                    socket_buf.extend_from_slice(&read_buf[..n]);

                    for record in drain_json_records(&mut socket_buf) {
                        let elapsed_ms = start.elapsed().as_millis();
                        println!(
                            "TELEM t={}ms prefix_len={} prefix_hex={} {}",
                            elapsed_ms,
                            record.prefix_len,
                            record.prefix_hex,
                            summarize_json(&record.value)
                        );
                        if config.print_json {
                            println!("JSON {}", serde_json::to_string(&record.value)?);
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => {
                    eprintln!(
                        "TELEM t={}ms telemetry socket read error: {}",
                        start.elapsed().as_millis(),
                        e
                    );
                    stream = None;
                    next_socket_retry = Instant::now() + config.sample_interval;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    println!("done elapsed_ms={}", start.elapsed().as_millis());
    Ok(())
}
