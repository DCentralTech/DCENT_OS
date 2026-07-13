#![cfg(feature = "mock-pool")]

use std::time::Duration;

use dcentrald_stratum::types::{
    default_version_rolling_mask, DonationConfig, PoolConfig, StratumConfig, ValidShare,
};
use dcentrald_stratum::v1::mock_pool::{MockV1Pool, MockV1PoolHandle};
use dcentrald_stratum::StratumV1Client;
use tokio::sync::mpsc;

fn test_config(pool_url: String) -> StratumConfig {
    let mut donation = DonationConfig::default();
    donation.enabled = false;
    StratumConfig {
        pool1: PoolConfig {
            url: pool_url,
            worker: "dcent.sim.worker".to_string(),
            password: "x".to_string(),
            sv2_url: None,
            protocol: Some("v1".to_string()),
            split_bps: None,
        },
        pool2: None,
        pool3: None,
        routing_mode: "failover".to_string(),
        split_cycle_duration_s: 1800,
        primary_return_stability_secs: 0,
        no_notify_failover_secs: 0,
        reject_rate_failover_pct: 0,
        reject_rate_failover_min_samples: 100,
        smart_failover_enabled: false,
        smart_failover_drive: false,
        sv2_max_inbound_frame_bytes: 1_048_576,
        v1_max_inbound_line_bytes: 65_536,
        donation,
        version_rolling: true,
        version_rolling_mask: default_version_rolling_mask(),
        suggest_difficulty: Some(1),
        hash_on_disconnect: true,
        nominal_hashrate_ghs: 110_000.0,
        sv2_extended_channel: false,
        protocol: Some("v1".to_string()),
    }
}

#[tokio::test]
async fn v1_client_serializes_submits_and_records_pool_acceptance() {
    let (addr, pool) = MockV1Pool::spawn().await.expect("spawn V1 loopback pool");
    let (job_tx, mut job_rx) = mpsc::channel(8);
    let (share_tx, share_rx) = mpsc::channel(8);
    let (status_tx, _status_rx) = mpsc::channel(32);
    let client = StratumV1Client::new(
        test_config(MockV1PoolHandle::url(addr)),
        job_tx,
        share_rx,
        status_tx,
    );

    let client_task = tokio::spawn(client.run_until_sv2_retry(Duration::from_millis(900)));
    let job = tokio::time::timeout(Duration::from_secs(2), job_rx.recv())
        .await
        .expect("job timeout")
        .expect("mock pool job");

    share_tx
        .send(ValidShare {
            worker_name: "dcent.sim.worker".to_string(),
            job_id: job.job_id,
            extranonce2: "00000000".to_string(),
            ntime: format!("{:08x}", job.ntime),
            nonce: "00000001".to_string(),
            version_bits: None,
            version: job.version,
            achieved_difficulty: Some(1.0),
        })
        .await
        .expect("submit simulated share to V1 client");

    tokio::time::timeout(Duration::from_secs(2), async {
        while pool.accepted_shares() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pool must accept submitted share");

    let returned = tokio::time::timeout(Duration::from_secs(3), client_task)
        .await
        .expect("client stop timeout")
        .expect("client task");
    assert_eq!(pool.accepted_shares(), 1);
    assert!(pool.requests().iter().any(|request| {
        request.contains("\"method\":\"mining.submit\"") && request.contains("dcent.sim.worker")
    }));
    let stats = returned.stats();
    let stats = stats.lock().await;
    assert_eq!(stats.shares_submitted, 1);
    assert_eq!(stats.shares_accepted, 1);
    assert_eq!(stats.shares_rejected, 0);
}
