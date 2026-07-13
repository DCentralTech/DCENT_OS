//! AM2 controller endpoint discovery with system-owned evidence.
//!
//! This module is intentionally narrower than the generic subtype issuer. An
//! AM2 dsPIC endpoint is issued only after exact image identity, canonical
//! slot/UART topology, positive hashboard EEPROM evidence, and a supported
//! firmware reply from the exact address all agree.

use std::time::{Duration, Instant};

use crate::i2c::{I2cMutationLabel, I2cServiceHandle, I2cTransactionStep};
use crate::platform::VoltageControllerEndpoint;
use crate::{HalError, Result};
use dcentrald_common::am2_topology::{dspic_address_for_slot, slot_for_uart, uart_for_slot};

const BOARD_TARGET_PATH: &str = "/etc/dcentos/board_target";
// Exact marker written by br2_external_dcentos/board/zynq/am2-s19jpro/post-build.sh.
// Historical xil aliases are feature fingerprints, not image identity, and
// deliberately cannot mint controller authority here.
const AM2_CONTROLLER_BOARD_TARGET: &str = "am2-s19j";
const EEPROM_PREAMBLE: [u8; 2] = [0x04, 0x11];
const GET_VERSION_FRAMED: [u8; 6] = [0x55, 0xAA, 0x04, 0x17, 0x00, 0x1B];
const GET_VERSION_SHORT: [u8; 3] = [0x55, 0xAA, 0x17];
const GET_VERSION_RETRIES: u32 = 15;
const GET_VERSION_RETRY_DELAY_MS: u64 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Am2ControllerPlan {
    board_target: String,
    contexts: Vec<Am2ControllerContext>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Am2ControllerContext {
    serial_device: String,
    slot: u8,
    address: u8,
}

impl Am2ControllerContext {
    pub fn serial_device(&self) -> &str {
        &self.serial_device
    }

    pub fn slot(&self) -> u8 {
        self.slot
    }

    pub fn address(&self) -> u8 {
        self.address
    }
}

impl Am2ControllerPlan {
    pub fn board_target(&self) -> &str {
        &self.board_target
    }

    pub fn contexts(&self) -> &[Am2ControllerContext] {
        &self.contexts
    }

    pub fn context_for_slot(&self, slot: u8) -> Option<&Am2ControllerContext> {
        self.contexts.iter().find(|context| context.slot == slot)
    }

    pub fn context_for_address(&self, address: u8) -> Option<&Am2ControllerContext> {
        self.contexts
            .iter()
            .find(|context| context.address == address)
    }
}

#[derive(Debug)]
pub struct Am2HashboardPresence {
    board_target: String,
    serial_device: String,
    slot: u8,
    address: u8,
    eeprom_bytes: Vec<u8>,
}

impl Am2HashboardPresence {
    pub fn slot(&self) -> u8 {
        self.slot
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn eeprom_bytes(&self) -> &[u8] {
        &self.eeprom_bytes
    }
}

fn plan_from_observations(
    board_target: Option<&str>,
    serial_devices: &[String],
) -> Result<Am2ControllerPlan> {
    let board_target = board_target.map(str::trim).unwrap_or_default();
    if board_target != AM2_CONTROLLER_BOARD_TARGET {
        return Err(HalError::Platform(format!(
            "exact AM2 controller board target is not admitted: {}",
            if board_target.is_empty() {
                "<missing>"
            } else {
                board_target
            }
        )));
    }
    if serial_devices.is_empty() {
        return Err(HalError::Platform(
            "AM2 controller plan has no chain UARTs".into(),
        ));
    }

    let mut contexts = Vec::with_capacity(serial_devices.len());
    for serial_device in serial_devices {
        let slot = slot_for_uart(serial_device).ok_or_else(|| {
                HalError::Platform(format!(
                    "AM2 controller UART is outside the canonical /dev/ttyS1..ttyS4 topology: {serial_device}"
                ))
            })?;
        if contexts
            .iter()
            .any(|context: &Am2ControllerContext| context.slot == slot)
        {
            return Err(HalError::Platform(format!(
                "duplicate AM2 controller slot {slot} in chain plan"
            )));
        }
        contexts.push(Am2ControllerContext {
            serial_device: serial_device.clone(),
            slot,
            address: dspic_address_for_slot(slot).ok_or_else(|| {
                HalError::Platform(format!("AM2 controller slot {slot} has no dsPIC address"))
            })?,
        });
    }

    Ok(Am2ControllerPlan {
        board_target: board_target.to_string(),
        contexts,
    })
}

/// Read the exact system board target and bind requested UARTs to the closed
/// AM2 slot/address table. Caller-selected paths cannot choose an address.
pub fn discover_system_am2_controller_plan(serial_devices: &[String]) -> Result<Am2ControllerPlan> {
    #[cfg(target_os = "linux")]
    let board_target = std::fs::read_to_string(BOARD_TARGET_PATH).ok();
    #[cfg(not(target_os = "linux"))]
    let board_target: Option<String> = None;
    plan_from_observations(board_target.as_deref(), serial_devices)
}

fn try_plan_from_observations(
    board_target: Option<&str>,
    serial_devices: &[String],
) -> Result<Option<Am2ControllerPlan>> {
    if board_target.map(str::trim) != Some(AM2_CONTROLLER_BOARD_TARGET) {
        return Ok(None);
    }
    plan_from_observations(board_target, serial_devices).map(Some)
}

/// Try to bind the exact `am2-s19j` production image to its closed AM2
/// topology. Non-target images return `None` so legacy/other-platform routes
/// retain their existing behavior. Once the exact marker is present, invalid
/// UARTs and duplicate slots fail closed through the strict planner.
pub fn try_discover_system_am2_controller_plan(
    serial_devices: &[String],
) -> Result<Option<Am2ControllerPlan>> {
    #[cfg(target_os = "linux")]
    let board_target = std::fs::read_to_string(BOARD_TARGET_PATH).ok();
    #[cfg(not(target_os = "linux"))]
    let board_target: Option<String> = None;
    try_plan_from_observations(board_target.as_deref(), serial_devices)
}

/// Consume the existing pre-energize EEPROM read for one planned AM2 slot.
/// The returned token is opaque and can only be obtained from an actual,
/// nonblank BHB42-family EEPROM read on the plan-bound slot.
pub fn observe_am2_hashboard_presence(
    plan: &Am2ControllerPlan,
    context: &Am2ControllerContext,
    deadline: Instant,
) -> Result<Am2HashboardPresence> {
    if !plan.contexts.iter().any(|candidate| candidate == context) {
        return Err(HalError::Platform(
            "AM2 controller context is not owned by this plan".into(),
        ));
    }
    let eeprom_addr = 0x50u8 + context.slot;
    loop {
        for bus in [1u8, 0] {
            let path = format!("/sys/bus/i2c/devices/{bus}-{eeprom_addr:04x}/eeprom");
            if let Ok(bytes) = std::fs::read(&path) {
                return bind_am2_hashboard_presence(plan, context, bytes);
            }
        }
        if Instant::now() >= deadline {
            return Err(HalError::Platform(format!(
                "AM2 slot {} EEPROM presence was not observed before the deadline",
                context.slot
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Bind bytes returned by the existing bounded EEPROM reader into opaque
/// presence evidence. This lets mining paths reuse their established read
/// transaction instead of issuing a second EEPROM access.
pub fn bind_am2_hashboard_presence(
    plan: &Am2ControllerPlan,
    context: &Am2ControllerContext,
    bytes: Vec<u8>,
) -> Result<Am2HashboardPresence> {
    if !plan.contexts.iter().any(|candidate| candidate == context) {
        return Err(HalError::Platform(
            "AM2 controller context is not owned by this plan".into(),
        ));
    }
    if bytes.len() < 2
        || bytes.iter().all(|byte| *byte == 0x00)
        || bytes.iter().all(|byte| *byte == 0xFF)
        || bytes[..2] != EEPROM_PREAMBLE
    {
        return Err(HalError::Platform(format!(
            "AM2 slot {} EEPROM is blank, short, or outside the supported BHB42 family",
            context.slot
        )));
    }
    Ok(Am2HashboardPresence {
        board_target: plan.board_target.clone(),
        serial_device: context.serial_device.clone(),
        slot: context.slot,
        address: context.address,
        eeprom_bytes: bytes,
    })
}

fn is_supported_firmware(version: u8) -> bool {
    matches!(version, 0x82 | 0x86 | 0x89 | 0x8A | 0xB9 | 0xFE)
}

fn is_shift_left_artifact(reply: &[u8]) -> bool {
    reply.len() >= 2
        && reply
            .windows(2)
            .all(|window| window[1] == window[0].wrapping_shl(1))
}

fn is_repeated_firmware_artifact(reply: &[u8]) -> bool {
    reply.len() > 1 && is_supported_firmware(reply[0]) && reply.iter().all(|byte| *byte == reply[0])
}

/// Parse only the exact supported AM2 GET_VERSION reply shapes.
pub fn parse_am2_pic_firmware_reply(reply: &[u8]) -> Option<u8> {
    if reply.is_empty()
        || reply.iter().all(|byte| *byte == 0x00)
        || reply.iter().all(|byte| *byte == 0xFF)
        || is_shift_left_artifact(reply)
        || is_repeated_firmware_artifact(reply)
    {
        return None;
    }
    if reply.len() >= 3 && reply[0] == 0x05 && reply[1] == 0x17 && is_supported_firmware(reply[2]) {
        return Some(reply[2]);
    }
    if reply.len() >= 4 && reply[0] == 0x17 && is_supported_firmware(reply[1]) && reply[2] == 0x00 {
        return Some(reply[1]);
    }
    if reply.len() >= 3 && reply[0] == 0x17 && reply[1] == 0x00 && is_supported_firmware(reply[2]) {
        return Some(reply[2]);
    }
    if reply.len() >= 3 && reply[0] == 0x17 && is_supported_firmware(reply[2]) {
        return Some(reply[2]);
    }
    if is_supported_firmware(reply[0]) {
        return Some(reply[0]);
    }
    None
}

pub fn am2_get_version_transaction_steps(
    frame: &[u8],
    read_len: usize,
    flush_first: bool,
) -> Vec<I2cTransactionStep> {
    let mut steps = vec![I2cTransactionStep::SetTimeout(10)];
    if flush_first {
        steps.extend([
            I2cTransactionStep::WriteByteByByte(vec![0u8; 16]),
            I2cTransactionStep::SleepMs(10),
        ]);
    }
    steps.extend([
        I2cTransactionStep::WriteByteByByte(frame.to_vec()),
        I2cTransactionStep::SleepMs(100),
    ]);
    for _ in 0..read_len {
        steps.push(I2cTransactionStep::Read(1));
    }
    steps
}

fn collect_single_byte_reads(reads: Vec<Vec<u8>>) -> Vec<u8> {
    reads
        .into_iter()
        .filter_map(|read| read.first().copied())
        .collect()
}

fn observe_supported_firmware(i2c: &I2cServiceHandle, address: u8) -> Result<u8> {
    let probes_original: [(&str, &[u8], usize); 2] = [
        ("framed", &GET_VERSION_FRAMED, 1),
        ("short", &GET_VERSION_SHORT, 1),
    ];
    let probes_strace_first: [(&str, &[u8], usize); 3] = [
        ("framed-read5-strace", &GET_VERSION_FRAMED, 5),
        ("framed", &GET_VERSION_FRAMED, 1),
        ("short", &GET_VERSION_SHORT, 1),
    ];
    let use_strace_first = std::env::var("DCENT_AM2_GET_VERSION_FRAMED_4B")
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false);
    let probes: &[(&str, &[u8], usize)] = if use_strace_first {
        &probes_strace_first
    } else {
        &probes_original
    };
    let mut samples = Vec::new();

    for (variant, frame, read_len) in probes.iter().copied() {
        for attempt in 1..=GET_VERSION_RETRIES {
            match i2c.transaction_mutating(
                I2cMutationLabel::QueryPrelude,
                address,
                am2_get_version_transaction_steps(frame, read_len, false),
            ) {
                Ok(reads) => {
                    let reply = collect_single_byte_reads(reads);
                    samples.push(format!("{variant}#{attempt}:{reply:02X?}"));
                    if let Some(firmware) = parse_am2_pic_firmware_reply(&reply) {
                        tracing::info!(
                            address = format_args!("0x{:02X}", address),
                            firmware = format_args!("0x{:02X}", firmware),
                            variant,
                            attempt,
                            "AM2 controller endpoint observed supported firmware"
                        );
                        return Ok(firmware);
                    }
                }
                Err(error) => {
                    samples.push(format!("{variant}#{attempt}:transaction-error:{error}"));
                }
            }
            std::thread::sleep(Duration::from_millis(GET_VERSION_RETRY_DELAY_MS));
        }
    }

    Err(HalError::I2c {
        bus: i2c.bus(),
        addr: address,
        detail: format!(
            "no supported AM2 controller firmware reply after bounded clean-frame probes: {}",
            samples.join(" | ")
        ),
    })
}

/// Issue an endpoint only when the exact plan-bound address returns a modeled
/// firmware revision. The GET_VERSION transaction is the address ACK proof;
/// no additional probe is emitted.
fn validate_am2_controller_presence(presence: &Am2HashboardPresence) -> Result<()> {
    if presence.board_target != AM2_CONTROLLER_BOARD_TARGET
        || uart_for_slot(presence.slot) != Some(presence.serial_device.as_str())
        || dspic_address_for_slot(presence.slot) != Some(presence.address)
        || presence.eeprom_bytes.get(..2) != Some(EEPROM_PREAMBLE.as_slice())
    {
        return Err(HalError::Platform(
            "AM2 controller presence token failed its bound identity/topology invariants".into(),
        ));
    }
    Ok(())
}

pub fn discover_am2_controller_endpoint(
    i2c: &I2cServiceHandle,
    presence: &Am2HashboardPresence,
) -> Result<VoltageControllerEndpoint> {
    if i2c.bus() != 0 {
        return Err(HalError::Platform(format!(
            "AM2 controller endpoint requires I2C bus 0, got {}",
            i2c.bus()
        )));
    }
    validate_am2_controller_presence(presence)?;
    let firmware = observe_supported_firmware(i2c, presence.address)?;
    Ok(VoltageControllerEndpoint::from_observed_am2(
        presence.address,
        firmware,
    ))
}

/// Issue an endpoint from the direct-serial route's already-retained
/// GET_VERSION reply. This performs no I2C access and accepts only the parser
/// grammar already owned by this module; an exact target with an ambiguous or
/// unsupported reply fails closed instead of falling back to caller-asserted
/// firmware/address authority.
pub fn bind_am2_controller_endpoint_from_observation(
    presence: &Am2HashboardPresence,
    firmware_reply: &[u8],
) -> Result<VoltageControllerEndpoint> {
    validate_am2_controller_presence(presence)?;
    let firmware = parse_am2_pic_firmware_reply(firmware_reply).ok_or_else(|| {
        HalError::Platform(format!(
            "AM2 controller endpoint did not retain a supported GET_VERSION reply: {firmware_reply:02X?}"
        ))
    })?;
    Ok(VoltageControllerEndpoint::from_observed_am2(
        presence.address,
        firmware,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn exact_targets_bind_only_canonical_uart_slot_address_tuples() {
        let plan = plan_from_observations(
            Some(AM2_CONTROLLER_BOARD_TARGET),
            &devices(&["/dev/ttyS1", "/dev/ttyS3", "/dev/ttyS4"]),
        )
        .unwrap();
        assert_eq!(plan.contexts[0].slot(), 0);
        assert_eq!(plan.contexts[0].address(), 0x20);
        assert_eq!(plan.contexts[1].slot(), 2);
        assert_eq!(plan.contexts[1].address(), 0x22);
        assert_eq!(plan.contexts[2].slot(), 3);
        assert_eq!(plan.contexts[2].address(), 0x23);
    }

    #[test]
    fn missing_prefix_suffix_unknown_uart_and_duplicates_fail_closed() {
        for target in [None, Some(""), Some("am2"), Some("future-am2-s19j")] {
            assert!(plan_from_observations(target, &devices(&["/dev/ttyS1"])).is_err());
        }
        for historical_alias in ["am2-s19jpro-zynq", "am2-xil", "am2-s19jpro-xil"] {
            assert!(
                plan_from_observations(Some(historical_alias), &devices(&["/dev/ttyS1"])).is_err()
            );
        }
        assert!(
            plan_from_observations(Some("am2-s19j"), &devices(&["/dev/ttyS1", "/dev/ttyS1"]))
                .is_err()
        );
    }

    #[test]
    fn optional_plan_preserves_non_target_routes_but_exact_target_errors_fail_closed() {
        let serial_devices = devices(&["/dev/ttyS2"]);
        for non_target in [
            None,
            Some(""),
            Some("am2-s19jpro-zynq"),
            Some("am3-bb-s19jpro"),
        ] {
            assert_eq!(
                try_plan_from_observations(non_target, &serial_devices).unwrap(),
                None
            );
        }
        assert!(try_plan_from_observations(
            Some(AM2_CONTROLLER_BOARD_TARGET),
            &devices(&["/dev/ttyZ9"]),
        )
        .is_err());
        assert!(try_plan_from_observations(
            Some(AM2_CONTROLLER_BOARD_TARGET),
            &devices(&["/dev/ttyS2", "/dev/ttyS2"]),
        )
        .is_err());
    }

    #[test]
    fn eeprom_presence_binding_requires_plan_membership_and_exact_bhb42_preamble() {
        let plan =
            plan_from_observations(Some(AM2_CONTROLLER_BOARD_TARGET), &devices(&["/dev/ttyS2"]))
                .unwrap();
        let context = &plan.contexts()[0];
        let presence =
            bind_am2_hashboard_presence(&plan, context, vec![0x04, 0x11, 0x42, 0x60, 0x01])
                .unwrap();
        assert_eq!(presence.slot(), 1);
        assert_eq!(presence.address(), 0x21);

        for bytes in [
            vec![],
            vec![0x04],
            vec![0x00; 8],
            vec![0xFF; 8],
            vec![0x05, 0x11, 0x56],
            vec![0x04, 0x12, 0x42],
        ] {
            assert!(bind_am2_hashboard_presence(&plan, context, bytes).is_err());
        }
    }

    #[test]
    fn retained_firmware_reply_binds_endpoint_without_widening_parser_grammar() {
        let plan =
            plan_from_observations(Some(AM2_CONTROLLER_BOARD_TARGET), &devices(&["/dev/ttyS2"]))
                .unwrap();
        let context = &plan.contexts()[0];
        let presence =
            bind_am2_hashboard_presence(&plan, context, vec![0x04, 0x11, 0x42, 0x60, 0x01])
                .unwrap();

        for reply in [
            &[0x89][..],
            &[0x05, 0x17, 0x89, 0x00, 0xA5][..],
            &[0x17, 0x89, 0x00, 0xA5][..],
        ] {
            let endpoint = bind_am2_controller_endpoint_from_observation(&presence, reply).unwrap();
            assert_eq!(endpoint.address(), 0x21);
            assert_eq!(endpoint.bus(), 0);
            assert_eq!(endpoint.observed_firmware(), Some(0x89));
        }

        for reply in [
            &[][..],
            &[0x17, 0x89][..],
            &[0x44, 0x17, 0x89, 0x00][..],
            &[0x89, 0x89, 0x89][..],
            &[0x17, 0x88, 0x00, 0x00][..],
        ] {
            assert!(bind_am2_controller_endpoint_from_observation(&presence, reply).is_err());
        }
    }

    #[test]
    fn firmware_parser_accepts_only_explicitly_modeled_revisions() {
        for firmware in [0x82, 0x86, 0x89, 0x8A, 0xB9, 0xFE] {
            assert_eq!(parse_am2_pic_firmware_reply(&[firmware]), Some(firmware));
            assert_eq!(
                parse_am2_pic_firmware_reply(&[0x05, 0x17, firmware, 0x00, 0xA5]),
                Some(firmware)
            );
            assert_eq!(
                parse_am2_pic_firmware_reply(&[0x17, firmware, 0x00, 0xA5]),
                Some(firmware)
            );
            assert_eq!(
                parse_am2_pic_firmware_reply(&[0x17, 0x00, firmware]),
                Some(firmware)
            );
        }
        for reply in [
            &[][..],
            &[0x00][..],
            &[0x00, 0x00, 0x00][..],
            &[0xFF][..],
            &[0xFF, 0xFF, 0xFF][..],
            &[0x17, 0x89][..],
            &[0x44, 0x17, 0x89, 0x00][..],
            &[0x86, 0x0C, 0x18, 0x30, 0x60][..],
            &[0x89, 0x89, 0x89][..],
            &[0x17, 0x88, 0x00, 0x00][..],
        ] {
            assert_eq!(
                parse_am2_pic_firmware_reply(reply),
                None,
                "reply={reply:02X?}"
            );
        }
    }

    #[test]
    fn get_version_grammar_has_no_implicit_probe_or_flush() {
        let steps = am2_get_version_transaction_steps(&GET_VERSION_FRAMED, 1, false);
        assert_eq!(
            steps,
            vec![
                I2cTransactionStep::SetTimeout(10),
                I2cTransactionStep::WriteByteByByte(GET_VERSION_FRAMED.to_vec()),
                I2cTransactionStep::SleepMs(100),
                I2cTransactionStep::Read(1),
            ]
        );
    }
}
