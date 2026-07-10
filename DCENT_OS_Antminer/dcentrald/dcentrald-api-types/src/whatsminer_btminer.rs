//!  wm-A — Whatsminer BTMiner JSON API codec (HAL-free).
//!
//! Source RE evidence:
//!  §4.
//!
//! Whatsminer M60S exposes two API protocols:
//! - **V2** (TCP socket, JSON line protocol). Read commands no auth;
//!   write commands need V2 token-encrypted payload.
//! - **V3** (TCP port 4433, length-prefixed JSON frames). Newer; simpler
//!   token auth.
//!
//! This module catalogs the documented V2/V3 commands, classifies them
//! by side (read/write) + auth requirement, and provides the wire-frame
//! shape for V3 length-prefixed framing. Crypto (md5_crypt + AES-256-ECB
//! for V2, SHA-256 token for V3) is left to a runtime adapter — this
//! module is HAL-free pure data + framing helpers.

use serde::{Deserialize, Serialize};

/// V2 BTMiner default TCP port. Per RE doc §4 lines 119-127.
pub const V2_TCP_PORT: u16 = 4028;

/// V3 BTMiner TCP port (newer protocol per RE doc §4.6 lines 236-248).
pub const V3_TCP_PORT: u16 = 4433;

/// Side classification — read commands need no auth, write commands
/// need the V2/V3 token dance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSide {
    Read,
    Write,
}

/// Discrete BTMiner V2 command names per RE doc §4 lines 165-261.
///
/// Variants are non-exhaustive — Whatsminer adds commands across
/// firmware versions. Operators using new commands fall back to the
/// `Other(name)` escape hatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BtMinerV2Command {
    // --- Read commands (RE doc §4.1) ---
    GetVersion,
    Summary,
    GetPsu,
    GetMinerInfo,
    Status,
    GetErrorCode,
    GetToken,
    // --- Write commands (RE doc §4.5; require token auth) ---
    UpdatePools,
    RestartBtminer,
    PowerOff,
    PowerOn,
    SetLed,
    SetLowPower,
    SetHighPower,
    SetNormalPower,
    UpdateFirmware,
    Reboot,
    FactoryReset,
    UpdatePwd,
    NetConfig,
    SetTargetFreq,
    EnableBtminerFastBoot,
    DisableBtminerFastBoot,
    EnableWebPools,
    DisableWebPools,
    SetHostname,
    SetZone,
    LoadLog,
    SetPowerPct,
    PrePowerOn,
    DownloadLogs,
    SetPowerPctV2,
    SetTempOffset,
    /// Escape hatch for any documented command not in the above list,
    /// or for newer firmware that adds commands we haven't catalogued.
    Other(String),
}

impl BtMinerV2Command {
    /// Wire-protocol command string for the `cmd` JSON field.
    pub fn wire_name(&self) -> &str {
        match self {
            BtMinerV2Command::GetVersion => "get_version",
            BtMinerV2Command::Summary => "summary",
            BtMinerV2Command::GetPsu => "get_psu",
            BtMinerV2Command::GetMinerInfo => "get_miner_info",
            BtMinerV2Command::Status => "status",
            BtMinerV2Command::GetErrorCode => "get_error_code",
            BtMinerV2Command::GetToken => "get_token",
            BtMinerV2Command::UpdatePools => "update_pools",
            BtMinerV2Command::RestartBtminer => "restart_btminer",
            BtMinerV2Command::PowerOff => "power_off",
            BtMinerV2Command::PowerOn => "power_on",
            BtMinerV2Command::SetLed => "set_led",
            BtMinerV2Command::SetLowPower => "set_low_power",
            BtMinerV2Command::SetHighPower => "set_high_power",
            BtMinerV2Command::SetNormalPower => "set_normal_power",
            BtMinerV2Command::UpdateFirmware => "update_firmware",
            BtMinerV2Command::Reboot => "reboot",
            BtMinerV2Command::FactoryReset => "factory_reset",
            BtMinerV2Command::UpdatePwd => "update_pwd",
            BtMinerV2Command::NetConfig => "net_config",
            BtMinerV2Command::SetTargetFreq => "set_target_freq",
            BtMinerV2Command::EnableBtminerFastBoot => "enable_btminer_fast_boot",
            BtMinerV2Command::DisableBtminerFastBoot => "disable_btminer_fast_boot",
            BtMinerV2Command::EnableWebPools => "enable_web_pools",
            BtMinerV2Command::DisableWebPools => "disable_web_pools",
            BtMinerV2Command::SetHostname => "set_hostname",
            BtMinerV2Command::SetZone => "set_zone",
            BtMinerV2Command::LoadLog => "load_log",
            BtMinerV2Command::SetPowerPct => "set_power_pct",
            BtMinerV2Command::PrePowerOn => "pre_power_on",
            BtMinerV2Command::DownloadLogs => "download_logs",
            BtMinerV2Command::SetPowerPctV2 => "set_power_pct_v2",
            BtMinerV2Command::SetTempOffset => "set_temp_offset",
            BtMinerV2Command::Other(s) => s.as_str(),
        }
    }

    pub fn side(&self) -> CommandSide {
        match self {
            BtMinerV2Command::GetVersion
            | BtMinerV2Command::Summary
            | BtMinerV2Command::GetPsu
            | BtMinerV2Command::GetMinerInfo
            | BtMinerV2Command::Status
            | BtMinerV2Command::GetErrorCode
            | BtMinerV2Command::GetToken => CommandSide::Read,
            // Other(_) defaults to Write — safer to assume auth needed.
            _ => CommandSide::Write,
        }
    }

    /// Whether this command can permanently change the miner state and
    /// requires explicit operator confirmation (UpdateFirmware,
    /// FactoryReset, UpdatePwd).
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            BtMinerV2Command::UpdateFirmware
                | BtMinerV2Command::FactoryReset
                | BtMinerV2Command::UpdatePwd
        )
    }
}

/// V3 length-prefixed framing per RE doc §4.6 lines 237-248.
///
/// Wire format: `[u32 LE length][JSON payload bytes]`. Frame the JSON
/// before sending; deframe on receive.
pub fn v3_frame(json_payload: &[u8]) -> Vec<u8> {
    let len = json_payload.len() as u32;
    let mut buf = Vec::with_capacity(4 + json_payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(json_payload);
    buf
}

/// Deframe a V3 wire buffer. Returns the JSON payload slice on success.
pub fn v3_deframe(wire: &[u8]) -> Result<&[u8], V3FrameError> {
    if wire.len() < 4 {
        return Err(V3FrameError::Truncated {
            got: wire.len(),
            need: 4,
        });
    }
    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&wire[..4]);
    let len = u32::from_le_bytes(len_bytes) as usize;
    // bug-hunt LOW #9 (2026-05-28): `4 + len` is computed in `usize`. `len` comes
    // from an UNTRUSTED u32 LE prefix, and on the real 32-bit armv7 firmware target
    // `usize == u32`, so a crafted `len` near u32::MAX wraps `4 + len` to a tiny
    // value — bypassing the truncation guard and letting `&wire[4..4+len]` slice a
    // wrong (wrapped) range. `checked_add` makes the guard correct on 32-bit.
    let need = match len.checked_add(4) {
        Some(n) => n,
        None => {
            return Err(V3FrameError::Truncated {
                got: wire.len(),
                need: usize::MAX,
            });
        }
    };
    if wire.len() < need {
        return Err(V3FrameError::Truncated {
            got: wire.len(),
            need,
        });
    }
    Ok(&wire[4..need])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum V3FrameError {
    Truncated { got: usize, need: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ports_match_re_doc() {
        assert_eq!(V2_TCP_PORT, 4028);
        assert_eq!(V3_TCP_PORT, 4433);
    }

    #[test]
    fn read_commands_classified_correctly() {
        for c in [
            BtMinerV2Command::GetVersion,
            BtMinerV2Command::Summary,
            BtMinerV2Command::GetPsu,
            BtMinerV2Command::GetMinerInfo,
            BtMinerV2Command::Status,
            BtMinerV2Command::GetErrorCode,
            BtMinerV2Command::GetToken,
        ] {
            assert_eq!(c.side(), CommandSide::Read);
            assert!(!c.is_destructive());
        }
    }

    #[test]
    fn write_commands_classified_correctly() {
        for c in [
            BtMinerV2Command::PowerOff,
            BtMinerV2Command::PowerOn,
            BtMinerV2Command::SetLed,
            BtMinerV2Command::Reboot,
            BtMinerV2Command::FactoryReset,
        ] {
            assert_eq!(c.side(), CommandSide::Write);
        }
    }

    #[test]
    fn destructive_commands_flagged_correctly() {
        for c in [
            BtMinerV2Command::UpdateFirmware,
            BtMinerV2Command::FactoryReset,
            BtMinerV2Command::UpdatePwd,
        ] {
            assert!(c.is_destructive());
        }
        // Reboot is NOT destructive (recoverable on next boot).
        assert!(!BtMinerV2Command::Reboot.is_destructive());
        assert!(!BtMinerV2Command::PowerOff.is_destructive());
    }

    #[test]
    fn wire_names_match_re_doc_verbatim() {
        // Spot-check a few from RE doc §4.5 line 254.
        assert_eq!(BtMinerV2Command::GetVersion.wire_name(), "get_version");
        assert_eq!(BtMinerV2Command::FactoryReset.wire_name(), "factory_reset");
        assert_eq!(
            BtMinerV2Command::EnableBtminerFastBoot.wire_name(),
            "enable_btminer_fast_boot"
        );
        assert_eq!(
            BtMinerV2Command::SetPowerPctV2.wire_name(),
            "set_power_pct_v2"
        );
        assert_eq!(
            BtMinerV2Command::SetTempOffset.wire_name(),
            "set_temp_offset"
        );
    }

    #[test]
    fn other_command_carries_custom_name() {
        let c = BtMinerV2Command::Other("future_cmd".to_string());
        assert_eq!(c.wire_name(), "future_cmd");
        // Other defaults to Write side (conservative).
        assert_eq!(c.side(), CommandSide::Write);
    }

    #[test]
    fn v3_frame_prepends_4byte_le_length() {
        let payload = b"{\"cmd\":\"get.device.info\"}";
        let framed = v3_frame(payload);
        assert_eq!(framed.len(), 4 + payload.len());
        // First 4 bytes = LE length.
        let len = u32::from_le_bytes([framed[0], framed[1], framed[2], framed[3]]);
        assert_eq!(len as usize, payload.len());
        // Payload follows.
        assert_eq!(&framed[4..], payload);
    }

    #[test]
    fn v3_deframe_round_trips_canonical_payload() {
        let payload = b"{\"cmd\":\"get.miner.status\"}";
        let framed = v3_frame(payload);
        let out = v3_deframe(&framed).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn v3_deframe_rejects_short_buffer() {
        let err = v3_deframe(&[0x01]).unwrap_err();
        assert!(matches!(err, V3FrameError::Truncated { got: 1, need: 4 }));
    }

    #[test]
    fn v3_deframe_rejects_max_len_without_panic_or_wrap() {
        // bug-hunt LOW #9 (2026-05-28): a u32::MAX length prefix must be rejected
        // as Truncated — never panic and never slice a wrapped range. On the real
        // 32-bit armv7 target the old `4 + len` wrapped (u32::MAX + 4 = 3), which
        // bypassed the guard and made `&wire[4..3]` panic; `checked_add` makes the
        // guard correct on every target. (On the 64-bit test host the add can't
        // wrap, so this pins the large-len rejection path; on 32-bit the same input
        // takes the `checked_add` None branch — both yield Truncated, never a panic.)
        let mut wire = vec![0xFFu8; 4]; // len prefix = u32::MAX
        wire.extend_from_slice(&[0xAA, 0xBB]); // tiny actual payload
        let err = v3_deframe(&wire).unwrap_err();
        assert!(
            matches!(err, V3FrameError::Truncated { .. }),
            "max-len frame must be Truncated, got {err:?}"
        );
    }

    #[test]
    fn v3_deframe_rejects_truncated_payload() {
        // Header says length 100 but payload is only 5 bytes.
        let mut wire = Vec::new();
        wire.extend_from_slice(&100u32.to_le_bytes());
        wire.extend_from_slice(&[0u8; 5]);
        let err = v3_deframe(&wire).unwrap_err();
        match err {
            V3FrameError::Truncated { got, need } => {
                assert_eq!(got, 9);
                assert_eq!(need, 104);
            }
        }
    }

    #[test]
    fn command_serializes_to_snake_case_for_simple_variants() {
        // Bare variants serialize as snake_case strings.
        let json = serde_json::to_string(&BtMinerV2Command::GetVersion).unwrap();
        assert_eq!(json, "\"get_version\"");
        let json = serde_json::to_string(&BtMinerV2Command::FactoryReset).unwrap();
        assert_eq!(json, "\"factory_reset\"");
    }

    #[test]
    fn other_variant_serde_round_trips() {
        let c = BtMinerV2Command::Other("custom_cmd".to_string());
        let json = serde_json::to_string(&c).unwrap();
        let back: BtMinerV2Command = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn v3_frame_error_round_trips_through_serde() {
        let e = V3FrameError::Truncated { got: 1, need: 4 };
        let json = serde_json::to_string(&e).unwrap();
        let back: V3FrameError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
        assert!(json.contains("\"error\":\"truncated\""));
    }
}
