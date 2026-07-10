//! `BridgeClient` — the HTTP client that talks the dcent-pack contract.
//!
//! Implements discovery (spec §1), pairing with the §2.5 status policy,
//! heartbeat (spec §5), telemetry polling + staleness (spec §3), and the
//! on-demand OTA upload / pull surfaces (spec §7.3 / §7.3.1).
//!
//! The WebSocket reverse-proxy client is deferred (out of scope for v1); the
//! `ws_sig` signing fn still ships and is unit-tested in `crypto.rs`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::crypto::{heartbeat_sig, ota_pull_sig, ota_sig, pair_hmac, UnitSecret};
use crate::error::{BridgeError, PairError};
use crate::protocol::{
    BridgeTelemetry, HealthResponse, HeartbeatRequest, HeartbeatResponse, PairRequest, PairResponse,
};

/// Default per-request timeout for the small JSON calls.
const SHORT_TIMEOUT: Duration = Duration::from_secs(5);
/// Discovery probe timeout (spec §1.3: >2 s ⇒ treat as not-a-bridge).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Heartbeat timeout (spec §7.1 skeleton used 3 s).
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);
/// Telemetry timeout (spec §7.2 skeleton used 3 s).
const TELEMETRY_TIMEOUT: Duration = Duration::from_secs(3);

/// Telemetry sample is unusable once it is older than this (spec §3.2).
const MAX_SAMPLE_AGE_MS: u64 = 5000;
const DEFAULT_TELEMETRY_PATH: &str = "/api/v1/telemetry";
const DEFAULT_PROXY_PATH: &str = "/";

/// Outcome of a heartbeat call, distinguishing the re-pair signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// 200 + `paired:true` — keep going.
    Ok,
    /// 200 + `paired:false` — bridge lost the pairing; re-pair (spec §6).
    NeedsRepair,
}

/// The dcent-pack bridge HTTP client.
///
/// Holds the shared `reqwest::Client` and the cached `/pair` response fields
/// (`telemetry_url`, `proxy_url`, `bridge_name`) once paired.
pub struct BridgeClient {
    http: reqwest::Client,
    /// Bridge base URL, e.g. `http://10.77.0.1` (no trailing slash).
    base_url: String,
    /// Cached from the last successful `/pair` (spec §2.6).
    pub telemetry_url: Option<String>,
    pub proxy_url: Option<String>,
    pub bridge_name: Option<String>,
    /// Staleness tracker: the last N `last_sample_age_ms` values seen.
    age_history: StalenessTracker,
}

impl BridgeClient {
    /// Construct a client for a bridge at `base_url` (e.g. `http://10.77.0.1`).
    ///
    /// Reuses a single `reqwest::Client` (connection pool) for the lifetime of
    /// the task. Built with the daemon's pinned feature set (rustls-tls, json).
    pub fn new(base_url: impl Into<String>) -> Result<Self, BridgeError> {
        let http = reqwest::Client::builder()
            .build()
            .map_err(BridgeError::from)?;
        Ok(Self::with_http(http, base_url))
    }

    /// Construct from a pre-built `reqwest::Client` (used by tests with a base
    /// URL pointed at a mock server).
    pub fn with_http(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        let base = base_url.into();
        let base = base.trim_end_matches('/').to_string();
        Self {
            http,
            base_url: base,
            telemetry_url: None,
            proxy_url: None,
            bridge_name: None,
            age_history: StalenessTracker::default(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    // ----------------------------------------------------------- discovery

    /// Probe `GET /api/v1/health` and return it iff it is a genuine dcent-pack
    /// bridge (`product == "dcent-pack"`). 2 s timeout (spec §1.3).
    ///
    /// On a 404 the caller may fall back to the telemetry-regex probe via
    /// [`Self::probe_telemetry_fallback`].
    pub async fn probe_health(&self) -> Result<Option<HealthResponse>, BridgeError> {
        let resp = self
            .http
            .get(self.url("/api/v1/health"))
            .timeout(PROBE_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // signal: try the telemetry fallback
        }
        if !resp.status().is_success() {
            return Ok(None);
        }
        let health: HealthResponse = match resp.json().await {
            Ok(h) => h,
            Err(_) => return Ok(None), // not JSON / wrong shape ⇒ not a bridge
        };
        if health.is_dcent_pack() {
            Ok(Some(health))
        } else {
            Ok(None)
        }
    }

    /// Fallback discovery for firmware without `/api/v1/health` (spec §1.2
    /// note): probe `GET /api/v1/telemetry` and accept a dcent-pack-shaped
    /// `firmware_version`. Returns the telemetry on success.
    pub async fn probe_telemetry_fallback(&self) -> Result<Option<BridgeTelemetry>, BridgeError> {
        let resp = self
            .http
            .get(self.url("/api/v1/telemetry"))
            .timeout(PROBE_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        match resp.json::<BridgeTelemetry>().await {
            Ok(t) if is_dcent_pack_version(&t.firmware_version) => Ok(Some(t)),
            _ => Ok(None),
        }
    }

    // ------------------------------------------------------------- pairing

    /// One `/pair` POST. The caller drives retry/backoff via
    /// [`Self::pair_with_retry`].
    ///
    /// `ts` is supplied by the caller so the retry wrapper can refresh it on a
    /// 409-replay (spec §2.5 / firmware) without re-deriving the whole request.
    #[allow(clippy::too_many_arguments)]
    pub async fn pair_once(
        &mut self,
        secret: &UnitSecret,
        device_id: &str,
        miner_mac: &str,
        model: &str,
        hostname: &str,
        api_port: u16,
        ts: u64,
    ) -> Result<PairResponse, PairError> {
        let req = PairRequest {
            device_id: device_id.to_string(),
            miner_mac: miner_mac.to_string(),
            ts,
            hmac: pair_hmac(secret.as_bytes(), device_id, miner_mac, ts),
            model: model.to_string(),
            hostname: hostname.to_string(),
            api_port,
        };

        let resp = self
            .http
            .post(self.url("/pair"))
            .timeout(SHORT_TIMEOUT)
            .json(&req)
            .send()
            .await?;

        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();

        match status {
            200 => {
                let mut parsed: PairResponse = serde_json::from_str(&body).map_err(|e| {
                    PairError::Other(anyhow::anyhow!("decode pair response: {e}: {body}"))
                })?;
                parsed.telemetry_url = normalize_same_origin_pair_url(
                    &self.base_url,
                    &parsed.telemetry_url,
                    DEFAULT_TELEMETRY_PATH,
                    "telemetry_url",
                )?;
                parsed.proxy_url = normalize_http_pair_url(
                    &self.base_url,
                    &parsed.proxy_url,
                    DEFAULT_PROXY_PATH,
                    "proxy_url",
                )?;
                self.telemetry_url = Some(parsed.telemetry_url.clone());
                self.proxy_url = Some(parsed.proxy_url.clone());
                self.bridge_name = Some(parsed.bridge_name.clone());
                Ok(parsed)
            }
            400 => Err(PairError::BadRequest(body)),
            401 => Err(PairError::AuthFailed(body)),
            403 => Err(PairError::EnrollmentLocked),
            409 if body.contains("replay") => Err(PairError::Replay),
            409 => Err(PairError::Http { status, body }),
            503 => Err(PairError::TimeNotSynced),
            _ => Err(PairError::Http { status, body }),
        }
    }

    /// Pair with the §2.5 retry policy:
    /// - 400 / 401 / 403 ⇒ fast-fail (return the error).
    /// - 503 ⇒ retry every 15 s, up to 20 attempts (5 min), then every 60 s.
    /// - 409-replay ⇒ refresh `ts` to a fresh second and re-sign, then retry.
    /// - 5xx / transport ⇒ exponential backoff 5 → 60 s.
    ///
    /// `now_unix_s` is injected so tests can drive the ts-refresh deterministically.
    #[allow(clippy::too_many_arguments)]
    pub async fn pair_with_retry(
        &mut self,
        secret: &UnitSecret,
        device_id: &str,
        miner_mac: &str,
        model: &str,
        hostname: &str,
        api_port: u16,
    ) -> Result<PairResponse, PairError> {
        let mut backoff = Duration::from_secs(5);
        let mut sntp_retries: u32 = 0;
        let mut last_ts: u64 = 0;

        loop {
            // Fresh wall-clock second per attempt; bump if identical to the
            // previous attempt so a 409-replay always re-signs a NEW ts.
            let mut ts = unix_now_s()?;
            if ts <= last_ts {
                ts = last_ts + 1;
            }
            last_ts = ts;

            match self
                .pair_once(secret, device_id, miner_mac, model, hostname, api_port, ts)
                .await
            {
                Ok(resp) => return Ok(resp),
                Err(PairError::BadRequest(b)) => return Err(PairError::BadRequest(b)),
                Err(PairError::AuthFailed(r)) => {
                    tracing::warn!(reason = %r, "bridge /pair refused (HMAC/skew)");
                    return Err(PairError::AuthFailed(r));
                }
                Err(PairError::EnrollmentLocked) => {
                    tracing::warn!("bridge enrollment locked; awaiting operator setup-button");
                    return Err(PairError::EnrollmentLocked);
                }
                Err(PairError::Replay) => {
                    // Regenerate ts (next loop bumps past last_ts) and re-sign.
                    tracing::debug!("bridge /pair 409-replay; refreshing ts and re-signing");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(PairError::TimeNotSynced) => {
                    if sntp_retries >= 20 {
                        tracing::warn!("bridge clock still not synced after 5 min; slow retry");
                        sntp_retries += 1;
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    } else {
                        sntp_retries += 1;
                        tokio::time::sleep(Duration::from_secs(15)).await;
                    }
                }
                Err(PairError::Http { status, body }) if status >= 500 => {
                    tracing::warn!(status, %body, ?backoff, "bridge 5xx; backing off");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
                Err(PairError::Http { status, body }) => {
                    return Err(PairError::Http { status, body });
                }
                Err(PairError::Transport(e)) => {
                    tracing::warn!(error = %e, ?backoff, "bridge unreachable; backing off");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
                Err(PairError::Other(e)) => return Err(PairError::Other(e)),
            }
        }
    }

    // ----------------------------------------------------------- heartbeat

    /// POST `/api/v1/miner/heartbeat` (spec §5) with the V0.2 Change-A body-bound
    /// signature.
    ///
    /// The caller supplies the fully-built [`HeartbeatRequest`] (so the expanded
    /// Change-B fields are populated from live miner stats) and, when available,
    /// the per-unit `secret` received at `/pair`. We serialize the request ONCE
    /// and sign THOSE EXACT bytes — never `.json(&req)` after signing — so the
    /// bridge verifier hashes the identical bytes it received:
    /// - `X-DCent-Heartbeat-Ts`  = current unix seconds (canonical decimal).
    /// - `X-DCent-Heartbeat-Sig` = [`heartbeat_sig`] over the serialized body.
    ///
    /// `secret == None` preserves today's unsigned behavior (the bridge accepts
    /// unsigned heartbeats while `DCENT_HEARTBEAT_REQUIRE_SIG=0` — staged rollout).
    ///
    /// - 200 + `paired:true` ⇒ [`HeartbeatOutcome::Ok`].
    /// - 200 + `paired:false` ⇒ [`HeartbeatOutcome::NeedsRepair`] (re-pair signal).
    /// - 403 ⇒ [`BridgeError::WrongMiner`] (surface + stop).
    /// - other non-2xx / transport ⇒ error (caller continues at next interval).
    pub async fn heartbeat(
        &self,
        req: &HeartbeatRequest,
        secret: Option<&UnitSecret>,
    ) -> Result<HeartbeatOutcome, BridgeError> {
        // Serialize once; sign and send the SAME bytes.
        let body = serde_json::to_vec(req)
            .map_err(|e| BridgeError::Other(anyhow::anyhow!("serialize heartbeat body: {e}")))?;

        let mut request = self
            .http
            .post(self.url("/api/v1/miner/heartbeat"))
            .timeout(HEARTBEAT_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/json");

        if let Some(secret) = secret {
            let ts = unix_now_s().map_err(|_| {
                BridgeError::Other(anyhow::anyhow!("system clock before unix epoch"))
            })?;
            let sig = heartbeat_sig(secret.as_bytes(), ts, &body);
            request = request
                // Canonical decimal seconds — matches the firmware "%PRId64".
                .header("X-DCent-Heartbeat-Ts", ts.to_string())
                .header("X-DCent-Heartbeat-Sig", sig);
        }

        let resp = request.body(body).send().await?;

        let status = resp.status().as_u16();
        if status == 403 {
            return Err(BridgeError::WrongMiner);
        }
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(BridgeError::Http { status, body });
        }
        let parsed: HeartbeatResponse = resp.json().await?;
        if parsed.paired {
            Ok(HeartbeatOutcome::Ok)
        } else {
            Ok(HeartbeatOutcome::NeedsRepair)
        }
    }

    // ----------------------------------------------------------- telemetry

    /// GET the cached `telemetry_url` (spec §3). Falls back to the default
    /// `/api/v1/telemetry` on the base URL if no telemetry_url is cached yet.
    pub async fn poll_telemetry(&self) -> Result<BridgeTelemetry, BridgeError> {
        let url = self
            .telemetry_url
            .clone()
            .unwrap_or_else(|| self.url(DEFAULT_TELEMETRY_PATH));
        let resp = self
            .http
            .get(&url)
            .timeout(TELEMETRY_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(BridgeError::Http { status, body });
        }
        Ok(resp.json().await?)
    }

    /// Record a telemetry sample's age into the staleness tracker and return
    /// the usable external temperature, if any.
    ///
    /// A sample is usable iff `status == "ok"`, `last_sample_age_ms <= 5000`,
    /// AND the bridge is still taking new samples (the last 3 ages are not all
    /// identical — spec §3.3).
    pub fn record_and_extract_temp(&mut self, t: &BridgeTelemetry) -> Option<f32> {
        let frozen = self
            .age_history
            .push_is_frozen(t.temperature.last_sample_age_ms);
        if usable_temperature(t).is_some() && !frozen {
            Some(t.temperature.external_temperature_c)
        } else {
            None
        }
    }

    /// Reset the staleness tracker (call on telemetry loss / re-pair).
    pub fn reset_staleness(&mut self) {
        self.age_history = StalenessTracker::default();
    }

    // --------------------------------------------------------------- OTA

    /// On-demand Mode-A OTA upload (spec §7.3): stream `image` to the bridge
    /// with an `X-DCent-Ota-Sig` HMAC header. 120 s timeout for slow Wi-Fi.
    pub async fn ota_upload(&self, secret: &UnitSecret, image: Vec<u8>) -> Result<(), BridgeError> {
        let sig = ota_sig(secret.as_bytes(), &image);
        let resp = self
            .http
            .post(self.url("/api/v1/ota/upload"))
            .timeout(Duration::from_secs(120))
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .header("X-DCent-Ota-Sig", sig)
            .body(image)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(BridgeError::Http { status, body });
        }
        Ok(())
    }

    /// On-demand Mode-B OTA URL-pull (spec §7.3.1): hand the bridge a URL +
    /// expected SHA256 + HMAC and let it fetch the image itself.
    pub async fn ota_pull(
        &self,
        secret: &UnitSecret,
        url: &str,
        expected_sha256_hex: &str,
        release_notes_url: Option<&str>,
    ) -> Result<(), BridgeError> {
        let sig = ota_pull_sig(secret.as_bytes(), url, expected_sha256_hex);
        let body = serde_json::json!({
            "url": url,
            "expected_sha256": expected_sha256_hex,
            "hmac": sig,
            "release_notes_url": release_notes_url,
        });
        let resp = self
            .http
            .post(self.url("/api/v1/ota/pull"))
            .timeout(Duration::from_secs(15))
            .json(&body)
            .send()
            .await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::Http { status, body: text });
        }
        Ok(())
    }
}

/// The `temperature.status == "ok" && last_sample_age_ms <= 5000` predicate
/// (spec §3.2). Pure — does NOT consult the 3-identical-age staleness tracker.
pub fn usable_temperature(t: &BridgeTelemetry) -> Option<f32> {
    if t.temperature.status == "ok" && t.temperature.last_sample_age_ms <= MAX_SAMPLE_AGE_MS {
        Some(t.temperature.external_temperature_c)
    } else {
        None
    }
}

fn normalize_same_origin_pair_url(
    base_url: &str,
    candidate: &str,
    default_path: &str,
    field_name: &str,
) -> Result<String, PairError> {
    let base = parse_bridge_base_url(base_url)?;
    let url = resolve_pair_url(&base, candidate, default_path, field_name)?;
    if !same_origin(&base, &url) {
        return Err(PairError::Other(anyhow::anyhow!(
            "bridge pair response {field_name} is outside paired bridge origin"
        )));
    }
    Ok(url.to_string())
}

fn normalize_http_pair_url(
    base_url: &str,
    candidate: &str,
    default_path: &str,
    field_name: &str,
) -> Result<String, PairError> {
    let base = parse_bridge_base_url(base_url)?;
    Ok(resolve_pair_url(&base, candidate, default_path, field_name)?.to_string())
}

fn parse_bridge_base_url(base_url: &str) -> Result<reqwest::Url, PairError> {
    let base = reqwest::Url::parse(base_url)
        .map_err(|e| PairError::Other(anyhow::anyhow!("invalid bridge base URL: {e}")))?;
    ensure_http_url(&base, "base_url")?;
    Ok(base)
}

fn resolve_pair_url(
    base: &reqwest::Url,
    candidate: &str,
    default_path: &str,
    field_name: &str,
) -> Result<reqwest::Url, PairError> {
    let raw = match candidate.trim() {
        "" => default_path,
        value => value,
    };
    let url = base.join(raw).map_err(|e| {
        PairError::Other(anyhow::anyhow!(
            "invalid bridge pair response {field_name}: {e}"
        ))
    })?;
    ensure_http_url(&url, field_name)?;
    Ok(url)
}

fn ensure_http_url(url: &reqwest::Url, field_name: &str) -> Result<(), PairError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PairError::Other(anyhow::anyhow!(
            "bridge pair response {field_name} must use http or https"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PairError::Other(anyhow::anyhow!(
            "bridge pair response {field_name} must not include credentials"
        )));
    }
    if url.host_str().is_none() {
        return Err(PairError::Other(anyhow::anyhow!(
            "bridge pair response {field_name} must include a host"
        )));
    }
    Ok(())
}

fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Spec §1.2 fallback: a bridge `firmware_version` matches `^0\.\d+\.\d+(-.+)?$`.
fn is_dcent_pack_version(v: &str) -> bool {
    let mut parts = v.splitn(3, '.');
    let major = parts.next();
    let minor = parts.next();
    let patch_rest = parts.next();
    let (Some("0"), Some(minor), Some(patch_rest)) = (major, minor, patch_rest) else {
        return false;
    };
    if minor.is_empty() || !minor.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // patch may carry a `-suffix`.
    let patch = patch_rest.split('-').next().unwrap_or("");
    !patch.is_empty() && patch.bytes().all(|b| b.is_ascii_digit())
}

/// Current Unix time in seconds, surfaced as a `PairError` on a bad clock.
fn unix_now_s() -> Result<u64, PairError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| PairError::Other(anyhow::anyhow!("system clock before unix epoch")))
}

/// Tracks the last three `last_sample_age_ms` values; reports "frozen" when all
/// three are identical (the bridge stopped sampling — spec §3.3).
#[derive(Debug, Default)]
struct StalenessTracker {
    last: [Option<u64>; 3],
}

impl StalenessTracker {
    /// Push a new age, returning true iff the last 3 observed ages are all equal.
    fn push_is_frozen(&mut self, age_ms: u64) -> bool {
        self.last[0] = self.last[1];
        self.last[1] = self.last[2];
        self.last[2] = Some(age_ms);
        matches!(
            (self.last[0], self.last[1], self.last[2]),
            (Some(a), Some(b), Some(c)) if a == b && b == c
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BridgeTelemetry, BridgeTemperature};

    fn telem(status: &str, age: u64, temp: f32) -> BridgeTelemetry {
        BridgeTelemetry {
            temperature: BridgeTemperature {
                sensor: "TMP102".into(),
                present: true,
                status: status.into(),
                external_temperature_c: temp,
                last_sample_age_ms: age,
            },
            ..Default::default()
        }
    }

    #[test]
    fn usable_temperature_predicate() {
        assert_eq!(usable_temperature(&telem("ok", 850, 23.4)), Some(23.4));
        assert_eq!(usable_temperature(&telem("ok", 5000, 23.4)), Some(23.4)); // boundary inclusive
        assert_eq!(usable_temperature(&telem("ok", 5001, 23.4)), None); // > 5000 stale
        assert_eq!(usable_temperature(&telem("missing", 10, 23.4)), None);
        assert_eq!(usable_temperature(&telem("stale", 10, 23.4)), None);
        assert_eq!(usable_temperature(&telem("fault", 10, 23.4)), None);
    }

    #[test]
    fn staleness_three_identical_ages_freezes() {
        let mut c = BridgeClient::with_http(reqwest::Client::new(), "http://10.77.0.1");
        // First two identical ages: not yet 3 in a row.
        assert_eq!(
            c.record_and_extract_temp(&telem("ok", 1000, 20.0)),
            Some(20.0)
        );
        assert_eq!(
            c.record_and_extract_temp(&telem("ok", 1000, 20.0)),
            Some(20.0)
        );
        // Third identical age -> frozen -> unavailable.
        assert_eq!(c.record_and_extract_temp(&telem("ok", 1000, 20.0)), None);
        // A fresh (different) age unfreezes.
        assert_eq!(
            c.record_and_extract_temp(&telem("ok", 1200, 21.0)),
            Some(21.0)
        );
    }

    #[test]
    fn staleness_status_not_ok_is_unavailable() {
        let mut c = BridgeClient::with_http(reqwest::Client::new(), "http://10.77.0.1");
        assert_eq!(c.record_and_extract_temp(&telem("fault", 100, 20.0)), None);
    }

    #[test]
    fn staleness_age_over_5000_is_unavailable() {
        let mut c = BridgeClient::with_http(reqwest::Client::new(), "http://10.77.0.1");
        assert_eq!(c.record_and_extract_temp(&telem("ok", 6000, 20.0)), None);
    }

    #[test]
    fn version_regex_fallback() {
        assert!(is_dcent_pack_version("0.1.0-dev"));
        assert!(is_dcent_pack_version("0.2.0"));
        assert!(is_dcent_pack_version("0.10.3-rc1"));
        assert!(!is_dcent_pack_version("1.0.0"));
        assert!(!is_dcent_pack_version("dcent-pack-0.1.0"));
        assert!(!is_dcent_pack_version("0.x.0"));
        assert!(!is_dcent_pack_version("0.1"));
    }

    #[test]
    fn pair_response_telemetry_url_accepts_same_origin_absolute() {
        let url = normalize_same_origin_pair_url(
            "http://10.77.0.1",
            "http://10.77.0.1/api/v1/telemetry",
            DEFAULT_TELEMETRY_PATH,
            "telemetry_url",
        )
        .expect("same-origin telemetry URL");
        assert_eq!(url, "http://10.77.0.1/api/v1/telemetry");
    }

    #[test]
    fn pair_response_telemetry_url_accepts_relative_path() {
        let url = normalize_same_origin_pair_url(
            "http://10.77.0.1",
            "/api/v1/telemetry",
            DEFAULT_TELEMETRY_PATH,
            "telemetry_url",
        )
        .expect("relative telemetry URL");
        assert_eq!(url, "http://10.77.0.1/api/v1/telemetry");
    }

    #[test]
    fn pair_response_telemetry_url_defaults_when_empty() {
        let url = normalize_same_origin_pair_url(
            "http://10.77.0.1",
            " ",
            DEFAULT_TELEMETRY_PATH,
            "telemetry_url",
        )
        .expect("default telemetry URL");
        assert_eq!(url, "http://10.77.0.1/api/v1/telemetry");
    }

    #[test]
    fn pair_response_telemetry_url_rejects_off_origin() {
        let err = normalize_same_origin_pair_url(
            "http://10.77.0.1",
            "http://203.0.113.1/api/v1/telemetry",
            DEFAULT_TELEMETRY_PATH,
            "telemetry_url",
        )
        .expect_err("off-origin telemetry URL must fail");
        assert!(err.to_string().contains("outside paired bridge origin"));
    }

    #[test]
    fn pair_response_proxy_url_accepts_mdns_alias() {
        let url = normalize_http_pair_url(
            "http://10.77.0.1",
            "http://dcent-pack-1234.local/",
            DEFAULT_PROXY_PATH,
            "proxy_url",
        )
        .expect("mDNS proxy alias");
        assert_eq!(url, "http://dcent-pack-1234.local/");
    }

    #[test]
    fn pair_response_proxy_url_rejects_credentials() {
        let err = normalize_http_pair_url(
            "http://10.77.0.1",
            "http://user:pass@dcent-pack-1234.local/",
            DEFAULT_PROXY_PATH,
            "proxy_url",
        )
        .expect_err("credentialed proxy URL must fail");
        assert!(err.to_string().contains("must not include credentials"));
    }
}
