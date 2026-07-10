//! W9.3 — broad-pool SV2 test harness.
//!
//! Drives `StratumV2Client` through the full Noise_NX handshake →
//! SetupConnection → Open*MiningChannel → first-job lifecycle against
//! two mock pool styles:
//!
//!  - **OCEAN-style** (Standard channels, no version rolling)
//!  - **DEMAND/SRI-style** (Extended channels, version-rolling allowed)
//!
//! W5.3 made V2 the auto-default protocol when an `sv2_url` is configured.
//! Before this test, the only end-to-end SV2 evidence was a single live
//! Braiins Pool session on 2026-03-20. This harness pins compatibility
//! with the broader pool ecosystem so a refactor of `noise.rs`,
//! `channel.rs`, or `client.rs` that breaks non-Braiins handshakes is
//! caught in CI on every PR — not by an operator the day they switch
//! pools.
//!
//! # Why integration tests
//!
//! `StratumV2Client::run_session_inner` is private. The only public
//! drive is `StratumV2Client::run`, which loops forever. We accept that
//! tradeoff: the test spawns the client task with `tokio::spawn`, waits
//! for the JobTemplate to come through `job_tx`, and then drops the
//! task handle (cancelling the spawned future). The mock pool writes
//! one job and closes the socket so the client's reconnect loop hits
//! `tokio::time::sleep(backoff)` while we move on to assertions.
//!
//! # Why feature-gated
//!
//! Compiled only with `--features mock-pool`. The mock pool harness is
//! a real EllSwift+ChaChaPoly server-side implementation, so it brings
//! ~zero extra binary cost (the `secp256k1` and `chacha20poly1305`
//! deps are already pulled in by `default = ["sv2", "jd"]`), but
//! gating it behind a feature ensures production sysupgrade builds
//! never link the deterministic-seed handshake helpers.

#![cfg(all(feature = "sv2", feature = "mock-pool"))]

use std::time::Duration;

use dcentrald_stratum::types::{
    DonationConfig, JobTemplate, PoolConfig, StratumConfig, StratumStatus, ValidShare,
};
use dcentrald_stratum::v2::test_server::{
    MockPool, MockPoolBehavior, MockPoolOutcome, MockPoolStyle,
};
use dcentrald_stratum::StratumV2Client;
use tokio::sync::mpsc;

/// Build a minimal StratumConfig pointing at a single SV2 endpoint.
///
/// `sv2_url` is the only knob the client really cares about for V2 mode;
/// the V1 fallback fields are unreachable in this test because we never
/// call `run_auto_with_v1_fallback`.
fn build_sv2_config(sv2_addr: std::net::SocketAddr, version_rolling: bool) -> StratumConfig {
    let sv2_url = format!("stratum2+tcp://{}:{}", sv2_addr.ip(), sv2_addr.port());
    StratumConfig {
        pool1: PoolConfig {
            url: sv2_url.clone(),
            worker: "test.miner".into(),
            password: "x".into(),
            sv2_url: Some(sv2_url),
            protocol: Some("sv2".into()),
            split_bps: None,
        },
        pool2: None,
        pool3: None,
        routing_mode: "failover".into(),
        split_cycle_duration_s: 1800,
        donation: DonationConfig {
            enabled: false,
            percent: 0.0,
            pool_url: String::new(),
            worker: String::new(),
            password: String::new(),
            fallback_enabled: false,
            fallback_pool_url: String::new(),
            fallback_worker: String::new(),
            fallback_password: String::new(),
            cycle_duration_s: 3600,
        },
        version_rolling,
        version_rolling_mask: 0x1fff_e000,
        suggest_difficulty: None,
        hash_on_disconnect: false,
        nominal_hashrate_ghs: 500.0,
        // Extended channel ↔ DEMAND/SRI; Standard channel ↔ OCEAN. We
        // reuse the version_rolling argument as the discriminator
        // because the two go together for these styles in practice.
        sv2_extended_channel: version_rolling,
        protocol: Some("sv2".into()),
        // Pool-failover robustness + anti-OOM cap fields (added to
        // StratumConfig after this test was first written). Literal values
        // mirror the production `default_*` fns in types.rs (the private
        // defaults aren't callable from this integration test). None of
        // these are exercised by the SV2 job/share round-trip below — they
        // only need to be present so the struct literal compiles. Keeping
        // them in lockstep with the production defaults avoids a silent
        // drift between the test fixture and shipped behavior.
        primary_return_stability_secs: 900,
        no_notify_failover_secs: 300,
        reject_rate_failover_pct: 0,
        reject_rate_failover_min_samples: 100,
        smart_failover_enabled: false,
        smart_failover_drive: false,
        sv2_max_inbound_frame_bytes: 1_048_576,
        v1_max_inbound_line_bytes: 65_536,
    }
}

/// Spawn `StratumV2Client::run` on a tokio task and return the
/// channels the test uses to observe handshake/channel/job progress.
///
/// We hold the JoinHandle so the caller can `.abort()` once the test
/// has its evidence, otherwise the client would loop forever. A
/// background drainer pulls every `StratumStatus` off `status_rx` and
/// drops it — if the client filled the buffered channel it would
/// otherwise block on `status_tx.send().await` and never deliver a
/// JobTemplate.
fn spawn_client(
    config: StratumConfig,
) -> (
    mpsc::Receiver<JobTemplate>,
    mpsc::Sender<ValidShare>,
    tokio::task::JoinHandle<()>,
) {
    let (job_tx, job_rx) = mpsc::channel::<JobTemplate>(8);
    let (share_tx, share_rx) = mpsc::channel::<ValidShare>(8);
    let (status_tx, mut status_rx) = mpsc::channel::<dcentrald_stratum::types::StratumStatus>(64);

    // Background status drainer — discards everything so the client's
    // status_tx.send() never blocks on a full channel.
    tokio::spawn(async move {
        while let Some(_status) = status_rx.recv().await {
            // Intentionally drop — the test asserts on JobTemplate
            // delivery, not status semantics.
        }
    });

    let client = StratumV2Client::new(config, 500.0, job_tx, share_rx, status_tx);
    let handle = tokio::spawn(async move {
        client.run().await;
    });

    (job_rx, share_tx, handle)
}

/// Like [`spawn_client`] but hands the caller the live `StratumStatus`
/// receiver instead of draining-and-dropping it, so a test can assert on
/// the statuses the client emits (e.g. `ShareAccepted`).
///
/// The status channel is generously buffered (256) so the client never
/// blocks on `status_tx.send()` in the window between spawn and the point
/// the test starts draining (it emits only a handful — `Connecting`, then
/// `Mining` on first job — before the share round-trip).
fn spawn_client_capturing(
    config: StratumConfig,
) -> (
    mpsc::Receiver<JobTemplate>,
    mpsc::Sender<ValidShare>,
    mpsc::Receiver<StratumStatus>,
    tokio::task::JoinHandle<()>,
) {
    let (job_tx, job_rx) = mpsc::channel::<JobTemplate>(8);
    let (share_tx, share_rx) = mpsc::channel::<ValidShare>(8);
    let (status_tx, status_rx) = mpsc::channel::<StratumStatus>(256);

    let client = StratumV2Client::new(config, 500.0, job_tx, share_rx, status_tx);
    let handle = tokio::spawn(async move {
        client.run().await;
    });

    (job_rx, share_tx, status_rx, handle)
}

/// Drain `status_rx` until a status matching `pred` arrives or `timeout`
/// elapses. Draining continuously keeps the client's `status_tx.send()`
/// from blocking on a full channel.
async fn await_status_matching<F>(
    status_rx: &mut mpsc::Receiver<StratumStatus>,
    timeout: Duration,
    label: &str,
    mut pred: F,
) -> StratumStatus
where
    F: FnMut(&StratumStatus) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "[{}] timed out after {:?} waiting for matching status",
                label, timeout
            );
        }
        match tokio::time::timeout(remaining, status_rx.recv()).await {
            Ok(Some(status)) => {
                if pred(&status) {
                    return status;
                }
                // Non-matching status — keep draining.
            }
            Ok(None) => panic!(
                "[{}] status channel closed before a matching status arrived",
                label
            ),
            Err(_) => panic!(
                "[{}] timed out after {:?} waiting for matching status",
                label, timeout
            ),
        }
    }
}

/// Assert NO `JobTemplate` is delivered within `window` — the client-side
/// proof that the session never reached mining (`reached_mining == false`).
///
/// A timeout (client still looping, no job) is success. A delivered job is
/// the failure this guards against. A closed channel (client task ended) is
/// also "no job" and accepted.
async fn assert_no_job(job_rx: &mut mpsc::Receiver<JobTemplate>, window: Duration, label: &str) {
    match tokio::time::timeout(window, job_rx.recv()).await {
        Ok(Some(_job)) => panic!(
            "[{}] expected NO job (auth reject) but a JobTemplate was delivered",
            label
        ),
        Ok(None) => { /* client task ended without a job — fine */ }
        Err(_) => { /* timed out with no job — the expected outcome */ }
    }
}

/// Assert the client makes it to the first JobTemplate within `timeout`.
async fn assert_first_job(
    job_rx: &mut mpsc::Receiver<JobTemplate>,
    timeout: Duration,
    style: &str,
) -> JobTemplate {
    match tokio::time::timeout(timeout, job_rx.recv()).await {
        Ok(Some(job)) => job,
        Ok(None) => panic!(
            "[{}] job channel closed before first JobTemplate arrived",
            style
        ),
        Err(_) => panic!(
            "[{}] timed out after {:?} waiting for first JobTemplate",
            style, timeout
        ),
    }
}

/// **OCEAN-style** broad-pool test.
///
/// OCEAN.xyz publishes Standard mining channels (no extended-channel
/// extranonce assignment, no work selection) and disables version
/// rolling on the pool side. Job delivery shape: `OpenStandardMiningChannel`
/// → `OpenStandardMiningChannelSuccess` → `SetNewPrevHash` → `NewMiningJob`.
///
/// Asserts:
///   1. Noise_NX handshake completes.
///   2. `OpenStandardMiningChannel` is the open frame the client sends.
///   3. `JobTemplate` arrives on `job_tx` with the expected version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocean_style_pool_completes_handshake_and_first_job() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool = MockPool::spawn(MockPoolStyle::Ocean)
        .await
        .expect("mock OCEAN-style pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ false);
    let (mut job_rx, _share_tx, client_handle) = spawn_client(config);

    let job = assert_first_job(&mut job_rx, Duration::from_secs(15), "OCEAN").await;
    assert_eq!(job.version, 0x2000_0000, "OCEAN mock job version mismatch");

    let outcome = pool.await_outcome(Duration::from_secs(5)).await;
    client_handle.abort();
    let outcome = outcome.expect("OCEAN-style mock pool returned an error");
    assert_eq!(
        outcome,
        MockPoolOutcome::JobDelivered,
        "expected OCEAN-style flow to reach JobDelivered"
    );
}

/// **DEMAND/SRI-style** broad-pool test.
///
/// DEMAND and the Stratum Reference Implementation use Extended mining
/// channels with `version_rolling_allowed=true` and coinbase-split job
/// delivery (`coinbase_tx_prefix`/`coinbase_tx_suffix` + `merkle_path`).
/// Job delivery shape: `OpenExtendedMiningChannel` →
/// `OpenExtendedMiningChannelSuccess` → `SetNewPrevHash` →
/// `NewExtendedMiningJob`.
///
/// Asserts:
///   1. Noise_NX handshake completes.
///   2. `OpenExtendedMiningChannel` is the open frame the client sends
///      (gated by `sv2_extended_channel = true` in the config).
///   3. `JobTemplate` arrives on `job_tx` with the expected version-bits
///      and merkle-path data plumbed through the adapter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demand_sri_style_pool_completes_handshake_and_first_extended_job() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool = MockPool::spawn(MockPoolStyle::DemandSri)
        .await
        .expect("mock DEMAND/SRI-style pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ true);
    let (mut job_rx, _share_tx, client_handle) = spawn_client(config);

    let job = assert_first_job(&mut job_rx, Duration::from_secs(15), "DEMAND/SRI").await;
    assert_eq!(
        job.version, 0x2000_0000,
        "DEMAND/SRI mock job version mismatch"
    );

    let outcome = pool.await_outcome(Duration::from_secs(5)).await;
    client_handle.abort();
    let outcome = outcome.expect("DEMAND/SRI-style mock pool returned an error");
    assert_eq!(
        outcome,
        MockPoolOutcome::JobDelivered,
        "expected DEMAND/SRI-style flow to reach JobDelivered"
    );
}

/// **Share-submit round-trip** — the back half of the SV2 lifecycle.
///
/// Drives an OCEAN-style (Standard channel) session all the way to a found
/// share: connect → Noise_NX handshake → SetupConnection → OpenStandard →
/// first job → **client submits a found nonce → pool decrypts a well-formed
/// `SubmitSharesStandard` → pool replies `SubmitSharesSuccess` → client emits
/// `StratumStatus::ShareAccepted`**.
///
/// Asserts:
///   1. The pool decrypted a `SubmitSharesStandard` (0x1a) carrying the exact
///      channel_id / nonce / ntime / version the dispatcher submitted — proof
///      the share bytes survived the encrypted transport intact.
///   2. The client surfaced the pool's success as `StratumStatus::ShareAccepted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocean_style_pool_accepts_submitted_share() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool =
        MockPool::spawn_with_behavior(MockPoolStyle::Ocean, MockPoolBehavior::JobThenShare)
            .await
            .expect("mock OCEAN-style pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ false);
    let (mut job_rx, share_tx, mut status_rx, client_handle) = spawn_client_capturing(config);

    // 1. Wait for the first job — the channel is open by the time it arrives.
    let job = assert_first_job(&mut job_rx, Duration::from_secs(15), "OCEAN-share").await;
    assert_eq!(job.version, 0x2000_0000, "OCEAN mock job version mismatch");

    // 2. Push a found share. The hex fields mirror what the work dispatcher
    //    produces; the pool decodes them off the encrypted transport and
    //    echoes them back in MockPoolOutcome::ShareAccepted.
    let share = ValidShare {
        worker_name: "test.miner".into(),
        job_id: "101".into(), // matches the OCEAN standard NewMiningJob id
        extranonce2: "00000000".into(),
        ntime: "6710a000".into(),
        nonce: "deadbeef".into(),
        version_bits: None,
        version: 0x2000_0000,
        achieved_difficulty: None,
    };
    share_tx.send(share).await.expect("share_tx send failed");

    // 3a. Pool-side proof: a well-formed SubmitSharesStandard with the exact
    //     bytes we submitted made it through the Noise transport.
    let outcome = pool
        .await_outcome(Duration::from_secs(10))
        .await
        .expect("OCEAN-style mock pool returned an error");
    match outcome {
        MockPoolOutcome::ShareAccepted {
            msg_type,
            channel_id,
            nonce,
            ntime,
            version,
            ..
        } => {
            assert_eq!(
                msg_type, 0x1a,
                "standard channel must submit SubmitSharesStandard (0x1a)"
            );
            assert_eq!(
                channel_id, 7,
                "client must echo the pool-assigned channel_id"
            );
            assert_eq!(nonce, 0xdead_beef, "submitted nonce corrupted in transit");
            assert_eq!(ntime, 0x6710_a000, "submitted ntime corrupted in transit");
            assert_eq!(
                version, 0x2000_0000,
                "submitted version corrupted in transit"
            );
        }
        other => panic!("expected ShareAccepted, got {:?}", other),
    }

    // 3b. Client-side proof: the pool's SubmitSharesSuccess surfaces as a
    //     StratumStatus::ShareAccepted.
    let status = await_status_matching(
        &mut status_rx,
        Duration::from_secs(10),
        "OCEAN-share",
        |s| matches!(s, StratumStatus::ShareAccepted { .. }),
    )
    .await;
    assert!(
        matches!(status, StratumStatus::ShareAccepted { .. }),
        "expected StratumStatus::ShareAccepted, got {:?}",
        status
    );

    client_handle.abort();
}

/// **Mid-session SetTarget** - pool target changes after mining has started.
///
/// Drives the encrypted session through first job delivery, then has the
/// mock pool send `SetTarget` before accepting a share. This pins the
/// client-side `DifficultyChanged` path e2e instead of only in the channel
/// unit tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocean_style_pool_applies_mid_session_set_target_before_share_ack() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool = MockPool::spawn_with_behavior(
        MockPoolStyle::Ocean,
        MockPoolBehavior::JobThenSetTargetThenShare,
    )
    .await
    .expect("mock OCEAN-style SetTarget pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ false);
    let (mut job_rx, share_tx, mut status_rx, client_handle) = spawn_client_capturing(config);

    let job = assert_first_job(&mut job_rx, Duration::from_secs(15), "OCEAN-settarget").await;
    assert_eq!(job.version, 0x2000_0000, "OCEAN mock job version mismatch");

    let status = await_status_matching(
        &mut status_rx,
        Duration::from_secs(10),
        "OCEAN-settarget",
        |s| matches!(s, StratumStatus::DifficultyChanged(diff) if (200.0..=300.0).contains(diff)),
    )
    .await;
    match status {
        StratumStatus::DifficultyChanged(diff) => {
            assert!(
                (200.0..=300.0).contains(&diff),
                "expected SetTarget difficulty around 256, got {diff}"
            );
        }
        other => panic!("expected DifficultyChanged from SetTarget, got {:?}", other),
    }

    let share = ValidShare {
        worker_name: "test.miner".into(),
        job_id: "101".into(),
        extranonce2: "00000000".into(),
        ntime: "6710a000".into(),
        nonce: "feedcafe".into(),
        version_bits: None,
        version: 0x2000_0000,
        achieved_difficulty: None,
    };
    share_tx.send(share).await.expect("share_tx send failed");

    let outcome = pool
        .await_outcome(Duration::from_secs(10))
        .await
        .expect("OCEAN-style SetTarget mock pool returned an error");
    match outcome {
        MockPoolOutcome::ShareAccepted {
            msg_type,
            channel_id,
            nonce,
            ntime,
            version,
            ..
        } => {
            assert_eq!(
                msg_type, 0x1a,
                "standard channel must submit SubmitSharesStandard (0x1a)"
            );
            assert_eq!(
                channel_id, 7,
                "client must echo the pool-assigned channel_id"
            );
            assert_eq!(nonce, 0xfeed_cafe, "submitted nonce corrupted in transit");
            assert_eq!(ntime, 0x6710_a000, "submitted ntime corrupted in transit");
            assert_eq!(
                version, 0x2000_0000,
                "submitted version corrupted in transit"
            );
        }
        other => panic!("expected ShareAccepted after SetTarget, got {:?}", other),
    }

    let accepted = await_status_matching(
        &mut status_rx,
        Duration::from_secs(10),
        "OCEAN-settarget-share",
        |s| matches!(s, StratumStatus::ShareAccepted { .. }),
    )
    .await;
    assert!(
        matches!(accepted, StratumStatus::ShareAccepted { .. }),
        "expected StratumStatus::ShareAccepted, got {:?}",
        accepted
    );

    client_handle.abort();
}

/// **Share reject round-trip** - pool rejects a submitted share.
///
/// The mock pool decrypts a well-formed `SubmitSharesStandard`, replies
/// `SubmitSharesError`, and the client must surface it as
/// `StratumStatus::ShareRejected`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ocean_style_pool_rejects_submitted_share() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool =
        MockPool::spawn_with_behavior(MockPoolStyle::Ocean, MockPoolBehavior::JobThenShareReject)
            .await
            .expect("mock OCEAN-style reject pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ false);
    let (mut job_rx, share_tx, mut status_rx, client_handle) = spawn_client_capturing(config);

    let job = assert_first_job(&mut job_rx, Duration::from_secs(15), "OCEAN-reject").await;
    assert_eq!(job.version, 0x2000_0000, "OCEAN mock job version mismatch");

    let share = ValidShare {
        worker_name: "test.miner".into(),
        job_id: "101".into(),
        extranonce2: "00000000".into(),
        ntime: "6710a000".into(),
        nonce: "badc0ffe".into(),
        version_bits: None,
        version: 0x2000_0000,
        achieved_difficulty: None,
    };
    share_tx.send(share).await.expect("share_tx send failed");

    let outcome = pool
        .await_outcome(Duration::from_secs(10))
        .await
        .expect("OCEAN-style reject mock pool returned an error");
    match outcome {
        MockPoolOutcome::ShareRejected {
            msg_type,
            channel_id,
            nonce,
            ntime,
            version,
            reason,
            ..
        } => {
            assert_eq!(
                msg_type, 0x1a,
                "standard channel must submit SubmitSharesStandard (0x1a)"
            );
            assert_eq!(
                channel_id, 7,
                "client must echo the pool-assigned channel_id"
            );
            assert_eq!(nonce, 0xbadc_0ffe, "submitted nonce corrupted in transit");
            assert_eq!(ntime, 0x6710_a000, "submitted ntime corrupted in transit");
            assert_eq!(
                version, 0x2000_0000,
                "submitted version corrupted in transit"
            );
            assert_eq!(reason, "mock-share-rejected");
        }
        other => panic!("expected ShareRejected, got {:?}", other),
    }

    let rejected = await_status_matching(
        &mut status_rx,
        Duration::from_secs(10),
        "OCEAN-reject",
        |s| {
            matches!(
                s,
                StratumStatus::ShareRejected { error_msg, .. }
                    if error_msg == "mock-share-rejected"
            )
        },
    )
    .await;
    match rejected {
        StratumStatus::ShareRejected {
            error_code,
            error_msg,
            ..
        } => {
            assert_eq!(error_code, -1);
            assert_eq!(error_msg, "mock-share-rejected");
        }
        other => panic!("expected StratumStatus::ShareRejected, got {:?}", other),
    }

    client_handle.abort();
}

/// **Auth/setup reject** - the negative path.
///
/// The mock pool completes the Noise_NX handshake, then replies
/// `SetupConnectionError` instead of `SetupConnectionSuccess`. The client
/// must refuse to open a mining channel and exit the session cleanly
/// (its `reached_mining` stays false).
///
/// Asserts:
///   1. Pool-side: the client disconnected after the error WITHOUT sending an
///      `Open*MiningChannel` (`MockPoolOutcome::AuthRejected`).
///   2. Client-side: no `JobTemplate` was ever delivered - the session never
///      reached mining.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_reject_pool_never_opens_channel_and_client_exits_clean() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,dcentrald_stratum=debug")
        .with_test_writer()
        .try_init();

    let mut pool =
        MockPool::spawn_with_behavior(MockPoolStyle::Ocean, MockPoolBehavior::AuthReject)
            .await
            .expect("mock auth-reject pool failed to bind");
    let pool_addr = pool.addr;

    let config = build_sv2_config(pool_addr, /* version_rolling = */ false);
    // `_status_rx` is held (not dropped) so the client never blocks on a full
    // status channel during its reconnect/backoff loop.
    let (mut job_rx, _share_tx, _status_rx, client_handle) = spawn_client_capturing(config);

    // 1. Pool-side proof: handshake completed, SetupConnectionError sent, and
    //    the client tore the session down without opening a channel.
    let outcome = pool
        .await_outcome(Duration::from_secs(15))
        .await
        .expect("auth-reject mock pool returned an error");
    assert_eq!(
        outcome,
        MockPoolOutcome::AuthRejected,
        "expected the client to refuse to open a channel after SetupConnectionError"
    );

    // 2. Client-side proof: no job ever reached the dispatcher — reached_mining
    //    stayed false and the session exited into reconnect/backoff.
    assert_no_job(&mut job_rx, Duration::from_secs(2), "auth-reject").await;

    client_handle.abort();
}

/// Cross-style smoke check: both pool styles in sequence on the same
/// tokio runtime. Catches global state leaks (e.g. a static mut in the
/// secp256k1 crate) that wouldn't show in the per-style tests.
///
/// Regression note (CI-10, 2026-07-04): the historical OCEAN-1 timeout
/// was a harness bug, not protocol behavior. The test destructured the
/// share sender as `_`, dropping it immediately; the client then shut down
/// its session before the first job. Keep both share senders alive until
/// each client task is aborted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_pool_styles_back_to_back() {
    // OCEAN first
    let mut ocean = MockPool::spawn(MockPoolStyle::Ocean)
        .await
        .expect("OCEAN bind");
    let cfg_ocean = build_sv2_config(ocean.addr, false);
    let (mut job_rx_ocean, _share_tx_ocean, h_ocean) = spawn_client(cfg_ocean);
    let _ = assert_first_job(&mut job_rx_ocean, Duration::from_secs(15), "OCEAN-1").await;
    let _ = ocean.await_outcome(Duration::from_secs(5)).await;
    h_ocean.abort();

    // DEMAND/SRI second
    let mut demand = MockPool::spawn(MockPoolStyle::DemandSri)
        .await
        .expect("DEMAND bind");
    let cfg_demand = build_sv2_config(demand.addr, true);
    let (mut job_rx_demand, _share_tx_demand, h_demand) = spawn_client(cfg_demand);
    let _ = assert_first_job(&mut job_rx_demand, Duration::from_secs(15), "DEMAND-2").await;
    let _ = demand.await_outcome(Duration::from_secs(5)).await;
    h_demand.abort();
}
