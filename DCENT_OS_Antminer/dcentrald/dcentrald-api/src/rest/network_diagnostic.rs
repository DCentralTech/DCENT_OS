//! Bounded subprocess owner for `/api/diagnostics/troubleshoot/network`.
//!
//! This is intentionally a narrow REST-owned port. The daemon does not yet
//! provide a platform networking service to `AppState`, so fixed allowlisted
//! read-only probes live here instead of widening every AppState constructor.
//! Admission, child lifetime, output bounds, and deadlines are centralized so
//! an HTTP cancellation cannot orphan an unlimited subprocess fleet.

use async_trait::async_trait;
use serde::Serialize;
use std::net::IpAddr;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Semaphore};
use tokio_util::sync::CancellationToken;

const NETWORK_PROBE_CONCURRENCY: usize = 1;
const NETWORK_PROBE_OUTPUT_LIMIT_BYTES: usize = 16 * 1024;
const NETWORK_PROBE_REAP_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy)]
struct ProbeBudgets {
    ip: Duration,
    ping: Duration,
    dns: Duration,
    overall: Duration,
}

impl Default for ProbeBudgets {
    fn default() -> Self {
        Self {
            ip: Duration::from_millis(500),
            ping: Duration::from_millis(2_500),
            dns: Duration::from_millis(2_000),
            overall: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct NetworkProbeInput {
    pub dns_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProbeStatus {
    Ok,
    Skipped,
    Busy,
    SpawnError,
    NonZeroExit,
    Timeout,
    Cancelled,
    OutputTooLarge,
    InvalidOutput,
    WorkerError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ProbeStageOutcome {
    pub status: ProbeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ProbeStageOutcome {
    fn new(status: ProbeStatus, detail: impl Into<Option<String>>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    fn ok() -> Self {
        Self::new(ProbeStatus::Ok, None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct NetworkProbeSnapshot {
    pub interface: Option<String>,
    pub ip_cidr: Option<String>,
    pub ip_address: Option<String>,
    pub gateway: Option<String>,
    pub gateway_reachable: Option<bool>,
    pub dns_ok: Option<bool>,
    pub ip_address_probe: ProbeStageOutcome,
    pub route_probe: ProbeStageOutcome,
    pub gateway_probe: ProbeStageOutcome,
    pub dns_probe: ProbeStageOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NetworkProbeError {
    Busy,
    Cancelled,
    Worker(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AllowlistedProbeCommand {
    IpRoute,
    IpAddress(String),
    PingGateway(IpAddr),
    DnsLookup(String),
}

impl AllowlistedProbeCommand {
    fn program_and_args(&self) -> (&'static str, Vec<String>) {
        match self {
            Self::IpAddress(interface) => (
                "ip",
                vec![
                    "-4".to_string(),
                    "addr".to_string(),
                    "show".to_string(),
                    "dev".to_string(),
                    interface.clone(),
                ],
            ),
            Self::IpRoute => (
                "ip",
                ["route", "show", "default"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            Self::PingGateway(gateway) => (
                "ping",
                vec![
                    "-c".to_string(),
                    "1".to_string(),
                    "-W".to_string(),
                    "2".to_string(),
                    gateway.to_string(),
                ],
            ),
            Self::DnsLookup(host) => ("nslookup", vec![host.clone()]),
        }
    }
}

#[derive(Debug)]
struct CapturedProcessOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessWaitError {
    Io(String),
    OutputTooLarge,
}

#[async_trait]
trait ProbeProcess: Send {
    async fn wait_bounded(
        &mut self,
        output_limit: usize,
    ) -> Result<CapturedProcessOutput, ProcessWaitError>;

    /// Kill and reap the child before returning whenever the OS permits it.
    /// `false` means the bounded reap wait expired; dropping the child still
    /// applies Tokio's `kill_on_drop` fallback.
    async fn kill_and_reap(&mut self) -> bool;
}

trait ProbeProcessFactory: Send + Sync {
    fn spawn(&self, command: &AllowlistedProbeCommand) -> Result<Box<dyn ProbeProcess>, String>;
}

struct TokioProbeProcessFactory;

impl ProbeProcessFactory for TokioProbeProcessFactory {
    fn spawn(&self, command: &AllowlistedProbeCommand) -> Result<Box<dyn ProbeProcess>, String> {
        let (program, args) = command.program_and_args();
        let mut process = Command::new(program);
        process
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = process
            .spawn()
            .map_err(|error| format!("failed to spawn allowlisted `{program}` probe: {error}"))?;
        Ok(Box::new(TokioProbeProcess {
            child,
            reaped: false,
        }))
    }
}

struct TokioProbeProcess {
    child: Child,
    reaped: bool,
}

async fn read_capped<R>(mut reader: R, limit: usize) -> Result<Vec<u8>, ProcessWaitError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|error| ProcessWaitError::Io(error.to_string()))?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > limit {
            return Err(ProcessWaitError::OutputTooLarge);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[async_trait]
impl ProbeProcess for TokioProbeProcess {
    async fn wait_bounded(
        &mut self,
        output_limit: usize,
    ) -> Result<CapturedProcessOutput, ProcessWaitError> {
        let stdout =
            self.child.stdout.take().ok_or_else(|| {
                ProcessWaitError::Io("probe stdout pipe is unavailable".to_string())
            })?;
        let stderr =
            self.child.stderr.take().ok_or_else(|| {
                ProcessWaitError::Io("probe stderr pipe is unavailable".to_string())
            })?;

        let (status, stdout, stderr) = tokio::try_join!(
            async {
                self.child
                    .wait()
                    .await
                    .map_err(|error| ProcessWaitError::Io(error.to_string()))
            },
            read_capped(stdout, output_limit),
            read_capped(stderr, output_limit),
        )?;
        self.reaped = true;
        Ok(CapturedProcessOutput {
            success: status.success(),
            stdout,
            stderr,
        })
    }

    async fn kill_and_reap(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.reaped = true;
                return true;
            }
            Ok(None) => {}
            Err(_) => {}
        }
        let _ = self.child.start_kill();
        match tokio::time::timeout(NETWORK_PROBE_REAP_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => {
                self.reaped = true;
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug)]
enum CommandCompletion {
    Output(CapturedProcessOutput),
    Timeout { reaped: bool },
    Cancelled { reaped: bool },
    SpawnError(String),
    OutputTooLarge { reaped: bool },
    IoError { message: String, reaped: bool },
}

struct NetworkProbeOwner {
    admission: Arc<Semaphore>,
    factory: Arc<dyn ProbeProcessFactory>,
    budgets: ProbeBudgets,
}

impl NetworkProbeOwner {
    fn production() -> Self {
        Self {
            admission: Arc::new(Semaphore::new(NETWORK_PROBE_CONCURRENCY)),
            factory: Arc::new(TokioProbeProcessFactory),
            budgets: ProbeBudgets::default(),
        }
    }

    async fn run(
        &self,
        input: NetworkProbeInput,
    ) -> Result<NetworkProbeSnapshot, NetworkProbeError> {
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| {
                if self.admission.is_closed() {
                    NetworkProbeError::Worker(
                        "network probe admission is poisoned after an unconfirmed child reap; daemon restart is required"
                            .to_string(),
                    )
                } else {
                    NetworkProbeError::Busy
                }
            })?;
        let cancellation = CancellationToken::new();
        let mut cancel_on_drop = CancelOnDrop::new(cancellation.clone());
        let factory = Arc::clone(&self.factory);
        let admission = Arc::clone(&self.admission);
        let budgets = self.budgets;
        let (reply_tx, reply_rx) = oneshot::channel();

        tokio::spawn(async move {
            let _permit = permit;
            let result =
                execute_network_probe(factory, budgets, input, cancellation, admission).await;
            let _ = reply_tx.send(result);
        });

        let result = reply_rx.await.map_err(|error| {
            NetworkProbeError::Worker(format!("network probe owner stopped: {error}"))
        })?;
        cancel_on_drop.disarm();
        result
    }
}

struct CancelOnDrop {
    cancellation: CancellationToken,
    armed: bool,
}

impl CancelOnDrop {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

static NETWORK_PROBE_OWNER: LazyLock<NetworkProbeOwner> =
    LazyLock::new(NetworkProbeOwner::production);

pub(super) async fn run_network_diagnostic(
    input: NetworkProbeInput,
) -> Result<NetworkProbeSnapshot, NetworkProbeError> {
    NETWORK_PROBE_OWNER.run(input).await
}

async fn execute_command(
    factory: &Arc<dyn ProbeProcessFactory>,
    command: AllowlistedProbeCommand,
    stage_budget: Duration,
    overall_deadline: tokio::time::Instant,
    cancellation: &CancellationToken,
) -> CommandCompletion {
    // Observe cancellation before constructing a child. In particular, a
    // request can disappear between two completed stages; the owner must not
    // launch one more process before the select below gets a chance to run.
    if cancellation.is_cancelled() {
        return CommandCompletion::Cancelled { reaped: true };
    }
    let Some(remaining) = overall_deadline.checked_duration_since(tokio::time::Instant::now())
    else {
        return CommandCompletion::Timeout { reaped: true };
    };
    if remaining.is_zero() {
        return CommandCompletion::Timeout { reaped: true };
    }
    let budget = stage_budget.min(remaining);
    let mut process = match factory.spawn(&command) {
        Ok(process) => process,
        Err(error) => return CommandCompletion::SpawnError(error),
    };

    tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            let reaped = process.kill_and_reap().await;
            CommandCompletion::Cancelled { reaped }
        }
        _ = tokio::time::sleep(budget) => {
            let reaped = process.kill_and_reap().await;
            CommandCompletion::Timeout { reaped }
        }
        result = process.wait_bounded(NETWORK_PROBE_OUTPUT_LIMIT_BYTES) => {
            match result {
                Ok(output) => CommandCompletion::Output(output),
                Err(ProcessWaitError::OutputTooLarge) => {
                    let reaped = process.kill_and_reap().await;
                    CommandCompletion::OutputTooLarge { reaped }
                }
                Err(ProcessWaitError::Io(message)) => {
                    let reaped = process.kill_and_reap().await;
                    CommandCompletion::IoError { message, reaped }
                }
            }
        }
    }
}

fn reject_unreaped_child(
    completion: &CommandCompletion,
    stage: &'static str,
    admission: &Semaphore,
) -> Result<(), NetworkProbeError> {
    let reaped = match completion {
        CommandCompletion::Timeout { reaped }
        | CommandCompletion::Cancelled { reaped }
        | CommandCompletion::OutputTooLarge { reaped }
        | CommandCompletion::IoError { reaped, .. } => *reaped,
        CommandCompletion::Output(_) | CommandCompletion::SpawnError(_) => true,
    };
    if reaped {
        Ok(())
    } else {
        // Fail closed for the lifetime of this owner. The child object is about
        // to be dropped with kill_on_drop armed, but until the OS confirms reap
        // we cannot safely claim the single subprocess slot is available.
        admission.close();
        Err(NetworkProbeError::Worker(format!(
            "{stage} probe child could not be reaped within the bounded shutdown deadline; remaining probes were not started and network probe admission is poisoned until restart"
        )))
    }
}

fn completion_outcome(completion: &CommandCompletion) -> ProbeStageOutcome {
    match completion {
        CommandCompletion::Output(output) if output.success => ProbeStageOutcome::ok(),
        CommandCompletion::Output(output) => ProbeStageOutcome::new(
            ProbeStatus::NonZeroExit,
            Some(bounded_stderr_detail(&output.stderr)),
        ),
        CommandCompletion::Timeout { reaped } => {
            ProbeStageOutcome::new(ProbeStatus::Timeout, Some(reap_detail(*reaped)))
        }
        CommandCompletion::Cancelled { reaped } => {
            ProbeStageOutcome::new(ProbeStatus::Cancelled, Some(reap_detail(*reaped)))
        }
        CommandCompletion::SpawnError(error) => {
            ProbeStageOutcome::new(ProbeStatus::SpawnError, Some(error.clone()))
        }
        CommandCompletion::OutputTooLarge { reaped } => {
            ProbeStageOutcome::new(ProbeStatus::OutputTooLarge, Some(reap_detail(*reaped)))
        }
        CommandCompletion::IoError { message, reaped } => ProbeStageOutcome::new(
            ProbeStatus::WorkerError,
            Some(format!("{message}; {}", reap_detail(*reaped))),
        ),
    }
}

fn reap_detail(reaped: bool) -> String {
    if reaped {
        "child killed and reaped".to_string()
    } else {
        "child kill requested; bounded reap wait expired and kill_on_drop remains armed".to_string()
    }
}

fn bounded_stderr_detail(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        "probe exited unsuccessfully without diagnostic output".to_string()
    } else {
        detail
    }
}

fn stdout_text(completion: &CommandCompletion) -> Option<String> {
    match completion {
        CommandCompletion::Output(output) if output.success => {
            String::from_utf8(output.stdout.clone()).ok()
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedInterfaceAddress {
    cidr: String,
    address: String,
}

fn parse_ipv4_address(stdout: &str) -> Option<ParsedInterfaceAddress> {
    stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        while let Some(field) = fields.next() {
            if field == "inet" {
                let cidr = fields.next()?;
                let (address, prefix) = cidr.split_once('/')?;
                if prefix
                    .parse::<u8>()
                    .ok()
                    .filter(|prefix| *prefix <= 32)
                    .is_none()
                {
                    return None;
                }
                return address.parse::<std::net::Ipv4Addr>().ok().map(|address| {
                    ParsedInterfaceAddress {
                        cidr: cidr.to_string(),
                        address: address.to_string(),
                    }
                });
            }
        }
        None
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDefaultRoute {
    interface: String,
    gateway: Option<IpAddr>,
}

fn valid_interface_name(interface: &str) -> bool {
    let bytes = interface.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 15
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

fn parse_default_route(stdout: &str) -> Option<ParsedDefaultRoute> {
    stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "default" {
            return None;
        }
        let mut interface = None;
        let mut gateway = None;
        while let Some(field) = fields.next() {
            match field {
                "dev" => interface = fields.next().map(str::to_string),
                "via" => gateway = Some(IpAddr::from_str(fields.next()?).ok()?),
                _ => {}
            }
        }
        let interface = interface.filter(|value| valid_interface_name(value))?;
        Some(ParsedDefaultRoute { interface, gateway })
    })
}

fn valid_dns_host(host: &str) -> bool {
    let host = host.trim();
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
}

fn invalid_output(detail: &'static str) -> ProbeStageOutcome {
    ProbeStageOutcome::new(ProbeStatus::InvalidOutput, Some(detail.to_string()))
}

async fn execute_network_probe(
    factory: Arc<dyn ProbeProcessFactory>,
    budgets: ProbeBudgets,
    input: NetworkProbeInput,
    cancellation: CancellationToken,
    admission: Arc<Semaphore>,
) -> Result<NetworkProbeSnapshot, NetworkProbeError> {
    let overall_deadline = tokio::time::Instant::now() + budgets.overall;

    let route_completion = execute_command(
        &factory,
        AllowlistedProbeCommand::IpRoute,
        budgets.ip,
        overall_deadline,
        &cancellation,
    )
    .await;
    reject_unreaped_child(&route_completion, "default route", &admission)?;
    if cancellation.is_cancelled()
        || matches!(route_completion, CommandCompletion::Cancelled { .. })
    {
        return Err(NetworkProbeError::Cancelled);
    }
    let default_route =
        stdout_text(&route_completion).and_then(|stdout| parse_default_route(&stdout));
    let interface = default_route.as_ref().map(|route| route.interface.clone());
    let gateway_ip = default_route.as_ref().and_then(|route| route.gateway);
    let gateway = gateway_ip.map(|gateway| gateway.to_string());
    let route_probe = if matches!(route_completion, CommandCompletion::Output(ref output) if output.success)
        && default_route.is_none()
    {
        invalid_output("route output did not contain a valid default-route interface")
    } else {
        completion_outcome(&route_completion)
    };

    let (parsed_address, ip_address_probe) = if let Some(interface) = interface.as_ref() {
        let completion = execute_command(
            &factory,
            AllowlistedProbeCommand::IpAddress(interface.clone()),
            budgets.ip,
            overall_deadline,
            &cancellation,
        )
        .await;
        reject_unreaped_child(&completion, "IP address", &admission)?;
        if cancellation.is_cancelled() || matches!(completion, CommandCompletion::Cancelled { .. })
        {
            return Err(NetworkProbeError::Cancelled);
        }
        let parsed = stdout_text(&completion).and_then(|stdout| parse_ipv4_address(&stdout));
        let outcome = if matches!(completion, CommandCompletion::Output(ref output) if output.success)
            && parsed.is_none()
        {
            invalid_output("ip address output did not contain a valid IPv4 `inet` field")
        } else {
            completion_outcome(&completion)
        };
        (parsed, outcome)
    } else {
        (
            None,
            ProbeStageOutcome::new(
                ProbeStatus::Skipped,
                Some(
                    "IP address probe skipped because no valid default-route interface was parsed"
                        .to_string(),
                ),
            ),
        )
    };
    let ip_cidr = parsed_address.as_ref().map(|address| address.cidr.clone());
    let ip_address = parsed_address.map(|address| address.address);

    let (gateway_reachable, gateway_probe) = match gateway_ip {
        Some(gateway_ip) => {
            let completion = execute_command(
                &factory,
                AllowlistedProbeCommand::PingGateway(gateway_ip),
                budgets.ping,
                overall_deadline,
                &cancellation,
            )
            .await;
            reject_unreaped_child(&completion, "gateway reachability", &admission)?;
            if cancellation.is_cancelled()
                || matches!(completion, CommandCompletion::Cancelled { .. })
            {
                return Err(NetworkProbeError::Cancelled);
            }
            let reachable = match &completion {
                CommandCompletion::Output(output) => Some(output.success),
                _ => None,
            };
            (reachable, completion_outcome(&completion))
        }
        None => (
            None,
            ProbeStageOutcome::new(
                ProbeStatus::Skipped,
                Some("gateway probe skipped because no valid gateway was parsed".to_string()),
            ),
        ),
    };

    let dns_host = input
        .dns_host
        .as_deref()
        .map(str::trim)
        .filter(|host| valid_dns_host(host))
        .map(str::to_string);
    let (dns_ok, dns_probe) = if let Some(dns_host) = dns_host {
        let completion = execute_command(
            &factory,
            AllowlistedProbeCommand::DnsLookup(dns_host),
            budgets.dns,
            overall_deadline,
            &cancellation,
        )
        .await;
        reject_unreaped_child(&completion, "DNS lookup", &admission)?;
        if cancellation.is_cancelled() || matches!(completion, CommandCompletion::Cancelled { .. })
        {
            return Err(NetworkProbeError::Cancelled);
        }
        let resolved = match &completion {
            CommandCompletion::Output(output) => Some(output.success),
            _ => None,
        };
        (resolved, completion_outcome(&completion))
    } else {
        (
            None,
            ProbeStageOutcome::new(
                ProbeStatus::Skipped,
                Some("DNS probe skipped because the sanitized host is invalid".to_string()),
            ),
        )
    };

    Ok(NetworkProbeSnapshot {
        interface,
        ip_cidr,
        ip_address,
        gateway,
        gateway_reachable,
        dns_ok,
        ip_address_probe,
        route_probe,
        gateway_probe,
        dns_probe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::sync::Notify;

    #[derive(Clone)]
    enum FakeBehavior {
        Complete {
            success: bool,
            stdout: &'static [u8],
            stderr: &'static [u8],
        },
        CompleteAndCancel {
            stdout: &'static [u8],
            cancellation: CancellationToken,
        },
        Pending,
        PendingUnreaped,
        Oversized,
    }

    struct FakeState {
        behaviors: Mutex<VecDeque<FakeBehavior>>,
        commands: Mutex<Vec<AllowlistedProbeCommand>>,
        spawned: AtomicUsize,
        killed: AtomicUsize,
        reaped: AtomicUsize,
        started: Notify,
    }

    impl FakeState {
        fn new(behaviors: impl IntoIterator<Item = FakeBehavior>) -> Arc<Self> {
            Arc::new(Self {
                behaviors: Mutex::new(behaviors.into_iter().collect()),
                commands: Mutex::new(Vec::new()),
                spawned: AtomicUsize::new(0),
                killed: AtomicUsize::new(0),
                reaped: AtomicUsize::new(0),
                started: Notify::new(),
            })
        }
    }

    struct FakeFactory {
        state: Arc<FakeState>,
    }

    impl ProbeProcessFactory for FakeFactory {
        fn spawn(
            &self,
            command: &AllowlistedProbeCommand,
        ) -> Result<Box<dyn ProbeProcess>, String> {
            let behavior = self
                .state
                .behaviors
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "no fake behavior remains".to_string())?;
            self.state.commands.lock().unwrap().push(command.clone());
            self.state.spawned.fetch_add(1, Ordering::SeqCst);
            self.state.started.notify_one();
            Ok(Box::new(FakeProcess {
                state: Arc::clone(&self.state),
                behavior,
            }))
        }
    }

    struct FakeProcess {
        state: Arc<FakeState>,
        behavior: FakeBehavior,
    }

    #[async_trait]
    impl ProbeProcess for FakeProcess {
        async fn wait_bounded(
            &mut self,
            _output_limit: usize,
        ) -> Result<CapturedProcessOutput, ProcessWaitError> {
            match self.behavior.clone() {
                FakeBehavior::Complete {
                    success,
                    stdout,
                    stderr,
                } => Ok(CapturedProcessOutput {
                    success,
                    stdout: stdout.to_vec(),
                    stderr: stderr.to_vec(),
                }),
                FakeBehavior::CompleteAndCancel {
                    stdout,
                    cancellation,
                } => {
                    cancellation.cancel();
                    Ok(CapturedProcessOutput {
                        success: true,
                        stdout: stdout.to_vec(),
                        stderr: Vec::new(),
                    })
                }
                FakeBehavior::Pending | FakeBehavior::PendingUnreaped => {
                    std::future::pending().await
                }
                FakeBehavior::Oversized => Err(ProcessWaitError::OutputTooLarge),
            }
        }

        async fn kill_and_reap(&mut self) -> bool {
            self.state.killed.fetch_add(1, Ordering::SeqCst);
            let reaped = !matches!(&self.behavior, FakeBehavior::PendingUnreaped);
            if reaped {
                self.state.reaped.fetch_add(1, Ordering::SeqCst);
            }
            reaped
        }
    }

    fn owner(state: Arc<FakeState>, budgets: ProbeBudgets) -> Arc<NetworkProbeOwner> {
        Arc::new(NetworkProbeOwner {
            admission: Arc::new(Semaphore::new(1)),
            factory: Arc::new(FakeFactory { state }),
            budgets,
        })
    }

    fn fast_budgets() -> ProbeBudgets {
        ProbeBudgets {
            ip: Duration::from_millis(20),
            ping: Duration::from_millis(20),
            dns: Duration::from_millis(20),
            overall: Duration::from_millis(60),
        }
    }

    fn controlled_budgets() -> ProbeBudgets {
        ProbeBudgets {
            ip: Duration::from_secs(3_600),
            ping: Duration::from_secs(3_600),
            dns: Duration::from_secs(3_600),
            overall: Duration::from_secs(3_600),
        }
    }

    fn input() -> NetworkProbeInput {
        NetworkProbeInput {
            dns_host: Some("pool.example.com".to_string()),
        }
    }

    #[tokio::test]
    async fn busy_admission_is_zero_queue_and_launches_no_second_probe() {
        let state = FakeState::new([FakeBehavior::Pending]);
        let owner = owner(Arc::clone(&state), controlled_budgets());
        let first = tokio::spawn({
            let owner = Arc::clone(&owner);
            async move { owner.run(input()).await }
        });
        state.started.notified().await;

        assert_eq!(owner.run(input()).await, Err(NetworkProbeError::Busy));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 1);
        first.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while owner.admission.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn stage_timeout_kills_reaps_and_returns_the_permit() {
        let state = FakeState::new([
            FakeBehavior::Pending,
            FakeBehavior::Complete {
                success: true,
                stdout: b"Address: 192.0.2.2\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());
        let result = owner.run(input()).await.unwrap();

        assert_eq!(result.route_probe.status, ProbeStatus::Timeout);
        assert_eq!(state.killed.load(Ordering::SeqCst), 1);
        assert_eq!(state.reaped.load(Ordering::SeqCst), 1);
        assert_eq!(owner.admission.available_permits(), 1);
    }

    #[tokio::test]
    async fn caller_cancellation_kills_reaps_before_owner_releases_permit() {
        let state = FakeState::new([FakeBehavior::Pending]);
        let owner = owner(Arc::clone(&state), controlled_budgets());
        let task = tokio::spawn({
            let owner = Arc::clone(&owner);
            async move { owner.run(input()).await }
        });
        state.started.notified().await;
        task.abort();

        tokio::time::timeout(Duration::from_secs(1), async {
            while owner.admission.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.killed.load(Ordering::SeqCst), 1);
        assert_eq!(state.reaped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn expired_overall_budget_prevents_every_child_spawn() {
        let state = FakeState::new([]);
        let budgets = ProbeBudgets {
            ip: Duration::from_millis(50),
            ping: Duration::from_millis(50),
            dns: Duration::from_millis(50),
            overall: Duration::ZERO,
        };
        let owner = owner(Arc::clone(&state), budgets);
        let result = owner.run(input()).await.unwrap();

        assert_eq!(result.route_probe.status, ProbeStatus::Timeout);
        assert!(matches!(result.gateway_probe.status, ProbeStatus::Skipped));
        assert_eq!(result.dns_probe.status, ProbeStatus::Timeout);
        assert_eq!(state.spawned.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pre_cancelled_stage_never_spawns_a_child() {
        let state = FakeState::new([FakeBehavior::Pending]);
        let factory: Arc<dyn ProbeProcessFactory> = Arc::new(FakeFactory {
            state: Arc::clone(&state),
        });
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let completion = execute_command(
            &factory,
            AllowlistedProbeCommand::IpRoute,
            Duration::from_secs(1),
            tokio::time::Instant::now() + Duration::from_secs(1),
            &cancellation,
        )
        .await;

        assert!(matches!(
            completion,
            CommandCompletion::Cancelled { reaped: true }
        ));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_between_stages_prevents_the_next_spawn() {
        let cancellation = CancellationToken::new();
        let state = FakeState::new([FakeBehavior::CompleteAndCancel {
            stdout: b"default via 192.0.2.1 dev enp1s0\n",
            cancellation: cancellation.clone(),
        }]);
        let factory: Arc<dyn ProbeProcessFactory> = Arc::new(FakeFactory {
            state: Arc::clone(&state),
        });
        let admission = Arc::new(Semaphore::new(1));

        let result = execute_network_probe(
            factory,
            controlled_budgets(),
            input(),
            cancellation,
            admission,
        )
        .await;

        assert_eq!(result, Err(NetworkProbeError::Cancelled));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unreaped_child_stops_sequence_before_another_spawn() {
        let state = FakeState::new([
            FakeBehavior::PendingUnreaped,
            FakeBehavior::Complete {
                success: true,
                stdout: b"default via 192.0.2.1 dev eth0\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());

        let result = owner.run(input()).await;

        assert!(matches!(
            result,
            Err(NetworkProbeError::Worker(ref message)) if message.contains("could not be reaped")
        ));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 1);
        assert_eq!(state.killed.load(Ordering::SeqCst), 1);
        assert_eq!(state.reaped.load(Ordering::SeqCst), 0);
        assert!(owner.admission.is_closed());
        assert!(matches!(
            owner.run(input()).await,
            Err(NetworkProbeError::Worker(ref message)) if message.contains("admission is poisoned")
        ));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oversized_output_is_killed_reaped_and_typed() {
        let state = FakeState::new([
            FakeBehavior::Oversized,
            FakeBehavior::Complete {
                success: true,
                stdout: b"default via 192.0.2.1 dev eth0\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"Address: 192.0.2.2\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());
        let result = owner.run(input()).await.unwrap();

        assert_eq!(result.route_probe.status, ProbeStatus::OutputTooLarge);
        assert_eq!(state.killed.load(Ordering::SeqCst), 1);
        assert_eq!(state.reaped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn malformed_ip_and_route_without_gateway_skip_ping_without_host_assumptions() {
        let state = FakeState::new([
            FakeBehavior::Complete {
                success: true,
                stdout: b"default dev enp1s0\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"malformed address output\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"Address: 192.0.2.2\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());
        let result = owner.run(input()).await.unwrap();

        assert_eq!(result.ip_address_probe.status, ProbeStatus::InvalidOutput);
        assert_eq!(result.route_probe.status, ProbeStatus::Ok);
        assert_eq!(result.gateway_probe.status, ProbeStatus::Skipped);
        assert_eq!(state.spawned.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn invalid_dns_host_is_skipped_without_reaching_the_process_factory() {
        let state = FakeState::new([
            FakeBehavior::Complete {
                success: true,
                stdout: b"default dev enp1s0\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"2: enp1s0 inet 192.0.2.10/24\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());

        let result = owner
            .run(NetworkProbeInput {
                dns_host: Some("pool.example.com; touch /tmp/probe-owned".to_string()),
            })
            .await
            .unwrap();

        assert_eq!(result.dns_probe.status, ProbeStatus::Skipped);
        assert_eq!(state.spawned.load(Ordering::SeqCst), 2);
        assert_eq!(
            *state.commands.lock().unwrap(),
            vec![
                AllowlistedProbeCommand::IpRoute,
                AllowlistedProbeCommand::IpAddress("enp1s0".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn missing_dns_host_is_skipped_without_a_fallback_probe() {
        let state = FakeState::new([
            FakeBehavior::Complete {
                success: true,
                stdout: b"default dev enp1s0\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"2: enp1s0 inet 192.0.2.10/24\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());

        let result = owner
            .run(NetworkProbeInput { dns_host: None })
            .await
            .unwrap();

        assert_eq!(result.dns_probe.status, ProbeStatus::Skipped);
        assert_eq!(result.interface.as_deref(), Some("enp1s0"));
        assert_eq!(result.ip_cidr.as_deref(), Some("192.0.2.10/24"));
        assert_eq!(result.ip_address.as_deref(), Some("192.0.2.10"));
        assert_eq!(state.spawned.load(Ordering::SeqCst), 2);
        assert_eq!(
            *state.commands.lock().unwrap(),
            vec![
                AllowlistedProbeCommand::IpRoute,
                AllowlistedProbeCommand::IpAddress("enp1s0".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_route_interface_cannot_become_an_argv_or_sysfs_component() {
        let state = FakeState::new([
            FakeBehavior::Complete {
                success: true,
                stdout: b"default via 192.0.2.1 dev ../../tmp\n",
                stderr: b"",
            },
            FakeBehavior::Complete {
                success: true,
                stdout: b"Address: 192.0.2.2\n",
                stderr: b"",
            },
        ]);
        let owner = owner(Arc::clone(&state), fast_budgets());

        let result = owner.run(input()).await.unwrap();

        assert_eq!(result.route_probe.status, ProbeStatus::InvalidOutput);
        assert_eq!(result.ip_address_probe.status, ProbeStatus::Skipped);
        assert_eq!(result.gateway_probe.status, ProbeStatus::Skipped);
        assert_eq!(state.spawned.load(Ordering::SeqCst), 2);
        assert_eq!(
            *state.commands.lock().unwrap(),
            vec![
                AllowlistedProbeCommand::IpRoute,
                AllowlistedProbeCommand::DnsLookup("pool.example.com".to_string()),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_probe_does_not_starve_tokio_executor() {
        let state = FakeState::new([FakeBehavior::Pending]);
        let owner = owner(Arc::clone(&state), fast_budgets());
        let heartbeat = Arc::new(AtomicBool::new(false));
        let heartbeat_task = tokio::spawn({
            let heartbeat = Arc::clone(&heartbeat);
            async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                heartbeat.store(true, Ordering::SeqCst);
            }
        });

        let _ = owner.run(input()).await;
        heartbeat_task.await.unwrap();
        assert!(heartbeat.load(Ordering::SeqCst));
    }
}
