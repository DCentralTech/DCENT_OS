// Integration: real StratumClient share drain path for solo- jobs.
// Drives drain_pending_shares / handle_mining_share — production entry points.
// Serialized via TEST_LOCK because SOLO_SHARE_HOOK is process-global.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;

use dcentaxe_stratum::set_solo_share_hook;
use dcentaxe_stratum::types::{MiningEvent, ShareSubmission, StratumConfig};
use dcentaxe_stratum::StratumClient;

static TEST_LOCK: Mutex<()> = Mutex::new(());
static HOOK_HITS: AtomicUsize = AtomicUsize::new(0);
static EN2_OK: AtomicUsize = AtomicUsize::new(0);

fn hook(share: &ShareSubmission) {
    HOOK_HITS.fetch_add(1, Ordering::SeqCst);
    if share.extranonce2 == "aabbccdd" || share.extranonce2 == "11223344" {
        EN2_OK.fetch_add(1, Ordering::SeqCst);
    }
}

fn solo_share(job: &str, en2: &str) -> ShareSubmission {
    ShareSubmission {
        job_id: job.into(),
        extranonce2: en2.into(),
        ntime: "65a7e340".into(),
        nonce: "01020304".into(),
        version: 0x2000_0000,
        version_bits: None,
        difficulty: 1.0,
    }
}

#[test]
fn real_client_solo_share_intercept_drain_and_handle() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    HOOK_HITS.store(0, Ordering::SeqCst);
    EN2_OK.store(0, Ordering::SeqCst);
    set_solo_share_hook(Some(hook));

    let (event_tx, _event_rx) = mpsc::channel();
    let (share_tx, share_rx) = mpsc::channel();
    let mut client = StratumClient::new(StratumConfig::default(), event_tx, share_rx);

    // --- drain_pending_shares path (message_loop body) ---
    share_tx
        .send(MiningEvent::SubmitShare(solo_share(
            "solo-regtest-100",
            "aabbccdd",
        )))
        .unwrap();
    client.drain_pending_shares().expect("solo must not pool-submit");
    assert_eq!(HOOK_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(EN2_OK.load(Ordering::SeqCst), 1);

    // --- handle_mining_share direct entry ---
    client
        .handle_mining_share(solo_share("solo-regtest-1", "11223344"))
        .unwrap();
    assert_eq!(HOOK_HITS.load(Ordering::SeqCst), 2);
    assert_eq!(EN2_OK.load(Ordering::SeqCst), 2);

    set_solo_share_hook(None);
}
