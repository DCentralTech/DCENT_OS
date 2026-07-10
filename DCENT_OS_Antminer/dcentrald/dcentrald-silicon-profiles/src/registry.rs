//!  W5-A: runtime profile registry + JSON-bundle loader.
//!
//! Replaces the compile-time const arrays with a disk-backed registry
//! at `/etc/dcentrald/profiles.d/`. The const arrays remain compiled in
//! as a fallback (the `load_baked` path is the migration script's
//! responsibility — see `scripts/migrate-baked-profiles.py` in W5-D).
//!
//! Spec: `plans/wave4-profile-import-infrastructure.md` §B.
//!
//! Safety: per §H, the loader enforces these rules:
//! - schema_version == 1 (else reject)
//! - voltage_v in chip-rail-or-chain-rail range
//! - freq_mhz in [100, 1000]
//! - step values unique within a bundle
//! - SECURE_BOOT_SET-tainted firmware → ALWAYS reject (eFuse-burning blob,
//!)
//! - Hashcore SHA-512 root hash present → ALWAYS reject (per
//!   )
//!
//! Loader never panics; malformed files are logged and skipped.

use crate::{Profile, ProfileSource};
use dcentrald_api_types::chip_init::ChipFamily;
use dcentrald_api_types::power_profile_preset::MinerModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

// ---------------------------------------------------------------------------
// JSON profile-bundle schema (spec §A).
// ---------------------------------------------------------------------------

/// One full profile bundle. Wire format documented at
/// `plans/wave4-profile-import-infrastructure.md` §A.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileBundle {
    pub schema_version: u32,
    pub miner_model: MinerModel,
    pub hashboard: String,
    pub chip: ChipFamily,
    pub source: ProfileSourceMetadata,
    pub source_class: ProfileSource,
    pub presets: Vec<Profile>,
    #[serde(default)]
    pub metadata: ProfileMetadata,
}

/// Provenance metadata for the firmware extraction this bundle came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSourceMetadata {
    pub vendor: String,
    pub firmware_version: String,
    pub extracted_from_sha256: String,
    pub extraction_date: String,
    #[serde(default)]
    pub extracted_by: Option<String>,
}

/// Optional metadata flags. Used for safety enforcement (per spec §H)
/// and operator-visible warnings (atlas SSH key, hotelfee devfee).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfileMetadata {
    /// VNish hotelfee.json devfee was present in the source firmware.
    /// (HIGH severity).
    #[serde(default)]
    pub vnish_hotelfee_devfee_present: bool,

    /// Source firmware contained the `Amlogic_S21/SECURE_BOOT_SET` blob
    /// (1024 B, SHA256 prefix `c3b77476bfc640ed`). Per
    /// . **Hard reject** — this is
    /// an eFuse-burning blob with no recovery path.
    #[serde(default)]
    pub secure_boot_set_seen: bool,

    /// Source firmware contained the Hashcore Toolkit's universal SHA-512
    /// root hash injection..
    /// **Hard reject** — this is a backdoor universal across every
    /// unlocked miner.
    #[serde(default)]
    pub hashcore_root_hash_seen: bool,

    /// VNish atlas@anthill.farm RSA-4096 SSH key was present in the
    /// source firmware.. NOT a
    /// hard reject — operator gets a warning during import.
    #[serde(default)]
    pub atlas_ssh_key_present: bool,

    /// Free-form notes from the operator/extractor.
    #[serde(default)]
    pub notes: String,
}

/// Composite key for the registry's `by_key` map.
pub type ProfileKey = (MinerModel, String, ChipFamily);

/// Composite key for the registry's `active_by_chain` map. The
/// `(MinerModel, hashboard)` tuple uniquely identifies a chain whose
/// operator-selected silicon profile the autotuner should consume.
/// W13-A intentionally does NOT carry a `chain_id: u8` — the same
/// silicon profile applies to every chain on the same hashboard
/// product (BHB42601 etc.), which is the granularity W8-D's
/// `PUT /api/profiles/silicon/active` was designed around.
pub type ActiveProfileKey = (MinerModel, String);

// ---------------------------------------------------------------------------
// Registry.
// ---------------------------------------------------------------------------

/// Disk-backed profile registry. One instance per `dcentrald` process,
/// shared via `global()` `RwLock`.
#[derive(Debug, Default)]
pub struct ProfileRegistry {
    by_key: HashMap<ProfileKey, ProfileBundle>,
    paths: HashMap<ProfileKey, PathBuf>,
    /// W13-A — operator-selected active silicon profile per
    /// (MinerModel, hashboard) chain. Populated by the
    /// `PUT /api/profiles/silicon/active` REST handler after the
    /// requested profile id has been resolved against `by_key`. The
    /// value is the wire profile id (`<model>__<hashboard>__<chip>__<source_class>`)
    /// the wire shape the API exposes; it can be parsed back into a
    /// `(MinerModel, hashboard, ChipFamily, ProfileSource)` tuple via
    /// the `routes::profiles::parse_profile_id` helper. The autotuner
    /// reads this via `get_active_profile_for_chain` at the top of each
    /// iteration to decide which preset table to apply.
    active_by_chain: HashMap<ActiveProfileKey, String>,
}

/// Errors the loader can return. Per spec §B, malformed files are
/// always logged + skipped — `load_from_disk` does NOT propagate per-file
/// errors. Errors here are only for catastrophic failures (e.g. cannot
/// read the directory at all).
#[derive(Debug, thiserror::Error)]
pub enum ProfileLoadError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("malformed JSON in {path}: {source}")]
    MalformedJson {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("schema version mismatch in {path}: got {got}, expected {expected}")]
    SchemaVersionMismatch {
        path: PathBuf,
        got: u32,
        expected: u32,
    },
    #[error("validation error in {path}: {reason}")]
    Validation { path: PathBuf, reason: String },
}

/// Aggregate statistics from a `load_from_disk` or `reload` call.
#[derive(Debug, Default)]
pub struct ReloadStats {
    pub loaded: usize,
    pub skipped: usize,
    pub errors: Vec<ProfileLoadError>,
}

impl ProfileRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `*.json` file under `dir`, recursing one level deep
    /// (to pick up the recommended `vendor/`, `operator/`, `baked/`
    /// subdirs from spec §B).
    ///
    /// Per the spec: malformed files are logged + skipped, not fatal.
    /// The function only returns `Err` for top-level I/O failures.
    pub fn load_from_disk(dir: &Path) -> Result<(Self, ReloadStats), ProfileLoadError> {
        let mut registry = Self::new();
        let mut stats = ReloadStats::default();

        if !dir.exists() {
            // Per spec §B "Boot path: ... if disk dir is missing or all
            // files malformed, fall back silently to baked." Treat as
            // success with zero loaded.
            return Ok((registry, stats));
        }

        let entries = std::fs::read_dir(dir).map_err(|e| ProfileLoadError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;

        let mut json_files: Vec<PathBuf> = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Recurse one level deep.
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        if sub_path.is_file()
                            && sub_path.extension().and_then(|s| s.to_str()) == Some("json")
                        {
                            json_files.push(sub_path);
                        }
                    }
                }
            } else if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
                json_files.push(path);
            }
        }

        for path in json_files {
            match load_one(&path) {
                Ok(bundle) => {
                    let key = (bundle.miner_model, bundle.hashboard.clone(), bundle.chip);
                    // Resolve same-key collisions (the same
                    // `(model, hashboard, chip)` present in more than one
                    // of the `vendor/`/`operator/`/`baked/` subdirs) by
                    // authority rank, NOT by directory-walk order — see
                    // `insert_ranked`. A blind `by_key.insert` here let an
                    // arbitrary lower-rank vendor default shadow an
                    // operator-confirmed override depending on filesystem
                    // iteration order.
                    registry.insert_ranked(key, bundle, Some(path));
                    stats.loaded += 1;
                }
                Err(e) => {
                    stats.skipped += 1;
                    stats.errors.push(e);
                }
            }
        }

        Ok((registry, stats))
    }

    /// Compile-time fallback: returns an empty registry. The migration
    /// script in W5-D writes the 65 baked rows + ~225 vendor rows out to
    /// `/etc/dcentrald/profiles.d/baked/` so `load_from_disk` picks them
    /// up. The const arrays in the per-chip files remain compiled in
    /// for emergency-fallback diagnostic use only.
    pub fn load_baked() -> Self {
        Self::default()
    }

    /// Look up a profile preset list by (model, hashboard, chip) tuple.
    pub fn lookup(
        &self,
        model: MinerModel,
        hashboard: &str,
        chip: ChipFamily,
    ) -> Option<&[Profile]> {
        self.by_key
            .get(&(model, hashboard.to_string(), chip))
            .map(|b| b.presets.as_slice())
    }

    /// Look up the full bundle (including provenance + metadata) by tuple.
    pub fn lookup_bundle(
        &self,
        model: MinerModel,
        hashboard: &str,
        chip: ChipFamily,
    ) -> Option<&ProfileBundle> {
        self.by_key.get(&(model, hashboard.to_string(), chip))
    }

    /// Insert the entries from `other` into `self`, preferring higher
    /// `source_class.rank()`. On equal rank, the newer file `mtime`
    /// wins (per spec §B "Conflict resolution").
    pub fn merge(&mut self, other: Self) {
        for (key, other_bundle) in other.by_key {
            let other_path = other.paths.get(&key).cloned();
            self.insert_ranked(key, other_bundle, other_path);
        }
    }

    /// Insert a single `(key, bundle)` (with its optional on-disk path)
    /// into `self`, resolving a same-key collision by `source_class`
    /// rank — higher rank wins; on a rank tie the newer file `mtime`
    /// wins (per spec §B "Conflict resolution"). A lower-rank candidate
    /// is dropped, leaving the incumbent in place.
    ///
    /// This is the single source of truth for conflict resolution. Both
    /// `merge` (cross-registry) and `load_from_disk` (per-file, across
    /// the `vendor/`/`operator/`/`baked/` subdirs that can each hold a
    /// bundle for the same `(model, hashboard, chip)` key) route through
    /// it so the loser is decided by authority, NOT by directory-walk
    /// order. Without this, an operator-confirmed override imported into
    /// `operator/` could be silently shadowed by a `vendor/` default that
    /// merely happened to be read last — the W13-A pipeline regression
    /// `test_pipeline_resolves_higher_authority_over_vendor` pins it.
    fn insert_ranked(&mut self, key: ProfileKey, bundle: ProfileBundle, path: Option<PathBuf>) {
        let mut should_insert = true;

        if let Some(existing) = self.by_key.get(&key) {
            let cmp_rank = bundle
                .source_class
                .rank()
                .cmp(&existing.source_class.rank());
            match cmp_rank {
                std::cmp::Ordering::Greater => {
                    should_insert = true;
                }
                std::cmp::Ordering::Less => {
                    should_insert = false;
                }
                std::cmp::Ordering::Equal => {
                    // Tie-break: newer mtime wins.
                    let existing_mtime = self
                        .paths
                        .get(&key)
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok());
                    let candidate_mtime = path
                        .as_ref()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok());
                    match (existing_mtime, candidate_mtime) {
                        (Some(eo), Some(no)) => {
                            should_insert = no > eo;
                        }
                        (None, Some(_)) => {
                            should_insert = true;
                        }
                        _ => {
                            should_insert = false;
                        }
                    }
                }
            }
        }

        if should_insert {
            self.by_key.insert(key.clone(), bundle);
            match path {
                Some(p) => {
                    self.paths.insert(key, p);
                }
                None => {
                    // No on-disk provenance for this candidate; drop any
                    // stale path so a later mtime tie-break doesn't read
                    // the previous incumbent's file by mistake.
                    self.paths.remove(&key);
                }
            }
        }
    }

    /// Atomic reload: build a new registry from `dir`, swap into `self`.
    /// Active autotuner sessions keep their pinned `&Profile` reference
    /// until the next lookup.
    ///
    /// W13-A: `active_by_chain` is preserved across reloads so an
    /// operator's `PUT /api/profiles/silicon/active` selection survives
    /// a disk reload. Stale selections (id no longer resolves to a
    /// loaded bundle) are dropped silently — the autotuner falls back
    /// to its previous behavior on the next iteration.
    pub fn reload(&mut self, dir: &Path) -> Result<ReloadStats, ProfileLoadError> {
        let (new_registry, stats) = Self::load_from_disk(dir)?;
        self.by_key = new_registry.by_key;
        self.paths = new_registry.paths;
        // Drop active selections whose underlying bundle no longer
        // resolves. We keep the rest verbatim.
        let stale: Vec<ActiveProfileKey> = self
            .active_by_chain
            .iter()
            .filter(|((model, hashboard), _)| {
                !self
                    .by_key
                    .keys()
                    .any(|(m, h, _)| m == model && h == hashboard)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.active_by_chain.remove(&k);
        }
        Ok(stats)
    }

    /// Iterate every key in the registry.
    pub fn iter_keys(&self) -> impl Iterator<Item = &ProfileKey> {
        self.by_key.keys()
    }

    /// Number of bundles currently loaded.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    // -----------------------------------------------------------------
    // W13-A: active-profile-per-chain selection.
    //
    // The autotuner consults `get_active_profile_for_chain` at the top
    // of each iteration to decide which preset table to apply. The
    // operator selects via `PUT /api/profiles/silicon/active`, which
    // calls `set_active_profile_for_chain` after validating the
    // profile id resolves.
    // -----------------------------------------------------------------

    /// Record `profile_id` as the operator-selected active silicon
    /// profile for the given `(model, hashboard)` chain. Returns
    /// `Err(reason)` if the profile id does not resolve to a loaded
    /// bundle on `(model, hashboard)`. The id-to-tuple parser lives in
    /// `dcentrald_api::routes::profiles::parse_profile_id`; we accept
    /// the rendered string here so the registry stays agnostic of the
    /// API-side serde rendering choice.
    pub fn set_active_profile_for_chain(
        &mut self,
        model: MinerModel,
        hashboard: &str,
        profile_id: &str,
    ) -> Result<(), String> {
        // Verify at least one bundle exists for the (model, hashboard)
        // tuple before we record an active id. We deliberately do NOT
        // re-parse the id here — the API-side handler is responsible
        // for that and for rejecting bare/malformed ids before calling
        // this method.
        let any_bundle = self
            .by_key
            .keys()
            .any(|(m, h, _)| *m == model && h == hashboard);
        if !any_bundle {
            return Err(format!(
                "no profiles loaded for ({:?}, {})",
                model, hashboard
            ));
        }
        self.active_by_chain
            .insert((model, hashboard.to_string()), profile_id.to_string());
        Ok(())
    }

    /// Operator-selected active profile id for the given chain, or
    /// `None` if no selection has been recorded.
    pub fn get_active_profile_for_chain(&self, model: MinerModel, hashboard: &str) -> Option<&str> {
        self.active_by_chain
            .get(&(model, hashboard.to_string()))
            .map(|s| s.as_str())
    }

    /// Resolve the active profile selection for a chain to its full
    /// `&ProfileBundle`. Returns `None` if no selection exists OR the
    /// recorded id no longer resolves (e.g. the operator deleted the
    /// underlying file between selection and consumption).
    pub fn get_active_bundle_for_chain(
        &self,
        model: MinerModel,
        hashboard: &str,
    ) -> Option<&ProfileBundle> {
        let id = self.get_active_profile_for_chain(model, hashboard)?;
        // Profile ids encode `<model>__<hashboard>__<chip>__<source_class>`.
        // Parse the chip segment so we can lookup_bundle. We re-implement
        // a minimal parser here to keep the registry crate free of an
        // `dcentrald-api` dep.
        let parts: Vec<&str> = id.split("__").collect();
        if parts.len() != 4 {
            return None;
        }
        let chip = serde_json::from_str::<ChipFamily>(&format!("\"{}\"", parts[2])).ok()?;
        self.lookup_bundle(model, hashboard, chip)
    }

    /// Number of chains with an operator-selected active profile.
    pub fn active_selection_count(&self) -> usize {
        self.active_by_chain.len()
    }

    /// Clear the active-profile selection for a single chain. Returns
    /// the previously selected id if there was one. Used by reload paths
    /// where the underlying file was deleted.
    pub fn clear_active_profile_for_chain(
        &mut self,
        model: MinerModel,
        hashboard: &str,
    ) -> Option<String> {
        self.active_by_chain.remove(&(model, hashboard.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Validation (spec §A "Validation rules").
// ---------------------------------------------------------------------------

/// Allowed voltage ranges for a chip family. Returns one or more
/// `(min, max)` envelopes in volts; a row passes validation if its
/// `voltage_v` is inside ANY listed range.
///
/// ** W7-D fix (round-trip blocker 1)**: SHA-256 BM139x/BM136x
/// chips operate on TWO distinct voltage axes depending on where the
/// row was measured:
/// - **chain-rail** (7.5..=15.0 V): the multi-volt rail set by the
///   APW PSU + dsPIC. Baked rows in `bm139[78].rs`, `bm136[268].rs`,
///   `bm1370.rs` use this convention (e.g. 13.4..14.4 V).
/// - **chip-rail** (0.5..=2.0 V): the per-chip core voltage downstream
///   of the on-board buck. Vendor `levels.json` rows (Bitmain stock)
///   ship millivolts ÷ 1000 in this band (e.g. 1.32..1.60 V).
///
/// Allowing both bands lets the migrate-baked-profiles round-trip
/// land cleanly without inventing a `voltage_rail` enum. Per-chip-rail
/// BitAxe profiles live in their own `dcentaxe/src/chip_profiles_bitaxe.rs`
/// envelope (W5-G) and don't go through this loader.
fn chip_voltage_ranges(chip: ChipFamily) -> &'static [(f32, f32)] {
    use ChipFamily::*;
    match chip {
        // BM1387 (S9 chain-rail, PIC DAC formula).
        // Range covers the safe S9 envelope 7.94..9.44 V plus a small
        // 0.5 V margin on either end for autotuner overshoot.
        Bm1387 => &[(7.5, 10.0)],
        // Scrypt L3+/L7/L9 chain-rail.
        Bm1485 | Bm1489 => &[(7.5, 13.5)],
        // SHA-256 BM139x/BM136x: dual envelope (chain-rail OR chip-rail).
        // chain-rail 7.5..15.0 V covers APW PSU operating window
        // (live-confirmed on .79: 13.700..14.250 V on BM1362/1366/1368).
        // chip-rail 0.5..2.0 V covers Bitmain `levels.json` exports
        // (1.32..1.60 V observed on BHB42xxx hashboards).
        Bm1397 | Bm1398 | Bm1362 | Bm1366 | Bm1368 | Bm1370 => &[(0.5, 2.0), (7.5, 15.0)],
        //  W8-A: NAMED-ONLY placeholder dual envelope. Mirrors
        // the SHA-256 dual envelope (chip-rail OR chain-rail) per
        // W7-D's pattern so any baked profile written against either
        // axis can be loaded for forward compatibility. The actual
        // operating envelope of these chips is genuinely UNKNOWN per
        // W7-A — operator must NOT trust either band as a safety
        // floor/ceiling until wave-9+ live capture confirms.
        // [GAP — wave-9 live verification needed]
        Bm1360 | Bm1491 => &[(0.5, 2.0), (7.5, 15.0)],
    }
}

/// Single-envelope helper for backwards compatibility with existing
/// tests that pin a chip-rail range. Returns the **first** envelope
/// listed by `chip_voltage_ranges`; for chips with multiple envelopes,
/// callers should prefer `chip_voltage_ranges` so chain-rail rows pass.
#[cfg(test)]
fn chip_voltage_range(chip: ChipFamily) -> (f32, f32) {
    chip_voltage_ranges(chip)[0]
}

/// Validate a bundle against the loader's safety rules. Returns
/// `Err(reason)` on the first violation; `Ok(())` if all rules pass.
pub fn validate(bundle: &ProfileBundle) -> Result<(), String> {
    if bundle.schema_version != 1 {
        return Err(format!(
            "schema_version must be 1, got {}",
            bundle.schema_version
        ));
    }

    // bug-hunt LOW #8 (2026-05-28): a bundle with zero presets passed every rule
    // (the per-row loops below just don't iterate), so `load_one` returned Ok and
    // `lookup()`/`get_active_bundle_for_chain` served a profile-less bundle — a
    // silent "loaded but useless" state that violates the loader's log+skip
    // contract. Reject it up front so a malformed/empty bundle is skipped, not
    // silently accepted.
    if bundle.presets.is_empty() {
        return Err("bundle has no presets (must contain at least one profile row)".to_string());
    }

    // Voltage + frequency range checks per spec §A.
    // W7-D: SHA-256 chips have a dual envelope (chain-rail OR chip-rail).
    // A row passes if its voltage falls inside ANY of the chip's ranges.
    let envelopes = chip_voltage_ranges(bundle.chip);
    let fmin: u32 = 100;
    let fmax: u32 = 1000;
    for p in &bundle.presets {
        let in_any = envelopes
            .iter()
            .any(|(vmin, vmax)| p.voltage_v >= *vmin && p.voltage_v <= *vmax);
        if !in_any {
            // Format the envelope list for the error message so an
            // operator sees BOTH valid bands (chip-rail + chain-rail).
            let envelope_str: Vec<String> = envelopes
                .iter()
                .map(|(vmin, vmax)| format!("[{:.2}, {:.2}]", vmin, vmax))
                .collect();
            return Err(format!(
                "voltage_v {:.3} out of range {} for chip {:?}",
                p.voltage_v,
                envelope_str.join(" or "),
                bundle.chip
            ));
        }
        if p.freq_mhz < fmin || p.freq_mhz > fmax {
            return Err(format!(
                "freq_mhz {} out of range [{}, {}]",
                p.freq_mhz, fmin, fmax
            ));
        }
    }

    // Step uniqueness within the bundle.
    let mut seen_steps = std::collections::HashSet::new();
    for p in &bundle.presets {
        if !seen_steps.insert(p.step) {
            return Err(format!("duplicate step {} in presets", p.step));
        }
    }

    // Safety blocklist (spec §H — hard-fail rules).
    if bundle.metadata.secure_boot_set_seen {
        return Err(
            "SECURE_BOOT_SET-tainted firmware (eFuse-burning blob); refused per \
             feedback_secure_boot_set_blocklist (no override available)"
                .to_string(),
        );
    }
    if bundle.metadata.hashcore_root_hash_seen {
        return Err("Hashcore SHA-512 universal root hash present; refused per \
             feedback_hashcore_universal_root_hash_blocklist (no override available)"
            .to_string());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Loader internals.
// ---------------------------------------------------------------------------

fn load_one(path: &Path) -> Result<ProfileBundle, ProfileLoadError> {
    let bytes = std::fs::read(path).map_err(|e| ProfileLoadError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let bundle: ProfileBundle =
        serde_json::from_slice(&bytes).map_err(|e| ProfileLoadError::MalformedJson {
            path: path.to_path_buf(),
            source: e,
        })?;
    if bundle.schema_version != 1 {
        return Err(ProfileLoadError::SchemaVersionMismatch {
            path: path.to_path_buf(),
            got: bundle.schema_version,
            expected: 1,
        });
    }
    if let Err(reason) = validate(&bundle) {
        return Err(ProfileLoadError::Validation {
            path: path.to_path_buf(),
            reason,
        });
    }
    Ok(bundle)
}

// ---------------------------------------------------------------------------
// Global accessor.
// ---------------------------------------------------------------------------

static GLOBAL_REGISTRY: OnceLock<RwLock<ProfileRegistry>> = OnceLock::new();

/// Get the process-wide profile registry. Initialized empty on first
/// access; the daemon's boot path should call
/// `global().write().unwrap().reload(dir)` after platform startup to
/// populate it.
pub fn global() -> &'static RwLock<ProfileRegistry> {
    GLOBAL_REGISTRY.get_or_init(|| RwLock::new(ProfileRegistry::new()))
}

// ---------------------------------------------------------------------------
// A01 (goldmine 2026-06-10): S11 / BM1391 registry geometry entry.
// ---------------------------------------------------------------------------

/// Default chips-per-chain for the Antminer S11 (BM1391) — the registry-side
/// "S11 entry" requested by A01, exposing the S11 geometry alongside the
/// per-chip catalog.
///
/// The disk-backed JSON registry above is keyed on `(MinerModel, hashboard,
/// ChipFamily)`, and `dcentrald-api-types` has **no** `AntminerS11`
/// `MinerModel` nor a `Bm1391` `ChipFamily` variant today — so a keyed bundle
/// entry can't be added without changing that crate (out of scope here). This
/// const is the additive, read-only stand-in: S9 (BM1387) defaults to 63
/// chips/chain, S11 (BM1391) to 84. Sourced from the HashSource S11 jig
/// `board_init@1338C` (findings/s1-bm1391-s11.md F10); mirrors
/// `crate::bm1387::BM1391_CHIPS_PER_CHAIN`. No runtime change.
pub const S11_BM1391_DEFAULT_CHIPS_PER_CHAIN: u32 = crate::bm1387::BM1391_CHIPS_PER_CHAIN;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::SystemTime;

    /// Generate a unique temp dir for each test (no `tempfile` dep
    /// available in this workspace).
    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dcentrald-registry-{}-{}-{}-{}",
            label, pid, nanos, n
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn sample_bundle(model: MinerModel, hashboard: &str, chip: ChipFamily) -> ProfileBundle {
        ProfileBundle {
            schema_version: 1,
            miner_model: model,
            hashboard: hashboard.to_string(),
            chip,
            source: ProfileSourceMetadata {
                vendor: "test".into(),
                firmware_version: "1.0.0".into(),
                extracted_from_sha256: "0".repeat(64),
                extraction_date: "2026-05-04".into(),
                extracted_by: Some("test".into()),
            },
            source_class: ProfileSource::VendorExtracted,
            presets: vec![Profile {
                step: 0,
                freq_mhz: 545,
                voltage_v: 1.34,
                wall_watts: None,
                hashrate_ths: None,
                source: ProfileSource::VendorExtracted,
            }],
            metadata: ProfileMetadata::default(),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn imported_profile_bundle_decode_validate_and_insert_never_panics(
            data in proptest::collection::vec(any::<u8>(), 0..4096)
        ) {
            if let Ok(bundle) = serde_json::from_slice::<ProfileBundle>(&data) {
                let validated = validate(&bundle);
                if validated.is_ok() {
                    let key = (bundle.miner_model, bundle.hashboard.clone(), bundle.chip);
                    let mut registry = ProfileRegistry::new();
                    registry.insert_ranked(key, bundle, None);
                }
            }
        }
    }

    #[test]
    fn valid_bundle_validates() {
        let bundle = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        assert!(validate(&bundle).is_ok());
    }

    #[test]
    fn s11_bm1391_registry_entry_is_84() {
        // A01 (goldmine 2026-06-10): registry-side S11 geometry mirror = 84,
        // matching crate::bm1387::BM1391_CHIPS_PER_CHAIN. Additive, no runtime
        // change.
        assert_eq!(S11_BM1391_DEFAULT_CHIPS_PER_CHAIN, 84);
        assert_eq!(
            S11_BM1391_DEFAULT_CHIPS_PER_CHAIN,
            crate::bm1387::BM1391_CHIPS_PER_CHAIN
        );
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.schema_version = 2;
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("schema_version"), "got: {}", err);
    }

    #[test]
    fn empty_presets_bundle_rejected() {
        // bug-hunt LOW #8 (2026-05-28): a bundle with zero presets must be
        // REJECTED, not silently accepted as a "loaded but useless" profile-less
        // bundle (the per-row loops just don't iterate otherwise).
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets.clear();
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("no presets"), "got: {}", err);
    }

    #[test]
    fn voltage_below_range_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets[0].voltage_v = 0.1; // chip-rail min is 0.5
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("voltage_v"), "got: {}", err);
    }

    #[test]
    fn voltage_above_range_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets[0].voltage_v = 5.0; // chip-rail max is 2.0
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("voltage_v"), "got: {}", err);
    }

    #[test]
    fn freq_out_of_range_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets[0].freq_mhz = 50; // min is 100
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("freq_mhz"), "got: {}", err);

        bundle.presets[0].freq_mhz = 1500; // max is 1000
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("freq_mhz"), "got: {}", err);
    }

    #[test]
    fn duplicate_step_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets.push(Profile {
            step: 0, // duplicate of the first row
            freq_mhz: 600,
            voltage_v: 1.40,
            wall_watts: None,
            hashrate_ths: None,
            source: ProfileSource::VendorExtracted,
        });
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("duplicate step"), "got: {}", err);
    }

    #[test]
    fn secure_boot_set_tainted_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.metadata.secure_boot_set_seen = true;
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("SECURE_BOOT_SET"), "got: {}", err);
    }

    #[test]
    fn hashcore_root_hash_tainted_rejected() {
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.metadata.hashcore_root_hash_seen = true;
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("Hashcore"), "got: {}", err);
    }

    #[test]
    fn chip_voltage_range_chain_for_bm1387() {
        // BM1387 is chain-rail: [7.5, 10.0] V.
        let (vmin, vmax) = chip_voltage_range(ChipFamily::Bm1387);
        assert!((vmin - 7.5).abs() < 1e-3);
        assert!((vmax - 10.0).abs() < 1e-3);
    }

    #[test]
    fn chip_voltage_range_chip_for_bm1362() {
        // BM1362 is chip-rail: [0.5, 2.0] V.
        let (vmin, vmax) = chip_voltage_range(ChipFamily::Bm1362);
        assert!((vmin - 0.5).abs() < 1e-3);
        assert!((vmax - 2.0).abs() < 1e-3);
    }

    #[test]
    fn chip_voltage_range_chain_for_bm1485_and_bm1489() {
        // BM1485 / BM1489 are scrypt chain-rail. W7-D widened the
        // BM1489 ceiling to cover L7 baked rows (12.5..13.4 V) which
        // the previous (7.5, 10.5) envelope rejected.
        for chip in [ChipFamily::Bm1485, ChipFamily::Bm1489] {
            let (vmin, vmax) = chip_voltage_range(chip);
            assert!(vmin >= 7.0, "{:?} vmin = {}", chip, vmin);
            assert!(vmax <= 14.0, "{:?} vmax = {}", chip, vmax);
        }
    }

    #[test]
    fn chip_voltage_dual_envelope_for_sha256_chips() {
        // W7-D: SHA-256 chips accept BOTH chip-rail (sub-2V) AND
        // chain-rail (7.5..15V) so baked tables (chain-rail) and
        // Bitmain levels.json (chip-rail mV/1000) both round-trip.
        for chip in [
            ChipFamily::Bm1397,
            ChipFamily::Bm1398,
            ChipFamily::Bm1362,
            ChipFamily::Bm1366,
            ChipFamily::Bm1368,
            ChipFamily::Bm1370,
        ] {
            let envelopes = chip_voltage_ranges(chip);
            assert_eq!(
                envelopes.len(),
                2,
                "{:?} should have a dual chip-rail/chain-rail envelope",
                chip
            );
            // First envelope is chip-rail.
            assert!((envelopes[0].0 - 0.5).abs() < 1e-3);
            assert!((envelopes[0].1 - 2.0).abs() < 1e-3);
            // Second envelope is chain-rail (covers 13.4..14.4 V baked).
            assert!(envelopes[1].0 >= 7.0);
            assert!(envelopes[1].1 >= 14.5);
        }
    }

    /// W8-A: BM1360 + BM1491 are NAMED-ONLY placeholders (per W7-A),
    /// but for forward-compat with future baked profiles the loader
    /// accepts BOTH chip-rail and chain-rail bands per W7-D's pattern.
    /// [GAP — wave-9 live verification needed before either band is
    /// trusted as a true safety floor/ceiling.]
    #[test]
    fn bm1360_voltage_envelope_accepts_both_rails() {
        for chip in [ChipFamily::Bm1360, ChipFamily::Bm1491] {
            let envelopes = chip_voltage_ranges(chip);
            assert_eq!(
                envelopes.len(),
                2,
                "{:?} (W8-A placeholder) should have a dual chip-rail/chain-rail envelope",
                chip
            );
            // First envelope is chip-rail (0.5..2.0 V).
            assert!((envelopes[0].0 - 0.5).abs() < 1e-3);
            assert!((envelopes[0].1 - 2.0).abs() < 1e-3);
            // Second envelope is chain-rail (7.5..15.0 V).
            assert!((envelopes[1].0 - 7.5).abs() < 1e-3);
            assert!((envelopes[1].1 - 15.0).abs() < 1e-3);
        }
    }

    #[test]
    fn baked_chain_rail_voltages_pass_validation() {
        // Pin the W7-D round-trip blocker fix: a 13.4 V (baked
        // chain-rail) BM1397 row must validate after the dual-envelope
        // change, not get rejected as "out of [0.5, 2.0]".
        let mut bundle = sample_bundle(
            MinerModel::AntminerS19,
            "BHB-S17-generic",
            ChipFamily::Bm1397,
        );
        bundle.presets[0].voltage_v = 13.4;
        bundle.presets[0].freq_mhz = 575;
        assert!(
            validate(&bundle).is_ok(),
            "BM1397 chain-rail 13.4 V must pass W7-D dual envelope"
        );
    }

    #[test]
    fn chip_rail_vendor_voltages_still_pass_validation() {
        // Vendor levels.json values land at 1.32..1.60 V (mV/1000).
        // These must still validate against the chip-rail envelope.
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets[0].voltage_v = 1.34;
        bundle.presets[0].freq_mhz = 545;
        assert!(
            validate(&bundle).is_ok(),
            "BM1362 chip-rail 1.34 V must still pass after W7-D change"
        );
    }

    #[test]
    fn voltage_outside_both_envelopes_still_rejected() {
        // 5.0 V is between the two envelopes; should be rejected.
        let mut bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        bundle.presets[0].voltage_v = 5.0;
        let err = validate(&bundle).unwrap_err();
        assert!(err.contains("voltage_v"), "got: {}", err);
    }

    /// W7-D round-trip integration test.
    ///
    /// Loads every `*.json` under
    /// `DCENT_OS_Antminer/dcentrald/etc/dcentrald/profiles.d/` and asserts
    /// that ALL of them validate. Pre-W7-D this test would have been
    /// stuck at `loaded ~= 9, skipped ~= 15` because vendor JSONs failed
    /// `serde_json::from_slice` (null watts) and chain-rail baked JSONs
    /// failed voltage validation. Post-W7-D the expectation is
    /// `loaded == 24, skipped == 0`.
    ///
    /// Wave M: the 24 emitted bundles are now COMMITTED at
    /// `dcentrald/etc/dcentrald/profiles.d/` (no longer migration-gated),
    /// so this is a PERMANENT regression test — the canonical source must
    /// always load cleanly as 24/0. The `scripts/check_profiles_drift.sh`
    /// gate separately verifies the two shipped rootfs overlays stay
    /// byte-identical to this source.
    #[test]
    fn w7d_round_trip_loads_all_24_emitted_bundles() {
        // Walk up from `dcentrald-silicon-profiles/` (CARGO_MANIFEST_DIR)
        // to repo root and into the profiles.d tree. Cargo sets
        // CARGO_MANIFEST_DIR for tests.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let profiles_dir = Path::new(manifest_dir)
            .join("..")
            .join("etc")
            .join("dcentrald")
            .join("profiles.d");

        if !profiles_dir.exists() {
            // Fresh checkout — emit a clear failure message with the
            // resolution command. Use `assert!(false)` so the test
            // still fails in CI when --ignored is forced.
            panic!(
                "profiles.d not found at {:?} — run `python scripts/migrate-baked-profiles.py` first",
                profiles_dir
            );
        }

        let (_registry, stats) = ProfileRegistry::load_from_disk(&profiles_dir)
            .expect("load_from_disk must not fail at top level");

        // The W5-D / W7-D round-trip target is 9 baked + 15 vendor = 24.
        assert_eq!(
            stats.skipped, 0,
            "W7-D round-trip must NOT skip any emitted bundle; errors: {:#?}",
            stats.errors
        );
        assert_eq!(
            stats.loaded, 24,
            "expected 24 bundles (9 baked + 15 vendor); got {}",
            stats.loaded
        );
    }

    #[test]
    fn merge_higher_class_wins() {
        let dir = unique_temp_dir("merge-rank");
        let key = (
            MinerModel::AntminerS19jProA,
            "BHB42601".to_string(),
            ChipFamily::Bm1362,
        );

        let mut left = ProfileRegistry::new();
        let mut left_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        left_bundle.source_class = ProfileSource::VendorExtracted;
        left_bundle.source.vendor = "left".into();
        let left_path = dir.join("left.json");
        std::fs::write(&left_path, serde_json::to_string(&left_bundle).unwrap()).unwrap();
        left.by_key.insert(key.clone(), left_bundle);
        left.paths.insert(key.clone(), left_path);

        let mut right = ProfileRegistry::new();
        let mut right_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        right_bundle.source_class = ProfileSource::LiveConfirmed; // higher rank
        right_bundle.source.vendor = "right".into();
        let right_path = dir.join("right.json");
        std::fs::write(&right_path, serde_json::to_string(&right_bundle).unwrap()).unwrap();
        right.by_key.insert(key.clone(), right_bundle);
        right.paths.insert(key.clone(), right_path);

        left.merge(right);

        let bundle =
            left.lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        assert!(bundle.is_some());
        assert_eq!(
            bundle.unwrap().source_class,
            ProfileSource::LiveConfirmed,
            "LiveConfirmed should override VendorExtracted"
        );
        assert_eq!(bundle.unwrap().source.vendor, "right");

        // Reverse direction: VendorExtracted MUST NOT downgrade LiveConfirmed.
        let mut already_live = ProfileRegistry::new();
        let mut live_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        live_bundle.source_class = ProfileSource::LiveConfirmed;
        live_bundle.source.vendor = "live-incumbent".into();
        let live_path = dir.join("live.json");
        std::fs::write(&live_path, serde_json::to_string(&live_bundle).unwrap()).unwrap();
        already_live.by_key.insert(key.clone(), live_bundle);
        already_live.paths.insert(key.clone(), live_path);

        let mut intruder = ProfileRegistry::new();
        let mut intruder_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        intruder_bundle.source_class = ProfileSource::VendorExtracted;
        intruder_bundle.source.vendor = "intruder".into();
        let intruder_path = dir.join("intruder.json");
        std::fs::write(
            &intruder_path,
            serde_json::to_string(&intruder_bundle).unwrap(),
        )
        .unwrap();
        intruder.by_key.insert(key.clone(), intruder_bundle);
        intruder.paths.insert(key.clone(), intruder_path);

        already_live.merge(intruder);
        let bundle2 = already_live
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .unwrap();
        assert_eq!(
            bundle2.source_class,
            ProfileSource::LiveConfirmed,
            "VendorExtracted MUST NOT downgrade LiveConfirmed"
        );
        assert_eq!(bundle2.source.vendor, "live-incumbent");

        cleanup(&dir);
    }

    #[test]
    fn load_from_disk_resolves_same_key_by_rank_not_walk_order() {
        // Regression for the W13-A profile-import pipeline bug: an
        // operator-confirmed bundle in `operator/` MUST outrank a vendor
        // bundle in `vendor/` for the SAME (model, hashboard, chip) key,
        // regardless of which subdir the directory walk happens to read
        // last. Before the fix, `load_from_disk` did a blind
        // last-writer-wins `by_key.insert`, so the winner was decided by
        // filesystem iteration order, not authority rank.
        let dir = unique_temp_dir("load-rank");
        let vendor = dir.join("vendor");
        let operator = dir.join("operator");
        std::fs::create_dir_all(&vendor).unwrap();
        std::fs::create_dir_all(&operator).unwrap();

        // Vendor default (rank 3) for the key.
        let mut vendor_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        vendor_bundle.source_class = ProfileSource::VendorExtracted;
        vendor_bundle.presets[0].source = ProfileSource::VendorExtracted;
        vendor_bundle.presets[0].freq_mhz = 545;
        std::fs::write(
            vendor.join("vendor.json"),
            serde_json::to_string(&vendor_bundle).unwrap(),
        )
        .unwrap();

        // Operator override (rank 4) for the SAME key.
        let mut operator_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        operator_bundle.source_class = ProfileSource::OperatorConfirmed;
        operator_bundle.presets[0].source = ProfileSource::OperatorConfirmed;
        operator_bundle.presets[0].freq_mhz = 600;
        std::fs::write(
            operator.join("operator.json"),
            serde_json::to_string(&operator_bundle).unwrap(),
        )
        .unwrap();

        let (registry, stats) = ProfileRegistry::load_from_disk(&dir).unwrap();
        assert_eq!(stats.loaded, 2, "both files parse");
        assert_eq!(stats.skipped, 0);
        // Only one survives per key, and it MUST be the operator override.
        assert_eq!(registry.len(), 1);
        let resolved = registry
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .expect("bundle present");
        assert_eq!(
            resolved.source_class,
            ProfileSource::OperatorConfirmed,
            "operator-confirmed override must outrank vendor default regardless of walk order"
        );
        assert_eq!(resolved.presets[0].freq_mhz, 600);

        cleanup(&dir);
    }

    #[test]
    fn load_from_disk_keeps_higher_rank_when_lower_rank_read_last() {
        // Same invariant under a different on-disk layout (an
        // alphabetically-later `zz-vendor/` subdir alongside `operator/`).
        // `read_dir` order is not OS-guaranteed, so this does not *force*
        // the vendor file to be read last — but the fix's whole point is
        // that the outcome is order-INDEPENDENT: whichever file the walk
        // visits last, the operator override must still win.
        let dir = unique_temp_dir("load-rank-order");
        let operator = dir.join("operator");
        let late = dir.join("zz-vendor");
        std::fs::create_dir_all(&operator).unwrap();
        std::fs::create_dir_all(&late).unwrap();

        let mut operator_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        operator_bundle.source_class = ProfileSource::OperatorConfirmed;
        operator_bundle.presets[0].source = ProfileSource::OperatorConfirmed;
        operator_bundle.presets[0].freq_mhz = 600;
        std::fs::write(
            operator.join("operator.json"),
            serde_json::to_string(&operator_bundle).unwrap(),
        )
        .unwrap();

        let mut vendor_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        vendor_bundle.source_class = ProfileSource::VendorExtracted;
        vendor_bundle.presets[0].source = ProfileSource::VendorExtracted;
        vendor_bundle.presets[0].freq_mhz = 545;
        std::fs::write(
            late.join("vendor.json"),
            serde_json::to_string(&vendor_bundle).unwrap(),
        )
        .unwrap();

        let (registry, _stats) = ProfileRegistry::load_from_disk(&dir).unwrap();
        let resolved = registry
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .expect("bundle present");
        assert_eq!(
            resolved.source_class,
            ProfileSource::OperatorConfirmed,
            "a lower-rank vendor file read after the operator file must NOT shadow it"
        );
        assert_eq!(resolved.presets[0].freq_mhz, 600);

        cleanup(&dir);
    }

    #[test]
    fn merge_mtime_tie_break() {
        let dir = unique_temp_dir("merge-mtime");
        let key = (
            MinerModel::AntminerS19jProA,
            "BHB42601".to_string(),
            ChipFamily::Bm1362,
        );

        let mut left = ProfileRegistry::new();
        let mut left_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        left_bundle.source_class = ProfileSource::LiveConfirmed;
        left_bundle.source.vendor = "older".into();
        let left_path = dir.join("older.json");
        std::fs::write(&left_path, serde_json::to_string(&left_bundle).unwrap()).unwrap();
        left.by_key.insert(key.clone(), left_bundle);
        left.paths.insert(key.clone(), left_path);

        // Sleep 50ms to ensure mtime delta is observable on coarse-grained
        // file systems (FAT32 / NTFS sub-100ms granularity).
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut right = ProfileRegistry::new();
        let mut right_bundle =
            sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        right_bundle.source_class = ProfileSource::LiveConfirmed; // SAME RANK
        right_bundle.source.vendor = "newer".into();
        let right_path = dir.join("newer.json");
        std::fs::write(&right_path, serde_json::to_string(&right_bundle).unwrap()).unwrap();
        right.by_key.insert(key.clone(), right_bundle);
        right.paths.insert(key.clone(), right_path);

        left.merge(right);

        let bundle = left
            .lookup_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .unwrap();
        assert_eq!(
            bundle.source.vendor, "newer",
            "newer mtime should win on rank tie"
        );

        cleanup(&dir);
    }

    #[test]
    fn load_from_disk_skips_malformed() {
        let dir = unique_temp_dir("malformed");

        // One valid file.
        let good = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        std::fs::write(dir.join("good.json"), serde_json::to_string(&good).unwrap()).unwrap();

        // One malformed JSON.
        std::fs::write(dir.join("bad.json"), "{ this is not valid json }").unwrap();

        // One file with wrong schema_version.
        let mut wrong_schema = good.clone();
        wrong_schema.schema_version = 99;
        std::fs::write(
            dir.join("wrong_schema.json"),
            serde_json::to_string(&wrong_schema).unwrap(),
        )
        .unwrap();

        // One file with secure_boot_set_seen → validation rejection.
        let mut tainted = good.clone();
        tainted.metadata.secure_boot_set_seen = true;
        std::fs::write(
            dir.join("tainted.json"),
            serde_json::to_string(&tainted).unwrap(),
        )
        .unwrap();

        let (registry, stats) = ProfileRegistry::load_from_disk(&dir).unwrap();
        assert_eq!(stats.loaded, 1, "only the good file should load");
        assert_eq!(stats.skipped, 3, "3 malformed/invalid files skipped");
        assert_eq!(stats.errors.len(), 3);
        assert_eq!(registry.len(), 1);
        assert!(registry
            .lookup(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362)
            .is_some());

        cleanup(&dir);
    }

    #[test]
    fn lookup_returns_none_for_unknown_key() {
        let registry = ProfileRegistry::new();
        let result = registry.lookup(MinerModel::AntminerS9, "unknown-board", ChipFamily::Bm1387);
        assert!(result.is_none());
    }

    #[test]
    fn iter_keys_works() {
        let dir = unique_temp_dir("iter-keys");

        let bundle1 = sample_bundle(MinerModel::AntminerS9, "BHB-S9-A", ChipFamily::Bm1387);
        let bundle2 = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);

        // Write valid voltage for BM1387 (chain-rail 7.5-10.0V).
        let mut bundle1 = bundle1;
        bundle1.presets[0].voltage_v = 9.1;
        bundle1.presets[0].freq_mhz = 600;

        std::fs::write(
            dir.join("s9.json"),
            serde_json::to_string(&bundle1).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("s19j.json"),
            serde_json::to_string(&bundle2).unwrap(),
        )
        .unwrap();

        let (registry, stats) = ProfileRegistry::load_from_disk(&dir).unwrap();
        assert_eq!(stats.loaded, 2);

        let keys: Vec<&ProfileKey> = registry.iter_keys().collect();
        assert_eq!(keys.len(), 2);

        cleanup(&dir);
    }

    #[test]
    fn load_from_disk_recurses_one_level_into_subdirs() {
        let dir = unique_temp_dir("subdirs");
        let baked = dir.join("baked");
        let vendor = dir.join("vendor");
        std::fs::create_dir_all(&baked).unwrap();
        std::fs::create_dir_all(&vendor).unwrap();

        // BM1387 / S9 in baked/.
        let mut s9 = sample_bundle(MinerModel::AntminerS9, "BHB-S9-A", ChipFamily::Bm1387);
        s9.presets[0].voltage_v = 9.1;
        s9.presets[0].freq_mhz = 600;
        s9.source_class = ProfileSource::LiveConfirmed;
        std::fs::write(baked.join("s9.json"), serde_json::to_string(&s9).unwrap()).unwrap();

        // BM1362 / S19jProA in vendor/.
        let mut s19j = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        s19j.source_class = ProfileSource::VendorExtracted;
        std::fs::write(
            vendor.join("s19j.json"),
            serde_json::to_string(&s19j).unwrap(),
        )
        .unwrap();

        // Top-level json (also picked up).
        let s21 = sample_bundle(MinerModel::AntminerS21, "BHB-S21", ChipFamily::Bm1368);
        std::fs::write(dir.join("s21.json"), serde_json::to_string(&s21).unwrap()).unwrap();

        let (registry, stats) = ProfileRegistry::load_from_disk(&dir).unwrap();
        assert_eq!(stats.loaded, 3);
        assert_eq!(stats.skipped, 0);
        assert_eq!(registry.len(), 3);

        cleanup(&dir);
    }

    #[test]
    fn missing_dir_returns_empty_registry_not_error() {
        // Per spec §B: "if disk dir is missing or all files malformed,
        // fall back silently to baked." load_from_disk should NOT error
        // when the dir doesn't exist.
        let nonexistent = std::env::temp_dir().join(format!(
            "dcentrald-registry-nonexistent-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // Make sure it really doesn't exist.
        assert!(!nonexistent.exists());

        let (registry, stats) = ProfileRegistry::load_from_disk(&nonexistent).unwrap();
        assert_eq!(stats.loaded, 0);
        assert_eq!(stats.skipped, 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn reload_swaps_registry_atomically() {
        let dir = unique_temp_dir("reload");

        let mut registry = ProfileRegistry::new();
        assert!(registry.is_empty());

        let bundle = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        std::fs::write(dir.join("v1.json"), serde_json::to_string(&bundle).unwrap()).unwrap();

        let stats = registry.reload(&dir).unwrap();
        assert_eq!(stats.loaded, 1);
        assert_eq!(registry.len(), 1);

        // Remove the file and reload — registry should swap to empty.
        std::fs::remove_file(dir.join("v1.json")).unwrap();
        let stats = registry.reload(&dir).unwrap();
        assert_eq!(stats.loaded, 0);
        assert!(registry.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn global_returns_initialized_lock() {
        // global() should return a usable RwLock even before the
        // daemon's reload path runs.
        let g = global();
        let r = g.read().unwrap();
        // Default state is empty; drop the lock cleanly.
        let _ = r.is_empty();
    }

    // -------------------------------------------------------------
    // W13-A: active-profile-per-chain tests
    // -------------------------------------------------------------

    #[test]
    fn set_active_profile_records_id_when_bundle_exists() {
        let dir = unique_temp_dir("active-set");
        let bundle = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        std::fs::write(dir.join("v1.json"), serde_json::to_string(&bundle).unwrap()).unwrap();
        let mut registry = ProfileRegistry::new();
        let _ = registry.reload(&dir).unwrap();

        let profile_id = "antminer_s19j_pro_a__BHB42601__bm1362__vendor_extracted";
        registry
            .set_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601", profile_id)
            .expect("set should succeed when bundle exists");

        assert_eq!(
            registry.get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601"),
            Some(profile_id)
        );
        assert_eq!(registry.active_selection_count(), 1);
        cleanup(&dir);
    }

    #[test]
    fn set_active_profile_rejects_unknown_chain() {
        let mut registry = ProfileRegistry::new();
        let err = registry
            .set_active_profile_for_chain(MinerModel::AntminerS9, "no-such-board", "ignored")
            .expect_err("must reject unknown chain");
        assert!(err.contains("no profiles loaded"), "got: {}", err);
    }

    #[test]
    fn get_active_bundle_resolves_through_id() {
        let dir = unique_temp_dir("active-bundle");
        let mut bundle = sample_bundle(MinerModel::AntminerS9, "BHB-S9-A", ChipFamily::Bm1387);
        bundle.presets[0].voltage_v = 9.1;
        bundle.presets[0].freq_mhz = 600;
        bundle.source.firmware_version = "tagged-w13a".into();
        std::fs::write(dir.join("v1.json"), serde_json::to_string(&bundle).unwrap()).unwrap();
        let mut registry = ProfileRegistry::new();
        let _ = registry.reload(&dir).unwrap();

        let profile_id = "antminer_s9__BHB-S9-A__bm1387__vendor_extracted";
        registry
            .set_active_profile_for_chain(MinerModel::AntminerS9, "BHB-S9-A", profile_id)
            .unwrap();

        let resolved = registry
            .get_active_bundle_for_chain(MinerModel::AntminerS9, "BHB-S9-A")
            .expect("active bundle resolves");
        assert_eq!(resolved.source.firmware_version, "tagged-w13a");
        cleanup(&dir);
    }

    #[test]
    fn reload_drops_stale_active_selection() {
        let dir = unique_temp_dir("active-reload");
        let bundle = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        let path = dir.join("v1.json");
        std::fs::write(&path, serde_json::to_string(&bundle).unwrap()).unwrap();
        let mut registry = ProfileRegistry::new();
        let _ = registry.reload(&dir).unwrap();

        registry
            .set_active_profile_for_chain(
                MinerModel::AntminerS19jProA,
                "BHB42601",
                "antminer_s19j_pro_a__BHB42601__bm1362__vendor_extracted",
            )
            .unwrap();
        assert_eq!(registry.active_selection_count(), 1);

        // Remove the bundle file and reload — selection should drop.
        std::fs::remove_file(&path).unwrap();
        let _ = registry.reload(&dir).unwrap();
        assert_eq!(
            registry.active_selection_count(),
            0,
            "stale active selection must be dropped on reload"
        );
        cleanup(&dir);
    }

    #[test]
    fn clear_active_profile_returns_previous_id() {
        let dir = unique_temp_dir("active-clear");
        let bundle = sample_bundle(MinerModel::AntminerS19jProA, "BHB42601", ChipFamily::Bm1362);
        std::fs::write(dir.join("v1.json"), serde_json::to_string(&bundle).unwrap()).unwrap();
        let mut registry = ProfileRegistry::new();
        let _ = registry.reload(&dir).unwrap();

        let id = "antminer_s19j_pro_a__BHB42601__bm1362__vendor_extracted";
        registry
            .set_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601", id)
            .unwrap();

        let cleared =
            registry.clear_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601");
        assert_eq!(cleared.as_deref(), Some(id));
        assert!(registry
            .get_active_profile_for_chain(MinerModel::AntminerS19jProA, "BHB42601")
            .is_none());
        cleanup(&dir);
    }

    #[test]
    fn profile_source_rank_hierarchy_pinned() {
        // Cross-check the rank values against the spec §A hierarchy.
        // LiveConfirmed > OperatorConfirmed > VendorExtracted >
        // Reconstructed > Datasheet.
        assert!(ProfileSource::LiveConfirmed.rank() > ProfileSource::OperatorConfirmed.rank());
        assert!(ProfileSource::OperatorConfirmed.rank() > ProfileSource::VendorExtracted.rank());
        assert!(ProfileSource::VendorExtracted.rank() > ProfileSource::Reconstructed.rank());
        assert!(ProfileSource::Reconstructed.rank() > ProfileSource::Datasheet.rank());

        // Pin exact values so a refactor that accidentally renumbered
        // them would fail noisily.
        assert_eq!(ProfileSource::LiveConfirmed.rank(), 5);
        assert_eq!(ProfileSource::OperatorConfirmed.rank(), 4);
        assert_eq!(ProfileSource::VendorExtracted.rank(), 3);
        assert_eq!(ProfileSource::Reconstructed.rank(), 2);
        assert_eq!(ProfileSource::Datasheet.rank(), 1);
    }
}
