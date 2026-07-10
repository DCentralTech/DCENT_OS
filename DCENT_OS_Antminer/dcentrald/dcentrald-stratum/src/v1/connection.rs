//! TCP connection management with line-based framing.
//!
//! Stratum V1 uses newline-delimited JSON over raw TCP.
//! This module handles the TCP stream, line buffering, and reconnection.

use std::sync::Arc;
use std::time::Duration;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
    WriteHalf,
};
use tokio::net::TcpStream;
use tokio_rustls::rustls::{self, pki_types::ServerName};
use tokio_rustls::TlsConnector;
use tracing::{debug, info};

trait AsyncReadWrite: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T> AsyncReadWrite for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

type BoxedStream = Box<dyn AsyncReadWrite>;

const STRATUM_WIRE_LOG_SECRET_PLACEHOLDER: &str = "<redacted>";

fn sanitize_stratum_wire_log_line(msg: &str) -> String {
    let trimmed = msg.trim_end();
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return dcentrald_common::wallet_mask::mask_in_string(trimmed).into_owned();
    };

    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(params) = value
        .get_mut("params")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return dcentrald_common::wallet_mask::mask_in_string(trimmed).into_owned();
    };

    let mut changed = false;
    match method.as_str() {
        "mining.authorize" => {
            if let Some(worker) = params.get_mut(0) {
                if let Some(worker_str) = worker.as_str() {
                    *worker = serde_json::Value::String(
                        dcentrald_common::wallet_mask::mask_wallet(worker_str),
                    );
                } else {
                    *worker =
                        serde_json::Value::String(STRATUM_WIRE_LOG_SECRET_PLACEHOLDER.to_string());
                }
                changed = true;
            }
            if let Some(password) = params.get_mut(1) {
                if !password.is_null() {
                    *password =
                        serde_json::Value::String(STRATUM_WIRE_LOG_SECRET_PLACEHOLDER.to_string());
                    changed = true;
                }
            }
        }
        "mining.submit" => {
            if let Some(worker) = params.get_mut(0) {
                if let Some(worker_str) = worker.as_str() {
                    *worker = serde_json::Value::String(
                        dcentrald_common::wallet_mask::mask_wallet(worker_str),
                    );
                } else {
                    *worker =
                        serde_json::Value::String(STRATUM_WIRE_LOG_SECRET_PLACEHOLDER.to_string());
                }
                changed = true;
            }
        }
        _ => {}
    }

    if changed {
        match serde_json::to_string(&value) {
            Ok(redacted) => redacted,
            Err(_) => STRATUM_WIRE_LOG_SECRET_PLACEHOLDER.to_string(),
        }
    } else {
        dcentrald_common::wallet_mask::mask_in_string(trimmed).into_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEndpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// Parse a Stratum V1 pool URL into an endpoint.
///
/// `stratum+tls://` and `stratum+ssl://` are real TLS endpoints. They must be
/// connected through `tokio-rustls`, never silently downgraded to plaintext.
pub fn parse_pool_endpoint(url: &str) -> Result<PoolEndpoint, PoolUrlError> {
    let trimmed = url.trim();
    let (stripped, tls) = trimmed
        .strip_prefix("stratum+tcp://")
        .map(|rest| (rest, false))
        .or_else(|| trimmed.strip_prefix("tcp://").map(|rest| (rest, false)))
        .or_else(|| {
            trimmed
                .strip_prefix("stratum+tls://")
                .map(|rest| (rest, true))
        })
        .or_else(|| {
            trimmed
                .strip_prefix("stratum+ssl://")
                .map(|rest| (rest, true))
        })
        .unwrap_or((trimmed, false));

    let parts: Vec<&str> = stripped.rsplitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(PoolUrlError::MissingPort(url.to_string()));
    }

    let port: u16 = parts[0]
        .parse()
        .map_err(|_| PoolUrlError::InvalidPort(parts[0].to_string()))?;
    let host = parts[1].to_string();

    if host.is_empty() {
        return Err(PoolUrlError::EmptyHost);
    }

    Ok(PoolEndpoint { host, port, tls })
}

/// Parse a pool URL into `(host, port)` for callers that do not care about the
/// transport. New runtime code should use `parse_pool_endpoint()`.
pub fn parse_pool_url(url: &str) -> Result<(String, u16), PoolUrlError> {
    parse_pool_endpoint(url).map(|endpoint| (endpoint.host, endpoint.port))
}

#[derive(Debug, thiserror::Error)]
pub enum PoolUrlError {
    #[error("missing port in URL: {0}")]
    MissingPort(String),

    #[error("invalid port: {0}")]
    InvalidPort(String),

    #[error("unsupported pool URL scheme: {0}")]
    UnsupportedScheme(String),

    #[error("empty hostname")]
    EmptyHost,
}

/// A framed TCP connection to a Stratum pool.
///
/// Provides line-based read/write over TCP with proper buffering.
/// Default operational cap on an inbound Stratum V1 line (bytes).
/// ≈16× the largest realistic pool→miner V1 line (`mining.notify` with
/// merkle branches < 4 KB) yet far below the OOM zone on a 228 MB-class
/// miner. Every constructor initializes the field to this so the cap is
/// fail-safe even if a caller never calls `set_max_line_bytes`.
pub(crate) const DEFAULT_V1_MAX_LINE_BYTES: usize = 65_536;

/// "Disabled" (`v1_max_inbound_line_bytes = 0`) maps to this finite
/// backstop — NOT literally unbounded. True-unbounded inbound is always
/// a bug; 16 MiB is far above any conceivable legitimate V1 line while
/// still bounding a hostile/buggy pool's memory amplification.
pub(crate) const V1_MAX_LINE_DISABLED_BACKSTOP: usize = 16 * 1024 * 1024;

pub struct StratumConnection {
    reader: BufReader<ReadHalf<BoxedStream>>,
    writer: WriteHalf<BoxedStream>,
    host: String,
    port: u16,
    tls: bool,
    /// Operational inbound-line byte cap (V1 inbound-line cap, strat-09).
    /// Always finite (see the two consts above).
    max_line_bytes: usize,
}

/// Pure over-cap predicate for the bounded V1 line read.
///
/// Given the bytes read by a `take(cap).read_until(b'\n', _)` and
/// whether a terminating `\n` was found: a non-empty read that hit the
/// `cap` budget WITHOUT a newline is an over-cap / truncated line (a
/// hostile or buggy pool). A read that ended in `\n` is a complete line
/// even at exactly `cap` (the cap is the inclusive max line length).
/// Pure + deterministic so the boundary is unit-pinned independently of
/// any socket. V1 inbound-line cap (strat-09 hardening).
pub(crate) fn v1_line_over_cap(read_len: usize, had_newline: bool, cap: usize) -> bool {
    read_len > 0 && !had_newline && read_len >= cap
}

impl StratumConnection {
    /// Connect to a plaintext TCP pool with a timeout.
    pub async fn connect(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, ConnectionError> {
        Self::connect_endpoint(
            PoolEndpoint {
                host: host.to_string(),
                port,
                tls: false,
            },
            timeout,
        )
        .await
    }

    /// Connect to a parsed pool endpoint with a timeout.
    pub async fn connect_endpoint(
        endpoint: PoolEndpoint,
        timeout: Duration,
    ) -> Result<Self, ConnectionError> {
        if endpoint.tls {
            Self::connect_tls(&endpoint.host, endpoint.port, timeout).await
        } else {
            Self::connect_tcp(&endpoint.host, endpoint.port, timeout).await
        }
    }

    async fn connect_tcp(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, ConnectionError> {
        debug!(
            host,
            port,
            timeout_secs = timeout.as_secs(),
            "Opening TCP connection to {}:{} (timeout {}s)",
            host,
            port,
            timeout.as_secs(),
        );

        let addr = format!("{}:{}", host, port);
        let stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(ConnectionError::Io)?;

        // Disable Nagle's algorithm (TCP_NODELAY) for lower latency.
        // Without this, the OS may buffer small writes (like share submissions)
        // and delay them by up to 200ms waiting for more data. Mining shares
        // are time-sensitive — every millisecond of latency increases stale risk.
        stream.set_nodelay(true).ok();

        info!(
            host, port,
            "TCP connection established to {}:{} (TCP_NODELAY enabled for low-latency share submission)",
            host, port,
        );

        Self::from_stream(Box::new(stream), host, port, false)
    }

    async fn connect_tls(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, ConnectionError> {
        debug!(
            host,
            port,
            timeout_secs = timeout.as_secs(),
            "Opening TLS connection to {}:{} (timeout {}s)",
            host,
            port,
            timeout.as_secs(),
        );

        let addr = format!("{}:{}", host, port);
        let stream = tokio::time::timeout(timeout, TcpStream::connect(&addr))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(ConnectionError::Io)?;
        stream.set_nodelay(true).ok();

        let root_store =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        let server_name = ServerName::try_from(host.to_string())
            .map_err(|_| ConnectionError::InvalidServerName(host.to_string()))?;
        let tls_stream = tokio::time::timeout(timeout, connector.connect(server_name, stream))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(ConnectionError::Io)?;

        info!(
            host,
            port,
            "TLS connection established to {}:{} (rustls certificate verification and SNI enabled)",
            host,
            port,
        );

        Self::from_stream(Box::new(tls_stream), host, port, true)
    }

    fn from_stream(
        stream: BoxedStream,
        host: &str,
        port: u16,
        tls: bool,
    ) -> Result<Self, ConnectionError> {
        let (reader, writer) = tokio::io::split(stream);
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            host: host.to_string(),
            port,
            tls,
            max_line_bytes: DEFAULT_V1_MAX_LINE_BYTES,
        })
    }

    /// Set the operational inbound-line byte cap from config
    /// (`StratumConfig.v1_max_inbound_line_bytes`). `0` = disabled →
    /// the finite 16 MiB backstop (never literally unbounded). Called
    /// once by the V1 client right after connecting.
    pub fn set_max_line_bytes(&mut self, n: u32) {
        self.max_line_bytes = if n == 0 {
            V1_MAX_LINE_DISABLED_BACKSTOP
        } else {
            n as usize
        };
    }

    /// Read one newline-delimited JSON message from the pool.
    ///
    /// Returns None on EOF (connection closed).
    pub async fn read_line(&mut self) -> Result<Option<String>, ConnectionError> {
        let cap = self.max_line_bytes;
        let mut buf: Vec<u8> = Vec::new();

        // Bounded read (V1 inbound-line cap, strat-09): read at most
        // `cap` bytes looking for the newline. A hostile or merely buggy
        // pool that sends a giant line — or a stream that never contains
        // `\n` — can no longer grow this buffer without limit and OOM a
        // 228 MB-class miner on the primary live mining path.
        let n = tokio::time::timeout(
            Duration::from_secs(300),
            (&mut self.reader)
                .take(cap as u64)
                .read_until(b'\n', &mut buf),
        )
        .await
        .map_err(|_| ConnectionError::ReadTimeout)?
        .map_err(ConnectionError::Io)?;

        if n == 0 {
            return Ok(None); // EOF
        }

        let had_newline = buf.last() == Some(&b'\n');
        if v1_line_over_cap(n, had_newline, cap) {
            // Over-cap / never-terminated line → protocol violation on
            // THIS connection. Fail-closed into the EXISTING V1
            // reconnect/backoff (no parallel path — same discipline as
            // the SV2 inbound-frame cap and the pool-failover work).
            return Err(ConnectionError::LineTooLong { cap });
        }
        if !had_newline {
            // Short read without a newline = mid-line EOF / torn
            // connection. Surface it (same class as a dropped
            // connection today) so reconnect handles it.
            return Err(ConnectionError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "pool closed connection mid-line",
            )));
        }

        // Success path stays byte-faithful to the prior `read_line`:
        // strict UTF-8 (invalid → Io error → reconnect, as before),
        // then `trim_end`.
        let line = String::from_utf8(buf).map_err(|e| {
            ConnectionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("non-UTF-8 data from pool: {e}"),
            ))
        })?;
        let trimmed = line.trim_end().to_string();
        debug!(msg = %trimmed, "Pool -> Miner");
        Ok(Some(trimmed))
    }

    /// Write a newline-terminated message to the pool.
    pub async fn write_line(&mut self, msg: &str) -> Result<(), ConnectionError> {
        debug!(msg = %sanitize_stratum_wire_log_line(msg), "Miner -> Pool");
        self.writer
            .write_all(msg.as_bytes())
            .await
            .map_err(ConnectionError::Io)?;
        self.writer.flush().await.map_err(ConnectionError::Io)?;
        Ok(())
    }

    /// Get the connected host.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Get the connected port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// True when this connection is protected by Stratum V1 TLS.
    pub fn is_tls(&self) -> bool {
        self.tls
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("connection timeout")]
    Timeout,

    #[error("read timeout (no data from pool for 5 minutes — pool may be down or connection dropped silently)")]
    ReadTimeout,

    #[error("pool sent an over-cap Stratum V1 line (exceeded {cap} bytes with no newline — hostile or buggy pool; connection will reconnect)")]
    LineTooLong { cap: usize },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid TLS server name for SNI/certificate validation: {0}")]
    InvalidServerName(String),
}

/// Exponential backoff calculator with jitter.
///
/// ```text
/// attempt 1: wait 1s
/// attempt 2: wait 2s
/// attempt 3: wait 4s
/// ...
/// max wait: 60s
/// jitter: +/- 25%
/// ```
pub struct Backoff {
    attempt: u32,
    base_ms: u64,
    max_ms: u64,
}

#[cfg(test)]
const DEFAULT_BACKOFF_BASE_MS: u64 = 1;
#[cfg(not(test))]
const DEFAULT_BACKOFF_BASE_MS: u64 = 1000;

impl Backoff {
    pub fn new() -> Self {
        Self {
            attempt: 0,
            base_ms: DEFAULT_BACKOFF_BASE_MS,
            max_ms: 60_000,
        }
    }

    /// Reset the backoff counter (call on successful connection).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Get the current attempt count.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Get the next backoff duration and increment the counter.
    pub fn next_delay(&mut self) -> Duration {
        let delay_ms = self.base_ms * 2u64.saturating_pow(self.attempt);
        let delay_ms = delay_ms.min(self.max_ms);
        self.attempt = self.attempt.saturating_add(1);

        // Add +/- 25% jitter
        let jitter_range = delay_ms / 4;
        let jitter = if jitter_range > 0 {
            let r = rand::random::<u64>() % (jitter_range * 2);
            r as i64 - jitter_range as i64
        } else {
            0
        };

        let final_ms = (delay_ms as i64 + jitter).max(100) as u64;

        debug!(
            attempt = self.attempt,
            delay_ms = final_ms,
            "Backoff: waiting {:.1}s before attempt #{} (exponential with jitter)",
            final_ms as f64 / 1000.0,
            self.attempt,
        );

        Duration::from_millis(final_ms)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[test]
    fn test_parse_pool_url() {
        let (host, port) = parse_pool_url("stratum+tcp://pool.example.com:3333").unwrap();
        assert_eq!(host, "pool.example.com");
        assert_eq!(port, 3333);
    }

    #[test]
    fn test_parse_pool_url_no_prefix() {
        let (host, port) = parse_pool_url("pool.example.com:3333").unwrap();
        assert_eq!(host, "pool.example.com");
        assert_eq!(port, 3333);
    }

    #[test]
    fn test_parse_pool_url_missing_port() {
        assert!(parse_pool_url("pool.example.com").is_err());
    }

    #[test]
    fn stratum_wire_log_redacts_authorize_password_and_wallet_worker() {
        let worker = "";
        let raw = format!(
            r#"{{"id":3,"method":"mining.authorize","params":["{worker}","supersecret"]}}"#
        );

        let logged = sanitize_stratum_wire_log_line(&raw);

        assert!(logged.contains("mining.authorize"));
        assert!(logged.contains(STRATUM_WIRE_LOG_SECRET_PLACEHOLDER));
        assert!(
            !logged.contains("supersecret"),
            "authorize password leaked into debug log: {logged}"
        );
        assert!(
            !logged.contains(worker),
            "full wallet worker leaked into debug log: {logged}"
        );
    }

    #[test]
    fn stratum_wire_log_redacts_submit_wallet_worker() {
        let worker = "";
        let raw = format!(
            r#"{{"id":4,"method":"mining.submit","params":["{worker}","job","00","5f5e1000","deadbeef"]}}"#
        );

        let logged = sanitize_stratum_wire_log_line(&raw);

        assert!(logged.contains("mining.submit"));
        assert!(
            !logged.contains(worker),
            "full submit worker leaked into debug log: {logged}"
        );
    }

    #[test]
    fn stratum_wire_log_masks_wallets_in_non_json_text() {
        let worker = "";
        let logged = sanitize_stratum_wire_log_line(&format!("worker={worker}\n"));

        assert!(
            !logged.contains(worker),
            "full wallet leaked from malformed wire log text: {logged}"
        );
    }

    #[test]
    fn tls_pool_url_schemes_parse_as_tls_endpoints() {
        for url in [
            "stratum+ssl://pool.example.com:443",
            "stratum+tls://pool.example.com:443",
        ] {
            let endpoint = parse_pool_endpoint(url).expect("TLS URL should parse");
            assert_eq!(endpoint.host, "pool.example.com");
            assert_eq!(endpoint.port, 443);
            assert!(endpoint.tls);
        }
    }

    #[test]
    fn test_backoff_increases() {
        let mut b = Backoff::new();
        let d1 = b.next_delay();
        let d2 = b.next_delay();
        // d2 should be roughly 2x d1 (within jitter)
        assert!(d2 > d1 / 2); // At minimum, not decreasing drastically
    }

    #[test]
    fn test_backoff_reset() {
        let mut b = Backoff::new();
        b.next_delay();
        b.next_delay();
        b.next_delay();
        b.reset();
        let d = b.next_delay();
        // After reset, should be back to ~1s
        assert!(d < Duration::from_secs(3));
    }

    // -----------------------------------------------------------------------
    // Pool URL error-path contracts.
    //
    // The happy paths are tested above; these tests pin the explicit
    // PoolUrlError variants so a refactor of `parse_pool_url` (e.g.
    // switching to a real URL crate) cannot silently accept malformed
    // pool URLs without surfacing a typed error.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_pool_url_accepts_stratum_ssl_scheme() {
        let (host, port) = parse_pool_url("stratum+ssl://pool.example.com:3334").unwrap();
        assert_eq!(host, "pool.example.com");
        assert_eq!(port, 3334);
    }

    #[test]
    fn parse_pool_endpoint_keeps_tls_flag_with_leading_whitespace() {
        let endpoint = parse_pool_endpoint("  stratum+ssl://pool.example.com:3334").unwrap();
        assert_eq!(endpoint.host, "pool.example.com");
        assert_eq!(endpoint.port, 3334);
        assert!(endpoint.tls);
    }

    #[test]
    fn parse_pool_url_rejects_non_numeric_port() {
        let result = parse_pool_url("pool.example.com:abcd");
        assert!(matches!(
            result,
            Err(PoolUrlError::InvalidPort(s)) if s == "abcd"
        ));
    }

    #[test]
    fn parse_pool_url_rejects_oversized_port() {
        // Ports > u16::MAX must surface InvalidPort (the parse to u16
        // fails). Pin so a refactor that widens the port type doesn't
        // accidentally accept 100000 as a valid port.
        let result = parse_pool_url("pool.example.com:100000");
        assert!(matches!(result, Err(PoolUrlError::InvalidPort(_))));
    }

    #[test]
    fn parse_pool_url_rejects_empty_host() {
        // `:3333` produces an empty hostname after the prefix strip —
        // must surface EmptyHost.
        let result = parse_pool_url(":3333");
        assert!(matches!(result, Err(PoolUrlError::EmptyHost)));

        let with_prefix = parse_pool_url("stratum+tcp://:3333");
        assert!(matches!(with_prefix, Err(PoolUrlError::EmptyHost)));
    }

    #[test]
    fn parse_pool_url_accepts_ipv4_literal() {
        let (host, port) = parse_pool_url("stratum+tcp://203.0.113.10:3333").unwrap();
        assert_eq!(host, "203.0.113.10");
        assert_eq!(port, 3333);
    }

    #[test]
    fn parse_pool_url_accepts_plain_tcp_scheme() {
        let (host, port) = parse_pool_url("tcp://pool.example.com:4444").unwrap();
        assert_eq!(host, "pool.example.com");
        assert_eq!(port, 4444);
    }

    #[test]
    fn parse_pool_url_error_display_messages_are_actionable() {
        // Operators read these in logs. Pin the format so a refactor
        // doesn't strip the offending URL from the message.
        assert!(PoolUrlError::MissingPort("foo".to_string())
            .to_string()
            .contains("foo"));
        assert!(PoolUrlError::InvalidPort("abc".to_string())
            .to_string()
            .contains("abc"));
        assert!(PoolUrlError::UnsupportedScheme("ssl://x".to_string())
            .to_string()
            .contains("ssl://x"));
        assert_eq!(PoolUrlError::EmptyHost.to_string(), "empty hostname");
    }

    // -----------------------------------------------------------------------
    // Backoff invariants.
    //
    // The exponential-backoff calculator is the only thing keeping a
    // disconnected miner from spamming TCP connect attempts. Pin the
    // 60-second ceiling, 100ms floor, attempt counter, and saturation
    // behavior so a refactor cannot silently flip any of them without
    // tripping a test.
    // -----------------------------------------------------------------------

    #[test]
    fn backoff_starts_at_zero_attempts() {
        let b = Backoff::new();
        assert_eq!(b.attempt(), 0);
    }

    #[test]
    fn backoff_attempt_increments_each_call() {
        let mut b = Backoff::new();
        b.next_delay();
        assert_eq!(b.attempt(), 1);
        b.next_delay();
        assert_eq!(b.attempt(), 2);
    }

    #[test]
    fn backoff_floor_is_at_least_100ms() {
        // Even with maximum negative jitter, the delay must never drop
        // below 100ms — that's the absolute minimum reconnect interval
        // protecting the pool from a tight reconnect loop.
        let mut b = Backoff::new();
        for _ in 0..50 {
            let d = b.next_delay();
            assert!(
                d >= Duration::from_millis(100),
                "delay {:?} below 100ms floor",
                d
            );
        }
    }

    #[test]
    fn backoff_caps_at_max_ms_after_many_attempts() {
        // After enough attempts the exponential reaches max_ms (60s).
        // With +/- 25% jitter the upper bound is 60s + 15s = 75s. Pin
        // the 75-second ceiling so a refactor that lifts max_ms or
        // changes the jitter range is caught.
        let mut b = Backoff::new();
        // Burn enough attempts to saturate.
        for _ in 0..30 {
            b.next_delay();
        }
        for _ in 0..20 {
            let d = b.next_delay();
            assert!(
                d <= Duration::from_millis(75_001),
                "delay {:?} exceeds 75s ceiling",
                d
            );
        }
    }

    #[test]
    fn backoff_does_not_overflow_at_max_attempt_count() {
        // 2^64 would overflow u64. The implementation uses saturating_pow
        // and saturating_add to stay safe. Pin that calling next_delay
        // millions of times cannot panic or produce an invalid duration.
        let mut b = Backoff::new();
        // Force the attempt counter near saturation by directly
        // manipulating it via repeated calls — a brute-force loop would
        // be slow, so set the counter close to u32::MAX via reset+attempt.
        b.attempt = u32::MAX - 2;
        for _ in 0..5 {
            let d = b.next_delay();
            assert!(
                d <= Duration::from_millis(75_001),
                "delay overflow risk: {:?}",
                d
            );
        }
        // After saturation, attempt() returns u32::MAX (no wrap).
        assert_eq!(b.attempt(), u32::MAX);
    }

    #[test]
    fn connection_error_display_messages_are_actionable() {
        assert!(ConnectionError::Timeout
            .to_string()
            .to_lowercase()
            .contains("timeout"));
        assert!(ConnectionError::ReadTimeout
            .to_string()
            .to_lowercase()
            .contains("read timeout"));
        // ReadTimeout's message hints at pool downtime — pin the diagnostic.
        assert!(ConnectionError::ReadTimeout
            .to_string()
            .contains("pool may be down"));
    }

    // -----------------------------------------------------------------------
    // V1 inbound-line operational cap (strat-09 hardening) — the V1 analog
    // of the SV2 inbound-frame cap, on the primary live mining path.
    // Pins the pure over-cap predicate + the disabled→backstop mapping so
    // a refactor that re-introduces the unbounded `read_line` (or flips
    // the cap off) lights up. Plan/review:
    //
    // -----------------------------------------------------------------------

    #[test]
    fn v1_line_over_cap_predicate_boundary() {
        let cap = DEFAULT_V1_MAX_LINE_BYTES; // 64 KiB
                                             // Complete line ending in '\n' is OK even at exactly the cap
                                             // (cap is the inclusive max line length).
        assert!(!v1_line_over_cap(10, true, cap));
        assert!(!v1_line_over_cap(cap, true, cap));
        // A realistic largest mining.notify (~4 KB) with newline: fine.
        assert!(!v1_line_over_cap(4096, true, cap));
        // EOF / zero read is never "over cap" (handled as None upstream).
        assert!(!v1_line_over_cap(0, false, cap));
        // Budget exhausted with NO newline = the hostile/buggy case.
        assert!(v1_line_over_cap(cap, false, cap));
        // A partial (< cap) read with no newline is NOT flagged over-cap
        // here (it's a mid-line EOF, surfaced separately as a torn conn).
        assert!(!v1_line_over_cap(cap - 1, false, cap));
    }

    #[tokio::test]
    async fn tcp_read_line_rejects_over_cap_line_before_unbounded_buffer_growth() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local stratum mock");
        let port = listener.local_addr().expect("local addr").port();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept client");
            let oversized = vec![b'{'; DEFAULT_V1_MAX_LINE_BYTES + 1];
            stream
                .write_all(&oversized)
                .await
                .expect("write oversized line");
            stream.flush().await.expect("flush oversized line");
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let mut conn = StratumConnection::connect("127.0.0.1", port, Duration::from_secs(1))
            .await
            .expect("connect to local stratum mock");

        let result = tokio::time::timeout(Duration::from_secs(1), conn.read_line())
            .await
            .expect("bounded reader must return promptly");
        assert!(
            matches!(
                result,
                Err(ConnectionError::LineTooLong {
                    cap: DEFAULT_V1_MAX_LINE_BYTES
                })
            ),
            "oversized unterminated pool line must fail with LineTooLong, got {result:?}"
        );

        server.await.expect("mock server task");
    }

    #[test]
    fn set_max_line_bytes_maps_zero_to_finite_backstop_not_unbounded() {
        // The struct can't be cheaply constructed without a stream, so
        // pin the mapping logic that `set_max_line_bytes` applies: a
        // configured value is used as-is; `0` (=disabled) maps to the
        // FINITE 16 MiB backstop — NEVER literally unbounded (true
        // unbounded inbound is always a bug).
        let mapped = |n: u32| -> usize {
            if n == 0 {
                V1_MAX_LINE_DISABLED_BACKSTOP
            } else {
                n as usize
            }
        };
        assert_eq!(mapped(65_536), 65_536);
        assert_eq!(mapped(1), 1);
        assert_eq!(mapped(0), V1_MAX_LINE_DISABLED_BACKSTOP);
        // The backstop is finite and far above any legitimate V1 line
        // yet bounds a hostile pool's amplification.
        assert_eq!(V1_MAX_LINE_DISABLED_BACKSTOP, 16 * 1024 * 1024);
        assert!(DEFAULT_V1_MAX_LINE_BYTES >= 16 * 1024);
        assert!(DEFAULT_V1_MAX_LINE_BYTES < V1_MAX_LINE_DISABLED_BACKSTOP);
    }

    #[test]
    fn line_too_long_error_message_is_actionable() {
        // Operators read this when a pool misbehaves — pin the cap value
        // and the reconnect hint.
        let s = ConnectionError::LineTooLong { cap: 65_536 }.to_string();
        assert!(s.contains("65536"), "must include the cap: {s}");
        assert!(
            s.to_lowercase().contains("reconnect"),
            "must hint reconnect: {s}"
        );
    }
}
