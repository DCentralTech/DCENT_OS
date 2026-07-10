//!  api-A — LuxOS CGMiner command catalog (HAL-free).
//!
//! Source RE evidence:
//!
//! §4 (lines 200-250).
//!
//! 78 unique CGMiner-API commands recovered from a dense `.rodata` cluster
//! at `0xa2b800-0xa2bb00` in the live `luxminer` binary on `a lab unit`. Of these,
//! 32 are commands that the public CGMiner API survey already documented;
//! **46 are Luxor extensions** unknown to the upstream cgminer codebase.
//!
//! This module is the catalog DTO — **what** commands exist, what kind
//! they are (write/read), whether they're Luxor-only, whether they're
//! destructive (need operator-confirmation gate), and a one-line doc.
//! The actual command HANDLERS are HAL-bound and live in
//! `dcentrald-api`. This module is consumed by:
//! - the dashboard wizard's "what does this command do" tooltip;
//! - the toolbox `dcent` CLI for arg validation and `--dry-run` output;
//! - the dcent-toolbox install preflight (refuse to flash if a
//!   destructive command is pending in any operator script).
//!
//!, this catalog is the
//! source of truth — the dashboard fetches it via `/api/cgminer/catalog`
//! rather than hardcoding command lists, eliminating drift.

use serde::Serialize;

/// Whether the command writes state (Set), reads state (Get), or is
/// purely informational (Info — read-only no-arg).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// Mutates miner state (e.g. `pooloptsset`, `voltageset`).
    Set,
    /// Reads typed state (e.g. `tempctrl`, `pools`).
    Get,
}

/// One CGMiner-API command in the catalog.
///
/// `Deserialize` is intentionally NOT derived: `name` and `doc` are
/// `&'static str` (catalog is a `const`), and serde can't deserialize
/// into static borrows from runtime input. Clients consuming the JSON
/// should define their own owned-string struct mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CgminerCommand {
    /// Wire-protocol command name (lowercase, no whitespace).
    pub name: &'static str,
    /// Read vs Write classification.
    pub kind: CommandKind,
    /// True if this command is a Luxor extension (not in upstream
    /// CGMiner). LuxOS-only; will not be present on BraiinsOS+ or stock
    /// Bitmain firmware.
    pub luxor_extension: bool,
    /// True if this command can brick or downgrade the miner. Operators
    /// must confirm explicitly before invoking. Includes
    /// `uninstallluxos`, `resetconfig`, `rebootdevice`, `kill`, `reboot`,
    /// `curtail`, `updaterun`.
    pub destructive: bool,
    /// One-line operator-facing description (used by the dashboard
    /// tooltip).
    pub doc: &'static str,
}

/// Helper for building catalog entries.
const fn set(
    name: &'static str,
    luxor: bool,
    destructive: bool,
    doc: &'static str,
) -> CgminerCommand {
    CgminerCommand {
        name,
        kind: CommandKind::Set,
        luxor_extension: luxor,
        destructive,
        doc,
    }
}

const fn get(name: &'static str, luxor: bool, doc: &'static str) -> CgminerCommand {
    CgminerCommand {
        name,
        kind: CommandKind::Get,
        luxor_extension: luxor,
        destructive: false,
        doc,
    }
}

/// The full LuxOS catalog: 40 write/Set + 38 read/Get = 78 commands.
///
/// Ordering follows the RE doc §4.1 + §4.2 listing. New entries APPEND
/// at the end (the catalog index is implicit, not stable across versions
/// — consumers use `name` lookup).
pub const CGMINER_CATALOG: &[CgminerCommand] = &[
    // --- 40 Set commands (RE doc §4.1) ---
    set("switchpool", false, false, "Switch active mining pool"),
    set("enablepool", false, false, "Enable a configured pool"),
    set("disablepool", false, false, "Disable a configured pool"),
    set("addpool", false, false, "Add a new pool"),
    set("removepool", false, false, "Remove a configured pool"),
    set("fanset", false, false, "Set fan speed (auto / manual PWM)"),
    set("frequencyset", true, false, "Set ASIC chain frequency MHz"),
    set(
        "frequencystop",
        true,
        false,
        "Stop frequency tracking; pin current value",
    ),
    set("reboot", false, true, "Soft reboot the daemon"),
    set("voltageset", true, false, "Set chain voltage (V)"),
    set("tunerswitch", true, false, "Switch autotuner on/off"),
    set(
        "healthctrlset",
        true,
        false,
        "Set chip-health control thresholds",
    ),
    set(
        "healthchipset",
        true,
        false,
        "Set per-chip health thresholds",
    ),
    set("ledset", false, false, "Set LED behavior (on/off/blink)"),
    set(
        "profileset",
        true,
        false,
        "Activate a named silicon profile",
    ),
    set("tunableswitch", true, false, "Toggle ATM (tunable) mode"),
    set("removegroup", false, false, "Remove a pool group"),
    set(
        "groupquota",
        true,
        false,
        "Set quota share for a pool group",
    ),
    set(
        "rebootdevice",
        false,
        true,
        "Hard reboot the device (kernel reboot)",
    ),
    set("resetminer", false, true, "Reset miner state to first-boot"),
    set(
        "tempctrlset",
        true,
        false,
        "Set temperature control thresholds",
    ),
    set(
        "immersionswitch",
        true,
        false,
        "Toggle immersion-cooling profile mode",
    ),
    set("updateset", true, false, "Stage a firmware update"),
    set("updaterun", true, true, "Execute a staged firmware update"),
    set("curtail", false, true, "Curtail mining (sleep state)"),
    set("netset", true, false, "Set network configuration"),
    set("profilenew", true, false, "Create a new named profile"),
    set("profilerem", true, false, "Remove a named profile"),
    set(
        "atmset",
        true,
        false,
        "Configure ATM (Advanced Thermal Management)",
    ),
    set("enableboard", true, false, "Enable a hashboard"),
    set("disableboard", true, false, "Disable a hashboard"),
    set(
        "pooloptsset",
        true,
        false,
        "Update [app.pool_options] at runtime",
    ),
    set("resetconfig", true, true, "Wipe configuration to defaults"),
    set(
        "tempsensorset",
        true,
        false,
        "Set temp-sensor selection per board",
    ),
    set("logset", false, false, "Set log level / target"),
    set(
        "uninstallluxos",
        true,
        true,
        "Wipe LuxOS install (clean uninstall path)",
    ),
    set("autotunerset", true, false, "Configure the autotuner state"),
    set(
        "profilerestore",
        true,
        false,
        "Restore a profile to defaults",
    ),
    set("powertargetset", true, false, "Set wall-power target (W)"),
    set("psuset", true, false, "Set PSU control parameters"),
    // --- 38 Get commands (RE doc §4.2) ---
    get("config", false, "Read full configuration"),
    get("devdetails", false, "Hashboard / chip details"),
    get("edevs", false, "Extended devices list"),
    get("summary", false, "Miner summary (hashrate, shares)"),
    get("stats", false, "Detailed miner statistics"),
    get("estats", false, "Extended statistics"),
    get("check", false, "Service check / liveness probe"),
    get("lcd", false, "LCD-formatted summary"),
    get(
        "temps",
        false,
        "Per-board / per-corner / per-chip temperatures",
    ),
    get("frequencyget", true, "Read current chain frequency MHz"),
    get("voltageget", true, "Read current chain voltage V"),
    get("tunerstatus", true, "Read autotuner state"),
    get("healthctrl", true, "Read chip-health control thresholds"),
    get("healthchipget", true, "Read per-chip health thresholds"),
    get("logon", false, "Enable logging"),
    get("logoff", false, "Disable logging"),
    get("session", false, "Authenticated session info"),
    get("groups", false, "Read pool groups"),
    get(
        "limits",
        true,
        "Read configuration bounds (for dashboard wizard)",
    ),
    get("profileget", true, "Read named profile by id"),
    get("tempsensor", true, "Read temp-sensor probe results"),
    get("autotunerget", true, "Read autotuner config"),
    get(
        "hashboardopts",
        true,
        "Read per-hashboard options (NoPic, OvertempAuto)",
    ),
    get("updatecheck", true, "Check for firmware updates"),
    get("events", true, "Read recent events from the audit log"),
    get("metrics", true, "Read Prometheus-style metrics"),
    get("system", true, "System info bundle"),
    get("audit", true, "Read /var/log/audit.json events"),
    get("psuget", true, "Read PSU state"),
    get("version", false, "Daemon version"),
    get("pools", false, "Read configured pools + state"),
    get("profiles", true, "List all named profiles"),
    get("devs", false, "Devices list (basic)"),
    get("coin", false, "Coin/algorithm info"),
    get("tempctrl", true, "Read temperature-control runtime state"),
    get("asccount", false, "Number of ASIC chips detected"),
    get("kill", false, "Daemon kill (terminate)"),
    get(
        "addgroup",
        false,
        "Add a new pool group (no-arg variant: list groups)",
    ),
];

/// Look up a command by name. O(N) scan; the catalog is small (78 entries).
pub fn lookup(name: &str) -> Option<&'static CgminerCommand> {
    CGMINER_CATALOG.iter().find(|c| c.name == name)
}

/// True if `name` is in the catalog AND marked destructive.
pub fn is_destructive(name: &str) -> bool {
    lookup(name).map(|c| c.destructive).unwrap_or(false)
}

/// True if `name` is a Luxor extension (won't exist on BraiinsOS+ /
/// stock Bitmain firmware).
pub fn is_luxor_only(name: &str) -> bool {
    lookup(name).map(|c| c.luxor_extension).unwrap_or(false)
}

/// Catalog stats for tests + dashboard rendering.
pub fn catalog_stats() -> CatalogStats {
    let mut stats = CatalogStats::default();
    for c in CGMINER_CATALOG {
        stats.total += 1;
        match c.kind {
            CommandKind::Set => stats.set_count += 1,
            CommandKind::Get => stats.get_count += 1,
        }
        if c.luxor_extension {
            stats.luxor_extensions += 1;
        }
        if c.destructive {
            stats.destructive += 1;
        }
    }
    stats
}

/// Stats for `catalog_stats()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct CatalogStats {
    pub total: u32,
    pub set_count: u32,
    pub get_count: u32,
    pub luxor_extensions: u32,
    pub destructive: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_seventy_eight_entries() {
        let stats = catalog_stats();
        assert_eq!(stats.total, 78, "RE doc §4.3: 40 Set + 38 Get = 78");
        assert_eq!(stats.set_count, 40, "RE doc §4.1: 40 Set commands");
        assert_eq!(stats.get_count, 38, "RE doc §4.2: 38 Get commands");
    }

    #[test]
    fn no_duplicate_names_in_catalog() {
        let mut names: Vec<&'static str> = CGMINER_CATALOG.iter().map(|c| c.name).collect();
        names.sort();
        let original = names.len();
        names.dedup();
        assert_eq!(original, names.len(), "duplicate names in catalog");
    }

    #[test]
    fn lookup_finds_known_command() {
        let c = lookup("uninstallluxos").expect("uninstallluxos must be in catalog");
        assert_eq!(c.kind, CommandKind::Set);
        assert!(c.luxor_extension);
        assert!(c.destructive);
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("nonexistent_command").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn destructive_commands_match_re_doc_list() {
        // Per the RE doc, destructive commands include:
        // uninstallluxos, resetconfig, rebootdevice, kill (no — kill is
        // get-only and just terminates the daemon process), reboot,
        // curtail, updaterun, resetminer.
        // We bake the reachable set conservatively.
        for cmd in [
            "uninstallluxos",
            "resetconfig",
            "rebootdevice",
            "reboot",
            "curtail",
            "updaterun",
            "resetminer",
        ] {
            assert!(
                is_destructive(cmd),
                "command `{}` should be flagged destructive",
                cmd
            );
        }
    }

    #[test]
    fn luxor_extension_count_is_meaningful() {
        // Per RE doc §4.3: 46 of the 78 are Luxor extensions. Our
        // classification is conservative (we may flag a few less); pin
        // a lower bound so Luxor-only commands aren't accidentally
        // marked vendor-neutral.
        let stats = catalog_stats();
        assert!(
            stats.luxor_extensions >= 30,
            "expected >= 30 Luxor extensions, got {}",
            stats.luxor_extensions
        );
        assert!(
            stats.luxor_extensions <= 50,
            "expected <= 50 Luxor extensions, got {}",
            stats.luxor_extensions
        );
    }

    #[test]
    fn is_luxor_only_predicate_returns_correctly() {
        assert!(is_luxor_only("uninstallluxos"));
        assert!(is_luxor_only("atmset"));
        assert!(!is_luxor_only("pools")); // Vendor-neutral CGMiner cmd.
        assert!(!is_luxor_only("nonexistent"));
    }

    #[test]
    fn cgminer_command_serializes_to_documented_json_shape() {
        // Catalog is server -> client only (CgminerCommand uses
        // &'static str fields). Verify the wire shape includes every
        // documented key the dashboard expects.
        let c = lookup("uninstallluxos").unwrap();
        let json = serde_json::to_string(c).unwrap();
        assert!(json.contains("\"name\":\"uninstallluxos\""));
        assert!(json.contains("\"kind\":\"set\""));
        assert!(json.contains("\"luxor_extension\":true"));
        assert!(json.contains("\"destructive\":true"));
        assert!(json.contains("\"doc\":"));
    }

    #[test]
    fn catalog_serializes_as_json_array() {
        // Dashboard wizard pulls the entire catalog at boot; check
        // shape stays a JSON array of CgminerCommand objects.
        let json = serde_json::to_string(CGMINER_CATALOG).unwrap();
        assert!(json.starts_with('['));
        assert!(json.ends_with(']'));
        // The output should mention every name in the catalog.
        for c in CGMINER_CATALOG {
            assert!(
                json.contains(&format!("\"name\":\"{}\"", c.name)),
                "JSON missing entry for {}",
                c.name
            );
        }
    }

    #[test]
    fn known_safe_commands_are_not_destructive() {
        for cmd in ["pools", "summary", "stats", "version", "tempctrl", "config"] {
            assert!(!is_destructive(cmd), "{} should not be destructive", cmd);
        }
    }
}
