//! Stratum V1 JSON-RPC message types.
//!
//! Each message is a single JSON object terminated by newline.
//! Protocol is JSON-RPC 2.0 over TCP.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{DEFAULT_V1_EXTRANONCE2_SIZE, MAX_V1_EXTRANONCE2_SIZE};

/// A JSON-RPC request (client -> server).
#[derive(Debug, Serialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// A JSON-RPC response (server -> client), for our requests.
#[derive(Debug, Deserialize)]
pub struct Response {
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

/// A server-initiated notification (no id, or id=null).
#[derive(Debug, Deserialize)]
pub struct Notification {
    pub id: Option<Value>,
    pub method: Option<String>,
    pub params: Option<Value>,
    // Also include result/error for responses
    pub result: Option<Value>,
    pub error: Option<Value>,
}

/// Parsed pool message — the result of decoding a JSON line from the pool.
#[derive(Debug, Clone)]
pub enum PoolMessage {
    /// mining.notify — new job from pool
    Notify {
        job_id: String,
        prev_hash: String,
        coinbase1: String,
        coinbase2: String,
        merkle_branches: Vec<String>,
        version: String,
        nbits: String,
        ntime: String,
        clean_jobs: bool,
    },

    /// mining.set_difficulty — pool difficulty change
    SetDifficulty(f64),

    /// mining.set_extranonce — extranonce rotation mid-session
    SetExtranonce {
        extranonce1: String,
        extranonce2_size: usize,
    },

    /// mining.set_version_mask — dynamic version mask update
    SetVersionMask(String),

    /// mining.ping — keepalive from pool
    Ping(u64),

    /// client.reconnect — pool requests migration
    Reconnect {
        host: String,
        port: u16,
        wait_seconds: u32,
    },

    /// client.get_version — pool requests our version string
    GetVersion(u64),

    /// client.show_message — informational message from pool
    ShowMessage(String),

    /// Response to one of our requests (subscribe, authorize, submit, etc.)
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    },

    /// Unrecognized message
    Unknown(String),
}

/// Parse a JSON line from the pool into a typed PoolMessage.
pub fn parse_pool_message(line: &str) -> Result<PoolMessage, serde_json::Error> {
    let v: Value = serde_json::from_str(line)?;

    // Check if this is a notification (has "method") or a response (has "result"/"error" + id)
    if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
        let params = v.get("params").cloned().unwrap_or(Value::Array(vec![]));
        let id = v.get("id").and_then(|i| i.as_u64());

        match method {
            "mining.notify" => parse_notify(params),
            "mining.set_difficulty" => parse_set_difficulty(params),
            "mining.set_extranonce" => parse_set_extranonce(params),
            "mining.set_version_mask" => parse_set_version_mask(params),
            "mining.ping" => Ok(PoolMessage::Ping(id.unwrap_or(0))),
            "client.reconnect" => parse_reconnect(params),
            "client.get_version" => Ok(PoolMessage::GetVersion(id.unwrap_or(0))),
            "client.show_message" => {
                let msg = params
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Ok(PoolMessage::ShowMessage(msg))
            }
            _ => Ok(PoolMessage::Unknown(line.to_string())),
        }
    } else if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
        // This is a response to one of our requests
        Ok(PoolMessage::Response {
            id,
            result: v.get("result").cloned(),
            error: v.get("error").cloned(),
        })
    } else {
        Ok(PoolMessage::Unknown(line.to_string()))
    }
}

fn parse_notify(params: Value) -> Result<PoolMessage, serde_json::Error> {
    let arr = match params.as_array() {
        Some(arr) => arr,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.notify: params is not an array".to_string(),
            ));
        }
    };
    if arr.len() < 9 {
        return Ok(PoolMessage::Unknown(format!(
            "mining.notify with {} params (need 9)",
            arr.len()
        )));
    }

    // Required hex-string fields. Previous code used `as_str().unwrap_or("")`
    // which silently produced a malformed Notify that the work dispatcher
    // would later fail to hex-decode. Catch the bad message at the parser
    // boundary so the V1 client debug-logs the specific bad field via the
    // existing PoolMessage::Unknown path and keeps mining at the last
    // healthy job. job_id is the dispatcher's correlation key — without it
    // we cannot tie shares back to their source job.
    fn extract_required_string<'a>(
        arr: &'a [Value],
        idx: usize,
        field: &str,
    ) -> Result<&'a str, String> {
        match arr.get(idx).and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => Ok(s),
            Some(_) => Err(format!("mining.notify: {field} string is empty")),
            None => Err(format!("mining.notify: {field} is missing or non-string")),
        }
    }

    let job_id = match extract_required_string(arr, 0, "job_id") {
        Ok(s) => s.to_string(),
        Err(msg) => return Ok(PoolMessage::Unknown(msg)),
    };
    let prev_hash = match extract_required_string(arr, 1, "prev_hash") {
        Ok(s) => s.to_string(),
        Err(msg) => return Ok(PoolMessage::Unknown(msg)),
    };
    // coinbase1/coinbase2: validate they are strings, but allow empty.
    // The Bitcoin protocol requires non-empty coinbase serialization in
    // practice, but the V1 spec doesn't forbid an empty split point and
    // the work dispatcher already handles empty inputs deterministically.
    let coinbase1 = match arr.get(2).and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.notify: coinbase1 is missing or non-string".to_string(),
            ));
        }
    };
    let coinbase2 = match arr.get(3).and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.notify: coinbase2 is missing or non-string".to_string(),
            ));
        }
    };
    // merkle_branches: must be an array. Empty array is legitimate (single
    // transaction block, just coinbase). Every element MUST be a string — a
    // non-string element is rejected as Unknown rather than silently dropped,
    // matching the fail-closed handling of every sibling field. Silently
    // filtering a middle element would shrink the branch list and compute a
    // WRONG merkle root over the surviving branches (100% share reject, or an
    // invalid solo block), with no diagnostic pointing at the cause.
    let merkle_branches: Vec<String> = match arr.get(4).and_then(|v| v.as_array()) {
        Some(a) => {
            let mut branches = Vec::with_capacity(a.len());
            for (i, v) in a.iter().enumerate() {
                match v.as_str() {
                    Some(s) => branches.push(s.to_string()),
                    None => {
                        return Ok(PoolMessage::Unknown(format!(
                            "mining.notify: merkle_branches[{}] is not a string",
                            i
                        )));
                    }
                }
            }
            branches
        }
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.notify: merkle_branches is missing or non-array".to_string(),
            ));
        }
    };
    let version = match extract_required_string(arr, 5, "version") {
        Ok(s) => s.to_string(),
        Err(msg) => return Ok(PoolMessage::Unknown(msg)),
    };
    let nbits = match extract_required_string(arr, 6, "nbits") {
        Ok(s) => s.to_string(),
        Err(msg) => return Ok(PoolMessage::Unknown(msg)),
    };
    let ntime = match extract_required_string(arr, 7, "ntime") {
        Ok(s) => s.to_string(),
        Err(msg) => return Ok(PoolMessage::Unknown(msg)),
    };
    // clean_jobs: must be bool. Non-bool defaults to false (matching the
    // legacy `unwrap_or(false)` behavior) is unsafe — a buggy pool sending
    // clean_jobs as the string "true" would silently lose the flush hint
    // and the dispatcher would keep mining stale work after a new block.
    let clean_jobs = match arr.get(8).and_then(|v| v.as_bool()) {
        Some(b) => b,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.notify: clean_jobs is missing or non-bool".to_string(),
            ));
        }
    };

    Ok(PoolMessage::Notify {
        job_id,
        prev_hash,
        coinbase1,
        coinbase2,
        merkle_branches,
        version,
        nbits,
        ntime,
        clean_jobs,
    })
}

fn parse_set_difficulty(params: Value) -> Result<PoolMessage, serde_json::Error> {
    // Validate strictly. A malformed `mining.set_difficulty` is a real risk:
    // the previous `unwrap_or(1.0)` silently coerced any garbage payload to
    // diff=1.0, which then overwrites a healthy `current_difficulty` and
    // makes the FPGA accept far easier shares than the pool actually wants.
    // Buggy pools, MITM attempts, or out-of-spec extensions all exhibit
    // this. Reject the message and let the session keep running at the
    // last-known-good difficulty by returning Unknown — the V1 client
    // debug-logs Unknown and ignores it without tearing down the connection.
    let arr = match params.as_array() {
        Some(arr) => arr,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_difficulty: params is not an array".to_string(),
            ));
        }
    };
    let first = match arr.first() {
        Some(value) => value,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_difficulty: params array is empty".to_string(),
            ));
        }
    };
    let diff = match first.as_f64() {
        Some(value) => value,
        None => {
            return Ok(PoolMessage::Unknown(format!(
                "mining.set_difficulty: params[0] is not numeric ({first})"
            )));
        }
    };
    // Reject NaN, infinities, and non-positive values. SHA256 mining
    // difficulty must be a finite positive scalar; anything else implies
    // a buggy pool message and would otherwise produce a junk share target
    // (NaN → no shares, 0 → divide-by-zero in `difficulty_to_target`,
    // negative → wrap-around in floating-point conversions).
    if !diff.is_finite() || diff <= 0.0 {
        return Ok(PoolMessage::Unknown(format!(
            "mining.set_difficulty: params[0]={diff} is not a finite positive number"
        )));
    }
    Ok(PoolMessage::SetDifficulty(diff))
}

fn parse_set_extranonce(params: Value) -> Result<PoolMessage, serde_json::Error> {
    // Validate the extranonce1 string strictly. The previous behavior
    // silently coerced missing/non-string params to "" — and because
    // `hex::decode("")` returns Ok([]) downstream, the V1 client would
    // rotate to an empty prefix without any warning. An empty extranonce1
    // means the pool is no longer issuing per-miner coinbase salt, so two
    // miners with the same template would compute the same coinbase txid.
    // Rare in practice but real correctness territory; reject as Unknown
    // and let the V1 client debug-log the bad rotation without changing
    // session state.
    //
    // The extranonce2_size oversize sentinel (MAX_V1_EXTRANONCE2_SIZE + 1)
    // is intentional — the V1 client checks `is_valid_v1_extranonce2_size`
    // and ignores the rotation with a warn. Preserve that behavior.
    let arr = match params.as_array() {
        Some(arr) => arr,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_extranonce: params is not an array".to_string(),
            ));
        }
    };
    let extranonce1 = match arr.first().and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        Some(_) => {
            return Ok(PoolMessage::Unknown(
                "mining.set_extranonce: extranonce1 string is empty".to_string(),
            ));
        }
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_extranonce: extranonce1 is missing or non-string".to_string(),
            ));
        }
    };
    // bug-hunt LOW #6 (2026-05-28): distinguish an OMITTED size (spec-permitted →
    // DEFAULT) from a PRESENT-but-malformed one. `as_u64()` returns None for
    // floats (`8.0`), negatives, and strings, so the old
    // `.and_then(as_u64).unwrap_or(DEFAULT)` SILENTLY coerced a malformed size to
    // DEFAULT(4) → wrong coinbase length → silent share rejection. Mirror the
    // strict `extranonce1` handling above: a present-but-non-integer size rejects
    // the rotation (Unknown) instead of guessing 4.
    let extranonce2_size = match arr.get(1) {
        None => DEFAULT_V1_EXTRANONCE2_SIZE,
        Some(v) => match v.as_u64() {
            Some(size) => usize::try_from(size).unwrap_or(MAX_V1_EXTRANONCE2_SIZE + 1),
            None => {
                return Ok(PoolMessage::Unknown(
                    "mining.set_extranonce: extranonce2_size present but not a \
                     non-negative integer (float/negative/string) — ignoring rotation"
                        .to_string(),
                ));
            }
        },
    };

    Ok(PoolMessage::SetExtranonce {
        extranonce1,
        extranonce2_size,
    })
}

fn parse_set_version_mask(params: Value) -> Result<PoolMessage, serde_json::Error> {
    // Validate at the wire layer. `parse_and_clamp_version_mask` downstream
    // already rejects non-hex strings (including ""), so this is mostly
    // belt-and-suspenders coverage — but pinning at the parser boundary
    // makes a future refactor that drops the downstream check less risky.
    // Preserves the existing string-shape contract: the parser still
    // returns the raw mask string and the downstream parses + clamps it.
    let arr = match params.as_array() {
        Some(arr) => arr,
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_version_mask: params is not an array".to_string(),
            ));
        }
    };
    let mask = match arr.first().and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        Some(_) => {
            return Ok(PoolMessage::Unknown(
                "mining.set_version_mask: mask string is empty".to_string(),
            ));
        }
        None => {
            return Ok(PoolMessage::Unknown(
                "mining.set_version_mask: mask is missing or non-string".to_string(),
            ));
        }
    };
    Ok(PoolMessage::SetVersionMask(mask))
}

fn parse_reconnect(params: Value) -> Result<PoolMessage, serde_json::Error> {
    // Validate strictly. The V1 client honors `client.reconnect` by storing
    // the requested host:port in `pending_reconnect`, calling
    // `backoff.reset()`, and looping immediately. A malformed payload that
    // silently coerces to host="" and port=0 (the previous behavior) means
    // connect-to-nothing fails fast → outer loop re-tries → tight reconnect
    // spiral with reset backoff. The `as u16` cast also silently truncated
    // ports >= 65536. Reject malformed messages as Unknown so the existing
    // session continues with normal exponential backoff.
    let arr = match params.as_array() {
        Some(arr) => arr,
        None => {
            return Ok(PoolMessage::Unknown(
                "client.reconnect: params is not an array".to_string(),
            ));
        }
    };
    let host = match arr.first().and_then(|v| v.as_str()) {
        Some(host) if !host.is_empty() => host.to_string(),
        Some(_) => {
            return Ok(PoolMessage::Unknown(
                "client.reconnect: host string is empty".to_string(),
            ));
        }
        None => {
            return Ok(PoolMessage::Unknown(
                "client.reconnect: host is missing or non-string".to_string(),
            ));
        }
    };
    let port = match arr.get(1).and_then(|v| v.as_u64()) {
        Some(p) if (1..=u64::from(u16::MAX)).contains(&p) => p as u16,
        Some(p) => {
            return Ok(PoolMessage::Unknown(format!(
                "client.reconnect: port {p} out of range 1..=65535"
            )));
        }
        None => {
            return Ok(PoolMessage::Unknown(
                "client.reconnect: port is missing or non-numeric".to_string(),
            ));
        }
    };
    // wait_seconds is optional (per most pool implementations) and must fit
    // in u32. Clamp to 5 minutes so a bogus pool-supplied wait cannot park
    // mining for hours.
    const MAX_RECONNECT_WAIT_SECS: u64 = 300;
    let wait_seconds = arr
        .get(2)
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(MAX_RECONNECT_WAIT_SECS) as u32;

    Ok(PoolMessage::Reconnect {
        host,
        port,
        wait_seconds,
    })
}

/// Build a mining.subscribe request.
pub fn subscribe_request(id: u64, user_agent: &str) -> Request {
    Request {
        id,
        method: "mining.subscribe".into(),
        params: serde_json::json!([user_agent]),
    }
}

/// Build a mining.authorize request.
pub fn authorize_request(id: u64, worker: &str, password: &str) -> Request {
    Request {
        id,
        method: "mining.authorize".into(),
        params: serde_json::json!([worker, password]),
    }
}

/// Build a mining.configure request.
///
/// BIP 310 lets the miner advertise independent extensions in one request.
/// `subscribe-extranonce` is always advertised because the V1 client handles
/// `mining.set_extranonce`; `minimum-difficulty` is advertised only when the
/// caller has a concrete difficulty hint to send.
pub fn configure_request(
    id: u64,
    version_rolling_mask: &str,
    minimum_difficulty: Option<u64>,
) -> Request {
    let mut capabilities = vec!["version-rolling", "subscribe-extranonce"];
    let mut options = Map::new();
    options.insert(
        "version-rolling.mask".to_string(),
        Value::String(version_rolling_mask.to_string()),
    );
    options.insert(
        "version-rolling.min-bit-count".to_string(),
        Value::Number(2.into()),
    );
    if let Some(difficulty) = minimum_difficulty {
        capabilities.insert(1, "minimum-difficulty");
        options.insert(
            "minimum-difficulty.value".to_string(),
            Value::Number(difficulty.into()),
        );
    }

    Request {
        id,
        method: "mining.configure".into(),
        params: Value::Array(vec![
            serde_json::json!(capabilities),
            Value::Object(options),
        ]),
    }
}

/// Build a mining.submit request.
pub fn submit_request(
    id: u64,
    worker: &str,
    job_id: &str,
    extranonce2: &str,
    ntime: &str,
    nonce: &str,
    version_bits: Option<&str>,
) -> Request {
    let mut params = vec![
        Value::String(worker.into()),
        Value::String(job_id.into()),
        Value::String(extranonce2.into()),
        Value::String(ntime.into()),
        Value::String(nonce.into()),
    ];

    if let Some(vb) = version_bits {
        params.push(Value::String(vb.into()));
    }

    Request {
        id,
        method: "mining.submit".into(),
        params: Value::Array(params),
    }
}

/// Build a mining.suggest_difficulty request.
///
/// This is a static startup hint, not a runtime difficulty floor. Pools remain
/// authoritative and may ignore it or later override difficulty with
/// `mining.set_difficulty`.
pub fn suggest_difficulty_request(id: u64, difficulty: u64) -> Request {
    Request {
        id,
        method: "mining.suggest_difficulty".into(),
        params: serde_json::json!([difficulty]),
    }
}

/// Build a `mining.extranonce.subscribe` request.
///
/// Bitmain extension (also implemented by ckpool, NiceHash, and most
/// modern pools). Tells the pool we want to receive `mining.set_extranonce`
/// notifications mid-session so the pool can rotate our extranonce1 prefix
/// without forcing a full reconnect+resubscribe cycle.
///
/// This is conceptually the same intent as advertising the
/// `subscribe-extranonce` capability inside `mining.configure`, but some
/// Bitmain-flavored pools key off the explicit method call and ignore the
/// configure-bag advertisement. We send both so we cover both dialects.
///
/// Pools that do not implement this extension respond with an error result;
/// the V1 client logs the rejection and keeps mining (the `mining.configure`
/// advertisement still applies if the pool honors it that way). The session
/// is never torn down on a missing extension.
pub fn extranonce_subscribe_request(id: u64) -> Request {
    Request {
        id,
        method: "mining.extranonce.subscribe".into(),
        // Bitmain pools ship empty params; the spec only carries the method
        // name. Keep the empty array form rather than `null` to match the
        // exact wire shape pools observe from cgminer/bmminer.
        params: Value::Array(vec![]),
    }
}

/// Build a response to mining.ping.
pub fn pong_response(id: u64) -> String {
    let mut s = serde_json::json!({"id": id, "result": null, "error": null}).to_string();
    s.push('\n');
    s
}

/// Build a response to client.get_version.
pub fn version_response(id: u64, version: &str) -> String {
    let mut s = serde_json::json!({"id": id, "result": version, "error": null}).to_string();
    s.push('\n');
    s
}

/// Serialize a request to a newline-terminated JSON string.
pub fn serialize_request(req: &Request) -> String {
    let mut s = serde_json::to_string(req).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pool_message_never_panics_on_arbitrary_or_malformed_input() {
        // Fuzz the untrusted pool-line parser (priority 1: production risk). A
        // malformed, out-of-spec, or hostile pool message must NEVER crash the
        // daemon — it may only return Err or PoolMessage::Unknown so the V1 client
        // keeps mining the last healthy job. Deterministic LCG (reproducible; the
        // harness forbids RNG). Three families: raw random strings (the JSON-parse
        // boundary), structured malformed JSON envelopes carrying every known
        // stratum method with randomized params (reaching every sub-parser: notify /
        // set_difficulty / set_extranonce / set_version_mask / reconnect / ping /
        // show_message), and response-shaped messages with random result/error. The
        // only assertion is that the call RETURNS — never panics.
        let mut lcg: u64 = 0xDA3E_39CB_94B6_95A5;
        let mut next = || {
            lcg = lcg
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (lcg >> 33) as u32
        };
        let tok = |n: u32| -> String {
            match n % 13 {
                0 => "1".into(),
                1 => "-1".into(),
                2 => "1e400".into(), // overflows to inf on as_f64
                3 => "-1e400".into(),
                4 => "3.14".into(),
                5 => "\"\"".into(),
                6 => "\"deadbeef\"".into(),
                7 => "true".into(),
                8 => "null".into(),
                9 => "[]".into(),
                10 => "{}".into(),
                11 => "100000000000000000000000".into(), // beyond u64/i64
                _ => format!("\"{}\"", "f".repeat(300)),
            }
        };
        let methods = [
            "mining.notify",
            "mining.set_difficulty",
            "mining.set_extranonce",
            "mining.set_version_mask",
            "mining.ping",
            "client.reconnect",
            "client.get_version",
            "client.show_message",
            "unknown.method",
        ];
        for _ in 0..6000u32 {
            let line = match next() % 3 {
                0 => {
                    // Raw random Latin-1 string — mostly invalid JSON.
                    let len = (next() % 220) as usize;
                    let mut s = String::with_capacity(len);
                    for _ in 0..len {
                        s.push(char::from((next() % 0x100) as u8));
                    }
                    s
                }
                1 => {
                    // Structured malformed JSON: known method + random params.
                    let method = methods[(next() as usize) % methods.len()];
                    let nparams = (next() % 14) as usize;
                    let mut params = String::from("[");
                    for i in 0..nparams {
                        if i > 0 {
                            params.push(',');
                        }
                        params.push_str(&tok(next()));
                    }
                    params.push(']');
                    format!(
                        "{{\"id\":{},\"method\":\"{method}\",\"params\":{params}}}",
                        next() % 1000
                    )
                }
                _ => {
                    // Response-shaped: id + random result/error scalars.
                    format!(
                        "{{\"id\":{},\"result\":{},\"error\":{}}}",
                        next(),
                        tok(next()),
                        tok(next())
                    )
                }
            };
            let _ = parse_pool_message(&line); // MUST NOT panic on any input
        }
    }

    #[test]
    fn test_parse_notify() {
        let json = r#"{"id":null,"method":"mining.notify","params":["bf","4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6a0000000000000000000000000","01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020e1304","0d2f5374726174756d506f6f6c2f",["b1e3f140fa3d8f1c5e6abc3c0d3a7e96e051e3e3a6f9a0d3c4b2a1e0f9d8c7b6"],"20000000","170b3ce9","65a7e340",true]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::Notify {
                job_id,
                clean_jobs,
                version,
                ..
            } => {
                assert_eq!(job_id, "bf");
                assert!(clean_jobs);
                assert_eq!(version, "20000000");
            }
            other => panic!("Expected Notify, got {:?}", other),
        }
    }

    /// Build a `mining.notify` JSON line from explicit field values for
    /// per-field rejection tests. Helper accepts raw JSON snippets so each
    /// test can substitute a single field with malformed content while
    /// keeping the rest valid.
    fn build_notify_json(
        job_id: &str,
        prev_hash: &str,
        coinbase1: &str,
        coinbase2: &str,
        merkle_branches: &str,
        version: &str,
        nbits: &str,
        ntime: &str,
        clean_jobs: &str,
    ) -> String {
        format!(
            r#"{{"id":null,"method":"mining.notify","params":[{job_id},{prev_hash},{coinbase1},{coinbase2},{merkle_branches},{version},{nbits},{ntime},{clean_jobs}]}}"#
        )
    }

    #[test]
    fn test_parse_notify_rejects_empty_job_id() {
        // Empty job_id used to silently produce Notify { job_id: "" }; the
        // dispatcher could not correlate shares back to a specific job and
        // any submit would be tagged with the empty string.
        let json = build_notify_json(
            "\"\"",
            "\"4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6a0000000000000000000000000\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("job_id string is empty")),
            "empty job_id must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_null_prev_hash() {
        let json = build_notify_json(
            "\"bf\"",
            "null",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("prev_hash is missing or non-string")),
            "null prev_hash must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_empty_prev_hash() {
        let json = build_notify_json(
            "\"bf\"",
            "\"\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("prev_hash string is empty")),
            "empty prev_hash must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_non_string_coinbase1() {
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "42",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("coinbase1 is missing or non-string")),
            "non-string coinbase1 must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_allows_empty_coinbase_strings_but_typed() {
        // Real pools never split coinbase at the boundaries, but the V1 spec
        // doesn't forbid an empty coinbase1 or coinbase2 — a notify with
        // typed-empty strings should still produce a valid Notify so the
        // dispatcher can decide what to do downstream.
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"\"",
            "\"\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        match msg {
            PoolMessage::Notify {
                coinbase1,
                coinbase2,
                merkle_branches,
                ..
            } => {
                assert!(coinbase1.is_empty());
                assert!(coinbase2.is_empty());
                assert!(merkle_branches.is_empty());
            }
            other => panic!(
                "expected Notify with empty coinbase fields, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_parse_notify_rejects_non_array_merkle_branches() {
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "\"deadbeef\"",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("merkle_branches is missing or non-array")),
            "non-array merkle_branches must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_non_string_merkle_branch_element() {
        // Regression (R-04): a non-string element in the middle of the merkle
        // branch array must be rejected as Unknown, not silently dropped — a
        // dropped element yields a wrong merkle root and 100% share rejection.
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "[\"aa\",42,\"bb\"]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("merkle_branches[1] is not a string")),
            "non-string merkle branch element must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_empty_version() {
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("version string is empty")),
            "empty version must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_empty_nbits_or_ntime() {
        let empty_nbits = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"\"",
            "\"65a7e340\"",
            "true",
        );
        let msg = parse_pool_message(&empty_nbits).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("nbits string is empty")),
            "empty nbits must surface as Unknown, got {:?}",
            msg
        );

        let empty_ntime = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"\"",
            "true",
        );
        let msg = parse_pool_message(&empty_ntime).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("ntime string is empty")),
            "empty ntime must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_non_bool_clean_jobs() {
        // Buggy pool ships clean_jobs as the string "true" — `as_bool`
        // returns None, and the previous `unwrap_or(false)` would silently
        // lose the flush hint. The dispatcher would then keep mining stale
        // work after a new block was found, wasting hashrate and burning
        // shares on dead work. Reject as Unknown.
        let json = build_notify_json(
            "\"bf\"",
            "\"4d16\"",
            "\"01\"",
            "\"02\"",
            "[]",
            "\"20000000\"",
            "\"170b3ce9\"",
            "\"65a7e340\"",
            "\"true\"",
        );
        let msg = parse_pool_message(&json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("clean_jobs is missing or non-bool")),
            "non-bool clean_jobs must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_notify_rejects_non_array_params() {
        let json = r#"{"id":null,"method":"mining.notify","params":"deadbeef"}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("not an array")),
            "non-array params must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_difficulty() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[16384]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::SetDifficulty(d) => assert_eq!(d, 16384.0),
            other => panic!("Expected SetDifficulty, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_difficulty_accepts_float_value() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[2048.5]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::SetDifficulty(d) => assert!((d - 2048.5).abs() < 1e-9, "got {d}"),
            other => panic!("Expected SetDifficulty, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_difficulty_rejects_empty_params_array() {
        // Empty array used to silently coerce to diff=1.0 and stomp the
        // healthy current_difficulty. Now it surfaces as Unknown so the
        // session keeps running at the previous difficulty.
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::Unknown(raw) => {
                assert!(raw.contains("set_difficulty"));
                assert!(raw.to_lowercase().contains("empty"));
            }
            other => panic!("Expected Unknown for empty params, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_difficulty_rejects_non_array_params() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":4096}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(msg, PoolMessage::Unknown(raw) if raw.contains("not an array")),
            "non-array params must surface as Unknown"
        );
    }

    #[test]
    fn test_parse_set_difficulty_rejects_string_value() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":["hard"]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(msg, PoolMessage::Unknown(raw) if raw.contains("not numeric")),
            "string difficulty must surface as Unknown"
        );
    }

    #[test]
    fn test_parse_set_difficulty_rejects_null_value() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[null]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(msg, PoolMessage::Unknown(raw) if raw.contains("not numeric")),
            "null difficulty must surface as Unknown"
        );
    }

    #[test]
    fn test_parse_set_difficulty_rejects_zero() {
        // diff=0 would divide-by-zero in difficulty_to_target.
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[0]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(msg, PoolMessage::Unknown(raw) if raw.contains("finite positive")),
            "zero difficulty must surface as Unknown"
        );
    }

    #[test]
    fn test_parse_set_difficulty_rejects_negative() {
        let json = r#"{"id":null,"method":"mining.set_difficulty","params":[-1]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(msg, PoolMessage::Unknown(raw) if raw.contains("finite positive")),
            "negative difficulty must surface as Unknown"
        );
    }

    #[test]
    fn test_parse_reconnect_happy_path() {
        let json =
            r#"{"id":null,"method":"client.reconnect","params":["pool.example.com",3333,5]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::Reconnect {
                host,
                port,
                wait_seconds,
            } => {
                assert_eq!(host, "pool.example.com");
                assert_eq!(port, 3333);
                assert_eq!(wait_seconds, 5);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_reconnect_omits_wait_seconds_defaults_to_zero() {
        let json = r#"{"id":null,"method":"client.reconnect","params":["pool.example.com",3333]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::Reconnect { wait_seconds, .. } => {
                assert_eq!(wait_seconds, 0);
            }
            other => panic!("Expected Reconnect, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_reconnect_rejects_empty_host() {
        // Empty host used to silently coerce — outer loop would store
        // pending_reconnect=("", port), reset backoff, and connect to
        // nothing → tight reconnect spiral.
        let json = r#"{"id":null,"method":"client.reconnect","params":["",3333,0]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("host string is empty")),
            "empty host must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_missing_host() {
        let json = r#"{"id":null,"method":"client.reconnect","params":[]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("host is missing")),
            "missing host must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_non_string_host() {
        let json = r#"{"id":null,"method":"client.reconnect","params":[42,3333]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("host is missing or non-string")),
            "non-string host must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_port_zero() {
        // Port 0 is not a valid TCP destination — the previous code
        // accepted it via `unwrap_or(0)` and the outer loop would still
        // attempt to connect.
        let json = r#"{"id":null,"method":"client.reconnect","params":["pool.example.com",0]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("port 0 out of range")),
            "port 0 must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_oversize_port() {
        // Previously `as u16` silently truncated 99999 → 34463.
        let json = r#"{"id":null,"method":"client.reconnect","params":["pool.example.com",99999]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("port 99999 out of range")),
            "oversize port must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_missing_port() {
        let json = r#"{"id":null,"method":"client.reconnect","params":["pool.example.com"]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("port is missing")),
            "missing port must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_rejects_non_numeric_port() {
        let json =
            r#"{"id":null,"method":"client.reconnect","params":["pool.example.com","3333"]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("port is missing or non-numeric")),
            "non-numeric port must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_reconnect_clamps_oversize_wait_seconds() {
        for raw_wait in [86_400_u64, u64::from(u32::MAX)] {
            let json = format!(
                r#"{{"id":null,"method":"client.reconnect","params":["pool.example.com",3333,{raw_wait}]}}"#
            );
            let msg = parse_pool_message(&json).unwrap();
            match msg {
                PoolMessage::Reconnect { wait_seconds, .. } => {
                    assert_eq!(
                        wait_seconds, 300,
                        "wait {raw_wait} must be clamped to 5 minutes"
                    );
                }
                other => panic!("Expected Reconnect with clamped wait, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_parse_reconnect_preserves_small_wait_seconds() {
        for raw_wait in [0_u32, 5] {
            let json = format!(
                r#"{{"id":null,"method":"client.reconnect","params":["pool.example.com",3333,{raw_wait}]}}"#
            );
            let msg = parse_pool_message(&json).unwrap();
            match msg {
                PoolMessage::Reconnect { wait_seconds, .. } => {
                    assert_eq!(wait_seconds, raw_wait);
                }
                other => panic!("Expected Reconnect, got {:?}", other),
            }
        }
    }

    #[test]
    fn test_parse_reconnect_rejects_non_array_params() {
        let json = r#"{"id":null,"method":"client.reconnect","params":42}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("not an array")),
            "non-array params must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_difficulty_treats_json_non_finite_as_null() {
        // JSON cannot transport NaN or Infinity — `serde_json::json!` renders
        // both as `null`. The protocol-realistic non-finite case therefore
        // reaches `parse_set_difficulty` as `Value::Null` and is rejected
        // by the "not numeric" branch, NOT by the inner `!is_finite()`
        // guard. Pin this so a future refactor that drops the null branch
        // doesn't silently let `1.0` through again.
        let nan_params = serde_json::json!([f64::NAN]);
        let inf_params = serde_json::json!([f64::INFINITY]);
        for (label, value) in [("NaN", nan_params), ("INF", inf_params)] {
            let result = parse_set_difficulty(value).unwrap();
            assert!(
                matches!(&result, PoolMessage::Unknown(raw) if raw.contains("not numeric")),
                "{label} via JSON must surface as Unknown via the not-numeric branch, got {:?}",
                result
            );
        }
    }

    #[test]
    fn test_parse_set_extranonce_default_size() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef"]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::SetExtranonce {
                extranonce1,
                extranonce2_size,
            } => {
                assert_eq!(extranonce1, "deadbeef");
                assert_eq!(extranonce2_size, DEFAULT_V1_EXTRANONCE2_SIZE);
            }
            other => panic!("Expected SetExtranonce, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_extranonce_preserves_oversize_for_client_reject() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef",9]}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::SetExtranonce {
                extranonce2_size, ..
            } => assert_eq!(extranonce2_size, MAX_V1_EXTRANONCE2_SIZE + 1),
            other => panic!("Expected SetExtranonce, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_set_extranonce_rejects_non_integer_size() {
        // bug-hunt LOW #6 (2026-05-28): a PRESENT-but-non-integer extranonce2_size
        // (float `8.0`, negative, or string) must be rejected (Unknown), NOT
        // silently coerced to DEFAULT(4) — which would mine at the wrong coinbase
        // length and get every share silently rejected. (Omitted size still
        // defaults — see test_parse_set_extranonce_default_size.)
        for bad in [
            r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef",8.0]}"#,
            r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef",-1]}"#,
            r#"{"id":null,"method":"mining.set_extranonce","params":["deadbeef","8"]}"#,
        ] {
            let msg = parse_pool_message(bad).unwrap();
            assert!(
                matches!(msg, PoolMessage::Unknown(_)),
                "non-integer extranonce2_size must be Unknown, got {msg:?} for {bad}"
            );
        }
    }

    #[test]
    fn test_parse_response() {
        let json = r#"{"id":3,"error":null,"result":true}"#;
        let msg = parse_pool_message(json).unwrap();
        match msg {
            PoolMessage::Response { id, result, .. } => {
                assert_eq!(id, 3);
                assert_eq!(result, Some(Value::Bool(true)));
            }
            other => panic!("Expected Response, got {:?}", other),
        }
    }

    #[test]
    fn test_serialize_subscribe() {
        let req = subscribe_request(1, "dcentrald/0.1.0");
        let s = serialize_request(&req);
        assert!(s.contains("mining.subscribe"));
        assert!(s.contains("dcentrald/0.1.0"));
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn configure_request_uses_caller_supplied_version_mask() {
        let req = configure_request(7, "00ffe000", Some(2048));
        let s = serialize_request(&req);
        let value: Value = serde_json::from_str(&s).expect("configure JSON");

        assert_eq!(value["method"], "mining.configure");
        assert_eq!(value["params"][1]["version-rolling.mask"], "00ffe000");
        assert_eq!(value["params"][1]["minimum-difficulty.value"], 2048);
    }

    #[test]
    fn parses_set_version_mask_raw_for_contextual_validation() {
        let msg = parse_pool_message(
            r#"{"id":null,"method":"mining.set_version_mask","params":["00ffe000"]}"#,
        )
        .unwrap();
        assert!(matches!(msg, PoolMessage::SetVersionMask(mask) if mask == "00ffe000"));

        // Previously this returned `SetVersionMask("")`. Now empty/missing
        // params surface as Unknown so the parser layer never produces a
        // mask string the downstream cannot use.
        let missing =
            parse_pool_message(r#"{"id":null,"method":"mining.set_version_mask","params":[]}"#)
                .unwrap();
        assert!(
            matches!(&missing, PoolMessage::Unknown(raw) if raw.contains("mask is missing")),
            "missing mask must surface as Unknown, got {:?}",
            missing
        );
    }

    #[test]
    fn test_parse_set_version_mask_rejects_empty_string() {
        let json = r#"{"id":null,"method":"mining.set_version_mask","params":[""]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("mask string is empty")),
            "empty mask must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_version_mask_rejects_non_string() {
        let json = r#"{"id":null,"method":"mining.set_version_mask","params":[123]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("mask is missing or non-string")),
            "non-string mask must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_version_mask_rejects_non_array_params() {
        let json = r#"{"id":null,"method":"mining.set_version_mask","params":"00ffe000"}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("not an array")),
            "non-array params must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_extranonce_rejects_empty_extranonce1() {
        // Empty extranonce1 used to silently rotate the V1 client to
        // an empty prefix because hex::decode("") returns Ok([]).
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":["",4]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("extranonce1 string is empty")),
            "empty extranonce1 must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_extranonce_rejects_missing_extranonce1() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":[]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("extranonce1 is missing")),
            "missing extranonce1 must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_extranonce_rejects_non_string_extranonce1() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":[42,4]}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("missing or non-string")),
            "non-string extranonce1 must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn test_parse_set_extranonce_rejects_non_array_params() {
        let json = r#"{"id":null,"method":"mining.set_extranonce","params":"deadbeef"}"#;
        let msg = parse_pool_message(json).unwrap();
        assert!(
            matches!(&msg, PoolMessage::Unknown(raw) if raw.contains("not an array")),
            "non-array params must surface as Unknown, got {:?}",
            msg
        );
    }

    #[test]
    fn submit_request_appends_bip310_version_bits_only_when_present() {
        let no_version_bits = submit_request(
            8,
            "user.worker",
            "job-1",
            "00000001",
            "66112233",
            "aabbccdd",
            None,
        );
        let no_version_bits_json: Value =
            serde_json::from_str(&serialize_request(&no_version_bits)).expect("submit JSON");
        assert_eq!(no_version_bits_json["params"].as_array().unwrap().len(), 5);

        let with_version_bits = submit_request(
            9,
            "user.worker",
            "job-2",
            "00000002",
            "66112234",
            "11223344",
            Some("0000e000"),
        );
        let with_version_bits_json: Value =
            serde_json::from_str(&serialize_request(&with_version_bits)).expect("submit JSON");
        assert_eq!(
            with_version_bits_json["params"].as_array().unwrap().len(),
            6
        );
        assert_eq!(with_version_bits_json["params"][5], "0000e000");
    }

    // -----------------------------------------------------------------------
    // V1 outbound request builder wire-format pins.
    // -----------------------------------------------------------------------

    #[test]
    fn subscribe_request_wire_format() {
        let req = subscribe_request(1, "dcentos/0.5.0");
        assert_eq!(req.id, 1);
        assert_eq!(req.method, "mining.subscribe");

        // Params is a single-element array carrying the user-agent.
        let params = req.params.as_array().expect("params is array");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].as_str(), Some("dcentos/0.5.0"));
    }

    #[test]
    fn subscribe_request_serializes_to_newline_terminated_json() {
        let req = subscribe_request(7, "ua");
        let line = serialize_request(&req);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["id"], 7);
        assert_eq!(parsed["method"], "mining.subscribe");
        assert_eq!(parsed["params"][0], "ua");
    }

    #[test]
    fn authorize_request_carries_worker_and_password_in_order() {
        let req = authorize_request(2, "user.worker", "x");
        assert_eq!(req.id, 2);
        assert_eq!(req.method, "mining.authorize");

        // Params: [worker, password] — order matters for the pool.
        let params = req.params.as_array().expect("params is array");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].as_str(), Some("user.worker"));
        assert_eq!(params[1].as_str(), Some("x"));
    }

    #[test]
    fn authorize_request_handles_empty_password() {
        // Some pools accept empty passwords. Pin so a refactor that
        // injected a default doesn't silently change the auth contract.
        let req = authorize_request(3, "worker", "");
        let params = req.params.as_array().unwrap();
        assert_eq!(params[1].as_str(), Some(""));
    }

    #[test]
    fn configure_request_method_and_envelope_layout() {
        let req = configure_request(4, "1fffe000", Some(8192));
        assert_eq!(req.method, "mining.configure");

        // Params layout per BIP310:
        //   [["version-rolling","minimum-difficulty","subscribe-extranonce"], {...}]
        let params = req.params.as_array().expect("params is array");
        assert_eq!(params.len(), 2);

        let extensions = params[0].as_array().expect("extensions is array");
        assert_eq!(extensions.len(), 3);
        assert_eq!(extensions[0].as_str(), Some("version-rolling"));
        assert_eq!(extensions[1].as_str(), Some("minimum-difficulty"));
        assert_eq!(extensions[2].as_str(), Some("subscribe-extranonce"));

        let opts = params[1].as_object().expect("opts is object");
        assert_eq!(opts["version-rolling.mask"].as_str(), Some("1fffe000"));
        assert_eq!(opts["version-rolling.min-bit-count"].as_u64(), Some(2));
        assert_eq!(opts["minimum-difficulty.value"].as_u64(), Some(8192));
    }

    #[test]
    fn configure_request_omits_minimum_difficulty_without_hint() {
        let req = configure_request(4, "1fffe000", None);
        let params = req.params.as_array().expect("params is array");

        let extensions = params[0].as_array().expect("extensions is array");
        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions[0].as_str(), Some("version-rolling"));
        assert_eq!(extensions[1].as_str(), Some("subscribe-extranonce"));

        let opts = params[1].as_object().expect("opts is object");
        assert!(!opts.contains_key("minimum-difficulty.value"));
    }

    #[test]
    fn suggest_difficulty_request_wire_format() {
        let req = suggest_difficulty_request(5, 8192);
        assert_eq!(req.id, 5);
        assert_eq!(req.method, "mining.suggest_difficulty");
        let params = req.params.as_array().unwrap();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].as_u64(), Some(8192));
    }

    #[test]
    fn pong_response_carries_id_with_null_result_and_error() {
        let resp = pong_response(42);
        assert!(resp.ends_with('\n'));
        let parsed: Value = serde_json::from_str(resp.trim_end()).unwrap();
        assert_eq!(parsed["id"], 42);
        assert!(parsed["result"].is_null());
        assert!(parsed["error"].is_null());
    }

    #[test]
    fn pong_response_handles_zero_id() {
        // Some pools send ping with id=0. Pin so the response also uses 0.
        let resp = pong_response(0);
        let parsed: Value = serde_json::from_str(resp.trim_end()).unwrap();
        assert_eq!(parsed["id"], 0);
    }

    #[test]
    fn version_response_carries_version_string_in_result() {
        let resp = version_response(99, "dcentos/0.5.0");
        assert!(resp.ends_with('\n'));
        let parsed: Value = serde_json::from_str(resp.trim_end()).unwrap();
        assert_eq!(parsed["id"], 99);
        assert_eq!(parsed["result"].as_str(), Some("dcentos/0.5.0"));
        assert!(parsed["error"].is_null());
    }

    #[test]
    fn serialize_request_appends_newline() {
        let req = Request {
            id: 1,
            method: "test.method".into(),
            params: serde_json::json!([1, 2, 3]),
        };
        let line = serialize_request(&req);
        assert!(line.ends_with('\n'));
        assert!(!line.trim_end().is_empty());

        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["method"], "test.method");
    }

    #[test]
    fn serialize_request_round_trips_through_json() {
        // Build → serialize → deserialize must preserve every field.
        let original = subscribe_request(8, "ua");
        let line = serialize_request(&original);
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();

        assert_eq!(parsed["id"].as_u64(), Some(8));
        assert_eq!(parsed["method"].as_str(), Some("mining.subscribe"));
        assert_eq!(parsed["params"][0].as_str(), Some("ua"));
    }

    #[test]
    fn extranonce_subscribe_request_wire_format() {
        // Bitmain extension: method name only, empty params array. Pools
        // that don't honor the extension respond with an error result;
        // the V1 client logs and keeps mining. Pin the wire shape so a
        // future refactor that ships `null` params or extra arguments
        // doesn't silently break the dialect for Antminer-flavored pools.
        let req = extranonce_subscribe_request(42);
        assert_eq!(req.id, 42);
        assert_eq!(req.method, "mining.extranonce.subscribe");

        let params = req.params.as_array().expect("params is array");
        assert!(
            params.is_empty(),
            "Bitmain mining.extranonce.subscribe carries no params, got {:?}",
            params
        );

        // Round-trip through serialization to make sure the JSON line
        // pools see is exactly `{"id":N,"method":"mining.extranonce.subscribe","params":[]}`
        // (plus trailing newline).
        let line = serialize_request(&req);
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["id"], 42);
        assert_eq!(parsed["method"], "mining.extranonce.subscribe");
        assert_eq!(parsed["params"], serde_json::json!([]));
    }

    #[test]
    fn submit_request_omits_version_bits_field_when_none() {
        // Pin: 5-element params (no 6th version_bits entry) when caller
        // passes None. Sending an extra null param confuses some pools.
        let req = submit_request(9, "w", "j", "00", "01", "02", None);
        let params = req.params.as_array().unwrap();
        assert_eq!(
            params.len(),
            5,
            "version_bits=None must produce 5 params, NOT 6 with a null"
        );
    }
}
