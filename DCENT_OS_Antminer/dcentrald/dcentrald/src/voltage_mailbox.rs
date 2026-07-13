//! Bounded, safety-prioritized mailbox for runtime voltage-controller commands.
//!
//! Ordinary voltage mutations use a bounded FIFO. Safe-off commands use a
//! separate endpoint-keyed lane, so an autotuner backlog cannot prevent a
//! thermal or terminal power cut from being admitted. Each disable advances
//! the endpoint lifecycle generation and supersedes older queued ordinary
//! commands before the worker can observe them.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::work_dispatcher::{VoltageCommand, VoltageCommandReply};

pub const DEFAULT_NORMAL_CAPACITY: usize = 64;
pub const DEFAULT_DISABLE_ENDPOINT_CAPACITY: usize = 64;
pub const DEFAULT_DISABLE_WAITER_CAPACITY: usize = 16;

type CommandResult = Result<VoltageCommandReply, String>;
type ReplySender = oneshot::Sender<CommandResult>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageMailboxCapacity {
    NormalCommands,
    DisableEndpoints,
    DisableWaiters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageTrySendError {
    Full(VoltageMailboxCapacity),
    Disconnected,
    TerminalLatched,
    Superseded { generation: u128 },
}

impl fmt::Display for VoltageTrySendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(VoltageMailboxCapacity::NormalCommands) => {
                f.write_str("ordinary voltage command capacity is full")
            }
            Self::Full(VoltageMailboxCapacity::DisableEndpoints) => {
                f.write_str("reserved disable endpoint capacity is full")
            }
            Self::Full(VoltageMailboxCapacity::DisableWaiters) => {
                f.write_str("coalesced disable waiter capacity is full")
            }
            Self::Disconnected => f.write_str("runtime voltage worker is disconnected"),
            Self::TerminalLatched => {
                f.write_str("terminal safe-off is latched; energizing command rejected")
            }
            Self::Superseded { generation } => write!(
                f,
                "endpoint disable generation {generation} is pending; ordinary command superseded"
            ),
        }
    }
}

impl std::error::Error for VoltageTrySendError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoltageTryRecvError {
    Empty,
    Disconnected,
}

#[derive(Debug, Clone, Copy)]
pub struct VoltageMailboxConfig {
    pub normal_capacity: usize,
    pub disable_endpoint_capacity: usize,
    pub disable_waiter_capacity: usize,
}

impl Default for VoltageMailboxConfig {
    fn default() -> Self {
        Self {
            normal_capacity: DEFAULT_NORMAL_CAPACITY,
            disable_endpoint_capacity: DEFAULT_DISABLE_ENDPOINT_CAPACITY,
            disable_waiter_capacity: DEFAULT_DISABLE_WAITER_CAPACITY,
        }
    }
}

#[derive(Debug)]
struct QueuedOrdinary {
    command: VoltageCommand,
    endpoint: EndpointKey,
    generation: u128,
}

/// The bus is implicit because one mailbox is owned by one runtime I2C worker.
/// Chip identity remains part of the key because it selects the controller
/// protocol; equal addresses under different profiles must never coalesce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EndpointKey {
    chip_id: u16,
    pic_addr: u8,
}

#[derive(Debug)]
struct PendingDisable {
    command: Option<VoltageCommand>,
    waiters: Vec<ReplySender>,
    in_flight: bool,
}

#[derive(Debug)]
struct MailboxState {
    normal: VecDeque<QueuedOrdinary>,
    disable_order: VecDeque<EndpointKey>,
    disables: HashMap<EndpointKey, PendingDisable>,
    generation: u128,
    terminal_latched: bool,
    receiver_open: bool,
    sender_count: usize,
}

#[derive(Debug)]
struct Shared {
    config: VoltageMailboxConfig,
    state: Mutex<MailboxState>,
}

pub struct VoltageCommandSender {
    shared: Arc<Shared>,
}

pub struct VoltageCommandReceiver {
    shared: Arc<Shared>,
}

pub struct VoltageCommandDelivery {
    command: VoltageCommand,
    completion: VoltageCommandCompletion,
}

pub struct VoltageCommandCompletion {
    target: Option<CompletionTarget>,
}

enum CompletionTarget {
    Direct(Vec<ReplySender>),
    Disable {
        shared: Arc<Shared>,
        endpoint: EndpointKey,
    },
}

pub fn voltage_command_mailbox() -> (VoltageCommandSender, VoltageCommandReceiver) {
    voltage_command_mailbox_with_config(VoltageMailboxConfig::default())
}

fn voltage_command_mailbox_with_config(
    config: VoltageMailboxConfig,
) -> (VoltageCommandSender, VoltageCommandReceiver) {
    assert!(config.normal_capacity > 0);
    assert!(config.disable_endpoint_capacity > 0);
    assert!(config.disable_waiter_capacity > 0);

    let shared = Arc::new(Shared {
        config,
        state: Mutex::new(MailboxState {
            normal: VecDeque::with_capacity(config.normal_capacity),
            disable_order: VecDeque::with_capacity(config.disable_endpoint_capacity),
            disables: HashMap::with_capacity(config.disable_endpoint_capacity),
            generation: 0,
            terminal_latched: false,
            receiver_open: true,
            sender_count: 1,
        }),
    });
    (
        VoltageCommandSender {
            shared: shared.clone(),
        },
        VoltageCommandReceiver { shared },
    )
}

fn lock_state(shared: &Shared) -> std::sync::MutexGuard<'_, MailboxState> {
    shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn endpoint(command: &VoltageCommand) -> EndpointKey {
    match command {
        VoltageCommand::SetVoltage {
            chip_id, pic_addr, ..
        }
        | VoltageCommand::DisableVoltage {
            chip_id, pic_addr, ..
        }
        | VoltageCommand::VerifyVoltage {
            chip_id, pic_addr, ..
        } => EndpointKey {
            chip_id: *chip_id,
            pic_addr: *pic_addr,
        },
    }
}

fn take_reply(command: &mut VoltageCommand) -> Option<ReplySender> {
    match command {
        VoltageCommand::SetVoltage { reply_tx, .. }
        | VoltageCommand::DisableVoltage { reply_tx, .. }
        | VoltageCommand::VerifyVoltage { reply_tx, .. } => reply_tx.take(),
    }
}

fn reject_command(command: &mut VoltageCommand, detail: String) {
    if let Some(reply) = take_reply(command) {
        let _ = reply.send(Err(detail));
    }
}

fn broadcast(waiters: Vec<ReplySender>, result: CommandResult) {
    for waiter in waiters {
        let _ = waiter.send(result.clone());
    }
}

impl Clone for VoltageCommandSender {
    fn clone(&self) -> Self {
        let mut state = lock_state(&self.shared);
        state.sender_count = state.sender_count.saturating_add(1);
        drop(state);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for VoltageCommandSender {
    fn drop(&mut self) {
        let mut state = lock_state(&self.shared);
        state.sender_count = state.sender_count.saturating_sub(1);
    }
}

impl VoltageCommandSender {
    /// Captures the current lifecycle generation for worker-owned ordinary
    /// work that cannot be represented as a queued `VoltageCommand` yet.
    pub fn capture_generation(
        &self,
        chip_id: u16,
        pic_addr: u8,
    ) -> Result<u128, VoltageTrySendError> {
        let state = lock_state(&self.shared);
        if !state.receiver_open {
            return Err(VoltageTrySendError::Disconnected);
        }
        if state.terminal_latched {
            return Err(VoltageTrySendError::TerminalLatched);
        }
        if state
            .disables
            .contains_key(&EndpointKey { chip_id, pic_addr })
        {
            return Err(VoltageTrySendError::Superseded {
                generation: state.generation,
            });
        }
        Ok(state.generation)
    }

    pub fn try_send(&self, mut command: VoltageCommand) -> Result<(), VoltageTrySendError> {
        let command_endpoint = endpoint(&command);
        let is_disable = matches!(command, VoltageCommand::DisableVoltage { .. });
        let mut state = lock_state(&self.shared);

        if !state.receiver_open {
            drop(state);
            reject_command(&mut command, VoltageTrySendError::Disconnected.to_string());
            return Err(VoltageTrySendError::Disconnected);
        }

        if is_disable {
            let has_waiter = match &command {
                VoltageCommand::DisableVoltage { reply_tx, .. } => reply_tx.is_some(),
                _ => false,
            };
            if let Some(pending) = state.disables.get_mut(&command_endpoint) {
                // Timed-out callers drop their receivers. Reclaim those bounded
                // slots before deciding whether a later safety observer can join
                // the already-admitted disable execution.
                pending.waiters.retain(|waiter| !waiter.is_closed());
                if has_waiter && pending.waiters.len() >= self.shared.config.disable_waiter_capacity
                {
                    drop(state);
                    let error = VoltageTrySendError::Full(VoltageMailboxCapacity::DisableWaiters);
                    reject_command(&mut command, error.to_string());
                    return Err(error);
                }
            } else if state.disables.len() >= self.shared.config.disable_endpoint_capacity {
                drop(state);
                let error = VoltageTrySendError::Full(VoltageMailboxCapacity::DisableEndpoints);
                reject_command(&mut command, error.to_string());
                return Err(error);
            }

            let generation = state.generation.saturating_add(1);
            state.generation = generation;

            let mut retained = VecDeque::with_capacity(state.normal.len());
            while let Some(mut queued) = state.normal.pop_front() {
                if queued.generation < generation {
                    reject_command(
                        &mut queued.command,
                        format!(
                            "voltage command superseded by DisableVoltage generation {generation}"
                        ),
                    );
                } else {
                    retained.push_back(queued);
                }
            }
            state.normal = retained;

            let waiter = take_reply(&mut command);
            if let Some(pending) = state.disables.get_mut(&command_endpoint) {
                if let Some(waiter) = waiter {
                    pending.waiters.push(waiter);
                }
                // Preserve the command already executing. Before execution begins,
                // use the newest metadata while still executing only once.
                if !pending.in_flight {
                    pending.command = Some(command);
                }
            } else {
                state.disable_order.push_back(command_endpoint);
                state.disables.insert(
                    command_endpoint,
                    PendingDisable {
                        command: Some(command),
                        waiters: waiter.into_iter().collect(),
                        in_flight: false,
                    },
                );
            }
            return Ok(());
        }

        if state.terminal_latched {
            drop(state);
            reject_command(
                &mut command,
                VoltageTrySendError::TerminalLatched.to_string(),
            );
            return Err(VoltageTrySendError::TerminalLatched);
        }

        let generation = state.generation;
        if state.disables.contains_key(&command_endpoint) {
            drop(state);
            let error = VoltageTrySendError::Superseded { generation };
            reject_command(&mut command, error.to_string());
            return Err(error);
        }
        if state.normal.len() >= self.shared.config.normal_capacity {
            drop(state);
            let error = VoltageTrySendError::Full(VoltageMailboxCapacity::NormalCommands);
            reject_command(&mut command, error.to_string());
            return Err(error);
        }
        state.normal.push_back(QueuedOrdinary {
            command,
            endpoint: command_endpoint,
            generation,
        });
        Ok(())
    }

    /// Irreversibly rejects future Set/Verify commands and resolves all queued
    /// ordinary waiters as superseded. Disable admission remains available.
    pub fn latch_terminal(&self) -> bool {
        let mut state = lock_state(&self.shared);
        let transitioned = !state.terminal_latched;
        state.terminal_latched = true;
        while let Some(mut queued) = state.normal.pop_front() {
            reject_command(
                &mut queued.command,
                VoltageTrySendError::TerminalLatched.to_string(),
            );
        }
        transitioned
    }

    pub fn is_terminal_latched(&self) -> bool {
        lock_state(&self.shared).terminal_latched
    }
}

impl VoltageCommandReceiver {
    pub fn is_terminal_latched(&self) -> bool {
        lock_state(&self.shared).terminal_latched
    }

    /// Returns true only while a direct worker-owned ordinary operation still
    /// belongs to the endpoint lifecycle generation in which it was created.
    /// This fences startup-deferred voltage targets that predate a prioritized
    /// disable even though they were never enqueued in the ordinary FIFO.
    pub fn permits_ordinary_generation(
        &self,
        chip_id: u16,
        pic_addr: u8,
        generation: u128,
    ) -> bool {
        let state = lock_state(&self.shared);
        let endpoint = EndpointKey { chip_id, pic_addr };
        state.receiver_open
            && !state.terminal_latched
            && !state.disables.contains_key(&endpoint)
            && state.generation == generation
    }

    pub fn try_recv(&self) -> Result<VoltageCommandDelivery, VoltageTryRecvError> {
        let mut state = lock_state(&self.shared);

        while let Some(command_endpoint) = state.disable_order.pop_front() {
            if let Some(pending) = state.disables.get_mut(&command_endpoint) {
                if pending.in_flight {
                    continue;
                }
                let Some(command) = pending.command.take() else {
                    continue;
                };
                pending.in_flight = true;
                return Ok(VoltageCommandDelivery {
                    command,
                    completion: VoltageCommandCompletion {
                        target: Some(CompletionTarget::Disable {
                            shared: self.shared.clone(),
                            endpoint: command_endpoint,
                        }),
                    },
                });
            }
        }

        while let Some(mut queued) = state.normal.pop_front() {
            let generation = state.generation;
            if queued.generation != generation || state.disables.contains_key(&queued.endpoint) {
                reject_command(
                    &mut queued.command,
                    format!(
                        "voltage command superseded by endpoint lifecycle generation {generation}"
                    ),
                );
                continue;
            }
            let waiters = take_reply(&mut queued.command).into_iter().collect();
            return Ok(VoltageCommandDelivery {
                command: queued.command,
                completion: VoltageCommandCompletion {
                    target: Some(CompletionTarget::Direct(waiters)),
                },
            });
        }

        if state.sender_count == 0 {
            Err(VoltageTryRecvError::Disconnected)
        } else {
            Err(VoltageTryRecvError::Empty)
        }
    }
}

impl Drop for VoltageCommandReceiver {
    fn drop(&mut self) {
        let mut waiters = Vec::new();
        {
            let mut state = lock_state(&self.shared);
            state.receiver_open = false;
            while let Some(mut queued) = state.normal.pop_front() {
                if let Some(waiter) = take_reply(&mut queued.command) {
                    waiters.push(waiter);
                }
            }
            for (_, mut pending) in state.disables.drain() {
                waiters.append(&mut pending.waiters);
            }
            state.disable_order.clear();
        }
        broadcast(
            waiters,
            Err("runtime voltage receiver closed before command execution".to_string()),
        );
    }
}

impl VoltageCommandDelivery {
    pub fn into_parts(self) -> (VoltageCommand, VoltageCommandCompletion) {
        (self.command, self.completion)
    }
}

impl VoltageCommandCompletion {
    pub fn complete(mut self, result: CommandResult) {
        if let Some(target) = self.target.take() {
            match target {
                CompletionTarget::Direct(waiters) => broadcast(waiters, result),
                CompletionTarget::Disable { shared, endpoint } => {
                    let waiters = {
                        let mut state = lock_state(&shared);
                        state
                            .disables
                            .remove(&endpoint)
                            .map(|pending| pending.waiters)
                            .unwrap_or_default()
                    };
                    broadcast(waiters, result);
                }
            }
        }
    }
}

impl Drop for VoltageCommandCompletion {
    fn drop(&mut self) {
        let Some(target) = self.target.take() else {
            return;
        };
        let result = Err("runtime voltage worker abandoned command before completion".to_string());
        match target {
            CompletionTarget::Direct(waiters) => broadcast(waiters, result),
            CompletionTarget::Disable { shared, endpoint } => {
                let waiters = {
                    let mut state = lock_state(&shared);
                    state
                        .disables
                        .remove(&endpoint)
                        .map(|pending| pending.waiters)
                        .unwrap_or_default()
                };
                broadcast(waiters, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(addr: u8) -> (VoltageCommand, oneshot::Receiver<CommandResult>) {
        let (tx, rx) = oneshot::channel();
        (
            VoltageCommand::SetVoltage {
                chain_id: None,
                chip_id: 0x1397,
                pic_addr: addr,
                target_mv: 9000,
                reply_tx: Some(tx),
            },
            rx,
        )
    }

    fn verify(addr: u8) -> (VoltageCommand, oneshot::Receiver<CommandResult>) {
        let (tx, rx) = oneshot::channel();
        (
            VoltageCommand::VerifyVoltage {
                chain_id: None,
                chip_id: 0x1397,
                pic_addr: addr,
                target_mv: 9000,
                reply_tx: Some(tx),
            },
            rx,
        )
    }

    fn disable(addr: u8) -> (VoltageCommand, oneshot::Receiver<CommandResult>) {
        disable_for_chip(0x1397, addr)
    }

    fn disable_for_chip(
        chip_id: u16,
        addr: u8,
    ) -> (VoltageCommand, oneshot::Receiver<CommandResult>) {
        let (tx, rx) = oneshot::channel();
        (
            VoltageCommand::DisableVoltage {
                chain_id: None,
                chip_id,
                pic_addr: addr,
                reply_tx: Some(tx),
            },
            rx,
        )
    }

    fn test_mailbox(
        normal: usize,
        endpoints: usize,
        waiters: usize,
    ) -> (VoltageCommandSender, VoltageCommandReceiver) {
        voltage_command_mailbox_with_config(VoltageMailboxConfig {
            normal_capacity: normal,
            disable_endpoint_capacity: endpoints,
            disable_waiter_capacity: waiters,
        })
    }

    #[tokio::test]
    async fn disable_lane_is_admitted_and_delivered_before_full_normal_fifo() {
        let (tx, rx) = test_mailbox(1, 2, 2);
        let (ordinary, ordinary_rx) = set(0x20);
        tx.try_send(ordinary).unwrap();
        let (full, full_rx) = set(0x21);
        assert_eq!(
            tx.try_send(full),
            Err(VoltageTrySendError::Full(
                VoltageMailboxCapacity::NormalCommands
            ))
        );
        assert!(full_rx.await.unwrap().is_err());

        let (off, off_rx) = disable(0x22);
        tx.try_send(off).unwrap();
        let (command, completion) = rx.try_recv().unwrap().into_parts();
        assert!(matches!(
            command,
            VoltageCommand::DisableVoltage { pic_addr: 0x22, .. }
        ));
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(matches!(
            off_rx.await.unwrap(),
            Ok(VoltageCommandReply::Disabled)
        ));
        assert!(ordinary_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("superseded by DisableVoltage"));
        assert_eq!(rx.try_recv().err(), Some(VoltageTryRecvError::Empty));
    }

    #[tokio::test]
    async fn duplicate_disables_coalesce_by_endpoint_and_broadcast_one_result() {
        let (tx, rx) = test_mailbox(2, 2, 3);
        let (first, first_rx) = disable(0x20);
        let (second, second_rx) = disable(0x20);
        tx.try_send(first).unwrap();
        tx.try_send(second).unwrap();

        let (command, completion) = rx.try_recv().unwrap().into_parts();
        assert!(matches!(
            command,
            VoltageCommand::DisableVoltage { pic_addr: 0x20, .. }
        ));
        assert_eq!(rx.try_recv().err(), Some(VoltageTryRecvError::Empty));
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(matches!(
            first_rx.await.unwrap(),
            Ok(VoltageCommandReply::Disabled)
        ));
        assert!(matches!(
            second_rx.await.unwrap(),
            Ok(VoltageCommandReply::Disabled)
        ));
    }

    #[tokio::test]
    async fn equal_address_with_different_protocol_identity_never_coalesces() {
        let (tx, rx) = test_mailbox(2, 2, 2);
        let (bm1397, bm1397_rx) = disable_for_chip(0x1397, 0x20);
        let (bm1362, bm1362_rx) = disable_for_chip(0x1362, 0x20);
        tx.try_send(bm1397).unwrap();
        tx.try_send(bm1362).unwrap();

        let (first, first_completion) = rx.try_recv().unwrap().into_parts();
        let (second, second_completion) = rx.try_recv().unwrap().into_parts();
        let first_chip = match first {
            VoltageCommand::DisableVoltage { chip_id, .. } => chip_id,
            _ => unreachable!(),
        };
        let second_chip = match second {
            VoltageCommand::DisableVoltage { chip_id, .. } => chip_id,
            _ => unreachable!(),
        };
        assert_ne!(first_chip, second_chip);
        first_completion.complete(Err("first protocol failed".to_string()));
        second_completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(bm1397_rx.await.unwrap().is_err());
        assert!(bm1362_rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn disable_supersedes_older_commands_and_blocks_new_ones_until_completion() {
        let (tx, rx) = test_mailbox(4, 2, 2);
        assert!(rx.permits_ordinary_generation(0x1397, 0x20, 0));
        let (old_set, old_set_rx) = set(0x20);
        let (old_verify, old_verify_rx) = verify(0x20);
        tx.try_send(old_set).unwrap();
        tx.try_send(old_verify).unwrap();
        let (off, off_rx) = disable(0x20);
        tx.try_send(off).unwrap();
        assert!(old_set_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("superseded"));
        assert!(old_verify_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("superseded"));

        let (racing_set, racing_rx) = set(0x20);
        assert!(matches!(
            tx.try_send(racing_set),
            Err(VoltageTrySendError::Superseded { .. })
        ));
        assert!(racing_rx.await.unwrap().unwrap_err().contains("pending"));

        let (_, completion) = rx.try_recv().unwrap().into_parts();
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(off_rx.await.unwrap().is_ok());
        assert!(!rx.permits_ordinary_generation(0x1397, 0x20, 0));
        let (later_set, _) = set(0x20);
        tx.try_send(later_set).unwrap();
    }

    #[tokio::test]
    async fn terminal_latch_is_irreversible_but_disable_remains_admissible() {
        let (tx, rx) = test_mailbox(2, 2, 2);
        assert!(tx.latch_terminal());
        assert!(!tx.latch_terminal());
        assert!(tx.is_terminal_latched());
        let (ordinary, ordinary_rx) = set(0x20);
        assert_eq!(
            tx.try_send(ordinary),
            Err(VoltageTrySendError::TerminalLatched)
        );
        assert!(ordinary_rx.await.unwrap().unwrap_err().contains("terminal"));
        let (off, off_rx) = disable(0x20);
        tx.try_send(off).unwrap();
        let (_, completion) = rx.try_recv().unwrap().into_parts();
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(off_rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn deferred_generation_capture_allows_new_work_and_supersedes_it_on_next_disable() {
        let (tx, rx) = test_mailbox(2, 2, 2);
        let (first_off, first_off_rx) = disable(0x20);
        tx.try_send(first_off).unwrap();
        let (_, first_completion) = rx.try_recv().unwrap().into_parts();
        first_completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(first_off_rx.await.unwrap().is_ok());

        let captured = tx.capture_generation(0x1397, 0x20).unwrap();
        assert_ne!(captured, 0);
        assert!(rx.permits_ordinary_generation(0x1397, 0x20, captured));

        let (second_off, second_off_rx) = disable(0x20);
        tx.try_send(second_off).unwrap();
        assert!(!rx.permits_ordinary_generation(0x1397, 0x20, captured));
        let (_, second_completion) = rx.try_recv().unwrap().into_parts();
        second_completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(second_off_rx.await.unwrap().is_ok());
        assert!(!rx.permits_ordinary_generation(0x1397, 0x20, captured));
    }

    #[tokio::test]
    async fn capture_during_pending_disable_cannot_create_delayed_reenable_token() {
        let (tx, rx) = test_mailbox(2, 2, 2);
        let (off, off_rx) = disable(0x20);
        tx.try_send(off).unwrap();
        assert!(matches!(
            tx.capture_generation(0x1397, 0x20),
            Err(VoltageTrySendError::Superseded { .. })
        ));

        let (_, completion) = rx.try_recv().unwrap().into_parts();
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(off_rx.await.unwrap().is_ok());
        assert!(tx.capture_generation(0x1397, 0x20).is_ok());
    }

    #[tokio::test]
    async fn waiter_and_endpoint_caps_fail_without_consuming_reserved_work() {
        let (tx, rx) = test_mailbox(1, 1, 1);
        let (first, first_rx) = disable(0x20);
        tx.try_send(first).unwrap();
        let (waiter_full, waiter_full_rx) = disable(0x20);
        assert_eq!(
            tx.try_send(waiter_full),
            Err(VoltageTrySendError::Full(
                VoltageMailboxCapacity::DisableWaiters
            ))
        );
        assert!(waiter_full_rx.await.unwrap().is_err());
        let (endpoint_full, endpoint_full_rx) = disable(0x21);
        assert_eq!(
            tx.try_send(endpoint_full),
            Err(VoltageTrySendError::Full(
                VoltageMailboxCapacity::DisableEndpoints
            ))
        );
        assert!(endpoint_full_rx.await.unwrap().is_err());
        let (_, completion) = rx.try_recv().unwrap().into_parts();
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(first_rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn closed_coalesced_waiters_are_reclaimed_before_capacity_check() {
        let (tx, rx) = test_mailbox(1, 1, 1);
        let (first, first_rx) = disable(0x20);
        tx.try_send(first).unwrap();
        drop(first_rx);

        let (replacement, replacement_rx) = disable(0x20);
        tx.try_send(replacement).unwrap();
        let (_, completion) = rx.try_recv().unwrap().into_parts();
        completion.complete(Ok(VoltageCommandReply::Disabled));
        assert!(replacement_rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn receiver_close_resolves_all_waiters_and_disconnects_producers() {
        let (tx, rx) = test_mailbox(2, 2, 2);
        let (off, off_rx) = disable(0x21);
        tx.try_send(off).unwrap();
        // Admit ordinary work after the reserved disable on a distinct
        // endpoint. Both waiters are now outstanding simultaneously; neither
        // has been superseded by the disable admission generation.
        let (ordinary, ordinary_rx) = set(0x20);
        tx.try_send(ordinary).unwrap();
        drop(rx);
        assert!(ordinary_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("receiver closed"));
        assert!(off_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("receiver closed"));
        let (later, later_rx) = disable(0x22);
        assert_eq!(tx.try_send(later), Err(VoltageTrySendError::Disconnected));
        assert!(later_rx
            .await
            .unwrap()
            .unwrap_err()
            .contains("disconnected"));
    }
}
