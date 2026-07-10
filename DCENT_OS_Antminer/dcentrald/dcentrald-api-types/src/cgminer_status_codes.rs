//!  cgmsg-A — full CGMiner status code map (HAL-free).
//!
//! Source RE evidence:
//!
//!   lines 287-407 (the canonical `MSG_*` constants + severity table).
//!
//!   for the LuxOS extension codes (202/310/316/318/319/323/324/339/357/401/408).
//!
//! Every CGMiner-compatible reply (Bitmain stock S9/S19j, BraiinsOS,
//! VNish, LuxOS, DCENT_OS) carries a numeric `Code=` field in the
//! `STATUS` block. This module exposes the full numeric mapping plus a
//! severity classifier so DCENT_OS REST/CGMiner-compat replies can
//! generate canonical error messages and downstream tooling
//! (pyasic, dcent-toolbox) can decode them losslessly.

use serde::{Deserialize, Serialize};

/// Severity bucket per cgminer 3.12.0 `enum code_severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CgminerSeverity {
    /// `SEVERITY_ERR` — request rejected with an error.
    Error,
    /// `SEVERITY_WARN` — request accepted with a warning.
    Warning,
    /// `SEVERITY_INFO` — informational; e.g. "already enabled".
    Info,
    /// `SEVERITY_SUCC` — request completed successfully.
    Success,
    /// `SEVERITY_FAIL` — fatal failure (rare; usually I/O).
    Fail,
}

impl CgminerSeverity {
    /// Single-letter wire form (matches the `STATUS=<letter>` field on
    /// the wire — `S/I/W/E/F`).
    pub fn letter(&self) -> char {
        match self {
            Self::Success => 'S',
            Self::Info => 'I',
            Self::Warning => 'W',
            Self::Error => 'E',
            Self::Fail => 'F',
        }
    }

    /// True iff this severity indicates the request was accepted.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success | Self::Info)
    }

    /// True iff this severity indicates the request failed.
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error | Self::Fail)
    }
}

/// Numeric CGMiner status code. Discriminant value is the wire `Code=`
/// field. Includes the upstream cgminer 3.12.0 codes (7-124) plus the
/// LuxOS extensions used in `?command=…` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u16)]
pub enum CgminerStatusCode {
    // ----- Upstream cgminer 3.12.0 (api.c #287-407) -----
    Pool = 7,
    NoPool = 8,
    Devs = 9,
    NoDevs = 10,
    Summary = 11,
    InvalidCommand = 14,
    MissingId = 15,
    Version = 22,
    InvalidJson = 23,
    MissingCommand = 24,
    MissingPid = 25,
    InvalidPid = 26,
    SwitchPool = 27,
    MissingValue = 28,
    NoAdl = 29,
    InvalidInt = 31,
    MineConfig = 33,
    MissingFn = 42,
    BadFn = 43,
    Saved = 44,
    AccessDenied = 45,
    AccessOk = 46,
    EnablePool = 47,
    DisablePool = 48,
    AlreadyEnabled = 49,
    AlreadyDisabled = 50,
    DisableLastPool = 51,
    MissingPoolDetails = 52,
    InvalidPoolDetails = 53,
    TooManyPools = 54,
    AddPool = 55,
    NoPgas = 56,
    PgaDevice = 57,
    InvalidPga = 58,
    NumPga = 59,
    Notify = 60,
    PgaAlreadyEna = 61,
    PgaAlreadyDis = 62,
    PgaEna = 63,
    PgaDis = 64,
    PgaUnwell = 65,
    RemoveLastPool = 66,
    ActivePool = 67,
    RemovePool = 68,
    DevDetails = 69,
    MineStats = 70,
    MissingCheck = 71,
    Check = 72,
    PoolPriorities = 73,
    DuplicatePid = 74,
    MissingBool = 75,
    InvalidBool = 76,
    Foo = 77,
    MineCoin = 78,
    DebugSet = 79,
    PgaIdent = 80,
    PgaNoId = 81,
    SetConfig = 82,
    UnknownConfig = 83,
    InvalidNum = 84,
    ConfigParam = 85,
    ConfigValue = 86,
    UsbStats = 87,
    NoUsbStats = 88,
    MissingPgaOpt = 89,
    PgaNoSet = 90,
    PgaHelp = 91,
    PgaSetOk = 92,
    PgaSetErr = 93,
    ZeroMis = 94,
    ZeroInv = 95,
    ZeroSum = 96,
    ZeroNoSum = 97,
    PgaUsbNoDev = 98,
    InvalidHotplug = 99,
    Hotplug = 100,
    DisHotplug = 101,
    NoHotplug = 102,
    MissingHotplug = 103,
    NumAsc = 104,
    AscNon = 105,
    AscDevice = 106,
    InvalidAsc = 107,
    AscAlreadyEna = 108,
    AscAlreadyDis = 109,
    AscEna = 110,
    AscDis = 111,
    AscUnwell = 112,
    AscIdent = 113,
    AscNoId = 114,
    AscUsbNoDev = 115,
    MissingAscOpt = 116,
    AscNoSet = 117,
    AscHelp = 118,
    AscSetOk = 119,
    AscSetErr = 120,
    InvalidNeg = 121,
    SetQuota = 122,
    LockOk = 123,
    LockDis = 124,

    // ----- LuxOS extensions (E-rest-api-8080.md + 09-authenticated-api.txt) -----
    /// `?command=fans` reply.
    LuxFans = 202,
    /// `?command=tunerstatus` reply.
    LuxTunerStatus = 310,
    /// Session created (`logon`).
    LuxSessionCreated = 316,
    /// Kill session (`kill`).
    LuxKillSession = 318,
    /// Session information (`session`).
    LuxSessionInfo = 319,
    /// Profiles list (`profiles`).
    LuxProfilesList = 323,
    /// Pool group list (`groups`).
    LuxGroupsList = 324,
    /// `?command=atm` reply.
    LuxAtm = 339,
    /// Miner events stream (`events`).
    LuxMinerEvents = 357,
    /// LuxOS-specific "Invalid <param> value" — read-protected request
    /// with bad/wrong param format.
    LuxInvalidParamValue = 401,
    /// LuxOS-specific "Invalid field 'session_id'" — `session_id` was
    /// supplied where it's not allowed (e.g. `metrics`).
    LuxInvalidSessionField = 408,
}

impl CgminerStatusCode {
    /// Numeric `Code=` wire field.
    pub fn code(&self) -> u16 {
        *self as u16
    }

    /// Severity classification for this code, drawn from cgminer's
    /// `codes[]` table or the LuxOS-extension RE captures.
    pub fn severity(&self) -> CgminerSeverity {
        use CgminerSeverity::*;
        use CgminerStatusCode::*;
        match self {
            // SEVERITY_SUCC
            Pool | Devs | Summary | Version | SwitchPool | MineConfig | Saved | AccessOk
            | EnablePool | DisablePool | PoolPriorities | AddPool | NumPga | Notify | NumAsc
            | RemovePool | DevDetails | MineStats | Check | MineCoin | DebugSet | PgaIdent
            | SetConfig | UsbStats | PgaSetOk | ZeroSum | Hotplug | DisHotplug | AscIdent
            | AscSetOk | SetQuota | LockOk | LockDis => Success,
            // SEVERITY_INFO
            AlreadyEnabled | AlreadyDisabled | PgaAlreadyEna | PgaAlreadyDis | PgaEna | PgaDis
            | AscAlreadyEna | AscAlreadyDis | AscEna | AscDis => Info,
            // SEVERITY_ERR — every other upstream code
            NoPool | NoDevs | InvalidCommand | MissingId | InvalidJson | MissingCommand
            | MissingPid | InvalidPid | MissingValue | NoAdl | InvalidInt | MissingFn | BadFn
            | AccessDenied | DisableLastPool | MissingPoolDetails | InvalidPoolDetails
            | TooManyPools | NoPgas | PgaDevice | InvalidPga | PgaUnwell | RemoveLastPool
            | ActivePool | MissingCheck | DuplicatePid | MissingBool | InvalidBool | Foo
            | PgaNoId | UnknownConfig | InvalidNum | ConfigParam | ConfigValue | NoUsbStats
            | MissingPgaOpt | PgaNoSet | PgaHelp | PgaSetErr | ZeroMis | ZeroInv | ZeroNoSum
            | PgaUsbNoDev | InvalidHotplug | NoHotplug | MissingHotplug | AscNon | AscDevice
            | InvalidAsc | AscUnwell | AscNoId | AscUsbNoDev | MissingAscOpt | AscNoSet
            | AscHelp | AscSetErr | InvalidNeg => Error,
            // ----- LuxOS extensions -----
            // 316/318/319/323/324/339/357 are SUCCESS replies.
            // 202/310 are SUCCESS (returning fans/tunerstatus blob).
            // 401/408 are ERROR codes (bad param / forbidden field).
            LuxFans | LuxTunerStatus | LuxSessionCreated | LuxKillSession | LuxSessionInfo
            | LuxProfilesList | LuxGroupsList | LuxAtm | LuxMinerEvents => Success,
            LuxInvalidParamValue | LuxInvalidSessionField => Error,
        }
    }

    /// Look up a status code by its numeric `Code=` value.
    pub fn from_code(code: u16) -> Option<Self> {
        ALL_CODES.iter().copied().find(|sc| sc.code() == code)
    }
}

/// Stable iteration order for tests and parity audits.
pub const ALL_CODES: &[CgminerStatusCode] = &[
    CgminerStatusCode::Pool,
    CgminerStatusCode::NoPool,
    CgminerStatusCode::Devs,
    CgminerStatusCode::NoDevs,
    CgminerStatusCode::Summary,
    CgminerStatusCode::InvalidCommand,
    CgminerStatusCode::MissingId,
    CgminerStatusCode::Version,
    CgminerStatusCode::InvalidJson,
    CgminerStatusCode::MissingCommand,
    CgminerStatusCode::MissingPid,
    CgminerStatusCode::InvalidPid,
    CgminerStatusCode::SwitchPool,
    CgminerStatusCode::MissingValue,
    CgminerStatusCode::NoAdl,
    CgminerStatusCode::InvalidInt,
    CgminerStatusCode::MineConfig,
    CgminerStatusCode::MissingFn,
    CgminerStatusCode::BadFn,
    CgminerStatusCode::Saved,
    CgminerStatusCode::AccessDenied,
    CgminerStatusCode::AccessOk,
    CgminerStatusCode::EnablePool,
    CgminerStatusCode::DisablePool,
    CgminerStatusCode::AlreadyEnabled,
    CgminerStatusCode::AlreadyDisabled,
    CgminerStatusCode::DisableLastPool,
    CgminerStatusCode::MissingPoolDetails,
    CgminerStatusCode::InvalidPoolDetails,
    CgminerStatusCode::TooManyPools,
    CgminerStatusCode::AddPool,
    CgminerStatusCode::NoPgas,
    CgminerStatusCode::PgaDevice,
    CgminerStatusCode::InvalidPga,
    CgminerStatusCode::NumPga,
    CgminerStatusCode::Notify,
    CgminerStatusCode::PgaAlreadyEna,
    CgminerStatusCode::PgaAlreadyDis,
    CgminerStatusCode::PgaEna,
    CgminerStatusCode::PgaDis,
    CgminerStatusCode::PgaUnwell,
    CgminerStatusCode::RemoveLastPool,
    CgminerStatusCode::ActivePool,
    CgminerStatusCode::RemovePool,
    CgminerStatusCode::DevDetails,
    CgminerStatusCode::MineStats,
    CgminerStatusCode::MissingCheck,
    CgminerStatusCode::Check,
    CgminerStatusCode::PoolPriorities,
    CgminerStatusCode::DuplicatePid,
    CgminerStatusCode::MissingBool,
    CgminerStatusCode::InvalidBool,
    CgminerStatusCode::Foo,
    CgminerStatusCode::MineCoin,
    CgminerStatusCode::DebugSet,
    CgminerStatusCode::PgaIdent,
    CgminerStatusCode::PgaNoId,
    CgminerStatusCode::SetConfig,
    CgminerStatusCode::UnknownConfig,
    CgminerStatusCode::InvalidNum,
    CgminerStatusCode::ConfigParam,
    CgminerStatusCode::ConfigValue,
    CgminerStatusCode::UsbStats,
    CgminerStatusCode::NoUsbStats,
    CgminerStatusCode::MissingPgaOpt,
    CgminerStatusCode::PgaNoSet,
    CgminerStatusCode::PgaHelp,
    CgminerStatusCode::PgaSetOk,
    CgminerStatusCode::PgaSetErr,
    CgminerStatusCode::ZeroMis,
    CgminerStatusCode::ZeroInv,
    CgminerStatusCode::ZeroSum,
    CgminerStatusCode::ZeroNoSum,
    CgminerStatusCode::PgaUsbNoDev,
    CgminerStatusCode::InvalidHotplug,
    CgminerStatusCode::Hotplug,
    CgminerStatusCode::DisHotplug,
    CgminerStatusCode::NoHotplug,
    CgminerStatusCode::MissingHotplug,
    CgminerStatusCode::NumAsc,
    CgminerStatusCode::AscNon,
    CgminerStatusCode::AscDevice,
    CgminerStatusCode::InvalidAsc,
    CgminerStatusCode::AscAlreadyEna,
    CgminerStatusCode::AscAlreadyDis,
    CgminerStatusCode::AscEna,
    CgminerStatusCode::AscDis,
    CgminerStatusCode::AscUnwell,
    CgminerStatusCode::AscIdent,
    CgminerStatusCode::AscNoId,
    CgminerStatusCode::AscUsbNoDev,
    CgminerStatusCode::MissingAscOpt,
    CgminerStatusCode::AscNoSet,
    CgminerStatusCode::AscHelp,
    CgminerStatusCode::AscSetOk,
    CgminerStatusCode::AscSetErr,
    CgminerStatusCode::InvalidNeg,
    CgminerStatusCode::SetQuota,
    CgminerStatusCode::LockOk,
    CgminerStatusCode::LockDis,
    // LuxOS extensions
    CgminerStatusCode::LuxFans,
    CgminerStatusCode::LuxTunerStatus,
    CgminerStatusCode::LuxSessionCreated,
    CgminerStatusCode::LuxKillSession,
    CgminerStatusCode::LuxSessionInfo,
    CgminerStatusCode::LuxProfilesList,
    CgminerStatusCode::LuxGroupsList,
    CgminerStatusCode::LuxAtm,
    CgminerStatusCode::LuxMinerEvents,
    CgminerStatusCode::LuxInvalidParamValue,
    CgminerStatusCode::LuxInvalidSessionField,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_codes_match_cgminer_312_verbatim() {
        // Spot-check the load-bearing codes — these are the ones the
        // dashboard + dcent-toolbox + pyasic decode by number.
        assert_eq!(CgminerStatusCode::Pool.code(), 7);
        assert_eq!(CgminerStatusCode::NoPool.code(), 8);
        assert_eq!(CgminerStatusCode::Devs.code(), 9);
        assert_eq!(CgminerStatusCode::Summary.code(), 11);
        assert_eq!(CgminerStatusCode::InvalidCommand.code(), 14);
        assert_eq!(CgminerStatusCode::Version.code(), 22);
        assert_eq!(CgminerStatusCode::Notify.code(), 60);
        assert_eq!(CgminerStatusCode::DevDetails.code(), 69);
        assert_eq!(CgminerStatusCode::MineStats.code(), 70);
        assert_eq!(CgminerStatusCode::PoolPriorities.code(), 73);
        assert_eq!(CgminerStatusCode::NumAsc.code(), 104);
        assert_eq!(CgminerStatusCode::LockDis.code(), 124);
    }

    #[test]
    fn luxos_extension_codes_match_capture() {
        // Per 09-authenticated-api.txt observed responses.
        assert_eq!(CgminerStatusCode::LuxFans.code(), 202);
        assert_eq!(CgminerStatusCode::LuxTunerStatus.code(), 310);
        assert_eq!(CgminerStatusCode::LuxSessionCreated.code(), 316);
        assert_eq!(CgminerStatusCode::LuxKillSession.code(), 318);
        assert_eq!(CgminerStatusCode::LuxSessionInfo.code(), 319);
        assert_eq!(CgminerStatusCode::LuxProfilesList.code(), 323);
        assert_eq!(CgminerStatusCode::LuxGroupsList.code(), 324);
        assert_eq!(CgminerStatusCode::LuxAtm.code(), 339);
        assert_eq!(CgminerStatusCode::LuxMinerEvents.code(), 357);
        assert_eq!(CgminerStatusCode::LuxInvalidParamValue.code(), 401);
        assert_eq!(CgminerStatusCode::LuxInvalidSessionField.code(), 408);
    }

    #[test]
    fn well_known_severity_classifications_pinned() {
        // Per cgminer 3.12.0 codes[] table (api.c lines 444-512).
        assert_eq!(CgminerStatusCode::Pool.severity(), CgminerSeverity::Success);
        assert_eq!(CgminerStatusCode::NoPool.severity(), CgminerSeverity::Error);
        assert_eq!(
            CgminerStatusCode::InvalidCommand.severity(),
            CgminerSeverity::Error
        );
        assert_eq!(
            CgminerStatusCode::Version.severity(),
            CgminerSeverity::Success
        );
        assert_eq!(
            CgminerStatusCode::AlreadyEnabled.severity(),
            CgminerSeverity::Info
        );
        assert_eq!(
            CgminerStatusCode::AlreadyDisabled.severity(),
            CgminerSeverity::Info
        );
        assert_eq!(
            CgminerStatusCode::SwitchPool.severity(),
            CgminerSeverity::Success
        );
        assert_eq!(
            CgminerStatusCode::AccessDenied.severity(),
            CgminerSeverity::Error
        );
        assert_eq!(
            CgminerStatusCode::AccessOk.severity(),
            CgminerSeverity::Success
        );
    }

    #[test]
    fn luxos_session_codes_classify_as_success() {
        for code in [
            CgminerStatusCode::LuxSessionCreated,
            CgminerStatusCode::LuxSessionInfo,
            CgminerStatusCode::LuxKillSession,
        ] {
            assert_eq!(code.severity(), CgminerSeverity::Success);
            assert!(code.severity().is_success());
        }
    }

    #[test]
    fn luxos_invalid_param_classifies_as_error() {
        assert_eq!(
            CgminerStatusCode::LuxInvalidParamValue.severity(),
            CgminerSeverity::Error
        );
        assert!(CgminerStatusCode::LuxInvalidParamValue
            .severity()
            .is_error());
        assert_eq!(
            CgminerStatusCode::LuxInvalidSessionField.severity(),
            CgminerSeverity::Error
        );
    }

    #[test]
    fn severity_letter_matches_wire_form() {
        // The `STATUS=<letter>` field on the wire — pin every variant.
        assert_eq!(CgminerSeverity::Success.letter(), 'S');
        assert_eq!(CgminerSeverity::Info.letter(), 'I');
        assert_eq!(CgminerSeverity::Warning.letter(), 'W');
        assert_eq!(CgminerSeverity::Error.letter(), 'E');
        assert_eq!(CgminerSeverity::Fail.letter(), 'F');
    }

    #[test]
    fn from_code_round_trips() {
        for code in ALL_CODES.iter().copied() {
            let n = code.code();
            assert_eq!(CgminerStatusCode::from_code(n), Some(code));
        }
    }

    #[test]
    fn from_code_returns_none_for_unknown_codes() {
        // 0 / 1 / 999 / 65535 are not in the table.
        for unknown in [0u16, 1, 12, 13, 16, 17, 999, 1000, 65535] {
            assert_eq!(CgminerStatusCode::from_code(unknown), None);
        }
    }

    #[test]
    fn full_catalog_size_matches_re_inventory() {
        // 100 upstream codes + 11 LuxOS extensions = 111 total.
        assert_eq!(ALL_CODES.len(), 111);
    }

    #[test]
    fn no_duplicate_numeric_codes() {
        // Every variant must have a unique `Code=` value.
        use std::collections::HashSet;
        let mut seen: HashSet<u16> = HashSet::new();
        for code in ALL_CODES.iter().copied() {
            assert!(
                seen.insert(code.code()),
                "duplicate numeric code {}",
                code.code()
            );
        }
    }

    #[test]
    fn severity_round_trips_through_serde() {
        for sev in [
            CgminerSeverity::Success,
            CgminerSeverity::Info,
            CgminerSeverity::Warning,
            CgminerSeverity::Error,
            CgminerSeverity::Fail,
        ] {
            let json = serde_json::to_string(&sev).unwrap();
            let back: CgminerSeverity = serde_json::from_str(&json).unwrap();
            assert_eq!(sev, back);
        }
    }

    #[test]
    fn severity_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&CgminerSeverity::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&CgminerSeverity::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn pga_already_enabled_severity_is_info_not_error() {
        // cgminer source line 478: SEVERITY_INFO. Idempotent operations
        // are info, not error — important for dashboard "already on"
        // toasts.
        assert_eq!(
            CgminerStatusCode::PgaAlreadyEna.severity(),
            CgminerSeverity::Info
        );
        assert_eq!(
            CgminerStatusCode::PgaAlreadyDis.severity(),
            CgminerSeverity::Info
        );
        assert_eq!(
            CgminerStatusCode::AscAlreadyEna.severity(),
            CgminerSeverity::Info
        );
    }

    #[test]
    fn upstream_count_is_100_per_cgminer_312() {
        // cgminer 3.12.0 has exactly 100 MSG_* constants (codes 7-124,
        // sparse). LuxOS contributed 11 more.
        let upstream = ALL_CODES.iter().filter(|c| c.code() <= 124).count();
        assert_eq!(upstream, 100);
        let lux = ALL_CODES.iter().filter(|c| c.code() >= 200).count();
        assert_eq!(lux, 11);
    }
}
