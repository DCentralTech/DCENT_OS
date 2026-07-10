// DCENT_axe WiFi Provisioning — Captive Portal
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// On first boot (no saved config), starts a WiFi AP hotspot with a captive
// portal web page. The user connects, enters WiFi credentials + pool config,
// and the device reboots onto their network.
//
// Flow:
//   1. Start WiFi AP: "DCENTaxe_XXXX" (last 4 of MAC, open network)
//   2. Start HTTP server on 192.168.4.1
//   3. Serve captive portal HTML (config form)
//   4. On POST /api/config — validate, save to NVS, reboot
//   5. DNS redirect: all domains → 192.168.4.1 (captive portal detection)

use std::io::Write;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::http::server::{Configuration as HttpConfig, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::wifi::{AccessPointConfiguration, AuthMethod, Configuration, EspWifi};
use log::*;
use std::sync::{Arc, Mutex};

use dcentaxe_hal::board::{BitAxeModel, BoardConfig, BoardVersionProfile};
use dcentaxe_hal::display::Ssd1306Display;
use dcentaxe_hal::i2c::I2cBus;

use crate::config::DcentAxeConfig;
use crate::nvs_config;

struct ProvisioningSubmission {
    config: DcentAxeConfig,
    owner_password: String,
}

/// AP IP address — ESP-IDF v5.4 defaults to 192.168.71.1 for SoftAP.
const AP_IP: &str = "192.168.71.1";

/// CFG-6 / W2: documented lab bench escape for the safety gate. COMPILE-TIME
/// ONLY — delegates to the shared `dcentaxe_hal::safety::lab_safety_bypass_enabled()`
/// gate (set via `DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1` at build time) so the
/// submit-time, boot-time (`main.rs::unsafe_lab_safety_bypass_enabled`), and HAL
/// safety layers all read ONE source of truth. The previous `std::env::var`
/// runtime arm was dropped: on the ESP32 firmware target there is no process
/// environment, so a runtime arm falsely implied a runtime toggle and could make
/// two safety layers DISAGREE about whether the bypass is active.
fn unsafe_lab_safety_bypass_enabled() -> bool {
    dcentaxe_hal::safety::lab_safety_bypass_enabled()
}

fn provisioning_build_model() -> BitAxeModel {
    crate::config::default_model_for_build()
}

fn validate_provisioning_board_model(board_model: &str) -> Result<BitAxeModel, String> {
    let requested = BitAxeModel::from_device_model(board_model)
        .ok_or_else(|| format!("Unsupported board model: {board_model}"))?;
    if unsafe_lab_safety_bypass_enabled() {
        return Ok(requested);
    }

    let build_model = provisioning_build_model();
    if requested == build_model {
        Ok(requested)
    } else {
        Err(format!(
            "This firmware image is built for {} ({}), not {} ({}). Flash the matching DCENT_OS for ESP image for this board.",
            build_model.name(),
            build_model.board_target(),
            requested.name(),
            requested.board_target()
        ))
    }
}

fn random_setup_password() -> String {
    let mut bytes = [0u8; 6];
    unsafe {
        esp_idf_svc::sys::esp_fill_random(bytes.as_mut_ptr() as *mut _, bytes.len());
    }
    format!(
        "dcent-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

/// Start a minimal DNS server that resolves ALL queries to the AP IP.
/// This triggers captive portal detection on Android/iOS/Windows.
fn start_dns_server() {
    std::thread::Builder::new()
        .name("dns".into())
        .stack_size(4 * 1024)
        .spawn(|| {
            use std::net::UdpSocket;

            let socket = match UdpSocket::bind("192.168.71.1:53") {
                Ok(s) => s,
                Err(e) => {
                    log::warn!(
                        "DNS server failed to bind: {} — captive portal may not auto-launch",
                        e
                    );
                    return;
                }
            };

            let ap_ip: [u8; 4] = [192, 168, 71, 1];
            let mut buf = [0u8; 512];

            log::info!(
                "DNS server started — all queries resolve to {}.{}.{}.{}",
                ap_ip[0],
                ap_ip[1],
                ap_ip[2],
                ap_ip[3]
            );

            loop {
                let (len, src) = match socket.recv_from(&mut buf) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                if len < 12 {
                    continue;
                } // too short for DNS header

                // Build a minimal DNS response:
                // Copy the query, set response flags, append an A record answer
                let mut resp = Vec::with_capacity(len + 16);
                resp.extend_from_slice(&buf[..len]);

                // Set response flags: QR=1, OPCODE=0, AA=1, TC=0, RD=1, RA=1, RCODE=0
                resp[2] = 0x85; // QR=1, AA=1, RD=1
                resp[3] = 0x80; // RA=1, RCODE=0

                // Set ANCOUNT = 1 (one answer)
                resp[6] = 0x00;
                resp[7] = 0x01;

                // Append answer: name pointer (0xC00C = pointer to question name),
                // TYPE A (0x0001), CLASS IN (0x0001), TTL 60s, RDLENGTH 4, RDATA = IP
                resp.extend_from_slice(&[
                    0xC0, 0x0C, // name pointer to question
                    0x00, 0x01, // TYPE A
                    0x00, 0x01, // CLASS IN
                    0x00, 0x00, 0x00, 0x3C, // TTL = 60 seconds
                    0x00, 0x04, // RDLENGTH = 4
                    ap_ip[0], ap_ip[1], ap_ip[2], ap_ip[3], // RDATA = IP address
                ]);

                let _ = socket.send_to(&resp, src);
            }
        })
        .ok();
}

/// Start the provisioning captive portal. Blocks until config is saved.
///
/// Returns the saved config. After this returns, the caller should reboot.
pub fn run_provisioning(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs_partition: EspDefaultNvsPartition,
    mut nvs: EspNvs<NvsDefault>,
    display: &mut Ssd1306Display,
    i2c: &mut I2cBus<'_>,
) -> DcentAxeConfig {
    info!("========================================");
    info!("  DCENT_axe — WiFi Setup Mode");
    info!("  Connect to the WiFi hotspot below");
    info!("========================================");

    // Start WiFi in AP mode (must init before any network I/O — LwIP requires it)
    let mut wifi =
        EspWifi::new(modem, sysloop.clone(), Some(nvs_partition)).expect("WiFi AP init failed");

    // Get MAC address for unique AP name (use AP interface MAC)
    let mac = wifi.ap_netif().get_mac().unwrap_or([0; 6]);
    let ap_ssid = format!("DCENTaxe_{:02X}{:02X}", mac[4], mac[5]);
    let ap_password = random_setup_password();

    let mut ssid_buf = heapless::String::<32>::new();
    ssid_buf.push_str(&ap_ssid).ok();
    let mut pass_buf = heapless::String::<64>::new();
    pass_buf.push_str(&ap_password).ok();

    wifi.set_configuration(&Configuration::AccessPoint(AccessPointConfiguration {
        ssid: ssid_buf,
        channel: 1,
        ssid_hidden: false,
        auth_method: AuthMethod::WPA2Personal,
        password: pass_buf,
        max_connections: 4,
        ..Default::default()
    }))
    .expect("WiFi AP config failed");

    wifi.start().expect("WiFi AP start failed");

    // Wait for AP to fully initialize before starting services
    std::thread::sleep(std::time::Duration::from_millis(500));

    info!(
        "WiFi AP started: '{}' — connect and open http://{}",
        ap_ssid, AP_IP
    );

    // Show setup info on display (no-op on headless boards)
    display.show_status(
        i2c,
        "DCENT_axe Setup",
        &format!("WiFi: {}", ap_ssid),
        &format!("Pass: {}", ap_password),
        &format!("http://{}", AP_IP),
    );

    // Start DNS server AFTER WiFi (LwIP TCP/IP stack must be initialized first)
    start_dns_server();

    // Shared state for the HTTP server to signal completion
    let config_result: Arc<Mutex<Option<DcentAxeConfig>>> = Arc::new(Mutex::new(None));

    // Start HTTP server
    let server_config = HttpConfig {
        stack_size: 8192,
        ..Default::default()
    };
    let mut server = EspHttpServer::new(&server_config).expect("HTTP server start failed");

    // GET / — serve the captive portal HTML page.
    // Captures the AP SSID + password so the page can echo them back — users
    // who disconnect from the hotspot and need to rejoin otherwise have no
    // way to recover the randomly-generated password.
    let ssid_for_portal = ap_ssid.clone();
    let pass_for_portal = ap_password.clone();
    server
        .fn_handler(
            "/",
            Method::Get,
            move |req| -> Result<(), Box<dyn std::error::Error>> {
                let page = portal_html(&ssid_for_portal, &pass_for_portal);
                let mut resp = req.into_ok_response()?;
                let _ = resp.write(page.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register GET /");

    // GET /generate_204, /hotspot-detect.html, /connecttest.txt — captive portal detection
    // Android, iOS, Windows all check different URLs to detect captive portals.
    // Returning a redirect to / triggers the captive portal popup.
    for path in &[
        "/generate_204",
        "/hotspot-detect.html",
        "/connecttest.txt",
        "/redirect",
        "/canonical.html",
        "/success.txt",
        "/ncsi.txt",
        "/fwlink",
    ] {
        server
            .fn_handler(
                path,
                Method::Get,
                |req| -> Result<(), Box<dyn std::error::Error>> {
                    let mut resp = req.into_response(
                        302,
                        None,
                        &[("Location", "/"), ("Cache-Control", "no-cache")],
                    )?;
                    let _ = resp.write(b"Redirecting to setup page...");
                    Ok(())
                },
            )
            .expect("Failed to register captive portal redirect");
    }

    // GET /api/scan — scan for WiFi networks and return JSON list
    // Note: We can't scan while in AP-only mode on all ESP32 variants.
    // Some support AP+STA, but for reliability we return a simple text input.

    // POST /api/config — receive config JSON, validate, save to NVS
    let config_clone = config_result.clone();
    let nvs_mutex = Arc::new(Mutex::new(nvs));

    server
        .fn_handler(
            "/api/config",
            Method::Post,
            move |mut req| -> Result<(), Box<dyn std::error::Error>> {
                // CFG-2: accumulate the full POST body (a complete config can
                // exceed a single buffered read and embedded-io's Read returns
                // only what is currently buffered) up to MAX_CONFIG_SIZE. An
                // over-cap body is REJECTED (HTTP 413), never silently truncated.
                let body = match read_full_body(&mut req, nvs_config::MAX_CONFIG_SIZE) {
                    Ok(b) => b,
                    Err(BodyReadError::TooLarge) => {
                        let msg =
                            format!("Config body exceeds {} bytes", nvs_config::MAX_CONFIG_SIZE);
                        let mut resp = req.into_response(413, Some(&msg), &[])?;
                        let _ = resp.write(msg.as_bytes());
                        return Ok(());
                    }
                    Err(BodyReadError::Io(e)) => {
                        let msg = format!("Body read error: {}", e);
                        error!("Provisioning: {}", msg);
                        let mut resp = req.into_response(400, Some(&msg), &[])?;
                        let _ = resp.write(msg.as_bytes());
                        return Ok(());
                    }
                };
                let len = body.len();
                let body_str = std::str::from_utf8(&body).unwrap_or("");

                info!("Provisioning: received config ({} bytes)", len);

                // Parse the form data (application/x-www-form-urlencoded or JSON)
                let submission = if body_str.starts_with('{') {
                    match parse_json_submission(body_str) {
                        Ok(submission) => submission,
                        Err(e) => {
                            let msg = format!("Invalid JSON: {}", e);
                            let mut resp = req.into_response(400, Some(&msg), &[])?;
                            let _ = resp.write(msg.as_bytes());
                            return Ok(());
                        }
                    }
                } else {
                    // URL-encoded form data
                    match parse_form_submission(body_str) {
                        Ok(submission) => submission,
                        Err(e) => {
                            let msg = format!("Invalid form data: {}", e);
                            let mut resp = req.into_response(400, Some(&msg), &[])?;
                            let _ = resp.write(msg.as_bytes());
                            return Ok(());
                        }
                    }
                };

                // Validate
                if submission.config.wifi_ssid.is_empty() {
                    let mut resp = req.into_response(400, Some("SSID required"), &[])?;
                    let _ = resp.write(b"WiFi SSID is required");
                    return Ok(());
                }

                // CFG-6: reject an unsafe custom mining config at SUBMIT TIME
                // instead of writing it to NVS and silently rebooting into a
                // non-mining (boot-time-refused) state. This HARDENS — it moves
                // the fail-closed master gate earlier; the boot-time
                // validate_safety in main.rs stays as the backstop (defense in
                // depth). A known-profile board (the common case) always passes,
                // so portal behavior is unchanged for normal users. The
                // documented lab bench escape (DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS=1)
                // still permits the bench exception.
                if let Err(e) = submission
                    .config
                    .validate_safety(unsafe_lab_safety_bypass_enabled())
                {
                    let msg = format!("Unsafe configuration rejected: {}", e);
                    warn!("Provisioning: {}", msg);
                    let mut resp = req.into_response(400, Some(&msg), &[])?;
                    let _ = resp.write(msg.as_bytes());
                    return Ok(());
                }

                // Save to NVS
                {
                    let mut nvs = nvs_mutex.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(e) = nvs_config::save_config(&mut nvs, &submission.config) {
                        let msg = format!("Save failed: {}", e);
                        error!("Provisioning: {}", msg);
                        let mut resp = req.into_response(500, Some(&msg), &[])?;
                        let _ = resp.write(msg.as_bytes());
                        return Ok(());
                    }
                    if let Err(e) = crate::auth::bootstrap_owner_password(
                        &mut nvs,
                        &submission.owner_password,
                        "owner-setup",
                    ) {
                        let msg = format!("Owner setup failed: {}", e);
                        error!("Provisioning: {}", msg);
                        let mut resp = req.into_response(500, Some(&msg), &[])?;
                        let _ = resp.write(msg.as_bytes());
                        return Ok(());
                    }
                    let _ = nvs.remove("force_setup");
                    let _ = nvs.remove("wifi_retries");
                }

                info!(
                    "Provisioning: config saved! SSID='{}', pool={}:{}",
                    submission.config.wifi_ssid,
                    // B-ESP-10: sanitize the pool URL (strip any user:pass@) in logs.
                    crate::shared::sanitize_pool_url(&submission.config.stratum.url),
                    submission.config.stratum.port
                );

                // Signal completion
                *config_clone.lock().unwrap_or_else(|e| e.into_inner()) = Some(submission.config);

                // Respond with success page
                let mut resp = req.into_ok_response()?;
                let _ = resp.write(SUCCESS_HTML.as_bytes());
                Ok(())
            },
        )
        .expect("Failed to register POST /api/config");

    // Wait for config to be submitted
    info!("Waiting for user to configure via http://{}...", AP_IP);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(config) = config_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            info!("Provisioning complete — rebooting in 3 seconds...");
            std::thread::sleep(std::time::Duration::from_secs(3));

            // Reboot
            unsafe {
                esp_idf_svc::sys::esp_restart();
            }

            // Unreachable, but return the config just in case
            return config;
        }
    }
}

/// Validate BTC address format (basic check: starts with bc1/1/3, length 26-62).
/// Returns true for valid-looking addresses. Some pools use custom worker name
/// formats — callers should treat failure as a WARNING, not a hard reject.
fn is_valid_btc_address(addr: &str) -> bool {
    if addr.len() < 26 || addr.len() > 62 {
        return false;
    }
    addr.starts_with("bc1") || addr.starts_with('1') || addr.starts_with('3')
}

fn build_submission(
    ssid: String,
    password: String,
    pool_url: String,
    pool_port: u16,
    worker: String,
    pool_pass: String,
    board_model: String,
    frequency: f32,
    voltage: u16,
    owner_password: String,
    owner_password_confirm: Option<String>,
) -> Result<ProvisioningSubmission, String> {
    if ssid.is_empty() {
        return Err("WiFi SSID is required".into());
    }

    if worker == "dcentaxe" || worker.is_empty() {
        return Err("Bitcoin address required as worker name. Enter your BTC address (bc1q..., 1..., or 3...).".into());
    }

    // CFG-12: reject an unconnectable pool endpoint (empty url / port 0 / a
    // scheme with no host) BEFORE we build + return the config the POST handler
    // saves and renders "Configuration Saved!" for. The caller surfaces this Err
    // as HTTP 400, so the operator is told instead of silently saving a config
    // the miner can never connect with.
    crate::config::validate_pool_endpoint(&pool_url, pool_port)?;

    if let Some(confirm) = owner_password_confirm {
        if confirm != owner_password {
            return Err("Owner password confirmation does not match".into());
        }
    }
    crate::auth::validate_owner_password(&owner_password).map_err(|detail| detail.to_string())?;

    // Validate BTC address format — warning only (some pools use custom worker name formats).
    // If the worker name contains a dot (e.g. "bc1q...address.workername"), validate the part before the dot.
    {
        let addr_part = worker.split('.').next().unwrap_or(&worker);
        if !is_valid_btc_address(addr_part) {
            // B-ESP-10: this warn is a read surface (serial console / log buffer);
            // the worker prefix is the operator BTC payout address, so mask it
            // (first6…last4) — enough to recognize the rejected value, no full leak.
            log::warn!(
                "Provisioning: worker name '{}' does not look like a BTC address \
                        (expected bc1.../1.../3..., length 26-62). \
                        Accepting anyway — may be a custom pool worker format.",
                crate::shared::mask_wallet(addr_part)
            );
        }
    }

    let resolved_model = validate_provisioning_board_model(&board_model)?;
    let resolved_profile = BoardVersionProfile::default_for_model(resolved_model);
    let resolved_board = BoardConfig::for_profile(resolved_profile);
    let base_config = DcentAxeConfig {
        wifi_ssid: ssid,
        wifi_password: password,
        stratum: dcentaxe_stratum::StratumConfig {
            url: pool_url,
            port: pool_port,
            worker_name: worker,
            password: pool_pass,
            suggest_difficulty: 0,
            // CFG-5: route through the shared classifier on the resolved chip
            // (provisioning always resolves a known profile, so behavior is
            // preserved — every supported chip but BM1397 rolls; this removes the
            // second hardcoded source of truth).
            version_rolling: crate::config::chip_rolls_versions(resolved_profile.asic_model),
        },
        board_model: resolved_model.canonical_key().to_string(),
        board_version: resolved_profile.board_version.to_string(),
        asic_model: resolved_profile.asic_model.to_string(),
        hostname: String::new(),
        target_frequency: resolved_board.default_frequency,
        target_voltage_mv: resolved_board.default_voltage_mv,
        fan_speed_pct: 100,
        asic_count: resolved_board.asic_count,
        overclock_enabled: false,
        display_inverted: false,
        fan_target_temp_c: crate::config::DEFAULT_FAN_TARGET_TEMP_C, // CFG-7
        fallback_pool: None,
        sv2_own_templates: crate::config::Sv2OwnTemplateConfig::default(),
        sv2_authority_pubkey: None,
        // MQTT/HA is opt-in from Settings, not the first-run provisioning form.
        mqtt: crate::config::MqttConfig::default(),
        metrics_require_auth: true,
        allow_unsigned_ota: false,
        split_pool: None,
        schedule_enabled: true,
        schedule_timezone_offset_minutes: 0,
        power_schedule: Vec::new(),
        hardware: None,
        room_temp_source: crate::config::RoomTempSource::Local,
        schema_version: crate::config::SCHEMA_VERSION,
    };
    let point = base_config.qualify_operating_point(
        if frequency > 0.0 {
            frequency
        } else {
            resolved_board.default_frequency
        },
        if voltage > 0 {
            voltage
        } else {
            resolved_board.default_voltage_mv
        },
        crate::config::ControlSurface::Provisioning,
    );

    let mut config = base_config;
    config.target_frequency = point.frequency_mhz;
    config.target_voltage_mv = point.voltage_mv;
    config.canonicalize_identity();
    Ok(ProvisioningSubmission {
        config,
        owner_password,
    })
}

/// Error from the bounded body reader (CFG-2).
enum BodyReadError {
    /// The body would exceed the supplied cap — reject with HTTP 413 rather than
    /// silently truncate.
    TooLarge,
    /// A non-retryable read error from the request connection.
    Io(String),
}

/// Read the full request body into a `Vec`, looping `req.read()` until EOF.
///
/// CFG-2: a single fixed-size `req.read()` can truncate a multi-segment body
/// (embedded-io's Read returns only what is currently buffered). This loops,
/// accumulating bytes, retrying on the esp-idf Timeout pseudo-error exactly like
/// the OTA receive path (`api.rs` reference loop), and REJECTS (returns
/// `TooLarge`) the moment accumulating the next chunk would push past `max` —
/// never truncating to a partial config. The per-read request is clamped to the
/// remaining capacity so a well-behaved client at the cap finishes cleanly.
fn read_full_body(
    req: &mut esp_idf_svc::http::server::Request<&mut esp_idf_svc::http::server::EspHttpConnection>,
    max: usize,
) -> Result<Vec<u8>, BodyReadError> {
    let mut body: Vec<u8> = Vec::new();
    let mut scratch = [0u8; 512];
    loop {
        // Clamp the next read so we never pull more than the remaining capacity
        // (+1 so we can DETECT an over-cap body rather than stop exactly at the
        // boundary and miss the overflow).
        let remaining = crate::config::next_take(body.len(), max);
        if remaining == 0 {
            // Already at the cap. Probe one more byte: if more data arrives the
            // body is over-cap and must be rejected; EOF means it fit exactly.
            match req.read(&mut scratch[..1]) {
                Ok(0) => break,
                Ok(_) => return Err(BodyReadError::TooLarge),
                Err(e) => {
                    if format!("{e}").contains("Timeout") {
                        continue;
                    }
                    return Err(BodyReadError::Io(format!("{e}")));
                }
            }
        }
        let take = remaining.min(scratch.len());
        match req.read(&mut scratch[..take]) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if !crate::config::body_read_capacity_ok(body.len(), n, max) {
                    return Err(BodyReadError::TooLarge);
                }
                body.extend_from_slice(&scratch[..n]);
            }
            Err(e) => {
                // esp-idf maps HTTPD_SOCK_ERR_TIMEOUT to a Timeout-flavored error;
                // retry like the OTA receive loop does.
                if format!("{e}").contains("Timeout") {
                    continue;
                }
                return Err(BodyReadError::Io(format!("{e}")));
            }
        }
    }
    Ok(body)
}

fn parse_json_submission(body: &str) -> Result<ProvisioningSubmission, String> {
    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let get_str = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| json.get(*key).and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let get_u16 = |keys: &[&str], default: u16| -> u16 {
        keys.iter()
            .find_map(|key| json.get(*key).and_then(|v| v.as_u64()))
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(default)
    };
    let get_f32 = |keys: &[&str]| -> f32 {
        keys.iter()
            .find_map(|key| json.get(*key).and_then(|v| v.as_f64()))
            .map(|v| v as f32)
            .unwrap_or(0.0)
    };

    build_submission(
        get_str(&["ssid"]),
        get_str(&["password", "wifi_password", "wifiPassword"]),
        get_str(&["pool_url", "stratumURL"]),
        get_u16(&["pool_port", "stratumPort"], 21496),
        get_str(&["worker", "stratumUser"]),
        {
            let pool_password = get_str(&["pool_password", "stratumPassword"]);
            if pool_password.is_empty() {
                "x".to_string()
            } else {
                pool_password
            }
        },
        {
            let model = get_str(&["board_model", "model"]);
            if model.is_empty() {
                DcentAxeConfig::default().board_model
            } else {
                model
            }
        },
        get_f32(&["frequency"]),
        get_u16(&["voltage"], 0),
        get_str(&["owner_password", "ownerPassword"]),
        Some(get_str(&["owner_password_confirm", "ownerPasswordConfirm"])),
    )
}

/// Parse URL-encoded form data into a provisioning submission.
fn parse_form_submission(body: &str) -> Result<ProvisioningSubmission, String> {
    let mut ssid = String::new();
    let mut password = String::new();
    let mut pool_url = String::from("public-pool.io");
    let mut pool_port: u16 = 21496;
    let mut worker = String::from("dcentaxe");
    let mut pool_pass = String::from("x");
    let default_cfg = DcentAxeConfig::default();
    let mut board_model = default_cfg.board_model.clone();
    let mut frequency: f32 = 0.0;
    let mut voltage: u16 = 0;
    let mut owner_password = String::new();
    let mut owner_password_confirm = String::new();

    for pair in body.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let value = kv.next().unwrap_or("");
        // CFG-8: UTF-8-correct URL-decode (+ → space, %XX → byte, multi-byte
        // sequences reassembled) — extracted into config.rs so it is host-tested.
        let value = crate::config::url_decode(value);

        match key {
            "ssid" => ssid = value,
            "password" | "wifi_password" => password = value,
            "pool_url" | "stratumURL" => pool_url = value,
            "pool_port" | "stratumPort" => pool_port = value.parse().unwrap_or(21496),
            "worker" | "stratumUser" => worker = value,
            "pool_password" => pool_pass = value,
            "board_model" | "model" => board_model = value,
            "frequency" => frequency = value.parse().unwrap_or(0.0),
            "voltage" => voltage = value.parse().unwrap_or(0),
            "owner_password" | "ownerPassword" => owner_password = value,
            "owner_password_confirm" | "ownerPasswordConfirm" => owner_password_confirm = value,
            _ => {}
        }
    }

    build_submission(
        ssid,
        password,
        pool_url,
        pool_port,
        worker,
        pool_pass,
        board_model,
        frequency,
        voltage,
        owner_password,
        Some(owner_password_confirm),
    )
}

// CFG-8: `url_decode` was extracted into `crate::config::url_decode` (host-tested
// for UTF-8 correctness). The form parser above calls it directly.

// ---------------------------------------------------------------------------
// Embedded HTML — captive portal setup page
// ---------------------------------------------------------------------------

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn portal_model_options() -> String {
    let model = provisioning_build_model();
    format!(
        r#"<option value="{}" selected>{} ({})</option>"#,
        html_escape(model.canonical_key()),
        html_escape(model.name()),
        html_escape(model.board_target())
    )
}

fn portal_html(ap_ssid: &str, ap_password: &str) -> String {
    let default_cfg = DcentAxeConfig::default();
    let stock = crate::config::stock_asic_settings(default_cfg.bitaxe_model());
    // Sanitise before embedding — the SSID is derived from MAC and the
    // password from CSPRNG hex so injection is effectively impossible, but
    // escape ampersands + angle brackets defensively anyway.
    PORTAL_HTML
        .replace("{{AP_SSID}}", &html_escape(ap_ssid))
        .replace("{{AP_PASSWORD}}", &html_escape(ap_password))
        .replace("{{DEFAULT_MODEL}}", &default_cfg.board_model)
        .replace("{{MODEL_OPTIONS}}", &portal_model_options())
        .replace("{{DEFAULT_FREQ}}", &stock.default_frequency.to_string())
        .replace("{{DEFAULT_VOLT}}", &stock.default_voltage_mv.to_string())
}

const PORTAL_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>DCENT_axe Setup</title>
<style>
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
background:#1a1a2e;color:#e0e0e0;min-height:100vh;display:flex;justify-content:center;
align-items:center;padding:20px}
.card{background:#16213e;border-radius:16px;padding:32px;max-width:440px;width:100%;
box-shadow:0 8px 32px rgba(0,0,0,0.4)}
h1{font-size:24px;margin-bottom:4px;color:#f97316}
.subtitle{color:#94a3b8;margin-bottom:24px;font-size:14px}
.section{margin-bottom:20px}
.section h2{font-size:14px;color:#f97316;text-transform:uppercase;letter-spacing:1px;
margin-bottom:12px;border-bottom:1px solid #334155;padding-bottom:4px}
label{display:block;font-size:13px;color:#94a3b8;margin-bottom:4px}
input,select{width:100%;padding:10px 12px;border:1px solid #334155;border-radius:8px;
background:#0f172a;color:#e0e0e0;font-size:15px;margin-bottom:12px;outline:none}
input:focus,select:focus{border-color:#f97316}
.row{display:flex;gap:12px}
.row>div{flex:1}
button{width:100%;padding:14px;background:#f97316;color:#fff;border:none;
border-radius:8px;font-size:16px;font-weight:600;cursor:pointer;margin-top:8px}
button:hover{background:#ea580c}
button:disabled{background:#64748b;cursor:not-allowed}
.footer{text-align:center;margin-top:16px;font-size:12px;color:#64748b}
.ap-creds{background:#0f172a;border:1px solid #f97316;border-radius:8px;padding:12px;margin-bottom:20px;font-size:12px}
.ap-creds b{color:#f97316}
.ap-creds code{display:inline-block;background:#1e293b;padding:2px 6px;border-radius:4px;font-family:ui-monospace,Menlo,Consolas,monospace;color:#e2e8f0;user-select:all}
.status{display:none;padding:12px;border-radius:8px;margin-top:12px;text-align:center}
.status.error{display:block;background:#7f1d1d;color:#fca5a5}
.status.saving{display:block;background:#1e3a5f;color:#93c5fd}
</style>
</head>
<body>
<div class="card">
<h1>DCENT_axe Setup</h1>
<p class="subtitle">D-Central Technologies — Mining Firmware</p>

<div class="ap-creds">
<div style="margin-bottom:6px"><b>Hotspot credentials</b> — note them in case you disconnect.</div>
<div>Network: <code>{{AP_SSID}}</code></div>
<div style="margin-top:4px">Password: <code>{{AP_PASSWORD}}</code></div>
</div>

<form id="configForm" action="/api/config" method="POST">
<div class="section">
<h2>WiFi Network</h2>
<label for="ssid">SSID (Network Name) *</label>
<input type="text" id="ssid" name="ssid" required placeholder="Your WiFi network">
<label for="password">Password</label>
<input type="password" id="password" name="password" placeholder="WiFi password">
</div>

<div class="section">
<h2>Mining Pool</h2>
<label for="pool_url">Pool URL</label>
<input type="text" id="pool_url" name="pool_url" value="public-pool.io">
<div class="row">
<div>
<label for="pool_port">Port</label>
<input type="number" id="pool_port" name="pool_port" value="21496">
</div>
<div>
<label for="pool_password">Password</label>
<input type="text" id="pool_password" name="pool_password" value="x">
</div>
</div>
<label for="worker">Worker Name (your BTC address)</label>
<input type="text" id="worker" name="worker" value="" required placeholder="bc1q... (required)">
<div style="font-size:11px;color:#64748b;margin-top:-8px;margin-bottom:12px">Your BTC wallet address. Mining rewards go here.</div>
</div>

<div class="section">
<h2>Hardware</h2>
<div class="row">
<div>
<label for="model">Board Model</label>
<select id="model" name="board_model">{{MODEL_OPTIONS}}</select>
</div>
<div></div>
</div>
<details style="margin-top:8px">
<summary style="color:#94a3b8;font-size:12px;cursor:pointer">Advanced Settings (optional)</summary>
<div class="row" style="margin-top:8px">
<div>
<label for="frequency">Frequency (MHz)</label>
<input type="number" id="frequency" name="frequency" value="{{DEFAULT_FREQ}}" step="5">
<div style="font-size:10px;color:#64748b">Default is safe. Higher = more heat.</div>
</div>
<div>
<label for="voltage">Voltage (mV)</label>
<input type="number" id="voltage" name="voltage" value="{{DEFAULT_VOLT}}" step="10">
<div style="font-size:10px;color:#64748b">Do not change unless you know what you're doing.</div>
</div>
</div>
</details>
</div>

<div class="section">
<h2>Owner Access</h2>
<label for="owner_password">Owner Password *</label>
<input type="password" id="owner_password" name="owner_password" required minlength="8" placeholder="Required to secure the miner">
<label for="owner_password_confirm">Confirm Owner Password *</label>
<input type="password" id="owner_password_confirm" name="owner_password_confirm" required minlength="8" placeholder="Repeat owner password">
<div style="font-size:11px;color:#64748b;margin-top:-8px;margin-bottom:12px">This password protects settings changes, MCP control, and authenticated updates after setup.</div>
</div>

<button type="submit" id="saveBtn">Secure & Connect</button>
<div id="status" class="status"></div>
</form>

<div class="footer">DCENT_axe v0.1.0 &mdash; GPL-3.0</div>
</div>

<script>
const defaultModel = '{{DEFAULT_MODEL}}';
const freqDefaults = {
  max: 425, ultra: 485, hexultra: 485,
  supra: 490, hexsupra: 490, gamma: 525,
  gammaduo: 400, gammaturbo: 525
};
const voltDefaults = {
  max: 1400, ultra: 1200, hexultra: 1200,
  supra: 1166, hexsupra: 1166, gamma: 1150,
  gammaduo: 1150, gammaturbo: 1150
};
function applyModelDefaults(model){
  if(freqDefaults[model]) document.getElementById('frequency').value = freqDefaults[model];
  if(voltDefaults[model]) document.getElementById('voltage').value = voltDefaults[model];
}
document.getElementById('model').value = defaultModel;
applyModelDefaults(defaultModel);
document.getElementById('model').addEventListener('change', function() {
  applyModelDefaults(this.value);
});

// Submit handler with status feedback
document.getElementById('configForm').addEventListener('submit', function(e) {
  const worker = document.getElementById('worker').value.trim();
  const ownerPass = document.getElementById('owner_password').value;
  const ownerConfirm = document.getElementById('owner_password_confirm').value;
  if (!worker || worker === 'dcentaxe') {
    e.preventDefault();
    document.getElementById('status').className = 'status error';
    document.getElementById('status').textContent = 'Bitcoin address is required as worker name.';
    return;
  }
  if (ownerPass.length < 8) {
    e.preventDefault();
    document.getElementById('status').className = 'status error';
    document.getElementById('status').textContent = 'Owner password must be at least 8 characters.';
    return;
  }
  if (ownerPass !== ownerConfirm) {
    e.preventDefault();
    document.getElementById('status').className = 'status error';
    document.getElementById('status').textContent = 'Owner password confirmation does not match.';
    return;
  }
  e.preventDefault();
  const btn = document.getElementById('saveBtn');
  const status = document.getElementById('status');
  btn.disabled = true;
  btn.textContent = 'Saving...';
  status.className = 'status saving';
  status.textContent = 'Saving configuration...';

  fetch('/api/config', {
    method: 'POST',
    body: new URLSearchParams(new FormData(this))
  }).then(r => {
    if (r.ok) {
      status.className = 'status saving';
      status.textContent = 'Configuration saved! Device is rebooting...';
      btn.textContent = 'Rebooting...';
    } else {
      return r.text().then(t => { throw new Error(t); });
    }
  }).catch(err => {
    status.className = 'status error';
    status.textContent = 'Error: ' + err.message;
    btn.disabled = false;
    btn.textContent = 'Secure & Connect';
  });
});
</script>
</body>
</html>"#;

const SUCCESS_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>DCENT_axe — Saved!</title>
<style>body{font-family:sans-serif;background:#1a1a2e;color:#e0e0e0;display:flex;
justify-content:center;align-items:center;min-height:100vh;text-align:center}
.ok{background:#16213e;padding:40px;border-radius:16px;max-width:400px}
h1{color:#22c55e;margin-bottom:12px}p{color:#94a3b8}</style>
</head><body><div class="ok">
<h1>Configuration Saved!</h1>
<p>Your DCENT_axe is rebooting, your owner password is now set, and the miner will connect to your WiFi network.</p>
<p style="margin-top:16px;font-size:13px;color:#94a3b8">
Check the OLED or your router/DHCP lease table for the miner's IP address after reboot.</p>
<p style="margin-top:12px;font-size:13px;color:#64748b">
You can close this page. Sign in from the dashboard with your owner password once the miner is back online.</p>
</div></body></html>"#;
