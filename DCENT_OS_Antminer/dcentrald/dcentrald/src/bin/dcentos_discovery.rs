use serde_json::json;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const BITMAIN_PORT: u16 = 14235;
const DCENT_PORT: u16 = 14237;
const MDNS_ADDR: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const MDNS_PORT: u16 = 5353;
const PACKET_BURST: usize = 5;
const PACKET_INTERVAL: Duration = Duration::from_millis(200);
const BUTTON_POLL_INTERVAL: Duration = Duration::from_millis(200);
const BUTTON_DEBOUNCE: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, PartialEq)]
struct Config {
    once: bool,
    interval: Duration,
    button_gpio: Option<u32>,
    model_override: Option<String>,
    api_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            once: false,
            interval: Duration::from_secs(60),
            button_gpio: None,
            model_override: None,
            api_port: 8080,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Identity {
    model: String,
    hostname: String,
    ip: String,
    mac: String,
    platform: String,
    dcentrald_status: String,
    uptime_s: Option<u64>,
}

fn main() {
    init_logging();

    let config = match parse_args_from(env::args().skip(1)) {
        Ok(ParseOutcome::Run(config)) => config,
        Ok(ParseOutcome::Help) => {
            let _ = io::stdout().write_all(usage().as_bytes());
            return;
        }
        Err(err) => {
            let _ = writeln!(io::stderr(), "dcentos-discovery: {err}");
            let _ = io::stderr().write_all(usage().as_bytes());
            std::process::exit(2);
        }
    };

    if let Err(err) = run(config) {
        warn!(error = %err, "discovery service exited with error");
        std::process::exit(1);
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn run(config: Config) -> io::Result<()> {
    if config.once {
        return send_burst(&config);
    }

    let responder = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, BITMAIN_PORT))?;
    responder.set_nonblocking(true)?;
    info!(
        port = BITMAIN_PORT,
        "listening for Bitmain IP Reporter discovery requests"
    );

    let mut watcher = config.button_gpio.map(ButtonWatcher::new);
    if let Some(watcher) = watcher.as_ref() {
        info!(
            gpio = watcher.gpio,
            path = %watcher.path.display(),
            "watching recovery-button GPIO read-only"
        );
    }

    let mut next_periodic = if config.interval.is_zero() {
        None
    } else {
        Some(Instant::now())
    };

    loop {
        if let Some(next) = next_periodic {
            if Instant::now() >= next {
                if let Err(err) = send_burst(&config) {
                    warn!(error = %err, "periodic discovery burst failed");
                }
                next_periodic = Some(Instant::now() + config.interval);
            }
        }

        if let Some(watcher) = watcher.as_mut() {
            match watcher.poll_pressed_edge() {
                Ok(true) => {
                    info!(
                        gpio = watcher.gpio,
                        "recovery-button edge triggered discovery burst"
                    );
                    if let Err(err) = send_burst(&config) {
                        warn!(error = %err, "button-triggered discovery burst failed");
                    }
                }
                Ok(false) => {}
                Err(err) => watcher.report_error(err),
            }
        }

        poll_ip_reporter_requests(&responder, &config);
        thread::sleep(BUTTON_POLL_INTERVAL);
    }
}

enum ParseOutcome {
    Run(Config),
    Help,
}

fn parse_args_from<I, S>(args: I) -> Result<ParseOutcome, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut config = Config::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--once" => config.once = true,
            "--help" | "-h" => return Ok(ParseOutcome::Help),
            "--interval-seconds" => {
                let value = take_value(&mut args, "--interval-seconds")?;
                config.interval = Duration::from_secs(parse_u64(&value, "--interval-seconds")?);
            }
            "--button-gpio" => {
                let value = take_value(&mut args, "--button-gpio")?;
                config.button_gpio = Some(parse_u64(&value, "--button-gpio")? as u32);
            }
            "--model" => {
                let value = take_value(&mut args, "--model")?;
                config.model_override = Some(value);
            }
            "--api-port" => {
                let value = take_value(&mut args, "--api-port")?;
                let port = parse_u64(&value, "--api-port")?;
                if port == 0 || port > u16::MAX as u64 {
                    return Err("--api-port must be in 1..65535".to_string());
                }
                config.api_port = port as u16;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(ParseOutcome::Run(config))
}

fn take_value<I, S>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    args.next()
        .map(|value| value.as_ref().to_string())
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_u64(value: &str, flag: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{flag} must be an integer"))
}

fn usage() -> &'static str {
    "Usage: dcentos-discovery [--once] [--interval-seconds N] [--button-gpio N] [--model NAME] [--api-port N]\n"
}

fn send_burst(config: &Config) -> io::Result<()> {
    let identity = collect_identity(config.model_override.as_deref());
    let bitmain = build_bitmain_payload(&identity);
    let dcent = build_dcent_payload(&identity);
    let mdns = build_mdns_packet(&identity, config.api_port);

    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let _ = socket.set_multicast_ttl_v4(1);

    for _ in 0..PACKET_BURST {
        send_best_effort(
            &socket,
            &bitmain,
            SocketAddrV4::new(Ipv4Addr::BROADCAST, BITMAIN_PORT),
            "bitmain",
        );
        send_best_effort(
            &socket,
            &dcent,
            SocketAddrV4::new(Ipv4Addr::BROADCAST, DCENT_PORT),
            "dcent",
        );
        send_best_effort(
            &socket,
            &mdns,
            SocketAddrV4::new(MDNS_ADDR, MDNS_PORT),
            "mdns",
        );
        thread::sleep(PACKET_INTERVAL);
    }

    info!(
        model = %identity.model,
        ip = %identity.ip,
        mac = %identity.mac,
        "discovery burst sent"
    );
    Ok(())
}

fn send_best_effort(socket: &UdpSocket, payload: &[u8], addr: SocketAddrV4, label: &str) {
    if let Err(err) = socket.send_to(payload, addr) {
        warn!(target = label, error = %err, "discovery packet send failed");
    }
}

fn poll_ip_reporter_requests(socket: &UdpSocket, config: &Config) {
    let mut buf = [0u8; 512];

    for _ in 0..16 {
        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                let identity = collect_identity(config.model_override.as_deref());
                let ipmac = build_ipmac_payload(&identity);
                let bitmain = build_bitmain_payload(&identity);

                if let Err(err) = socket.send_to(&ipmac, peer) {
                    warn!(peer = %peer, error = %err, "ipmac discovery reply failed");
                }
                if let Err(err) = socket.send_to(&bitmain, peer) {
                    warn!(
                        peer = %peer,
                        error = %err,
                        "Bitmain-compatible discovery reply failed"
                    );
                }
                info!(
                    peer = %peer,
                    request_len = len,
                    ip = %identity.ip,
                    mac = %identity.mac,
                    "answered IP Reporter discovery request"
                );
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                warn!(error = %err, "IP Reporter discovery receive failed");
                break;
            }
        }
    }
}

fn collect_identity(model_override: Option<&str>) -> Identity {
    let platform = detect_platform();
    let model = model_override
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| detect_model(&platform));
    let hostname = detect_hostname();
    let ip = local_ipv4().to_string();
    let mac = detect_mac();
    let dcentrald_status = if process_alive("dcentrald") {
        "alive"
    } else {
        "dead"
    }
    .to_string();

    Identity {
        model,
        hostname,
        ip,
        mac,
        platform,
        dcentrald_status,
        uptime_s: read_uptime_s(),
    }
}

fn detect_model(platform: &str) -> String {
    for path in ["/etc/dcentos/model", "/etc/dcentos-platform"] {
        if let Some(value) = read_trimmed(path) {
            let model = model_from_platform_token(&value);
            if !model.is_empty() {
                return model;
            }
        }
    }

    if let Some(model) = read_toml_string("/etc/dcentrald.toml", "model") {
        return model_from_platform_token(&model);
    }

    if let Some(model) = read_device_tree_model() {
        if !model.is_empty() {
            return model;
        }
    }

    if platform_contains_am335x() {
        return "Antminer S19j Pro".to_string();
    }

    model_from_platform_token(platform)
}

fn model_from_platform_token(token: &str) -> String {
    let trimmed = token.trim();
    let lowered = trimmed.to_ascii_lowercase();

    match lowered.as_str() {
        "am3-bb" | "s19jpro-bb" | "s19j-pro-bb" => "Antminer S19j Pro".to_string(),
        "am3-aml-s19k" | "s19kpro" | "s19k-pro" => "Antminer S19K Pro".to_string(),
        "am3-aml-s19xp" | "s19xp" | "s19-xp" => "Antminer S19 XP".to_string(),
        "am3-aml-s21" | "am3-aml" | "s21" => "Antminer S21".to_string(),
        "am2-s19j" | "s19j" | "s19jpro" | "s19j-pro" => "Antminer S19j Pro".to_string(),
        "am1-s9" | "s9" => "Antminer S9".to_string(),
        _ => {
            if lowered.contains("am3-bb") || lowered.contains("s19j") {
                "Antminer S19j Pro".to_string()
            } else if lowered.contains("s19k") {
                "Antminer S19K Pro".to_string()
            } else if lowered.contains("s19xp") {
                "Antminer S19 XP".to_string()
            } else if lowered.contains("s21") || lowered.contains("am3-aml") {
                "Antminer S21".to_string()
            } else if lowered.contains("s9") {
                "Antminer S9".to_string()
            } else {
                trimmed.to_string()
            }
        }
    }
}

fn detect_platform() -> String {
    for path in [
        "/etc/dcentos/platform",
        "/etc/dcentos/board_target",
        "/etc/dcentos-platform",
    ] {
        if let Some(value) = read_trimmed(path) {
            if !value.is_empty() {
                return value;
            }
        }
    }

    if let Some(target) = read_toml_string("/etc/dcentrald.toml", "target") {
        return target;
    }

    if platform_contains_am335x() {
        return "am3-bb".to_string();
    }

    "unknown".to_string()
}

fn platform_contains_am335x() -> bool {
    read_trimmed("/proc/cpuinfo")
        .map(|cpuinfo| {
            let lowered = cpuinfo.to_ascii_lowercase();
            lowered.contains("am33xx") || lowered.contains("am335")
        })
        .unwrap_or(false)
}

fn detect_hostname() -> String {
    read_trimmed("/etc/hostname")
        .or_else(|| env::var("HOSTNAME").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "dcentos".to_string())
}

fn local_ipv4() -> Ipv4Addr {
    UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket.connect(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 80))?;
            socket.local_addr()
        })
        .ok()
        .and_then(|addr| match addr {
            SocketAddr::V4(addr) => Some(*addr.ip()),
            SocketAddr::V6(_) => None,
        })
        .unwrap_or(Ipv4Addr::UNSPECIFIED)
}

fn detect_mac() -> String {
    read_mac_path("/sys/class/net/eth0/address")
        .or_else(|| {
            fs::read_dir("/sys/class/net").ok().and_then(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .filter(|name| name != "lo")
                    .find_map(|name| read_mac_path(format!("/sys/class/net/{name}/address")))
            })
        })
        .unwrap_or_else(|| "00:00:00:00:00:00".to_string())
}

fn read_mac_path(path: impl AsRef<Path>) -> Option<String> {
    let mac = read_trimmed(path)?;
    if mac.len() >= 17 {
        Some(mac.to_ascii_uppercase())
    } else {
        None
    }
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim_matches(char::from(0)).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_device_tree_model() -> Option<String> {
    fs::read("/proc/device-tree/model").ok().and_then(|bytes| {
        String::from_utf8(bytes)
            .ok()
            .map(|value| value.trim_matches(char::from(0)).trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn read_toml_string(path: &str, key: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let mut parts = line.splitn(2, '=');
        let line_key = parts.next()?.trim();
        if line_key != key {
            continue;
        }
        let value = parts.next()?.trim().trim_matches('"').to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn process_alive(name: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };

    entries.flatten().any(|entry| {
        let Ok(pid) = entry.file_name().into_string() else {
            return false;
        };
        if !pid.chars().all(|ch| ch.is_ascii_digit()) {
            return false;
        }
        read_trimmed(entry.path().join("comm")).as_deref() == Some(name)
    })
}

fn read_uptime_s() -> Option<u64> {
    read_trimmed("/proc/uptime").and_then(|value| {
        value
            .split_whitespace()
            .next()
            .and_then(|seconds| seconds.parse::<f64>().ok())
            .map(|seconds| seconds as u64)
    })
}

fn build_bitmain_payload(identity: &Identity) -> Vec<u8> {
    format!("{},{},{}", identity.model, identity.ip, identity.mac).into_bytes()
}

fn build_ipmac_payload(identity: &Identity) -> Vec<u8> {
    format!("ipmac:{},{}", identity.ip, identity.mac).into_bytes()
}

fn build_dcent_payload(identity: &Identity) -> Vec<u8> {
    let payload = json!({
        "service": "dcentos-discovery",
        "firmware": "DCENT_OS",
        "model": identity.model,
        "hostname": identity.hostname,
        "platform": identity.platform,
        "ip": identity.ip,
        "mac": identity.mac,
        "dcentrald_status": identity.dcentrald_status,
        "uptime_s": identity.uptime_s,
        "ts": unix_time_s(),
    });
    serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec())
}

fn unix_time_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn build_mdns_packet(identity: &Identity, api_port: u16) -> Vec<u8> {
    let hostname = sanitize_dns_label(&identity.hostname);
    let service = "_dcentos._udp.local";
    let instance = format!("{hostname}.{service}");
    let target = format!("{hostname}.local");

    let mut packet = vec![0; 12];
    packet[2] = 0x84;
    packet[3] = 0x00;

    let mut record_count = 0u16;
    append_record(
        &mut packet,
        service,
        12,
        0x0001,
        120,
        encode_name(&instance),
    );
    record_count += 1;

    let mut srv = Vec::new();
    srv.extend_from_slice(&0u16.to_be_bytes());
    srv.extend_from_slice(&0u16.to_be_bytes());
    srv.extend_from_slice(&api_port.to_be_bytes());
    srv.extend_from_slice(&encode_name(&target));
    append_record(&mut packet, &instance, 33, 0x8001, 120, srv);
    record_count += 1;

    append_record(
        &mut packet,
        &instance,
        16,
        0x8001,
        120,
        encode_txt(identity, api_port),
    );
    record_count += 1;

    if let Ok(ip) = identity.ip.parse::<Ipv4Addr>() {
        if !ip.is_unspecified() {
            append_record(&mut packet, &target, 1, 0x8001, 120, ip.octets().to_vec());
            record_count += 1;
        }
    }

    packet[6..8].copy_from_slice(&record_count.to_be_bytes());
    packet
}

fn append_record(
    packet: &mut Vec<u8>,
    name: impl AsRef<str>,
    record_type: u16,
    class: u16,
    ttl: u32,
    rdata: Vec<u8>,
) {
    packet.extend_from_slice(&encode_name(name.as_ref()));
    packet.extend_from_slice(&record_type.to_be_bytes());
    packet.extend_from_slice(&class.to_be_bytes());
    packet.extend_from_slice(&ttl.to_be_bytes());
    packet.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    packet.extend_from_slice(&rdata);
}

fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        let safe = sanitize_dns_label(label);
        out.push(safe.len() as u8);
        out.extend_from_slice(safe.as_bytes());
    }
    out.push(0);
    out
}

fn sanitize_dns_label(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
        if out.len() >= 63 {
            break;
        }
    }

    while out.starts_with('-') {
        out.remove(0);
    }
    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        "dcentos".to_string()
    } else {
        out
    }
}

fn encode_txt(identity: &Identity, api_port: u16) -> Vec<u8> {
    let entries = [
        format!("model={}", identity.model),
        format!("platform={}", identity.platform),
        format!("ip={}", identity.ip),
        format!("mac={}", identity.mac),
        format!("api_port={api_port}"),
        format!("dcentrald={}", identity.dcentrald_status),
    ];

    let mut out = Vec::new();
    for entry in entries {
        let bytes = entry.as_bytes();
        let len = bytes.len().min(u8::MAX as usize);
        out.push(len as u8);
        out.extend_from_slice(&bytes[..len]);
    }
    out
}

struct ButtonWatcher {
    gpio: u32,
    path: PathBuf,
    last_pressed: Option<bool>,
    last_triggered: Instant,
    last_error_log: Instant,
}

impl ButtonWatcher {
    fn new(gpio: u32) -> Self {
        Self {
            gpio,
            path: PathBuf::from(format!("/sys/class/gpio/gpio{gpio}/value")),
            last_pressed: None,
            last_triggered: Instant::now() - BUTTON_DEBOUNCE,
            last_error_log: Instant::now() - Duration::from_secs(60),
        }
    }

    fn poll_pressed_edge(&mut self) -> io::Result<bool> {
        let raw = fs::read_to_string(&self.path)?;
        let pressed = raw.trim() == "0";
        let was_pressed = self.last_pressed.replace(pressed).unwrap_or(false);
        let edge = pressed && !was_pressed;

        if edge && self.last_triggered.elapsed() >= BUTTON_DEBOUNCE {
            self.last_triggered = Instant::now();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn report_error(&mut self, err: io::Error) {
        if self.last_error_log.elapsed() >= Duration::from_secs(60) {
            warn!(
                gpio = self.gpio,
                path = %self.path.display(),
                error = %err,
                "recovery-button GPIO read failed; continuing periodic beacon"
            );
            self.last_error_log = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> Identity {
        Identity {
            model: "Antminer S19j Pro".to_string(),
            hostname: "dcentos-bb".to_string(),
            ip: "192.0.2.79".to_string(),
            mac: "AA:BB:CC:DD:EE:FF".to_string(),
            platform: "am3-bb".to_string(),
            dcentrald_status: "alive".to_string(),
            uptime_s: Some(42),
        }
    }

    #[test]
    fn am3_bb_platform_tokens_map_to_s19j_pro() {
        assert_eq!(model_from_platform_token("am3-bb"), "Antminer S19j Pro");
        assert_eq!(model_from_platform_token("s19jpro-bb"), "Antminer S19j Pro");
    }

    #[test]
    fn bitmain_payload_matches_ip_reporter_format() {
        let payload = build_bitmain_payload(&sample_identity());
        assert_eq!(
            String::from_utf8(payload).unwrap(),
            "Antminer S19j Pro,192.0.2.79,AA:BB:CC:DD:EE:FF"
        );
    }

    #[test]
    fn ipmac_payload_matches_monitor_ipsig_responder_hint() {
        let payload = build_ipmac_payload(&sample_identity());
        assert_eq!(
            String::from_utf8(payload).unwrap(),
            "ipmac:192.0.2.79,AA:BB:CC:DD:EE:FF"
        );
    }

    #[test]
    fn dcent_payload_is_json_with_no_localhost_listener_field() {
        let payload = build_dcent_payload(&sample_identity());
        let value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(value["service"], "dcentos-discovery");
        assert_eq!(value["platform"], "am3-bb");
        assert!(String::from_utf8(payload).unwrap().find("22322").is_none());
    }

    #[test]
    fn mdns_packet_announces_dcentos_udp_service() {
        let packet = build_mdns_packet(&sample_identity(), 8080);
        assert_eq!(&packet[2..4], &[0x84, 0x00]);
        assert_eq!(u16::from_be_bytes([packet[6], packet[7]]), 4);
        assert!(packet
            .windows(b"_dcentos".len())
            .any(|window| window == b"_dcentos"));
        assert!(packet
            .windows(b"model=Antminer S19j Pro".len())
            .any(|window| window == b"model=Antminer S19j Pro"));
    }

    #[test]
    fn parse_args_supports_once_button_and_model() {
        let ParseOutcome::Run(config) = parse_args_from([
            "--once",
            "--interval-seconds",
            "5",
            "--button-gpio",
            "445",
            "--model",
            "Antminer S19j Pro",
            "--api-port",
            "8081",
        ])
        .unwrap() else {
            panic!("expected runnable config");
        };

        assert!(config.once);
        assert_eq!(config.interval, Duration::from_secs(5));
        assert_eq!(config.button_gpio, Some(445));
        assert_eq!(config.model_override.as_deref(), Some("Antminer S19j Pro"));
        assert_eq!(config.api_port, 8081);
    }
}
