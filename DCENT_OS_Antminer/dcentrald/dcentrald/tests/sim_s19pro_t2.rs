#![cfg(feature = "sim-hal")]

use std::time::Duration;

use dcentrald_asic::chain::Chain as HashChain;
use dcentrald_asic::drivers::ChipRegistry;
use dcentrald_hal::chain_backend::Bm1397PlusChainBackend;
use dcentrald_hal::fpga_chain::FpgaChain;
use dcentrald_hal::platform::sim::{SimModel, SimNoncePolicy, SimPlatform};
use dcentrald_stratum::types::{
    default_version_rolling_mask, DonationConfig, PoolConfig, StratumConfig, ValidShare,
};
use dcentrald_stratum::v1::mock_pool::{MockV1Pool, MockV1PoolHandle};
use dcentrald_stratum::{validate_full_header, StratumV1Client, WorkBuilder};
use tokio::sync::mpsc;

fn stratum_config(pool_url: String, worker: &str) -> StratumConfig {
    let mut donation = DonationConfig::default();
    donation.enabled = false;
    StratumConfig {
        pool1: PoolConfig {
            url: pool_url,
            worker: worker.to_string(),
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

fn header_for(work: &dcentrald_stratum::MiningWork, nonce: u32) -> [u8; 80] {
    let mut header = [0_u8; 80];
    header[0..4].copy_from_slice(&work.version.to_le_bytes());
    header[4..36].copy_from_slice(&work.prev_block_hash);
    header[36..68].copy_from_slice(&work.merkle_root);
    header[68..72].copy_from_slice(&work.ntime.to_le_bytes());
    header[72..76].copy_from_slice(&work.nbits.to_le_bytes());
    header[76..80].copy_from_slice(&nonce.to_le_bytes());
    header
}

/// T2 means one model has crossed all four device-free boundaries in one
/// proof: selected geometry, enumeration, the production ASIC init path, and
/// a target-valid nonce serialized to a loopback pool and accepted.
async fn assert_headless_t2(
    model: SimModel,
    chip_id: u16,
    chip_count: u8,
    frequency_mhz: u16,
    worker: &str,
) {
    let platform = SimPlatform::new(model);
    assert_eq!(platform.profile().chips_per_chain, Some(chip_count));

    let enumeration = platform
        .open_bm1397plus_backend(0)
        .expect("modern simulated chain backend");
    enumeration
        .send_get_address_bm1397plus()
        .expect("enumeration request");
    let responses = enumeration
        .read_all_responses(0)
        .expect("enumeration responses");
    assert_eq!(responses.len(), usize::from(chip_count));
    assert!(responses
        .iter()
        .all(|response| response.starts_with(&chip_id.to_be_bytes())));

    let registry = ChipRegistry::production();
    let driver = registry.detect(chip_id).expect("production ASIC driver");
    let mut chain = FpgaChain::open_sim_for_model(0, model).expect("simulated FPGA chain");
    driver
        .init_chain(&mut chain, chip_count, frequency_mhz)
        .expect("production ASIC initialization");

    assert_share_accept(chain, worker).await;
}

async fn assert_share_accept(chain: FpgaChain, worker: &str) {
    let (addr, pool) = MockV1Pool::spawn().await.expect("loopback V1 pool");
    let (job_tx, mut job_rx) = mpsc::channel(8);
    let (share_tx, share_rx) = mpsc::channel(8);
    let (status_tx, _status_rx) = mpsc::channel(32);
    let client = StratumV1Client::new(
        stratum_config(MockV1PoolHandle::url(addr), worker),
        job_tx,
        share_rx,
        status_tx,
    );
    let client_task = tokio::spawn(client.run_until_sv2_retry(Duration::from_millis(900)));
    let mut job = tokio::time::timeout(Duration::from_secs(2), job_rx.recv())
        .await
        .expect("job timeout")
        .expect("loopback job");

    // The mock injects an explicit accept-all target for a deterministic host
    // proof. This avoids falsely claiming a CPU brute-force result at pdiff-1;
    // the exact 80-byte header still passes the production local validator.
    job.share_target = [0xff; 32];
    let work = WorkBuilder::new().next_work(&job);
    let expected_nonce = 0x1357_2468;
    let header = header_for(&work, expected_nonce);
    assert!(validate_full_header(&header, &work.share_target));

    chain
        .set_sim_nonce_policy(SimNoncePolicy::Valid)
        .expect("valid nonce policy");
    chain
        .set_sim_next_nonce(expected_nonce)
        .expect("inject target-valid nonce");
    let words: Vec<u32> = header
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four-byte header word")))
        .collect();
    chain.write_work(&words);
    let (sim_nonce, _metadata) = chain.read_nonce().expect("simulated nonce");
    assert_eq!(sim_nonce, expected_nonce);

    share_tx
        .send(ValidShare {
            worker_name: worker.to_string(),
            job_id: work.job_id,
            extranonce2: work.extranonce2,
            ntime: format!("{:08x}", work.ntime),
            nonce: format!("{sim_nonce:08x}"),
            version_bits: None,
            version: work.version,
            achieved_difficulty: None,
        })
        .await
        .expect("submit simulator nonce");

    tokio::time::timeout(Duration::from_secs(2), async {
        while pool.accepted_shares() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("loopback pool must accept simulator share");

    let returned = tokio::time::timeout(Duration::from_secs(3), client_task)
        .await
        .expect("client stop timeout")
        .expect("client task");
    assert_eq!(pool.accepted_shares(), 1);
    assert!(pool.requests().iter().any(|request| {
        request.contains("\"method\":\"mining.submit\"")
            && request.contains(&format!("{expected_nonce:08x}"))
    }));
    let stats = returned.stats();
    let stats = stats.lock().await;
    assert_eq!(stats.shares_submitted, 1);
    assert_eq!(stats.shares_accepted, 1);
    assert_eq!(stats.shares_rejected, 0);
}

#[tokio::test]
async fn s9_reaches_headless_t2_through_legacy_fpga_fifo() {
    let registry = ChipRegistry::production();
    let driver = registry.detect(0x1387).expect("production BM1387 driver");
    let mut hash_chain = HashChain::new(FpgaChain::open_sim(6).expect("simulated S9 FPGA"), 6);

    let enumeration = hash_chain
        .enumerate_chips()
        .expect("legacy FPGA FIFO enumeration");
    assert_eq!(
        (enumeration.chip_count(), enumeration.chip_id()),
        (63, 0x1387)
    );
    hash_chain
        .assign_addresses()
        .expect("legacy BM1387 address assignment");
    hash_chain
        .init_with_driver(driver, 600)
        .expect("production BM1387 initialization");
    hash_chain
        .fpga
        .set_sim_nonce_policy(SimNoncePolicy::Valid)
        .expect("valid S9 open-core nonce policy");
    assert_eq!(
        driver
            .send_open_core_work(&mut hash_chain.fpga, enumeration.chip_count())
            .expect("production BM1387 open-core sequence"),
        114
    );

    assert_share_accept(hash_chain.fpga, "dcent.sim.s9").await;
}

#[tokio::test]
async fn s19pro_reaches_headless_t2() {
    assert_headless_t2(SimModel::S19Pro, 0x1398, 114, 650, "dcent.sim.s19pro").await;
}

#[tokio::test]
async fn s17_reaches_headless_t2() {
    assert_headless_t2(SimModel::S17, 0x1397, 48, 650, "dcent.sim.s17").await;
}

#[tokio::test]
async fn s17pro_reaches_headless_t2() {
    assert_headless_t2(SimModel::S17Pro, 0x1397, 48, 650, "dcent.sim.s17pro").await;
}

#[tokio::test]
async fn t17_reaches_headless_t2() {
    assert_headless_t2(SimModel::T17, 0x1397, 30, 650, "dcent.sim.t17").await;
}

#[tokio::test]
async fn s19jpro_reaches_headless_t2() {
    assert_headless_t2(SimModel::S19jPro, 0x1362, 126, 545, "dcent.sim.s19jpro").await;
}

#[tokio::test]
async fn s19xp_reaches_headless_t2() {
    assert_headless_t2(SimModel::S19Xp, 0x1366, 110, 675, "dcent.sim.s19xp").await;
}

#[tokio::test]
async fn s19kpro_reaches_headless_t2() {
    assert_headless_t2(SimModel::S19kPro, 0x1366, 77, 670, "dcent.sim.s19kpro").await;
}

#[tokio::test]
async fn s21_reaches_headless_t2() {
    assert_headless_t2(SimModel::S21, 0x1368, 108, 525, "dcent.sim.s21").await;
}

#[tokio::test]
async fn s21pro_reaches_headless_t2() {
    assert_headless_t2(SimModel::S21Pro, 0x1370, 65, 525, "dcent.sim.s21pro").await;
}
