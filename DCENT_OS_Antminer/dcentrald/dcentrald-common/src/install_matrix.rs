//! Install / packaging matrix derived from [`BoardDesc`] (ADR-0011).
//!
//! Pure host-safe view for toolbox, Docker target tables, docs, and CI.
//! Does **not** replace live install scripts yet — generators should consume
//! this so stringly-typed board lists stop diverging.

use crate::board_desc::{
    BoardDesc, BoardFamily, ChainTransportKind, SlotPolicy, VoltageControllerClass, WorkEngineKind,
};

/// One row of the install/product matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallMatrixRow {
    pub board_target: &'static str,
    pub family: BoardFamily,
    pub chain_transport: ChainTransportKind,
    pub work_engine: WorkEngineKind,
    pub voltage_controller: VoltageControllerClass,
    pub slot_policy: SlotPolicy,
    /// Signed public-beta package is appropriate.
    pub public_beta_install: bool,
    /// Fresh image may auto-enable mining (almost always false).
    pub mining_default_enabled: bool,
    /// Product install without lab override is allowed.
    pub product_install_allowed: bool,
    /// A/B sysupgrade with fw_setenv is the supported update rail.
    pub ab_sysupgrade: bool,
    /// Lab-only / evidence-gated first-flash.
    pub lab_gated_install: bool,
}

impl From<&BoardDesc> for InstallMatrixRow {
    fn from(d: &BoardDesc) -> Self {
        Self {
            board_target: d.board_target,
            family: d.family,
            chain_transport: d.chain_transport,
            work_engine: d.work_engine,
            voltage_controller: d.voltage_controller,
            slot_policy: d.slot_policy,
            public_beta_install: d.public_beta_install,
            mining_default_enabled: d.mining_default_enabled,
            product_install_allowed: d.product_install_allowed(),
            ab_sysupgrade: matches!(d.slot_policy, SlotPolicy::ZynqAbFwSetenv),
            lab_gated_install: matches!(d.slot_policy, SlotPolicy::LabGated)
                || !d.public_beta_install,
        }
    }
}

/// Full matrix for every registered [`BoardDesc`].
pub fn install_matrix() -> Vec<InstallMatrixRow> {
    BoardDesc::all_registered()
        .iter()
        .map(InstallMatrixRow::from)
        .collect()
}

/// Board targets allowed for public-beta product install packages.
pub fn public_beta_board_targets() -> Vec<&'static str> {
    install_matrix()
        .into_iter()
        .filter(|r| r.public_beta_install)
        .map(|r| r.board_target)
        .collect()
}

/// Board targets that use Zynq A/B + fw_setenv update rail.
pub fn ab_sysupgrade_board_targets() -> Vec<&'static str> {
    install_matrix()
        .into_iter()
        .filter(|r| r.ab_sysupgrade)
        .map(|r| r.board_target)
        .collect()
}

/// Stable TSV for docs/CI (no external deps).
///
/// Columns: board_target, family, public_beta, ab_sysupgrade, lab_gated, slot_policy
pub fn install_matrix_tsv() -> String {
    let mut out = String::from(
        "board_target\tfamily\tpublic_beta\tab_sysupgrade\tlab_gated\tslot_policy\ttransport\n",
    );
    for row in install_matrix() {
        out.push_str(&format!(
            "{}\t{:?}\t{}\t{}\t{}\t{:?}\t{:?}\n",
            row.board_target,
            row.family,
            row.public_beta_install as u8,
            row.ab_sysupgrade as u8,
            row.lab_gated_install as u8,
            row.slot_policy,
            row.chain_transport,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_beta_is_exactly_xil_pair() {
        assert_eq!(public_beta_board_targets(), vec!["am1-s9", "am2-s19j"]);
    }

    #[test]
    fn every_public_beta_row_is_ab_sysupgrade() {
        for row in install_matrix()
            .into_iter()
            .filter(|r| r.public_beta_install)
        {
            assert!(
                row.ab_sysupgrade,
                "{} public beta must use A/B fw_setenv",
                row.board_target
            );
            assert!(!row.mining_default_enabled);
            assert!(row.product_install_allowed);
        }
    }

    #[test]
    fn amlogic_rows_are_lab_gated_not_public_beta() {
        for row in install_matrix()
            .into_iter()
            .filter(|r| r.family == BoardFamily::Amlogic)
        {
            assert!(!row.public_beta_install, "{}", row.board_target);
            assert!(row.lab_gated_install, "{}", row.board_target);
            assert_eq!(row.chain_transport, ChainTransportKind::Serial);
        }
    }

    #[test]
    fn tsv_contains_header_and_beta_targets() {
        let tsv = install_matrix_tsv();
        assert!(tsv.starts_with("board_target\t"));
        assert!(tsv.contains("am1-s9\t"));
        assert!(tsv.contains("am2-s19j\t"));
        assert!(tsv.contains("am3-s21\t"));
    }

    #[test]
    fn matrix_len_matches_registry() {
        assert_eq!(install_matrix().len(), BoardDesc::all_registered().len());
    }

    #[test]
    fn tsv_row_count_matches_registry_plus_header() {
        let tsv = install_matrix_tsv();
        let lines: Vec<_> = tsv.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), BoardDesc::all_registered().len() + 1);
        assert!(tsv.contains('\t'), "TSV must use tab separators");
    }

    /// Drift pin: committed `docs/architecture/install_matrix.tsv` must match
    /// `install_matrix_tsv()`. Regenerate with
    /// `scripts/export_install_matrix.ps1` after BoardDesc changes.
    #[test]
    fn committed_install_matrix_tsv_matches_generator() {
        // Crate root is dcentrald-common/; docs live under DCENT_OS_Antminer/docs/.
        // src/ -> .. = crate, ../.. = dcentrald/, ../../.. = dcentos/
        let committed = include_str!("../../../docs/architecture/install_matrix.tsv");
        let generated = install_matrix_tsv();
        let norm = |s: &str| {
            s.replace("\r\n", "\n")
                .trim_end()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(
            norm(committed),
            norm(&generated),
            "install_matrix.tsv drifted from BoardDesc registry — re-run scripts/export_install_matrix.ps1"
        );
    }
}
