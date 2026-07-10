//!  luxos-B — full LuxOS REST command catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §3.3 (62-row table verified against luxminer.strings + SPA caller).
//!
//! LuxOS exposes a CGMiner-compatible JSON command dispatcher on port 8080
//! (and TCP 4028). Every mutating command requires a session — obtained
//! via `logon` and surfaced as the first comma-token in `parameter`. Read
//! commands are open by default unless an HTTP password is set.
//!
//! This catalog is HAL-free; `dcent-toolbox` and the dashboard
//! competitive-readiness widget can use it for parity comparisons and
//! per-command capability gating without depending on the runtime API
//! surface.

use serde::{Deserialize, Serialize};

/// Auth tier required to invoke a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosAuthTier {
    /// Read-only command. Open unless HTTP password is set.
    None,
    /// Mutating command — requires `logon` session as first parameter token.
    Session,
}

/// Action class for the command (mirrors the SPA classification in
/// `J-web-ui.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosCommandKind {
    /// No state change (info, telemetry, query).
    Read,
    /// Persists configuration (pool, profile, network, fan, temp).
    Write,
    /// Restarts mining or device.
    Lifecycle,
    /// Erases flash, removes installation, triggers stock-firmware boot.
    Destructive,
}

/// Each LuxOS REST command is one variant. Names match the `?command=`
/// dispatcher token verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LuxosCommand {
    // Read-only — system + telemetry
    Version,
    Summary,
    Stats,
    Config,
    Devs,
    Edevs,
    Devdetails,
    Pools,
    Groups,
    Poolopts,
    Psuget,
    Coin,
    Lcd,
    Asccount,
    Tempctrl,
    Temps,
    Tempsensor,
    Tunerstatus,
    Profiles,
    Profileget,
    Metrics,
    Events,
    Fans,
    Power,
    Limits,
    Hashboardopts,
    Atm,
    Autotunerget,
    Frequencyget,
    Voltageget,
    Healthchipget,
    Systemaudit,
    // Session lifecycle
    Logon,
    Session,
    Logoff,
    Kill,
    // Mutating — pools
    Addgroup,
    Removegroup,
    Groupquota,
    Addpool,
    Removepool,
    Enablepool,
    Disablepool,
    Switchpool,
    Pooloptsset,
    // Mutating — profiles & autotuner
    Profilenew,
    Profileset,
    Profilerem,
    Profilerestore,
    Autotunerset,
    Atmset,
    // Mutating — thermal & power
    Tempctrlset,
    Tempsensorset,
    Fanset,
    Powertargetset,
    Psuset,
    Hashboardoptsset,
    Curtail,
    Immersionswitch,
    Ledset,
    // Mutating — lifecycle
    Reboot,
    Rebootdevice,
    Resetminer,
    Uninstallluxos,
    // Mutating — networking & updates
    Netset,
    Logset,
    Updateset,
    Updaterun,
}

/// Descriptor for a single LuxOS REST command.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LuxosCommandDescriptor {
    pub command: LuxosCommand,
    /// Wire-form `?command=…` token.
    pub name: &'static str,
    pub auth: LuxosAuthTier,
    pub kind: LuxosCommandKind,
    /// Free-text parameter shape for documentation. Empty if no parameter.
    pub parameter_shape: &'static str,
    /// True iff the SPA actually invokes this command via `$v(...)`.
    pub verified_in_spa: bool,
}

/// Look up the canonical descriptor for a command.
pub fn descriptor(command: LuxosCommand) -> LuxosCommandDescriptor {
    use LuxosAuthTier::{None as A_None, Session as A_Session};
    use LuxosCommand::*;
    use LuxosCommandKind::*;
    let (name, auth, kind, parameter_shape, verified) = match command {
        // Read-only
        Version => ("version", A_None, Read, "", true),
        Summary => ("summary", A_None, Read, "", true),
        Stats => ("stats", A_None, Read, "", true),
        Config => ("config", A_None, Read, "", true),
        Devs => ("devs", A_None, Read, "", true),
        Edevs => ("edevs", A_None, Read, "", true),
        Devdetails => ("devdetails", A_None, Read, "", true),
        Pools => ("pools", A_None, Read, "", true),
        Groups => ("groups", A_None, Read, "", true),
        Poolopts => ("poolopts", A_None, Read, "", true),
        Psuget => ("psuget", A_None, Read, "", true),
        Coin => ("coin", A_None, Read, "", true),
        Lcd => ("lcd", A_None, Read, "", true),
        Asccount => ("asccount", A_None, Read, "", true),
        Tempctrl => ("tempctrl", A_None, Read, "", true),
        Temps => ("temps", A_None, Read, "", true),
        Tempsensor => ("tempsensor", A_None, Read, "", true),
        Tunerstatus => ("tunerstatus", A_None, Read, "", true),
        Profiles => ("profiles", A_None, Read, "", true),
        Profileget => ("profileget", A_None, Read, "<profile_name>", true),
        Metrics => (
            "metrics",
            A_None,
            Read,
            "bucket=<name>[,window=<secs>]",
            true,
        ),
        Events => ("events", A_None, Read, "", true),
        Fans => ("fans", A_None, Read, "", true),
        Power => ("power", A_None, Read, "", true),
        Limits => ("limits", A_None, Read, "", true),
        Hashboardopts => ("hashboardopts", A_None, Read, "", true),
        Atm => ("atm", A_None, Read, "", true),
        Autotunerget => ("autotunerget", A_None, Read, "", true),
        Frequencyget => ("frequencyget", A_None, Read, "<board_id>", true),
        Voltageget => ("voltageget", A_None, Read, "<board_id>", true),
        Healthchipget => ("healthchipget", A_None, Read, "<board_id>", true),
        Systemaudit => ("systemaudit", A_None, Read, "", true),
        // Session lifecycle
        Logon => ("logon", A_None, Read, "", true),
        Session => ("session", A_Session, Read, "<sid>", false),
        Logoff => ("logoff", A_Session, Lifecycle, "<sid>", true),
        Kill => ("kill", A_Session, Lifecycle, "<sid>", true),
        // Mutating — pools
        Addgroup => ("addgroup", A_Session, Write, "<sid>,<name>", true),
        Removegroup => ("removegroup", A_Session, Write, "<sid>,<group_id>", true),
        Groupquota => ("groupquota", A_Session, Write, "<sid>,<quota>", true),
        Addpool => (
            "addpool",
            A_Session,
            Write,
            "<sid>,<url>,<user>,<password>",
            true,
        ),
        Removepool => ("removepool", A_Session, Write, "<sid>,<pool_id>", true),
        Enablepool => ("enablepool", A_Session, Write, "<sid>,<pool_id>", false),
        Disablepool => ("disablepool", A_Session, Write, "<sid>,<pool_id>", false),
        Switchpool => ("switchpool", A_Session, Write, "<sid>,<pool_id>", true),
        Pooloptsset => (
            "pooloptsset",
            A_Session,
            Write,
            "<sid>,<key>=<val>,...",
            true,
        ),
        // Mutating — profiles & autotuner
        Profilenew => (
            "profilenew",
            A_Session,
            Write,
            "<sid>,<name>,<freq>,<voltage>",
            true,
        ),
        Profileset => ("profileset", A_Session, Write, "<sid>,<name>", true),
        Profilerem => ("profilerem", A_Session, Write, "<sid>,<name>", true),
        Profilerestore => ("profilerestore", A_Session, Write, "<sid>,<name>", true),
        Autotunerset => (
            "autotunerset",
            A_Session,
            Write,
            "<sid>,enabled=<bool>",
            true,
        ),
        Atmset => ("atmset", A_Session, Write, "<sid>,<key>=<val>,...", true),
        // Mutating — thermal & power
        Tempctrlset => (
            "tempctrlset",
            A_Session,
            Write,
            "<sid>,<target>,<hot>,<panic>",
            true,
        ),
        Tempsensorset => (
            "tempsensorset",
            A_Session,
            Write,
            "<sid>,<key>=<val>,...",
            true,
        ),
        Fanset => ("fanset", A_Session, Write, "<sid>,<key>=<val>,...", true),
        Powertargetset => (
            "powertargetset",
            A_Session,
            Write,
            "<sid>,power=<watts>",
            true,
        ),
        Psuset => ("psuset", A_Session, Write, "<sid>,<key>=<val>,...", true),
        Hashboardoptsset => (
            "hashboardoptsset",
            A_Session,
            Write,
            "<sid>,<key>=<val>,...",
            true,
        ),
        Curtail => ("curtail", A_Session, Write, "<sid>,<active|sleep>", true),
        Immersionswitch => ("immersionswitch", A_Session, Write, "<sid>,<bool>", true),
        Ledset => ("ledset", A_Session, Write, "<sid>,<color>,<state>", true),
        // Mutating — lifecycle
        Reboot => (
            "reboot",
            A_Session,
            Lifecycle,
            "<sid>,<quick_reboot|long_reboot|no_reboot>",
            true,
        ),
        Rebootdevice => ("rebootdevice", A_Session, Lifecycle, "<sid>", true),
        Resetminer => ("resetminer", A_Session, Lifecycle, "<sid>", true),
        Uninstallluxos => ("uninstallluxos", A_Session, Destructive, "<sid>", true),
        // Mutating — networking & updates
        Netset => ("netset", A_Session, Write, "<sid>,<key>=<val>,...", true),
        Logset => ("logset", A_Session, Write, "<sid>,file_level=<level>", true),
        Updateset => ("updateset", A_Session, Write, "<sid>,<key>=<val>,...", true),
        Updaterun => ("updaterun", A_Session, Lifecycle, "<sid>", true),
    };
    LuxosCommandDescriptor {
        command,
        name,
        auth,
        kind,
        parameter_shape,
        verified_in_spa: verified,
    }
}

/// True iff this command may erase flash, remove the installation, or
/// trigger stock-firmware fallback. The dashboard MUST require an
/// explicit confirmation before invoking these.
pub fn is_destructive(command: LuxosCommand) -> bool {
    descriptor(command).kind == LuxosCommandKind::Destructive
}

/// True iff this command is a session-bound mutating operation.
pub fn requires_session(command: LuxosCommand) -> bool {
    descriptor(command).auth == LuxosAuthTier::Session
}

/// All commands in a stable enumeration order. Used by tests + the
/// `dcent-toolbox` parity audit.
pub const ALL_COMMANDS: &[LuxosCommand] = &[
    LuxosCommand::Version,
    LuxosCommand::Summary,
    LuxosCommand::Stats,
    LuxosCommand::Config,
    LuxosCommand::Devs,
    LuxosCommand::Edevs,
    LuxosCommand::Devdetails,
    LuxosCommand::Pools,
    LuxosCommand::Groups,
    LuxosCommand::Poolopts,
    LuxosCommand::Psuget,
    LuxosCommand::Coin,
    LuxosCommand::Lcd,
    LuxosCommand::Asccount,
    LuxosCommand::Tempctrl,
    LuxosCommand::Temps,
    LuxosCommand::Tempsensor,
    LuxosCommand::Tunerstatus,
    LuxosCommand::Profiles,
    LuxosCommand::Profileget,
    LuxosCommand::Metrics,
    LuxosCommand::Events,
    LuxosCommand::Fans,
    LuxosCommand::Power,
    LuxosCommand::Limits,
    LuxosCommand::Hashboardopts,
    LuxosCommand::Atm,
    LuxosCommand::Autotunerget,
    LuxosCommand::Frequencyget,
    LuxosCommand::Voltageget,
    LuxosCommand::Healthchipget,
    LuxosCommand::Systemaudit,
    LuxosCommand::Logon,
    LuxosCommand::Session,
    LuxosCommand::Logoff,
    LuxosCommand::Kill,
    LuxosCommand::Addgroup,
    LuxosCommand::Removegroup,
    LuxosCommand::Groupquota,
    LuxosCommand::Addpool,
    LuxosCommand::Removepool,
    LuxosCommand::Enablepool,
    LuxosCommand::Disablepool,
    LuxosCommand::Switchpool,
    LuxosCommand::Pooloptsset,
    LuxosCommand::Profilenew,
    LuxosCommand::Profileset,
    LuxosCommand::Profilerem,
    LuxosCommand::Profilerestore,
    LuxosCommand::Autotunerset,
    LuxosCommand::Atmset,
    LuxosCommand::Tempctrlset,
    LuxosCommand::Tempsensorset,
    LuxosCommand::Fanset,
    LuxosCommand::Powertargetset,
    LuxosCommand::Psuset,
    LuxosCommand::Hashboardoptsset,
    LuxosCommand::Curtail,
    LuxosCommand::Immersionswitch,
    LuxosCommand::Ledset,
    LuxosCommand::Reboot,
    LuxosCommand::Rebootdevice,
    LuxosCommand::Resetminer,
    LuxosCommand::Uninstallluxos,
    LuxosCommand::Netset,
    LuxosCommand::Logset,
    LuxosCommand::Updateset,
    LuxosCommand::Updaterun,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_all_documented_commands() {
        // E-rest-api-8080.md §3.3 categories: 32 read-only + 4 session
        // lifecycle + 9 pools + 6 profiles/autotuner + 9 thermal/power
        // + 4 mutating lifecycle + 4 networking/updates = 68 commands
        // total (3 of which — session, enablepool, disablepool — are
        // TCP-confirmed but not SPA-verified).
        assert_eq!(ALL_COMMANDS.len(), 68);
    }

    #[test]
    fn read_only_commands_have_auth_none() {
        // The 32 read commands in §3.3 must all be auth=None.
        let read_commands = [
            LuxosCommand::Version,
            LuxosCommand::Summary,
            LuxosCommand::Stats,
            LuxosCommand::Config,
            LuxosCommand::Devs,
            LuxosCommand::Pools,
            LuxosCommand::Profiles,
            LuxosCommand::Tempctrl,
            LuxosCommand::Atm,
            LuxosCommand::Logon, // logon is the auth-none gateway
        ];
        for cmd in read_commands {
            assert_eq!(
                descriptor(cmd).auth,
                LuxosAuthTier::None,
                "{:?} must be auth=None",
                cmd
            );
        }
    }

    #[test]
    fn mutating_commands_require_session() {
        // Every mutating + lifecycle command needs a session.
        for cmd in [
            LuxosCommand::Addpool,
            LuxosCommand::Switchpool,
            LuxosCommand::Profileset,
            LuxosCommand::Atmset,
            LuxosCommand::Tempctrlset,
            LuxosCommand::Fanset,
            LuxosCommand::Psuset,
            LuxosCommand::Curtail,
            LuxosCommand::Reboot,
            LuxosCommand::Resetminer,
            LuxosCommand::Uninstallluxos,
            LuxosCommand::Netset,
            LuxosCommand::Updaterun,
        ] {
            assert!(requires_session(cmd), "{:?} must require session", cmd);
        }
    }

    #[test]
    fn uninstall_luxos_is_destructive() {
        // The single canonical destructive command. flash_erase mtd5+11.
        assert!(is_destructive(LuxosCommand::Uninstallluxos));
        // Reboot / resetminer are NOT destructive (no flash erase).
        assert!(!is_destructive(LuxosCommand::Reboot));
        assert!(!is_destructive(LuxosCommand::Resetminer));
        assert!(!is_destructive(LuxosCommand::Rebootdevice));
    }

    #[test]
    fn descriptor_names_match_re_doc_verbatim() {
        // Spot-check: command tokens are exactly as they appear in the
        // `?command=…` URL.
        assert_eq!(descriptor(LuxosCommand::Stats).name, "stats");
        assert_eq!(descriptor(LuxosCommand::Tempctrlset).name, "tempctrlset");
        assert_eq!(descriptor(LuxosCommand::Pooloptsset).name, "pooloptsset");
        assert_eq!(
            descriptor(LuxosCommand::Uninstallluxos).name,
            "uninstallluxos"
        );
        assert_eq!(
            descriptor(LuxosCommand::Healthchipget).name,
            "healthchipget"
        );
        assert_eq!(
            descriptor(LuxosCommand::Immersionswitch).name,
            "immersionswitch"
        );
    }

    #[test]
    fn lifecycle_kind_classifies_logoff_kill_and_reboots() {
        // `logoff`, `kill`, `reboot`, `rebootdevice`, `resetminer`,
        // `updaterun` are all Lifecycle (mutating but not destructive).
        for cmd in [
            LuxosCommand::Logoff,
            LuxosCommand::Kill,
            LuxosCommand::Reboot,
            LuxosCommand::Rebootdevice,
            LuxosCommand::Resetminer,
            LuxosCommand::Updaterun,
        ] {
            assert_eq!(descriptor(cmd).kind, LuxosCommandKind::Lifecycle);
        }
    }

    #[test]
    fn parameter_shape_pinned_for_session_commands() {
        // Session-bound commands always start their parameter with `<sid>`.
        for cmd in ALL_COMMANDS.iter().copied() {
            let d = descriptor(cmd);
            if d.auth == LuxosAuthTier::Session && !d.parameter_shape.is_empty() {
                assert!(
                    d.parameter_shape.starts_with("<sid>"),
                    "{:?} parameter shape '{}' must start with <sid>",
                    cmd,
                    d.parameter_shape
                );
            }
        }
    }

    #[test]
    fn most_commands_are_spa_verified() {
        // E-rest-api-8080.md notes only `session`, `enablepool`,
        // `disablepool` are TCP-confirmed without SPA invocation. All
        // other 64 are SPA-verified.
        let unverified_count = ALL_COMMANDS
            .iter()
            .filter(|cmd| !descriptor(**cmd).verified_in_spa)
            .count();
        assert_eq!(unverified_count, 3);
    }

    #[test]
    fn auth_tier_serializes_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LuxosAuthTier::None).unwrap(),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&LuxosAuthTier::Session).unwrap(),
            "\"session\""
        );
    }

    #[test]
    fn command_kind_serializes_in_snake_case() {
        for (kind, expected) in [
            (LuxosCommandKind::Read, "\"read\""),
            (LuxosCommandKind::Write, "\"write\""),
            (LuxosCommandKind::Lifecycle, "\"lifecycle\""),
            (LuxosCommandKind::Destructive, "\"destructive\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), expected);
        }
    }

    #[test]
    fn command_round_trips_through_serde() {
        for cmd in ALL_COMMANDS.iter().copied() {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: LuxosCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(cmd, back);
        }
    }

    #[test]
    fn command_serde_lowercase_token_equals_descriptor_name() {
        // The serde wire form is the exact `?command=…` token. Cross-check
        // every variant's serde name against its descriptor.name.
        for cmd in ALL_COMMANDS.iter().copied() {
            let serde_form = serde_json::to_string(&cmd).unwrap();
            let stripped = serde_form.trim_matches('"');
            assert_eq!(
                stripped,
                descriptor(cmd).name,
                "serde form '{}' must match descriptor name for {:?}",
                stripped,
                cmd
            );
        }
    }

    #[test]
    fn all_command_names_are_unique() {
        // No two variants share a `?command=…` token.
        use std::collections::HashSet;
        let mut seen: HashSet<&'static str> = HashSet::new();
        for cmd in ALL_COMMANDS.iter().copied() {
            let name = descriptor(cmd).name;
            assert!(seen.insert(name), "duplicate command token: {}", name);
        }
    }

    #[test]
    fn read_kind_for_logon_and_session() {
        // logon + session are read-class (returns SessionID / status, no
        // mutation). The lifecycle teardown commands are logoff/kill.
        assert_eq!(descriptor(LuxosCommand::Logon).kind, LuxosCommandKind::Read);
        assert_eq!(
            descriptor(LuxosCommand::Session).kind,
            LuxosCommandKind::Read
        );
    }
}
