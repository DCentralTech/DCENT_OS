//! Bounded mining-pipeline snapshot publisher.
//!
//! This publisher is observability-only. It consumes the existing session
//! `mining_sync` broadcast stream and copies bounded counters into a watch
//! channel for REST to clone. It does not inspect dispatcher internals, poll
//! pool sockets, touch hardware, read logs, or mutate the filesystem.

use tokio::sync::{broadcast, watch};
use tokio::time::{Duration, Instant, MissedTickBehavior};

use crate::websocket::{WsMiningSyncEventKind, WsMiningSyncMessage};
use crate::{MiningPipelineSnapshot, MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS};

pub const MINING_PIPELINE_SNAPSHOT_SOURCE: &str = "mining_sync_bounded_publisher";
pub const MINING_PIPELINE_SNAPSHOT_WAITING_SOURCE: &str = "mining_sync_bounded_publisher_waiting";

fn publisher_limitations() -> Vec<String> {
    vec![
        "Publisher consumes existing mining_sync broadcasts only.".to_string(),
        "REST clones the latest watch value and does not subscribe to mining_sync.".to_string(),
        "No hardware registers, dispatcher internals, pool sockets, logs, or filesystem state are read.".to_string(),
        "dispatch_queue_depth and work_ring_occupancy remain null until the mining loop publishes those exact fields.".to_string(),
        "Drop counters remain null unless future mining_sync events carry explicit drop evidence.".to_string(),
        "Share achieved difficulty is copied only from locally computed event difficulty; target_difficulty remains separate.".to_string(),
    ]
}

fn event_count(msg: &WsMiningSyncMessage) -> u64 {
    msg.count
        .map(u64::from)
        .filter(|count| *count > 0)
        .unwrap_or(1)
}

fn add_counter(slot: &mut Option<u64>, increment: u64) {
    let current = slot.unwrap_or(0);
    *slot = Some(current.saturating_add(increment));
}

fn copy_job_id(snapshot: &mut MiningPipelineSnapshot, msg: &WsMiningSyncMessage) {
    if let Some(job_id) = msg
        .job_id
        .as_ref()
        .filter(|job_id| !job_id.trim().is_empty())
    {
        snapshot.current_job_id = Some(job_id.clone());
    }
}

fn copy_last_share(snapshot: &mut MiningPipelineSnapshot, msg: &WsMiningSyncMessage, result: &str) {
    snapshot.last_share_timestamp_ms = Some(msg.timestamp_ms);
    snapshot.last_share_result = Some(result.to_string());
    snapshot.last_share_job_id = msg.job_id.clone();
    snapshot.last_share_achieved_difficulty = msg.difficulty.filter(|value| value.is_finite());
    snapshot.last_share_target_difficulty = msg
        .target_difficulty
        .filter(|value| value.is_finite() && *value > 0.0);
    snapshot.last_share_error_code = msg.error_code;
    snapshot.last_share_error_msg = msg.error_msg.clone();
}

fn value_u32(raw: &serde_json::Value, key: &str) -> Option<u32> {
    raw.get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
}

fn value_u64(raw: &serde_json::Value, key: &str) -> Option<u64> {
    raw.get(key).and_then(|value| value.as_u64())
}

fn value_bool(raw: &serde_json::Value, key: &str) -> Option<bool> {
    raw.get(key).and_then(|value| value.as_bool())
}

fn value_string(raw: &serde_json::Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn copy_pipeline_fields(snapshot: &mut MiningPipelineSnapshot, raw: &serde_json::Value) {
    if let Some(value) = value_u32(raw, "work_ring_occupancy") {
        snapshot.work_ring_occupancy = Some(value);
    }
    if let Some(value) = value_u32(raw, "dispatch_queue_depth") {
        snapshot.dispatch_queue_depth = Some(value);
    }
    if let Some(value) = value_u64(raw, "stale_nonce_drops_total") {
        snapshot.stale_nonce_drops_total = Some(value);
    }
    if let Some(value) = value_u64(raw, "unsupported_version_drops_total") {
        snapshot.unsupported_version_drops_total = Some(value);
    }
    if let Some(value) = value_u64(raw, "local_validation_drops_total") {
        snapshot.local_validation_drops_total = Some(value);
    }
    if let Some(value) = value_bool(raw, "pool_authorized") {
        snapshot.pool_authorized = Some(value);
    }
    if let Some(value) =
        value_string(raw, "authorize_state").or_else(|| value_string(raw, "pool_authorize_state"))
    {
        snapshot.pool_authorize_state = Some(value);
    }
}

#[derive(Debug, Clone)]
pub struct MiningPipelineSnapshotAccumulator {
    snapshot: MiningPipelineSnapshot,
    stale_after_ms: u64,
}

impl MiningPipelineSnapshotAccumulator {
    pub fn new(stale_after_ms: u64) -> Self {
        let mut snapshot = MiningPipelineSnapshot {
            publisher_enabled: true,
            source: MINING_PIPELINE_SNAPSHOT_WAITING_SOURCE.to_string(),
            limitations: publisher_limitations(),
            ..MiningPipelineSnapshot::default()
        };
        snapshot.status = crate::MiningPipelineSnapshotStatus::Unavailable;
        snapshot.snapshot_available = false;

        Self {
            snapshot,
            stale_after_ms: stale_after_ms.max(1),
        }
    }

    pub fn apply_json(&mut self, raw: &str) -> Option<MiningPipelineSnapshot> {
        let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
        let msg = serde_json::from_value::<WsMiningSyncMessage>(value.clone()).ok()?;
        if msg.msg_type != "mining_sync" {
            return None;
        }
        Some(self.apply_message_with_raw(&msg, Some(&value)))
    }

    pub fn apply_message(&mut self, msg: &WsMiningSyncMessage) -> MiningPipelineSnapshot {
        self.apply_message_with_raw(msg, None)
    }

    fn apply_message_with_raw(
        &mut self,
        msg: &WsMiningSyncMessage,
        raw: Option<&serde_json::Value>,
    ) -> MiningPipelineSnapshot {
        self.snapshot.publisher_enabled = true;
        self.snapshot.publisher_last_update_ms = Some(msg.timestamp_ms);
        self.snapshot.source = MINING_PIPELINE_SNAPSHOT_SOURCE.to_string();
        self.snapshot.limitations = publisher_limitations();
        if let Some(raw) = raw {
            copy_pipeline_fields(&mut self.snapshot, raw);
        }

        match msg.event {
            WsMiningSyncEventKind::AuthorizeState => {
                if self.snapshot.pool_authorize_state.is_none() {
                    self.snapshot.pool_authorize_state = Some("unknown".to_string());
                }
            }
            WsMiningSyncEventKind::JobReceived => {
                self.snapshot.last_notify_timestamp_ms = Some(msg.timestamp_ms);
                copy_job_id(&mut self.snapshot, msg);
            }
            WsMiningSyncEventKind::CleanJob => {
                self.snapshot.last_notify_timestamp_ms = Some(msg.timestamp_ms);
                add_counter(&mut self.snapshot.clean_jobs_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
            }
            WsMiningSyncEventKind::DispatchBurst => {
                add_counter(&mut self.snapshot.dispatch_bursts_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
            }
            WsMiningSyncEventKind::NonceBurst => {
                add_counter(&mut self.snapshot.nonce_bursts_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
            }
            WsMiningSyncEventKind::ShareAccepted => {
                add_counter(&mut self.snapshot.shares_accepted_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
                copy_last_share(&mut self.snapshot, msg, "accepted");
            }
            WsMiningSyncEventKind::ShareRejected => {
                add_counter(&mut self.snapshot.shares_rejected_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
                copy_last_share(&mut self.snapshot, msg, "rejected");
            }
            WsMiningSyncEventKind::LuckyShare => {
                add_counter(&mut self.snapshot.lucky_shares_total, event_count(msg));
                copy_job_id(&mut self.snapshot, msg);
                copy_last_share(&mut self.snapshot, msg, "lucky");
            }
        }

        self.snapshot
            .clone()
            .normalize_freshness(msg.timestamp_ms, self.stale_after_ms)
    }
}

pub fn spawn_mining_pipeline_snapshot_publisher(
    mining_sync_tx: &broadcast::Sender<String>,
    stale_after_ms: u64,
) -> watch::Receiver<MiningPipelineSnapshot> {
    let stale_after_ms = if stale_after_ms == 0 {
        MINING_PIPELINE_SNAPSHOT_DEFAULT_STALE_AFTER_MS
    } else {
        stale_after_ms
    };
    let mut initial = MiningPipelineSnapshot {
        publisher_enabled: true,
        source: MINING_PIPELINE_SNAPSHOT_WAITING_SOURCE.to_string(),
        limitations: publisher_limitations(),
        ..MiningPipelineSnapshot::default()
    }
    .normalize_freshness(0, stale_after_ms);
    initial.publisher_enabled = true;
    initial.source = MINING_PIPELINE_SNAPSHOT_WAITING_SOURCE.to_string();

    let (snapshot_tx, snapshot_rx) = watch::channel(initial);
    let mut mining_sync_rx = mining_sync_tx.subscribe();

    tokio::spawn(async move {
        let mut accumulator = MiningPipelineSnapshotAccumulator::new(stale_after_ms);
        let mut pending_snapshot: Option<MiningPipelineSnapshot> = None;
        let mut last_publish_at: Option<Instant> = None;
        let mut publish_timer = tokio::time::interval(Duration::from_millis(1_000));
        publish_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                result = mining_sync_rx.recv() => {
                    match result {
                        Ok(raw) => {
                            if let Some(snapshot) = accumulator.apply_json(&raw) {
                                let now = Instant::now();
                                let publish_now = last_publish_at
                                    .map(|last| now.duration_since(last) >= Duration::from_millis(1_000))
                                    .unwrap_or(true);

                                if publish_now {
                                    let _ = snapshot_tx.send(snapshot);
                                    last_publish_at = Some(now);
                                    pending_snapshot = None;
                                } else {
                                    pending_snapshot = Some(snapshot);
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                },
                _ = publish_timer.tick() => {
                    if let Some(snapshot) = pending_snapshot.take() {
                        let _ = snapshot_tx.send(snapshot);
                        last_publish_at = Some(Instant::now());
                    }
                }
            }
        }
    });

    snapshot_rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync_msg(event: WsMiningSyncEventKind, timestamp_ms: u64) -> WsMiningSyncMessage {
        WsMiningSyncMessage {
            msg_type: "mining_sync".to_string(),
            timestamp_ms,
            event,
            chain_id: Some(1),
            count: None,
            job_id: Some("job-7".to_string()),
            difficulty: None,
            target_difficulty: None,
            intensity: None,
            error_code: None,
            error_msg: None,
        }
    }

    #[test]
    fn accumulator_tracks_job_dispatch_nonce_and_share_lifecycle() {
        let mut accumulator = MiningPipelineSnapshotAccumulator::new(5_000);

        let mut snapshot =
            accumulator.apply_message(&sync_msg(WsMiningSyncEventKind::JobReceived, 100_000));
        assert_eq!(snapshot.last_notify_timestamp_ms, Some(100_000));
        assert_eq!(snapshot.current_job_id.as_deref(), Some("job-7"));
        assert!(snapshot.snapshot_available);

        let mut dispatch = sync_msg(WsMiningSyncEventKind::DispatchBurst, 100_250);
        dispatch.count = Some(4);
        snapshot = accumulator.apply_message(&dispatch);
        assert_eq!(snapshot.dispatch_bursts_total, Some(4));
        assert_eq!(snapshot.dispatch_queue_depth, None);
        assert_eq!(snapshot.work_ring_occupancy, None);
        assert_eq!(snapshot.last_notify_age_ms, Some(250));

        let mut nonce = sync_msg(WsMiningSyncEventKind::NonceBurst, 100_500);
        nonce.count = Some(9);
        snapshot = accumulator.apply_message(&nonce);
        assert_eq!(snapshot.nonce_bursts_total, Some(9));

        let mut accepted = sync_msg(WsMiningSyncEventKind::ShareAccepted, 101_000);
        accepted.difficulty = Some(65_536.0);
        accepted.target_difficulty = Some(8_192.0);
        snapshot = accumulator.apply_message(&accepted);
        assert_eq!(snapshot.shares_accepted_total, Some(1));
        assert_eq!(snapshot.last_share_result.as_deref(), Some("accepted"));
        assert_eq!(snapshot.last_share_achieved_difficulty, Some(65_536.0));
        assert_eq!(snapshot.last_share_target_difficulty, Some(8_192.0));

        let mut rejected = sync_msg(WsMiningSyncEventKind::ShareRejected, 101_250);
        rejected.error_code = Some(23);
        rejected.error_msg = Some("low difficulty share".to_string());
        snapshot = accumulator.apply_message(&rejected);
        assert_eq!(snapshot.shares_rejected_total, Some(1));
        assert_eq!(snapshot.last_share_result.as_deref(), Some("rejected"));
        assert_eq!(snapshot.last_share_error_code, Some(23));
        assert_eq!(
            snapshot.last_share_error_msg.as_deref(),
            Some("low difficulty share")
        );
    }

    #[test]
    fn accumulator_ignores_non_mining_sync_json() {
        let mut accumulator = MiningPipelineSnapshotAccumulator::new(5_000);

        assert!(accumulator
            .apply_json(r#"{"type":"stats","timestamp_ms":1}"#)
            .is_none());
        assert!(accumulator.apply_json("not json").is_none());
    }

    #[test]
    fn accumulator_copies_authorize_queue_ring_and_drop_fields_from_json() {
        let mut accumulator = MiningPipelineSnapshotAccumulator::new(5_000);

        let snapshot = accumulator
            .apply_json(
                r#"{
                    "type":"mining_sync",
                    "timestamp_ms":200000,
                    "event":"authorize_state",
                    "pool_authorized":true,
                    "authorize_state":"authorized",
                    "dispatch_queue_depth":3,
                    "work_ring_occupancy":41,
                    "stale_nonce_drops_total":5,
                    "unsupported_version_drops_total":2,
                    "local_validation_drops_total":7
                }"#,
            )
            .expect("snapshot");

        assert_eq!(snapshot.pool_authorized, Some(true));
        assert_eq!(snapshot.pool_authorize_state.as_deref(), Some("authorized"));
        assert_eq!(snapshot.dispatch_queue_depth, Some(3));
        assert_eq!(snapshot.work_ring_occupancy, Some(41));
        assert_eq!(snapshot.stale_nonce_drops_total, Some(5));
        assert_eq!(snapshot.unsupported_version_drops_total, Some(2));
        assert_eq!(snapshot.local_validation_drops_total, Some(7));
        assert!(snapshot.snapshot_available);
    }
}
