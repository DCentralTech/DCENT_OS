//! Bounded blocking boundary for diagnostic report rendering and storage.
//!
//! Tokio handlers hand owned report values to this module. CPU-heavy rendering,
//! JSON conversion, durable pair publication, bounded reads, and directory
//! listing all execute on blocking workers behind zero-queue semaphore owners.

use super::*;
use serde::Serialize;
use std::sync::LazyLock;
use tokio::sync::Semaphore;

const DIAGNOSTIC_PERSISTENCE_CONCURRENCY: usize = 1;
static DIAGNOSTIC_PERSISTENCE_OWNER: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(DIAGNOSTIC_PERSISTENCE_CONCURRENCY)));
const DIAGNOSTIC_READ_CONCURRENCY: usize = 2;
static DIAGNOSTIC_READ_OWNER: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(DIAGNOSTIC_READ_CONCURRENCY)));

#[derive(Debug, PartialEq, Eq)]
enum BlockingReportOperationError<E> {
    Busy,
    Worker(String),
    Operation(E),
}

/// Run one synchronous report-store operation outside the Tokio worker pool.
///
/// Admission is deliberately zero-queue. Persistence has one permit because
/// immutable HTML/JSON publication is serialized; reads have two permits so a
/// pair of bounded responses cannot starve evidence publication or grow an
/// unbounded waiting-task/payload queue. An owned permit moves into the blocking
/// closure, so caller cancellation cannot abandon a publication halfway through.
/// A kernel/filesystem call that has already started remains unabortable.
async fn run_bounded_report_operation<T, E, F>(
    owner: Arc<Semaphore>,
    operation: F,
) -> Result<T, BlockingReportOperationError<E>>
where
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    let permit = owner
        .try_acquire_owned()
        .map_err(|_| BlockingReportOperationError::Busy)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| BlockingReportOperationError::Worker(error.to_string()))?
    .map_err(BlockingReportOperationError::Operation)
}

fn report_storage_busy_response() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "status": "error",
            "message": "diagnostic report persistence is busy; retry after the active immutable report pair is committed",
        })),
    )
        .into_response()
}

pub(super) async fn persist_snapshot_artifact<A, F>(
    test_id: Uuid,
    artifact: A,
    render: F,
) -> Result<(A, serde_json::Value), axum::response::Response>
where
    A: Serialize + Send + 'static,
    F: FnOnce(&ReportGenerator, &A) -> dcentrald_diagnostics::Result<Option<String>>
        + Send
        + 'static,
{
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_PERSISTENCE_OWNER), move || {
        let generator = ReportGenerator::new();
        let html = render(&generator, &artifact)?;
        let json_value = serde_json::to_value(&artifact).map_err(|error| {
            dcentrald_diagnostics::DiagnosticError::ReportGeneration(format!(
                "failed to serialize diagnostic artifact: {error}"
            ))
        })?;
        generator.save_report(&test_id, html.as_deref(), &json_value)?;
        Ok::<_, dcentrald_diagnostics::DiagnosticError>((artifact, json_value))
    })
    .await
    .map_err(|error| match error {
        BlockingReportOperationError::Busy => report_storage_busy_response(),
        BlockingReportOperationError::Worker(error) => {
            report_storage_error_response(&format!("diagnostic persistence worker failed: {error}"))
        }
        BlockingReportOperationError::Operation(error) => report_storage_error_response(&error),
    })
}

fn report_read_busy_response() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "status": "error",
            "message": "diagnostic report reads are busy; retry after an active bounded read completes",
        })),
    )
        .into_response()
}

fn map_report_read_error(
    test_id: Option<Uuid>,
    error: BlockingReportOperationError<dcentrald_diagnostics::DiagnosticError>,
) -> axum::response::Response {
    match error {
        BlockingReportOperationError::Busy => report_read_busy_response(),
        BlockingReportOperationError::Worker(error) => {
            report_storage_error_response(&format!("diagnostic read worker failed: {error}"))
        }
        BlockingReportOperationError::Operation(dcentrald_diagnostics::DiagnosticError::Io(io))
            if io.kind() == std::io::ErrorKind::NotFound =>
        {
            match test_id {
                Some(test_id) => report_not_found_response(&test_id.to_string()),
                None => report_storage_error_response(&io),
            }
        }
        BlockingReportOperationError::Operation(error) => report_storage_error_response(&error),
    }
}

pub(super) async fn load_snapshot_artifact(
    test_id: Uuid,
) -> Result<serde_json::Value, axum::response::Response> {
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_READ_OWNER), move || {
        ReportGenerator::new().load_report_json(&test_id)
    })
    .await
    .map_err(|error| map_report_read_error(Some(test_id), error))
}

pub(super) async fn load_snapshot_artifact_with_html_status(
    test_id: Uuid,
) -> Result<(serde_json::Value, bool), axum::response::Response> {
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_READ_OWNER), move || {
        let generator = ReportGenerator::new();
        let report = generator.load_report_json(&test_id)?;
        let html_available = generator
            .report_dir()
            .join(format!("{test_id}.html"))
            .exists();
        Ok::<_, dcentrald_diagnostics::DiagnosticError>((report, html_available))
    })
    .await
    .map_err(|error| map_report_read_error(Some(test_id), error))
}

pub(super) async fn snapshot_html_available(
    test_id: Uuid,
) -> Result<bool, axum::response::Response> {
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_READ_OWNER), move || {
        Ok::<_, dcentrald_diagnostics::DiagnosticError>(
            ReportGenerator::new()
                .report_dir()
                .join(format!("{test_id}.html"))
                .exists(),
        )
    })
    .await
    .map_err(|error| map_report_read_error(Some(test_id), error))
}

pub(super) async fn load_snapshot_html(test_id: Uuid) -> Result<String, axum::response::Response> {
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_READ_OWNER), move || {
        ReportGenerator::new().load_report_html(&test_id)
    })
    .await
    .map_err(|error| map_report_read_error(Some(test_id), error))
}

pub(super) async fn list_snapshot_reports(
) -> Result<Vec<dcentrald_diagnostics::report::ReportMetadata>, axum::response::Response> {
    run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_READ_OWNER), move || {
        ReportGenerator::new().list_reports()
    })
    .await
    .map_err(|error| map_report_read_error(None, error))
}

#[cfg(test)]
mod tests {
    use super::{run_bounded_report_operation, BlockingReportOperationError};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::{oneshot, Semaphore};

    const STORE_SOURCE: &str = include_str!("diagnostic_store.rs");
    const REST_SOURCE: &str = include_str!("../rest.rs");
    const LATE_SOURCE: &str = include_str!("late.rs");
    const DIAGNOSTICS_SOURCE: &str = include_str!("../../../dcentrald-diagnostics/src/lib.rs");

    #[tokio::test]
    async fn busy_owner_rejects_without_queueing_or_running_the_operation() {
        let owner = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&owner).try_acquire_owned().unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_operation = Arc::clone(&ran);

        let result = run_bounded_report_operation(Arc::clone(&owner), move || {
            ran_in_operation.store(true, Ordering::SeqCst);
            Ok::<_, &'static str>(())
        })
        .await;

        assert_eq!(result, Err(BlockingReportOperationError::Busy));
        assert!(!ran.load(Ordering::SeqCst));
        drop(held);
        assert_eq!(owner.available_permits(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_caller_does_not_cancel_inflight_pair_owner() {
        let owner = Arc::new(Semaphore::new(1));
        let completed = Arc::new(AtomicBool::new(false));
        let completed_in_operation = Arc::clone(&completed);
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let task = tokio::spawn(run_bounded_report_operation(
            Arc::clone(&owner),
            move || {
                let _ = started_tx.send(());
                release_rx.recv().unwrap();
                completed_in_operation.store(true, Ordering::SeqCst);
                Ok::<_, &'static str>(())
            },
        ));

        started_rx.await.unwrap();
        assert_eq!(owner.available_permits(), 0);
        task.abort();
        release_tx.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while owner.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking persistence owner did not finish after caller cancellation");

        assert!(completed.load(Ordering::SeqCst));
        assert_eq!(owner.available_permits(), 1);
    }

    #[test]
    fn render_save_read_and_list_route_through_bounded_blocking_owners() {
        let save_call = [".save_", "report("].concat();
        assert_eq!(
            STORE_SOURCE.matches(&save_call).count(),
            1,
            "diagnostic store must have one persistence mutation choke point"
        );
        let admission_call = ["try_acquire_", "owned()"].concat();
        let blocking_call = ["tokio::task::spawn_", "blocking"].concat();
        assert!(STORE_SOURCE.contains(&admission_call));
        assert!(STORE_SOURCE.contains(&blocking_call));

        let timed_marker = "persist_snapshot_artifact(test_id, report, |generator, report|";
        let timed_call = REST_SOURCE
            .find(&timed_marker)
            .expect("timed HashReport persistence call");
        let timed_end = (timed_call + 800).min(REST_SOURCE.len());
        assert!(REST_SOURCE[timed_call..timed_end].contains(".await"));
        let operation_start = STORE_SOURCE
            .find("run_bounded_report_operation(Arc::clone(&DIAGNOSTIC_PERSISTENCE_OWNER)")
            .expect("bounded persistence operation");
        let operation_end = (operation_start + 1_200).min(STORE_SOURCE.len());
        let operation = &STORE_SOURCE[operation_start..operation_end];
        assert!(operation.contains("let html = render(&generator, &artifact)?;"));
        assert!(operation.contains("serde_json::to_value(&artifact)"));
        let save_choke_point = ["generator.save_", "report("].concat();
        assert!(operation.contains(&save_choke_point));

        assert_eq!(LATE_SOURCE.matches("persist_snapshot_artifact(").count(), 2);
        for call in LATE_SOURCE.match_indices("persist_snapshot_artifact(") {
            let end = (call.0 + 240).min(LATE_SOURCE.len());
            assert!(LATE_SOURCE[call.0..end].contains(".await"));
        }

        assert!(STORE_SOURCE.contains("static DIAGNOSTIC_READ_OWNER:"));
        assert!(STORE_SOURCE.contains("async fn load_snapshot_artifact("));
        assert!(STORE_SOURCE.contains("async fn load_snapshot_artifact_with_html_status("));
        assert!(STORE_SOURCE.contains("async fn snapshot_html_available("));
        assert!(STORE_SOURCE.contains("async fn load_snapshot_html("));
        assert!(STORE_SOURCE.contains("async fn list_snapshot_reports("));
        assert_eq!(LATE_SOURCE.matches("load_snapshot_artifact(").count(), 4);
        assert_eq!(
            LATE_SOURCE
                .matches("load_snapshot_artifact_with_html_status(")
                .count(),
            4
        );
        assert_eq!(LATE_SOURCE.matches("load_snapshot_html(").count(), 3);
        assert!(LATE_SOURCE.contains("list_snapshot_reports().await"));
        for call in LATE_SOURCE.match_indices("load_snapshot_artifact(") {
            let end = (call.0 + 120).min(LATE_SOURCE.len());
            assert!(LATE_SOURCE[call.0..end].contains(".await"));
        }
        for call in LATE_SOURCE.match_indices("load_snapshot_artifact_with_html_status(") {
            let end = (call.0 + 140).min(LATE_SOURCE.len());
            assert!(LATE_SOURCE[call.0..end].contains(".await"));
        }
        assert!(LATE_SOURCE.contains("snapshot_html_available(test_id).await"));
        for call in LATE_SOURCE.match_indices("load_snapshot_html(") {
            let end = (call.0 + 120).min(LATE_SOURCE.len());
            assert!(LATE_SOURCE[call.0..end].contains(".await"));
        }

        assert!(DIAGNOSTICS_SOURCE
            .contains("match (config.finalize)(test_id, tracker.elapsed_s()).await"));
    }
}
