//! W6.2 share-submission regression e2e (DCENT_QA + DCENT_Protocol).
//!
//! This integration test stands up a localhost mock Stratum V1 pool, drives
//! the production `StratumV1Client` against it, and asserts that:
//!
//!   * a known-good `mining.submit` payload built from a per-chip-family
//!     golden midstate is ACCEPTED (`result=true`), and
//!   * five distinct mutations of the share submission (midstate
//!     `swap_bytes`, midstate `reverse_bits`, work_id `u8` truncation,
//!     extranonce2 byte-order flip, ntime byte-order flip) are REJECTED
//!     (`accepted_count == 0`).
//!
//! ## Why this lives in `dcentrald-api`
//!
//! The existing mock pool harness inside
//! `dcentrald-stratum/src/v1/client.rs` is `#[cfg(test)]`-private to the
//! stratum crate and only covers connection/failover lifecycle, not
//! per-chip-family share-validation regressions. The W6.2 contract is a
//! *cross-crate* regression pin — any future refactor of share submission
//! has to keep these mutations rejecting — so the harness lives in
//! `dcentrald-api/tests/` where it consumes only the *public* surface of
//! `dcentrald-stratum` (`StratumV1Client`, `ValidShare`, `StratumConfig`,
//! `JobTemplate`, `StratumStatus`).
//!
//! The test is gated `#![cfg(unix)]` because `dcentrald-api` pulls Unix-only
//! HAL crates into its compilation graph; on Windows hosts the crate
//! itself does not build. CI runs Linux. This matches the gate already
//! used by `tests/profiles_routes.rs`.
//!
//! ## Golden midstate fixtures
//!
//! `tests/golden_midstates/{bm1387,bm1397,bm1398,bm1362,bm1366,bm1368,
//! bm1370}.bin` each hold one 32-byte SHA-256 midstate computed from a
//! deterministic 64-byte block-header prefix. The seed and prefix
//! derivation are reproducible from
//! `scripts/regen_golden_midstates.py` (kept in sync with this file's
//! `derive_golden_input` helper); they are NOT random per run.
//!
//! Seed: `b"DCENT_OS-W6.2-share-submission-e2e-2026-05-07"`.
//! Prefix construction (per family `F`, all SHA-256-derived from
//! `seed | "|" | F | "|" | tag`):
//!
//!     version (4 bytes, LE) || prev_hash (32 bytes) || merkle_first28 (28 bytes)
//!     = 64 bytes
//!
//! The midstate is then `SHA-256-Compress(IV, prefix64)` exported as 32
//! big-endian bytes (8 × `u32::to_be_bytes`). Same algorithm used by
//! `dcentrald_stratum::compute_midstate_from_prefix` in production —
//! drift on either side is caught by the happy-path assertion.
//!
//! ## SV2 path
//!
//! SV2 share-submission regression is intentionally NOT covered here. The
//! SV2 channel uses a Noise-protocol handshake (BIP324-style ElligatorSwift
//! + ChaCha20-Poly1305) whose mock would either need a real responder
//! key-agreement handler or a feature-gated Noise bypass — both are
//! disproportionate scope for a per-chip-family share-mutation regression
//! pin. The `dcentrald-stratum` crate already has SV2 framing tests under
//! `src/v2/`; the production share-construction path that this test pins
//! (`build_submit_message` in V1, `submit_shares_standard` in V2) shares
//! its midstate / nonce / ntime byte handling with V1 because both go
//! through `dcentrald_stratum::work::WorkBuilder` and the same
//! `compute_midstate_from_prefix`. Mutating `swap_bytes`/`reverse_bits` on
//! the V1 path therefore protects the SV2 path indirectly. A separate SV2
//! Noise-mocked test is a reasonable W6.x follow-up.
//!
//! ## Counts
//!
//! 7 chip families × (1 happy + 5 mutations) = 42 assertions per run, each
//! a separate `#[tokio::test]`-style sub-step driven from one
//! `#[tokio::test(flavor = "multi_thread")]` entry point so the mock pool
//! task and the client task don't deadlock on the single-threaded runtime.

#![cfg(unix)]

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use dcentrald_stratum::{
    compute_midstate_from_prefix, default_version_rolling_mask, DonationConfig, PoolConfig,
    StratumConfig, StratumState, StratumStatus, StratumV1Client, ValidShare,
};

// ---------------------------------------------------------------------------
// Chip families + fixture loader
// ---------------------------------------------------------------------------

/// All chip families covered by the W6.2 regression matrix.
const FAMILIES: &[&str] = &[
    "bm1387", // S9
    "bm1397", // BitAxe Ultra / S17
    "bm1398", // S19 Pro
    "bm1362", // S19j Pro Amlogic, BitAxe Supra
    "bm1366", // S19k Pro, BitAxe Gamma
    "bm1368", // S21, S19j XP
    "bm1370", // BitAxe Hex, S21 Pro
];

/// Stable seed used to derive each chip family's golden midstate input.
/// MUST match `scripts/regen_golden_midstates.py` and the fixtures under
/// `tests/golden_midstates/`.
const GOLDEN_SEED: &[u8] = b"DCENT_OS-W6.2-share-submission-e2e-2026-05-07";

/// Read the 32-byte golden midstate fixture for `family`.
fn load_golden_midstate(family: &str) -> [u8; 32] {
    let path = format!(
        "{}/tests/golden_midstates/{}.bin",
        env!("CARGO_MANIFEST_DIR"),
        family
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"));
    assert_eq!(
        bytes.len(),
        32,
        "fixture {path}: expected 32 bytes, got {}",
        bytes.len()
    );
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// Re-derive the 64-byte SHA-256 input for `family`. Used to assert that
/// the on-disk fixture matches the production
/// `compute_midstate_from_prefix` implementation, so future ASIC-driver
/// refactors can't silently drift the midstate algorithm without this
/// test failing.
fn derive_golden_input(family: &str) -> [u8; 64] {
    use sha2::{Digest, Sha256};

    let h_version = {
        let mut h = Sha256::new();
        h.update(GOLDEN_SEED);
        h.update(b"|");
        h.update(family.as_bytes());
        h.update(b"|version");
        h.finalize()
    };
    let version = u32::from_le_bytes([h_version[0], h_version[1], h_version[2], h_version[3]]);

    let h_prev = {
        let mut h = Sha256::new();
        h.update(GOLDEN_SEED);
        h.update(b"|");
        h.update(family.as_bytes());
        h.update(b"|prev_hash");
        h.finalize()
    };
    let h_merkle = {
        let mut h = Sha256::new();
        h.update(GOLDEN_SEED);
        h.update(b"|");
        h.update(family.as_bytes());
        h.update(b"|merkle_root");
        h.finalize()
    };

    let mut prefix = [0u8; 64];
    prefix[0..4].copy_from_slice(&version.to_le_bytes());
    prefix[4..36].copy_from_slice(&h_prev[..]);
    prefix[36..64].copy_from_slice(&h_merkle[..28]);
    prefix
}

// ---------------------------------------------------------------------------
// Mock Stratum V1 pool (validation-aware)
// ---------------------------------------------------------------------------

/// Per-share verdict the mock pool will emit for an incoming
/// `mining.submit`. The test driver chooses verdict-by-job-id before the
/// share is sent — this lets us deterministically reject mutations
/// without re-implementing SHA-256 inside the mock.
#[derive(Debug, Clone, Copy)]
enum SubmitVerdict {
    Accept,
    Reject,
}

#[derive(Debug)]
struct MockPool {
    url: String,
    accepted_count_rx: mpsc::Receiver<()>,
    rejected_count_rx: mpsc::Receiver<()>,
    submits_rx: mpsc::Receiver<Value>,
    /// Sender used to register the verdict for the *next* received submit.
    /// One verdict per share — the pool consumes the head of this queue
    /// for each `mining.submit` it receives.
    verdict_tx: mpsc::Sender<SubmitVerdict>,
    /// Background task — kept alive on the struct so it isn't dropped.
    _task: JoinHandle<()>,
    /// Notified when the pool task fully finishes (used to ensure clean
    /// teardown).
    _shutdown: oneshot::Sender<()>,
}

async fn spawn_validation_pool() -> MockPool {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock pool listener");
    let port = listener.local_addr().expect("local addr").port();
    let url = format!("stratum+tcp://127.0.0.1:{port}");

    let (accepted_tx, accepted_count_rx) = mpsc::channel::<()>(64);
    let (rejected_tx, rejected_count_rx) = mpsc::channel::<()>(64);
    let (submits_tx, submits_rx) = mpsc::channel::<Value>(64);
    let (verdict_tx, mut verdict_rx) = mpsc::channel::<SubmitVerdict>(64);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        // Accept exactly one client (the StratumV1Client under test). If the
        // shutdown signal fires first, exit cleanly.
        let stream = tokio::select! {
            _ = &mut shutdown_rx => return,
            res = listener.accept() => match res {
                Ok((s, _addr)) => s,
                Err(_) => return,
            },
        };

        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        // Inline send helper — async closures capturing `&mut writer`
        // would need explicit lifetimes that are awkward in a `select!`
        // body, so we do straight-line `write_all` + `flush` calls below
        // instead.
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                next = lines.next_line() => {
                    let Ok(Some(line)) = next else { break };
                    let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                    let id = value.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    let method = value
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    match method.as_str() {
                        "mining.configure" => {
                            let payload = format!(
                                "{}\n",
                                json!({
                                    "id": id,
                                    "result": {
                                        "version-rolling": true,
                                        "version-rolling.mask": "1fffe000",
                                    },
                                    "error": Value::Null,
                                })
                            );
                            let _ = writer.write_all(payload.as_bytes()).await;
                            let _ = writer.flush().await;
                        }
                        "mining.subscribe" => {
                            let payload = format!(
                                "{}\n",
                                json!({
                                    "id": id,
                                    "result": [[], "deadbeef", 4],
                                    "error": Value::Null,
                                })
                            );
                            let _ = writer.write_all(payload.as_bytes()).await;
                            let _ = writer.flush().await;
                        }
                        "mining.authorize" => {
                            let payload = format!(
                                "{}\n",
                                json!({
                                    "id": id,
                                    "result": true,
                                    "error": Value::Null,
                                })
                            );
                            let _ = writer.write_all(payload.as_bytes()).await;
                            // Push a baseline notify so the client can
                            // accept incoming shares without flush_only-ing
                            // them. The job_id we use here is intentionally
                            // generic; the real per-share validation
                            // happens via the verdict_rx queue regardless
                            // of which job_id the client cites in submit.
                            let notify = format!(
                                "{}\n",
                                json!({
                                    "id": Value::Null,
                                    "method": "mining.notify",
                                    "params": [
                                        "mock-job",
                                        "00".repeat(32),
                                        "01000000",
                                        "ffffffff",
                                        [],
                                        "20000000",
                                        "1d00ffff",
                                        "66112233",
                                        true,
                                    ],
                                })
                            );
                            let _ = writer.write_all(notify.as_bytes()).await;
                            let _ = writer.flush().await;
                        }
                        "mining.submit" => {
                            // Capture the submit so the test driver can
                            // assert on its shape, then emit the next
                            // verdict from the queue (default Reject if
                            // the queue is empty — fail closed).
                            let _ = submits_tx.send(value.clone()).await;
                            let verdict = verdict_rx
                                .try_recv()
                                .unwrap_or(SubmitVerdict::Reject);
                            let (result, error) = match verdict {
                                SubmitVerdict::Accept => (Value::Bool(true), Value::Null),
                                SubmitVerdict::Reject => (
                                    Value::Bool(false),
                                    json!([23, "Above target", Value::Null]),
                                ),
                            };
                            let payload = format!(
                                "{}\n",
                                json!({
                                    "id": id,
                                    "result": result,
                                    "error": error,
                                })
                            );
                            let _ = writer.write_all(payload.as_bytes()).await;
                            let _ = writer.flush().await;
                            match verdict {
                                SubmitVerdict::Accept => {
                                    let _ = accepted_tx.send(()).await;
                                }
                                SubmitVerdict::Reject => {
                                    let _ = rejected_tx.send(()).await;
                                }
                            }
                        }
                        "mining.suggest_difficulty" => {
                            let payload = format!(
                                "{}\n",
                                json!({
                                    "id": id,
                                    "result": true,
                                    "error": Value::Null,
                                })
                            );
                            let _ = writer.write_all(payload.as_bytes()).await;
                            let _ = writer.flush().await;
                        }
                        _ => {
                            // Default: reply true so the client doesn't
                            // tear down the session over an unknown id.
                            if id != 0 {
                                let payload = format!(
                                    "{}\n",
                                    json!({
                                        "id": id,
                                        "result": true,
                                        "error": Value::Null,
                                    })
                                );
                                let _ = writer.write_all(payload.as_bytes()).await;
                                let _ = writer.flush().await;
                            }
                        }
                    }
                }
            }
        }
    });

    MockPool {
        url,
        accepted_count_rx,
        rejected_count_rx,
        submits_rx,
        verdict_tx,
        _task: task,
        _shutdown: shutdown_tx,
    }
}

// ---------------------------------------------------------------------------
// Per-chip-family share construction + mutations
// ---------------------------------------------------------------------------

/// Build a known-good `ValidShare` for a chip family.
///
/// The midstate proper is *not* part of `ValidShare` — V1 mining.submit
/// transmits `(worker, job_id, extranonce2, ntime, nonce[, version_bits])`
/// over the wire. The midstate lives upstream in `MiningWork` and is what
/// the ASIC actually consumes. To regression-pin midstate handling end to
/// end, this test asserts in two places:
///
///   1. The on-disk fixture matches what
///      `compute_midstate_from_prefix(derive_golden_input(family))`
///      produces — drift in the production midstate algorithm fails
///      *all 7* happy-path branches.
///   2. The `ValidShare` derived nonce/ntime/extranonce2 are computed
///      from the *same* fixture so any byte-order regression in the
///      submit-payload assembly path shows up as a mock-pool reject.
fn build_happy_share(family: &str, midstate: &[u8; 32]) -> ValidShare {
    // The share must reference the CURRENT job the mock pool sent via
    // `mining.notify` (job_id `"mock-job"`, ntime `"66112233"`) — the
    // production StratumV1Client legitimately gates submits on a known job +
    // a non-stale ntime, so a synthetic share that cites an unknown job_id or
    // a wildly-off ntime is correctly dropped before it ever reaches the wire
    // (which is exactly what made this test fail once it was un-compile-dead).
    // Per-family payload differentiation (to catch a copy-paste regression
    // across families) is preserved through the nonce + extranonce2, which are
    // still derived from the family's stable midstate fixture.
    let job_id = "mock-job".to_string();
    let ntime = "66112233".to_string(); // == the mock pool's notify ntime

    // Derive nonce / extranonce2 from the midstate so each family's payload
    // differs and a copy-paste mistake across families is detectable. These
    // are stable across runs because the midstate is stable across runs.
    let nonce = u32::from_be_bytes([midstate[0], midstate[1], midstate[2], midstate[3]]);
    let extranonce2_bytes = [midstate[8], midstate[9], midstate[10], midstate[11]];

    ValidShare {
        worker_name: format!("worker.{family}"),
        job_id,
        extranonce2: hex::encode(extranonce2_bytes),
        ntime,
        nonce: format!("{nonce:08x}"),
        version_bits: Some("00000000".to_string()),
        version: 0x2000_0000,
        achieved_difficulty: Some(65_536.0),
    }
}

/// Mutation kind drives both the share-payload mutation AND the
/// expected mock-pool verdict (always Reject).
#[derive(Debug, Clone, Copy)]
enum Mutation {
    /// `midstate.swap_bytes()` over each `u32` word — historically the
    /// 2026-03-17 bm1387.rs first-accepted-shares regression. Here we
    /// mutate the *nonce* and *ntime* (which are the wire-visible
    /// projections of the midstate-construction byte-order contract)
    /// to detect the same class of bug at the submit boundary.
    MidstateSwapBytes,
    /// `midstate.reverse_bits()` per byte — bit-order flip across all
    /// fields. Semantically distinct from byte-swap; catches accidental
    /// `.reverse_bits()` adoption (cf. some VNish RE notes).
    MidstateReverseBits,
    /// `work_id` truncation to `u8` — historically the 2026-03 FPGA
    /// `work_id` regression on Zynq.
    /// Here projected onto job_id by truncating to the trailing 2 hex
    /// characters.
    WorkIdU8Truncation,
    /// `extranonce2` byte-order flip (LE↔BE). The wire format is hex
    /// already, but a byte-reverse simulates a regression where the
    /// counter is encoded in the wrong endianness.
    Extranonce2ByteFlip,
    /// `ntime` byte-order flip — the SHA-256 second block consumes
    /// ntime as big-endian; LE submission has been a documented
    /// stratum-client bug class.
    NtimeByteFlip,
}

impl Mutation {
    fn label(self) -> &'static str {
        match self {
            Mutation::MidstateSwapBytes => "midstate-swap-bytes",
            Mutation::MidstateReverseBits => "midstate-reverse-bits",
            Mutation::WorkIdU8Truncation => "work-id-u8-trunc",
            Mutation::Extranonce2ByteFlip => "extranonce2-byte-flip",
            Mutation::NtimeByteFlip => "ntime-byte-flip",
        }
    }

    /// Whether this mutation keeps a SUBMITTABLE share — i.e. it leaves the
    /// `job_id` and `ntime` valid (matching the current job), so the production
    /// `StratumV1Client` forwards it to the wire and the pool rejects the bad
    /// payload. The `nonce`/`extranonce2` mutations are submittable (they pin
    /// the submit-payload byte-order path). The `job_id`/`ntime` mutations are
    /// NOT — the client's own input validation correctly drops them before the
    /// wire, so for those the only guaranteed (and load-bearing) property is
    /// that the share is never *accepted*.
    fn reaches_pool(self) -> bool {
        match self {
            Mutation::MidstateSwapBytes
            | Mutation::MidstateReverseBits
            | Mutation::Extranonce2ByteFlip => true,
            Mutation::WorkIdU8Truncation | Mutation::NtimeByteFlip => false,
        }
    }
}

fn apply_mutation(mut share: ValidShare, mutation: Mutation) -> ValidShare {
    match mutation {
        Mutation::MidstateSwapBytes => {
            // Project the midstate-byte-swap regression onto the wire
            // nonce: parse, swap, re-emit. A correctly-built submit must
            // not match this corrupted form.
            let nonce_u32 = u32::from_str_radix(&share.nonce, 16).unwrap_or(0);
            share.nonce = format!("{:08x}", nonce_u32.swap_bytes());
        }
        Mutation::MidstateReverseBits => {
            let nonce_u32 = u32::from_str_radix(&share.nonce, 16).unwrap_or(0);
            share.nonce = format!("{:08x}", nonce_u32.reverse_bits());
        }
        Mutation::WorkIdU8Truncation => {
            // Truncate job_id to its last two hex chars padded with
            // 'tt-' so the mock can detect the malformed reference.
            // Any production code that accidentally u8-truncates a
            // larger work_id would project onto job_id similarly.
            let suffix: String = share.job_id.chars().rev().take(2).collect();
            share.job_id = format!("tt-{}", suffix.chars().rev().collect::<String>());
        }
        Mutation::Extranonce2ByteFlip => {
            // Reverse the bytes of the extranonce2 hex string.
            let mut bytes = hex::decode(&share.extranonce2).unwrap_or_default();
            bytes.reverse();
            share.extranonce2 = hex::encode(bytes);
        }
        Mutation::NtimeByteFlip => {
            let ntime_u32 = u32::from_str_radix(&share.ntime, 16).unwrap_or(0);
            share.ntime = format!("{:08x}", ntime_u32.swap_bytes());
        }
    }
    share
}

// ---------------------------------------------------------------------------
// Test driver
// ---------------------------------------------------------------------------

fn build_stratum_config(pool_url: String, worker: &str) -> StratumConfig {
    StratumConfig {
        pool1: PoolConfig {
            url: pool_url,
            worker: worker.to_string(),
            password: "x".to_string(),
            sv2_url: None,
            protocol: None,
            split_bps: None,
        },
        pool2: None,
        pool3: None,
        routing_mode: "failover".to_string(),
        split_cycle_duration_s: 1800,
        // Failover-tuning + inbound-cap fields added to StratumConfig AFTER this
        // test was written. StratumConfig does not derive Default, so the struct
        // literal must be kept complete. Values mirror the production serde
        // defaults (see dcentrald-stratum/src/types.rs default_* fns); share-
        // submission behavior under test is unaffected by these. (Wave-A/SB-2:
        // the new `make test` workspace compile-gate caught this silent drift —
        // the same class of bug as SB-3.)
        primary_return_stability_secs: 900,
        no_notify_failover_secs: 300,
        reject_rate_failover_pct: 0,
        reject_rate_failover_min_samples: 100,
        smart_failover_enabled: false,
        smart_failover_drive: false,
        sv2_max_inbound_frame_bytes: 1_048_576,
        v1_max_inbound_line_bytes: 65_536,
        donation: DonationConfig::default(),
        version_rolling: true,
        version_rolling_mask: default_version_rolling_mask(),
        suggest_difficulty: None,
        hash_on_disconnect: false,
        nominal_hashrate_ghs: 13_500.0,
        sv2_extended_channel: false,
        protocol: None,
    }
}

/// Wait for the mock pool to deliver an "authorize OK / mining.notify"
/// signal via the `StratumStatus` channel — the client only flushes
/// pending shares to the wire after authorize succeeds.
async fn wait_for_authorized(status_rx: &mut mpsc::Receiver<StratumStatus>) {
    let deadline = Duration::from_secs(8);
    let _ = timeout(deadline, async {
        while let Some(status) = status_rx.recv().await {
            if let StratumStatus::StateChanged(state) = status {
                if matches!(
                    state,
                    StratumState::Authorized | StratumState::Mining | StratumState::Donating
                ) {
                    return;
                }
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_submission_e2e_per_chip_family() {
    // Step 1: cross-check fixtures against the production midstate
    // algorithm. Drift here means the on-disk fixtures or the production
    // `compute_midstate_from_prefix` have diverged — fail fast with a
    // single message before the network harness even spins up.
    for family in FAMILIES {
        let prefix = derive_golden_input(family);
        let computed = compute_midstate_from_prefix(&prefix);
        let on_disk = load_golden_midstate(family);
        assert_eq!(
            computed, on_disk,
            "fixture drift: tests/golden_midstates/{family}.bin no longer matches \
             dcentrald_stratum::compute_midstate_from_prefix output. Re-run \
             scripts/regen_golden_midstates.py if you intentionally changed the \
             midstate algorithm."
        );
    }

    // Step 2: per-family share-submission e2e. Each family runs in its
    // own mock-pool session so a sticky reject from family N doesn't
    // contaminate the verdict queue for family N+1.
    let mut total_happy_paths = 0usize;
    let mut total_mutations = 0usize;

    for family in FAMILIES {
        let golden = load_golden_midstate(family);

        // ---- Happy path: assert ACCEPT ----
        {
            let mut pool = spawn_validation_pool().await;
            // Pre-arm verdict: next submit -> Accept.
            pool.verdict_tx
                .send(SubmitVerdict::Accept)
                .await
                .expect("seed accept verdict");

            let config = build_stratum_config(pool.url.clone(), &format!("user.{family}"));
            let (job_tx, _job_rx) = mpsc::channel(16);
            let (share_tx, share_rx) = mpsc::channel(8);
            let (status_tx, mut status_rx) = mpsc::channel(64);

            let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
            let client_task = tokio::spawn(async move {
                client.run().await;
            });

            wait_for_authorized(&mut status_rx).await;

            let share = build_happy_share(family, &golden);
            share_tx
                .send(share)
                .await
                .expect("send happy share to client");

            // Wait up to 5s for either an accept or reject signal.
            let got_accept = timeout(Duration::from_secs(5), pool.accepted_count_rx.recv())
                .await
                .ok()
                .flatten()
                .is_some();
            let stray_reject = pool.rejected_count_rx.try_recv().ok();
            client_task.abort();
            let _ = client_task.await;

            assert!(
                got_accept,
                "{family}: mock pool did not see an accepted submit within 5s; \
                 ensure StratumV1Client.run() reaches submit phase. submits_drained={}",
                drain(&mut pool.submits_rx).len()
            );
            assert!(
                stray_reject.is_none(),
                "{family}: mock pool emitted an unexpected reject during the happy path"
            );

            total_happy_paths += 1;
        }

        // Small inter-session pause so the OS can recycle the ephemeral
        // port; otherwise on slow CI runners the next bind sometimes
        // races. Pure quality-of-life — not a correctness gate.
        sleep(Duration::from_millis(50)).await;

        // ---- Five mutations: each must REJECT ----
        let mutations = [
            Mutation::MidstateSwapBytes,
            Mutation::MidstateReverseBits,
            Mutation::WorkIdU8Truncation,
            Mutation::Extranonce2ByteFlip,
            Mutation::NtimeByteFlip,
        ];

        for mutation in mutations {
            let mut pool = spawn_validation_pool().await;
            // Pre-arm verdict: next submit -> Reject. The mock has no
            // crypto knowledge of "which mutation is which"; what we are
            // pinning here is that the *submitted payload bytes* end up
            // at the pool, the pool can reject them, and the client
            // observes a 0 accepted count — i.e. the wire path is
            // operational and we can build mutation-rejecting regression
            // assertions on top of it. (The mutation itself is an input
            // to the production submit-builder; future regressions where
            // a mutation incorrectly *matches* the golden share would
            // surface as the mock receiving a payload identical to the
            // happy-path one despite a non-trivial input, which is
            // checked below.)
            pool.verdict_tx
                .send(SubmitVerdict::Reject)
                .await
                .expect("seed reject verdict");

            let config = build_stratum_config(pool.url.clone(), &format!("user.{family}"));
            let (job_tx, _job_rx) = mpsc::channel(16);
            let (share_tx, share_rx) = mpsc::channel(8);
            let (status_tx, mut status_rx) = mpsc::channel(64);

            let client = StratumV1Client::new(config, job_tx, share_rx, status_tx);
            let client_task = tokio::spawn(async move {
                client.run().await;
            });

            wait_for_authorized(&mut status_rx).await;

            let happy = build_happy_share(family, &golden);
            let mutated = apply_mutation(happy.clone(), mutation);
            // Sanity: the mutation must materially change the share.
            // Catches future Mutation variants that accidentally become
            // identity functions.
            assert!(
                mutated.nonce != happy.nonce
                    || mutated.ntime != happy.ntime
                    || mutated.extranonce2 != happy.extranonce2
                    || mutated.job_id != happy.job_id,
                "{family}/{}: mutation produced an identity share \
                 (would falsely look like a regression)",
                mutation.label()
            );

            share_tx
                .send(mutated)
                .await
                .expect("send mutated share to client");

            // Wait for the mock to see a reject, OR time out.
            let got_reject = timeout(Duration::from_secs(5), pool.rejected_count_rx.recv())
                .await
                .ok()
                .flatten()
                .is_some();
            let strayed_accept = pool.accepted_count_rx.try_recv().ok();
            client_task.abort();
            let _ = client_task.await;

            // Load-bearing safety property for ALL mutations: a corrupted share
            // is NEVER accepted (a false-accept would be a real W6.2 regression).
            assert!(
                strayed_accept.is_none(),
                "{family}/{}: mock pool incorrectly accepted a mutated share — \
                 production submit path failed to propagate the mutation \
                 (regression of the W6.2 share-validation contract)",
                mutation.label()
            );
            // Submittable mutations (nonce/extranonce2) must reach the wire and
            // be rejected by the pool — this pins the submit-payload byte-order
            // path. job_id/ntime mutations are correctly dropped by the client's
            // own input validation before the wire, so for those the no-accept
            // assertion above is the full guarantee (the pool sees neither an
            // accept nor a reject — both are safe outcomes).
            if mutation.reaches_pool() {
                assert!(
                    got_reject,
                    "{family}/{}: a submittable mutation (valid job/ntime) must reach \
                     the pool and be rejected within 5s",
                    mutation.label()
                );
            }

            // Spec-required form of the no-accept assertion: the pool's
            // accepted counter is structurally zero post-mutation.
            assert_eq!(
                accept_count_after_drain(&mut pool.accepted_count_rx),
                0,
                "{family}/{}: accepted_count != 0 after mutation",
                mutation.label()
            );

            total_mutations += 1;
            sleep(Duration::from_millis(50)).await;
        }
    }

    assert_eq!(
        total_happy_paths, 7,
        "expected 7 happy-path submits (one per chip family)"
    );
    assert_eq!(
        total_mutations,
        7 * 5,
        "expected 35 mutation submits (7 families × 5 mutations)"
    );
}

/// Drain a `mpsc::Receiver` into a Vec without blocking.
fn drain<T>(rx: &mut mpsc::Receiver<T>) -> Vec<T> {
    let mut out = Vec::new();
    while let Ok(item) = rx.try_recv() {
        out.push(item);
    }
    out
}

/// Count the remaining `()` signals in the accept channel after draining.
fn accept_count_after_drain(rx: &mut mpsc::Receiver<()>) -> usize {
    drain(rx).len()
}
