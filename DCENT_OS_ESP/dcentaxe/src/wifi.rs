// DCENT_axe WiFi Connection
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi};
use log::{error, info, warn};

/// WiFi connection timeout in seconds.
const WIFI_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Connect to WiFi AP and block until IP is assigned.
///
/// Returns the EspWifi handle which must be kept alive for the connection to persist.
/// Returns Err if connection fails or times out.
pub fn connect_wifi(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Result<Box<EspWifi<'static>>, String> {
    info!("WiFi: connecting to '{}'", ssid);

    let mut wifi = Box::new(
        EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .map_err(|e| format!("WiFi init failed: {:?}", e))?,
    );

    let auth = if password.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };

    let mut ssid_buf = heapless::String::<32>::new();
    ssid_buf
        .push_str(ssid)
        .map_err(|_| "SSID too long (max 32 chars)".to_string())?;

    let mut pass_buf = heapless::String::<64>::new();
    pass_buf
        .push_str(password)
        .map_err(|_| "Password too long (max 64 chars)".to_string())?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: ssid_buf,
        password: pass_buf,
        auth_method: auth,
        ..Default::default()
    }))
    .map_err(|e| format!("WiFi config failed: {:?}", e))?;

    wifi.start()
        .map_err(|e| format!("WiFi start failed: {:?}", e))?;

    // Debug: scan for visible networks before connecting
    if let Ok(networks) = wifi.scan() {
        info!("WiFi scan found {} networks:", networks.len());
        for ap in networks.iter().take(10) {
            info!(
                "  '{}' ch:{} rssi:{} auth:{:?}",
                ap.ssid,
                ap.channel,
                ap.signal_strength,
                ap.auth_method.unwrap_or(AuthMethod::None)
            );
        }
    }

    wifi.connect()
        .map_err(|e| format!("WiFi connect failed: {:?}", e))?;

    // Wait for IP assignment (poll with timeout)
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(WIFI_CONNECT_TIMEOUT_SECS);
    while std::time::Instant::now() < deadline {
        if wifi.is_connected().unwrap_or(false) {
            let ip_info = wifi
                .sta_netif()
                .get_ip_info()
                .map_err(|e| format!("Get IP info failed: {:?}", e))?;
            if !ip_info.ip.is_unspecified() {
                info!(
                    "WiFi: connected! IP={}, GW={}",
                    ip_info.ip, ip_info.subnet.gateway
                );
                return Ok(wifi);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    error!(
        "WiFi: timeout waiting for IP assignment after {}s",
        WIFI_CONNECT_TIMEOUT_SECS
    );
    Err(format!(
        "WiFi: timeout connecting to '{}' after {}s",
        ssid, WIFI_CONNECT_TIMEOUT_SECS
    ))
}

/// Try to connect to WiFi, returning None on failure instead of panicking.
///
/// Used by main.rs to fall back to provisioning mode if WiFi connection fails.
pub fn try_connect_wifi(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: &str,
    password: &str,
) -> Option<Box<EspWifi<'static>>> {
    match connect_wifi(modem, sysloop, nvs, ssid, password) {
        Ok(wifi) => Some(wifi),
        Err(e) => {
            warn!("WiFi connection failed: {} — will enter setup mode", e);
            None
        }
    }
}

/// Linear backoff schedule for poll-based WiFi reconnect (in seconds).
///
/// We deliberately use a poll-based approach instead of subscribing to the
/// `WifiEvent::StaDisconnected` async stream because esp-idf-svc's
/// `EspSystemSubscription` API requires a `'static` callback that captures the
/// EspWifi handle, which we already share through `state.wifi` behind a Mutex.
/// Polling once per main-loop tick (5 s) avoids that lifetime mess and gives
/// us deterministic, lock-friendly reconnect behaviour. Trade-off:
/// disconnects are detected within one tick (≤5 s) instead of on-event.
const RECONNECT_BACKOFF_SECS: [u64; 5] = [1, 2, 5, 10, 30];

/// Tracks reconnect state across main-loop ticks. Only the main loop owns
/// this; pass it by `&mut` into `tick_reconnect`.
#[derive(Debug, Default)]
pub struct ReconnectState {
    /// Number of failed reconnect attempts since the last successful connect.
    pub attempts: u32,
    /// Unix-ms timestamp of the next reconnect attempt. `0` means "no
    /// reconnect pending" — the WiFi link is up.
    pub next_attempt_ms: u64,
    /// True iff the previous tick observed `is_connected() == false`. We use
    /// the rising edge to log "WiFi reconnected" exactly once.
    pub was_disconnected: bool,
}

impl ReconnectState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the current tick as healthy (link up). Logs the rising edge if
    /// we were previously disconnected.
    pub fn mark_connected(&mut self) {
        if self.was_disconnected || self.attempts > 0 {
            info!("WiFi reconnected");
        }
        self.attempts = 0;
        self.next_attempt_ms = 0;
        self.was_disconnected = false;
    }

    /// Compute backoff delay (seconds) for the n-th attempt. Capped at 30 s.
    fn backoff_secs(attempts: u32) -> u64 {
        let idx = (attempts as usize).min(RECONNECT_BACKOFF_SECS.len() - 1);
        RECONNECT_BACKOFF_SECS[idx]
    }
}

/// Poll-based WiFi link health check + reconnect with linear backoff.
///
/// Call once per main-loop tick (~5 s). On each call:
///   1. If `wifi.is_connected()` returns true → mark healthy, no-op.
///   2. Otherwise → respect backoff schedule (1 s → 2 s → 5 s → 10 s → 30 s,
///      capped at 30 s) and call `wifi.connect()` when due.
///
/// Logs are kept terse so they don't drown the main-loop output during a
/// long-running outage:
///   `WiFi disconnected; reconnect attempt N (backoff Ns)`
///   `WiFi reconnected`
pub fn tick_reconnect(wifi: &mut EspWifi<'static>, state: &mut ReconnectState, now_ms: u64) {
    let connected = wifi.is_connected().unwrap_or(false);

    if connected {
        state.mark_connected();
        return;
    }

    // Link is down. If this is the first tick observing the disconnect, log
    // it and seed the backoff schedule so the very next tick attempts a
    // reconnect (1 s wait — but we're already past it because the main tick
    // is 5 s).
    if !state.was_disconnected {
        state.was_disconnected = true;
        state.attempts = 0;
        state.next_attempt_ms = now_ms; // try immediately on next eligible tick
    }

    if now_ms < state.next_attempt_ms {
        return;
    }

    let backoff = ReconnectState::backoff_secs(state.attempts);
    state.attempts = state.attempts.saturating_add(1);
    state.next_attempt_ms = now_ms.saturating_add(backoff.saturating_mul(1000));

    info!(
        "WiFi disconnected; reconnect attempt {} (backoff {}s)",
        state.attempts, backoff
    );

    // `wifi.connect()` is non-blocking on esp-idf-svc — it kicks the supplicant
    // and returns. If it errors (rare; usually the supplicant is busy), we
    // just log + try again next tick.
    if let Err(e) = wifi.connect() {
        warn!("WiFi: connect() failed: {:?}", e);
    }
}
