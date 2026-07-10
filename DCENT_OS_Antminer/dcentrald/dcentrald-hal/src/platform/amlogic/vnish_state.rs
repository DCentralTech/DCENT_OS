//! VNish vs stock-Bitmain firmware-state detection on am3-aml platform (W15.D1).
//!
//! Same A113D silicon — DCENT_OS does NOT register a 4th `Platform` enum
//! variant.
//! Detection is by USERSPACE marker only (filesystem probes), not by SoC ID.
//!
//! ## Distinction
//!
//! - **`StockBitmain`** — Bitmain shipping config: `/usr/bin/bmminer` exists,
//!   `/etc/init.d/S70cgminer` matches Bitmain template. AES-encrypted boot.img
//!   blocker → control-board-swap canonical install path per
//!   . See
//!   `docs/security/AMLCTRL_BOUNDARY.md` §5.
//! - **`VnishCgminer`** — VNish v1.2.7+ replaced the Bitmain boot chain:
//!   `/usr/bin/cgminer` exists, `/etc/init.d/S11board` matches VNish template,
//!   `/etc/keys/master-public.pem` is the VNish RSA-4096. Rootfs-window
//!   install MAY work (gated route per W15.D4 — `amlogic-vnish-rootfs_window_lab`,
//!   deferred — handoff notes).
//!
//! ## Cross-references
//!
//! -  — VNish rcS
//!   sequence +  Q10 PWR_EN active-HIGH confirmation.
//! - `vnish_cold_boot.rs` (W15.D2) — VNish AML cold-boot phase machine
//!   (data-only).
//! - `docs/security/AMLCTRL_BOUNDARY.md` §2.1 — VNish-installed AMLCtrl carve-out.
//!
//! ## Memory rules
//!
//! -  —
//!   same A113D silicon, different firmware. NEVER register a 4th
//!   `Platform` enum variant.

use std::path::Path;

/// Userspace firmware state detected on an Amlogic A113D platform.
///
/// Probed at runtime from filesystem markers; the SoC itself is identical
/// across both states (same A113D silicon, same hashboard topology, same
/// PWR_EN GPIO). Only the userspace daemon + boot chain differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmlFirmwareState {
    /// Stock Bitmain bmminer userspace.
    ///
    /// AES-encrypted boot.img blocker → control-board-swap canonical install
    /// path.
    StockBitmain,
    /// VNish v1.2.7+ cgminer userspace.
    ///
    /// VNish replaced the Bitmain boot chain themselves; the rootfs-window
    /// install path MAY apply per the W14.D §2.1 carve-out in
    /// `docs/security/AMLCTRL_BOUNDARY.md` (still bench-only, gated).
    VnishCgminer,
    /// Detection probe inconclusive (neither marker present, or only one
    /// marker present without its companion).
    ///
    /// Defensive: refuses to commit to either firmware state when the
    /// expected pair of markers (binary + init script) is incomplete.
    Unknown,
}

impl AmlFirmwareState {
    /// Detect from filesystem markers under `root` (default `/` for live
    /// system; pass tempdir root for tests).
    ///
    /// Markers (both must be present for a positive detection):
    ///
    /// | Variant       | Binary marker            | Init-script marker          |
    /// |---------------|--------------------------|-----------------------------|
    /// | `StockBitmain`| `/usr/bin/bmminer`       | `/etc/init.d/S70cgminer`    |
    /// | `VnishCgminer`| `/usr/bin/cgminer`       | `/etc/init.d/S11board`      |
    ///
    /// Either marker alone (e.g. only the binary copied over by an operator
    /// during recovery) is treated as `Unknown` to avoid false-positive
    /// firmware-state classification.
    pub fn detect_at(root: &Path) -> Self {
        let cgminer = root.join("usr/bin/cgminer");
        let bmminer = root.join("usr/bin/bmminer");
        let s11board = root.join("etc/init.d/S11board");
        let s70cgminer = root.join("etc/init.d/S70cgminer");

        // VNish marker: cgminer present + S11board init script
        if cgminer.is_file() && s11board.is_file() {
            return AmlFirmwareState::VnishCgminer;
        }
        // Stock-Bitmain marker: bmminer present + S70cgminer init script
        if bmminer.is_file() && s70cgminer.is_file() {
            return AmlFirmwareState::StockBitmain;
        }
        // Either marker alone is ambiguous — return Unknown
        AmlFirmwareState::Unknown
    }

    /// Live system detection — probes filesystem at `/`.
    ///
    /// On Windows hosts (developer machines) this will return `Unknown`
    /// because the markers don't exist. On a real Amlogic miner running
    /// stock or VNish, this returns the matching variant.
    pub fn detect_live() -> Self {
        Self::detect_at(Path::new("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Spin up a fresh per-test scratch directory under `std::env::temp_dir()`.
    /// We avoid `tempfile` as a dev-dep (matches the discipline in
    /// `crate::platform::config::probe_tty_picks_first_existing_candidate`).
    fn fresh_root(tag: &str) -> PathBuf {
        let nonce = format!(
            "dcentrald_hal_vnish_state_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let root = std::env::temp_dir().join(nonce);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(&path).unwrap();
    }

    fn cleanup(root: &Path) {
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detect_vnish_when_cgminer_and_s11board_present() {
        let root = fresh_root("vnish_pos");
        touch(&root, "usr/bin/cgminer");
        touch(&root, "etc/init.d/S11board");

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::VnishCgminer);
    }

    #[test]
    fn detect_stock_when_bmminer_and_s70cgminer_present() {
        let root = fresh_root("stock_pos");
        touch(&root, "usr/bin/bmminer");
        touch(&root, "etc/init.d/S70cgminer");

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::StockBitmain);
    }

    #[test]
    fn detect_unknown_when_no_markers_present() {
        let root = fresh_root("none");
        // No files created — empty rootfs.

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::Unknown);
    }

    /// Defensive: an operator who copied only `cgminer` into `/usr/bin`
    /// during a recovery session must NOT trip the VnishCgminer detection
    /// without the matching `S11board` init script.
    #[test]
    fn detect_unknown_when_only_cgminer_present_no_s11board() {
        let root = fresh_root("cgminer_only");
        touch(&root, "usr/bin/cgminer");
        // S11board missing.

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::Unknown);
    }

    /// Defensive mirror: bare `bmminer` without `S70cgminer` is also
    /// inconclusive — refuse to default to `StockBitmain`.
    #[test]
    fn detect_unknown_when_only_bmminer_present_no_s70cgminer() {
        let root = fresh_root("bmminer_only");
        touch(&root, "usr/bin/bmminer");
        // S70cgminer missing.

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::Unknown);
    }

    /// On the test host (Windows / Linux dev machine) the markers won't
    /// exist on `/`, so `detect_live` returns `Unknown` and does NOT panic.
    #[test]
    fn live_detect_returns_some_state_without_panicking() {
        let state = AmlFirmwareState::detect_live();
        // Outcome depends on host filesystem; the only contract is
        // no-panic + returns one of the three enum variants. On a real
        // Amlogic miner this would return Stock or Vnish; on the
        // Windows dev host it returns Unknown.
        match state {
            AmlFirmwareState::StockBitmain
            | AmlFirmwareState::VnishCgminer
            | AmlFirmwareState::Unknown => {}
        }
    }

    /// VNish marker pair must take precedence in scenarios where both
    /// pairs happen to exist (operator left both binaries on disk during
    /// a partial recovery). VNish wins because its boot chain has
    /// already replaced Bitmain's, and the userspace cgminer is what
    /// actually runs.
    #[test]
    fn vnish_wins_when_both_marker_pairs_present() {
        let root = fresh_root("both_pairs");
        touch(&root, "usr/bin/cgminer");
        touch(&root, "etc/init.d/S11board");
        touch(&root, "usr/bin/bmminer");
        touch(&root, "etc/init.d/S70cgminer");

        let state = AmlFirmwareState::detect_at(&root);
        cleanup(&root);
        assert_eq!(state, AmlFirmwareState::VnishCgminer);
    }
}
