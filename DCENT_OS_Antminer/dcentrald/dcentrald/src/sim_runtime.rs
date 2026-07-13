//! Host-only full-daemon simulation runtime.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use dcentrald_asic::chain::Chain as HashChain;
use dcentrald_asic::drivers::ChipRegistry;
use dcentrald_hal::chain_backend::Bm1397PlusChainBackend;
use dcentrald_hal::fpga_chain::FpgaChain;
use dcentrald_hal::platform::sim::{SimModel, SimNoncePolicy, SimPlatform};
use dcentrald_stratum::share_pipeline::{validate_full_header, WorkBuilder};
use dcentrald_stratum::types::{
    default_version_rolling_mask, DonationConfig, PoolConfig, StratumConfig, ValidShare,
};
use dcentrald_stratum::v1::mock_pool::{MockV1Pool, MockV1PoolHandle};
use dcentrald_stratum::StratumV1Client;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::DcentraldConfig;

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
        nominal_hashrate_ghs: 100_000.0,
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

async fn accepted_loopback_share(chain: &FpgaChain, worker: &str) -> Result<MockV1PoolHandle> {
    let (addr, pool) = MockV1Pool::spawn().await?;
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
        .context("loopback job timeout")?
        .ok_or_else(|| anyhow!("loopback pool closed before job"))?;
    job.share_target = [0xff; 32];
    let work = WorkBuilder::new().next_work(&job);
    let expected_nonce = 0x1357_2468;
    let header = header_for(&work, expected_nonce);
    if !validate_full_header(&header, &work.share_target) {
        return Err(anyhow!("simulated header did not satisfy injected target"));
    }
    chain.set_sim_nonce_policy(SimNoncePolicy::Valid)?;
    chain.set_sim_next_nonce(expected_nonce)?;
    let words: Vec<u32> = header
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .collect();
    chain.write_work(&words);
    let (nonce, _) = chain
        .read_nonce()
        .ok_or_else(|| anyhow!("simulated chain produced no nonce"))?;
    share_tx
        .send(ValidShare {
            worker_name: worker.to_string(),
            job_id: work.job_id,
            extranonce2: work.extranonce2,
            ntime: format!("{:08x}", work.ntime),
            nonce: format!("{nonce:08x}"),
            version_bits: None,
            version: work.version,
            achieved_difficulty: None,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(2), async {
        while pool.accepted_shares() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("loopback share acceptance timeout")?;
    let returned = tokio::time::timeout(Duration::from_secs(3), client_task).await??;
    let stats = returned.stats();
    let stats = stats.lock().await;
    if stats.shares_accepted != 1 || pool.accepted_shares() != 1 {
        return Err(anyhow!("loopback pool did not accept exactly one share"));
    }
    Ok(pool)
}

pub async fn run(config: DcentraldConfig, shutdown: CancellationToken) -> Result<()> {
    // Exercise the production detection entry point, including all safety gates
    // and real-hardware signature refusal, before constructing typed sim state.
    let _detected = dcentrald_hal::platform::detect_platform()?;
    let model = SimModel::from_str(
        &std::env::var("DCENT_SIM_MODEL").context("DCENT_SIM_MODEL is required")?,
    )?;
    let platform = SimPlatform::from_env()?;
    let profile = platform.profile();
    let chip_count = profile
        .chips_per_chain
        .ok_or_else(|| anyhow!("{} has no evidence-backed T2 geometry", model.slug()))?;
    let frequency = platform
        .silicon()
        .default_operating_point()
        .map(|point| point.freq_mhz as u16)
        .ok_or_else(|| anyhow!("{} has no silicon operating point", model.slug()))?;
    let voltage_mv = platform
        .silicon()
        .default_operating_point()
        .map(|point| (point.voltage_v * 1000.0).round() as u16)
        .unwrap_or(0);
    let registry = ChipRegistry::production();
    let driver = registry
        .detect(profile.chip_id)
        .ok_or_else(|| anyhow!("chip 0x{:04x} is not production-enabled", profile.chip_id))?;

    let mut chain = FpgaChain::open_sim_for_model(0, model)?;
    if model == SimModel::S9 {
        let mut legacy = HashChain::new(chain, 6);
        let enumerated = legacy.enumerate_chips()?;
        if (enumerated.chip_count(), enumerated.chip_id()) != (chip_count, profile.chip_id) {
            return Err(anyhow!("legacy enumeration mismatch: {enumerated:?}"));
        }
        legacy.assign_addresses()?;
        legacy.init_with_driver(driver, frequency)?;
        legacy.fpga.set_sim_nonce_policy(SimNoncePolicy::Valid)?;
        let activated = driver.send_open_core_work(&mut legacy.fpga, chip_count)?;
        if activated != 114 {
            return Err(anyhow!("S9 open-core activated {activated}, expected 114"));
        }
        chain = legacy.fpga;
    } else {
        let backend = platform.open_bm1397plus_backend(0)?;
        backend.send_get_address_bm1397plus()?;
        let responses = backend.read_all_responses(0)?;
        if responses.len() != usize::from(chip_count) {
            return Err(anyhow!(
                "enumerated {} of {chip_count} chips",
                responses.len()
            ));
        }
        driver.init_chain(&mut chain, chip_count, frequency)?;
        let _ = driver.send_open_core_work(&mut chain, chip_count)?;
    }

    let worker = format!("dcent.sim.{}", model.slug());
    let _pool = accepted_loopback_share(&chain, &worker).await?;

    let mode = dcentrald_api::OperatingMode::Standard;
    let mut state = dcentrald_api::MinerState::empty(mode);
    state.accepted = 1;
    state.pool.url = "loopback://mock-v1".to_string();
    state.pool.worker = worker;
    state.pool.status = "connected".to_string();
    state.chains = (0..profile.chain_count)
        .map(|id| dcentrald_api::ChainState {
            id,
            chips: chip_count,
            frequency_mhz: frequency,
            voltage_mv,
            temp_c: 25.0,
            temp_source: Some("simulated".to_string()),
            hashrate_ghs: 0.0,
            errors: 0,
            status: "simulated-ready".to_string(),
        })
        .collect();
    state.fans.pwm = 30;
    state.fans.rpm = 4200;
    let (_state_tx, state_rx) = tokio::sync::watch::channel(state);
    let (_health_tx, health_rx) = tokio::sync::watch::channel(
        dcentrald_api::RuntimeHealthSnapshot::for_mode(dcentrald_api::RuntimeHealthMode::Native),
    );
    let _api = crate::runtime::api::spawn_proxy_mode_api_with_state(
        config,
        dcentrald_api::RuntimeHealthMode::Native,
        Some(health_rx),
        Some(state_rx),
        shutdown.clone(),
    )
    .await?;
    tracing::info!(
        model = model.slug(),
        chip_id = format_args!("0x{:04x}", profile.chip_id),
        chip_count,
        accepted_shares = 1,
        "SIM_HAL_RUNTIME_READY"
    );
    shutdown.cancelled().await;
    tracing::info!(model = model.slug(), "SIM_HAL_RUNTIME_STOPPED");
    Ok(())
}
