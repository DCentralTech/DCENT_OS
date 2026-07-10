//! Platform subtype detection + voltage-controller classification.
//!
//! W2A.2 / W10 PIC1704 wire-up (2026-05-09); W11.3 marker expansion
//! (RE2 §2.5 + §6.1); W12 T19 expansion (RE3 §2.5 + §5.1):
//! Bitmain BraiinsOS+ and the DCENT_OS dev-kit both ship `/etc/subtype` to
//! distinguish hashboard / control-board pairings. The canonical strings
//! observed in the dev-kit rootfs trees:
//!
//! | `/etc/subtype` value | Carrier      | Hashboard family | Voltage controller |
//! |----------------------|--------------|------------------|--------------------|
//! | `CVCtrl_BHB42XXX`    | CV1835/CV183x| BHB42XXX (S19j Pro / S19 / S19i / S19 XP / **T19**) | PIC1704 |
//! | `BBCtrl_BHB42XXX`    | AM335x BB    | BHB42XXX (S19j Pro) | PIC1704 (this wave) |
//! | `AMLCtrl_BHB42XXX`   | Amlogic A113D| BHB42XXX (S19j Pro AML) | PIC1704 (this wave) |
//! | `AMLCtrl_BHB56xxx`   | Amlogic A113D| BHB569xx (S19k Pro / S21) | dsPIC33EP (existing) |
//! | `AMLCtrl_BHB68xxx`   | Amlogic A113D| BHB68xxx (S21 NoPic) | NoPic |
//!
//! W11.3 marker expansion (2026-05-09): RE2 hardware catalog §6.1 confirms
//! S19, S19i, and S19 XP all use the **same** PIC1704 register map at
//! I²C 0x20 as S19j Pro. RE2 also confirms all CVCtrl-family hashboards
//! share the `CVCtrl_BHB42XXX` `/etc/subtype` string — there is no
//! per-SKU subtype string. The classifier therefore needs no new arms;
//! only the compile-time `Pic1704Authorized` whitelist in
//! `dcentrald_asic::pic1704::service::platforms` grew (Cv1835S19,
//! Cv1835S19i, Cv1835S19XP). Subtype + 0x20 ACK probe still both gate
//! routing —  invariants intact.
//!
//! W12 T19 expansion (2026-05-10): RE3 hardware catalog §2.5 (Antminer
//! T19) lists ONLY Cvitek CV183x as the T19 carrier (no AM335x BB or
//! Amlogic alternates). RE3 §5.1 (PSU table) confirms T19 ships APW12
//! (the SMBus-opcode PSU paired with PIC1704). The T19 hashboard reuses
//! the BM1362-family `CVCtrl_BHB42XXX` subtype family, so the classifier
//! decision table needs no new arms — T19 routes through the existing
//! `CVCtrl_BHB42XXX → Pic1704` branch. Only the compile-time
//! `Pic1704Authorized` whitelist grew (`Cv1835T19`). The two-stage
//! subtype + 0x20 ACK probe still gates routing.
//!
//! S21 Amlogic note: RE2 §2.6 lists S21 with PIC1704 at I²C 0x20, but
//! the live S21 unit at .135 has been proven NoPic (TAS5782M kernel-
//! managed DAC).  root corruption-prevention guarantee #2
//! and  lock S21 to NoPic. The
//! classifier therefore catches *any* unknown `AMLCtrl_*` string in the
//! NoPic catch-all arm — which is the correct conservative posture for
//! S21 SKUs whose subtype string we have not ground-truthed.
//!
//! Source-of-truth files used for the canonical strings:
//! - `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/.../ROOTFS_AM335x/BBCtrl_rootfs/etc/subtype`
//!   → `BBCtrl_BHB42XXX`
//! - `DCENT_OS_DEVELOPMENT_KIT_FROMRE1/.../ROOTFS_CV1835/CVCtrl_rootfs/etc/subtype`
//!   → `CVCtrl_BHB42XXX`
//!
//! The classifier is paired with a runtime `i2cdetect 0x20` ACK probe so
//! a unit whose `/etc/subtype` lies (or is missing entirely) cannot
//! accidentally route into the PIC1704 path. A misclassification on a
//! production am2 / am3-aml unit would attempt to talk PIC1704 protocol
//! to a dsPIC and corrupt MSSP — the probe is the defense in depth.
//!
//! This module is host-safe: on Windows / non-Linux the file read returns
//! `None` and the I²C probe returns `false`, so unit tests run without
//! touching `/dev/`.

use crate::platform::config::VoltageControllerKind;

/// Canonical `/etc/subtype` path on Linux. Static for ground-truthing.
pub const SUBTYPE_PATH: &str = "/etc/subtype";

/// Read `/etc/subtype` and return its trimmed contents.
///
/// On non-Linux hosts (e.g. Windows test harness), returns `None` so
/// tests can run without touching the filesystem.
pub fn read_subtype() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(SUBTYPE_PATH) {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    tracing::warn!(
                        path = SUBTYPE_PATH,
                        "platform subtype: file present but empty"
                    );
                    None
                } else {
                    tracing::info!(
                        path = SUBTYPE_PATH,
                        subtype = %trimmed,
                        "platform subtype: read"
                    );
                    Some(trimmed.to_string())
                }
            }
            Err(e) => {
                tracing::debug!(
                    path = SUBTYPE_PATH,
                    error = %e,
                    "platform subtype: not present (legacy / non-Bitmain firmware)"
                );
                None
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Host (Windows/macOS) test path — no /etc/subtype.
        None
    }
}

/// Classify a `/etc/subtype` string into a `VoltageControllerKind`.
///
/// Pure function: no I/O. Logging side-effects only — `tracing::debug!` for a
/// matched arm (and for an absent `/etc/subtype`, which is normal on some
/// units), and `tracing::warn!` for a present-but-unrecognized subtype string
/// (visible in a beta, conservative default preserved). Safe to call from unit
/// tests.
///
/// Decision table:
/// - `*Ctrl_BHB42XXX` (CV / BB / AML) → `Pic1704`
/// - `AMLCtrl_BHB56xxx` (S19k Pro / S21 with framed dsPIC) → `Dspic33Ep`
/// - other `AML*` strings (catch-all for S21 NoPic variants) → `NoPic`
/// - `S9` / Bitmain stock S9 → `Pic16f1704`
/// - missing → `Dspic33Ep` (legacy safe path for units without `/etc/subtype`)
/// - present but unknown → `NoPic` (fail closed: issue no PIC/dsPIC voltage commands)
pub fn classify_voltage_controller(subtype: Option<&str>) -> VoltageControllerKind {
    let s = match subtype {
        Some(s) => s,
        None => {
            tracing::debug!("subtype: absent → defaulting to Dspic33Ep");
            return VoltageControllerKind::Dspic33Ep;
        }
    };

    // Pre-compute lower-case-or-as-is for case-tolerant matching of the
    // BHB-family suffix. The `Ctrl_` prefix is already case-stable across
    // the dev-kit rootfs tree.
    let upper = s.to_ascii_uppercase();

    // BHB42XXX hashboard family — PIC1704 across all 3 carriers.
    if upper.contains("BHB42XXX")
        && (upper.starts_with("CVCTRL_")
            || upper.starts_with("BBCTRL_")
            || upper.starts_with("AMLCTRL_"))
    {
        tracing::debug!(subtype = %s, "subtype: BHB42XXX → Pic1704");
        return VoltageControllerKind::Pic1704;
    }

    // BHB56xxx hashboard family on Amlogic — framed dsPIC33EP path
    // (S19k Pro at .78, S21 framed-dsPIC variants).
    if upper.starts_with("AMLCTRL_") && upper.contains("BHB56") {
        tracing::debug!(subtype = %s, "subtype: AMLCtrl_BHB56xxx → Dspic33Ep");
        return VoltageControllerKind::Dspic33Ep;
    }

    // Catch-all for other Amlogic carrier strings — treat as S21-class
    // NoPic until a more specific subtype lands. The historical S21 unit
    // at .135 boots without a `/etc/subtype` at all, so this branch is
    // mostly future-proofing for fresh BraiinsOS+ images.
    if upper.starts_with("AMLCTRL_") {
        tracing::debug!(subtype = %s, "subtype: AMLCtrl_* (non-BHB42/56) → NoPic");
        return VoltageControllerKind::NoPic;
    }

    // S9 stock Bitmain (`S9`, `S9j`, `S9k`, …) — PIC16F1704.
    if upper.starts_with("S9") || upper == "S9_BHB09001" {
        tracing::debug!(subtype = %s, "subtype: S9 family → Pic16f1704");
        return VoltageControllerKind::Pic16f1704;
    }

    // A subtype string was present but matched no known family. Fail closed to
    // NoPic so a fresh or mistyped controller marker never silently opts into a
    // PIC/dsPIC voltage command family. Missing subtype still keeps the legacy
    // Dspic33Ep fallback above for older images that never shipped the file.
    tracing::warn!(
        subtype = %s,
        "subtype: UNKNOWN string → NoPic fail-closed \
         (no PIC/dsPIC voltage commands; report this subtype so it can be classified)"
    );
    VoltageControllerKind::NoPic
}

/// Probe `/dev/i2c-{bus}` for an ACK at I²C address 0x20.
///
/// Mirrors `i2cdetect -y -q {bus} 0x20 0x20`: opens the bus, performs a
/// zero-byte write, returns true if the slave ACK'd. Used as the runtime
/// gate: even when `/etc/subtype` says PIC1704 is present, the daemon
/// will not construct a `Pic1704Service` until this returns true.
///
/// On non-Linux hosts (test harness), returns `false`.
///
/// Note on safety: this MUST go through the in-crate `I2cBus::open` path
/// rather than a raw `/dev/i2c-N` open, because production am2 / am3-aml
/// builds enforce the SINGLE-I2C-OWNER architecture. The `pub(crate)`
/// `I2cBus::open` is reachable from this module because `subtype.rs`
/// lives inside the `dcentrald-hal` crate. The probe is a one-shot
/// performed BEFORE the long-running I²C service is spawned.
pub fn probe_pic1704_at_0x20(bus: u8) -> bool {
    #[cfg(target_os = "linux")]
    {
        const PIC1704_PROBE_ADDR: u8 = 0x20;
        let mut i2c = match crate::i2c::I2cBus::open(bus) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    bus,
                    error = %e,
                    "PIC1704 probe: /dev/i2c-{} open failed → false", bus
                );
                return false;
            }
        };
        if let Err(e) = i2c.set_slave(PIC1704_PROBE_ADDR) {
            tracing::warn!(
                bus,
                addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                error = %e,
                "PIC1704 probe: set_slave failed → false"
            );
            return false;
        }
        // Zero-byte write to test ACK. `i2cdetect` uses SMBus quick-write
        // for safe-list addresses; 0x20 is on its read list, so writing
        // an empty byte slice issues a START + addr-w + STOP only — no
        // payload byte is sent. This matches the safe `i2cdetect`
        // convention and never modifies a slave register.
        match i2c.write(&[]) {
            Ok(_) => {
                tracing::info!(
                    bus,
                    addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                    "PIC1704 probe: ACK observed → PIC1704 candidate confirmed"
                );
                true
            }
            Err(e) => {
                tracing::info!(
                    bus,
                    addr = format_args!("0x{:02X}", PIC1704_PROBE_ADDR),
                    error = %e,
                    "PIC1704 probe: NACK / write error → PIC1704 not present"
                );
                false
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = bus;
        false
    }
}

/// Classify from a DCENT_OS *board-target* name / PSU-kind string when
/// there is no `/etc/subtype` to read (Phase B, am3-bb on
/// `S19J_IO_BOARD_V2_0`).
///
/// LuxOS units do not ship `/etc/subtype` — the BeagleBone platform reads
/// `/etc/dcentos/board_target` instead and (on `am3-bb-s19jpro`) loads a
/// per-board-target TOML whose `[platform].voltage_controller` can declare
/// `"dspic33ep-fw89"` and whose legacy `[i2c].psu_kind` still declares
/// `"apw12-uart-tunnel"`. The APW121215f PSU is upstream on bus 1, but the
/// 2026-05-13 LuxOS ftrace proves fw=0x89 dsPIC33EP controllers at
/// 0x20/0x21/0x22 on hashboard bus 0. So both the explicit dsPIC string and
/// the legacy APW tunnel string classify to [`VoltageControllerKind::Dspic33Ep`].
///
/// Returns `None` for any string this helper doesn't recognize, so callers
/// can fall back to their static default.
///
/// # Why NOT a new `VoltageControllerKind` variant
///
/// A dedicated `Apw12UartTunnel` variant would mix PSU transport with
/// hashboard voltage-controller identity. The reusable controller identity is
/// still `Dspic33Ep`; the AM3 BB mining path owns the LuxOS-trace fw=0x89
/// sequence and heartbeat timing.
pub fn classify_from_board_target(s: &str) -> Option<VoltageControllerKind> {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "dspic33ep" | "dspic33ep-fw89" | "dspic33ep16gs202" | "dspic-fw89" => {
            tracing::info!(
                board_target_voltage_controller = %s,
                "board-target classification: explicit dsPIC33EP voltage controller",
            );
            Some(VoltageControllerKind::Dspic33Ep)
        }
        // Legacy AM3 BB board-targets used the PSU transport string as the
        // voltage-controller hint. Preserve compatibility, but route to the
        // dsPIC path now that the live LuxOS trace proved the controllers.
        "apw12-uart-tunnel" | "apw12_uart_tunnel" | "apw-uart-tunnel" => {
            tracing::info!(
                board_target_psu_kind = %s,
                "board-target classification: apw12-uart-tunnel → Dspic33Ep \
                 (am3-bb S19J_IO_BOARD_V2_0; APW12 upstream PSU plus hashboard fw=0x89 dsPICs)"
            );
            Some(VoltageControllerKind::Dspic33Ep)
        }
        _ => {
            tracing::debug!(
                board_target_psu_kind = %s,
                "board-target classification: unrecognized PSU-kind string → no override"
            );
            None
        }
    }
}

/// Combined classification + runtime probe. Call this once at platform
/// init: the result becomes `PlatformConfig::voltage_controller`.
///
/// If the subtype string says PIC1704 but the runtime probe FAILS, this
/// returns `Dspic33Ep` (the existing safe path) instead of `Pic1704`.
/// This guarantees no regression on s19jpro (sustained-mining unit
/// running existing dsPIC path) or any other unit whose `/etc/subtype`
/// might disagree with reality.
pub fn classify_with_probe(subtype: Option<&str>, i2c_bus: u8) -> VoltageControllerKind {
    let classified = classify_voltage_controller(subtype);
    if classified == VoltageControllerKind::Pic1704 {
        if probe_pic1704_at_0x20(i2c_bus) {
            tracing::info!(
                subtype = %subtype.unwrap_or("<missing>"),
                bus = i2c_bus,
                kind = classified.as_str(),
                "voltage controller classification: PIC1704 confirmed by subtype + probe"
            );
            VoltageControllerKind::Pic1704
        } else {
            tracing::warn!(
                subtype = %subtype.unwrap_or("<missing>"),
                bus = i2c_bus,
                "voltage controller classification: subtype says PIC1704 but 0x20 probe \
                 NACK — falling back to Dspic33Ep (existing safe path)"
            );
            VoltageControllerKind::Dspic33Ep
        }
    } else {
        tracing::info!(
            subtype = %subtype.unwrap_or("<missing>"),
            kind = classified.as_str(),
            "voltage controller classification: subtype-only (no PIC1704 probe needed)"
        );
        classified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_voltage_controller_full_table() {
        // Canonical PIC1704 carriers (BHB42XXX hashboard family).
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );

        // Existing dsPIC33EP path (S19k Pro / S21 framed-dsPIC).
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56902")),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56999")),
            VoltageControllerKind::Dspic33Ep,
        );

        // Other Amlogic strings → NoPic catch-all.
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB68900")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_S21Pro")),
            VoltageControllerKind::NoPic,
        );

        // S9 family.
        assert_eq!(
            classify_voltage_controller(Some("S9")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9_BHB09001")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9j")),
            VoltageControllerKind::Pic16f1704,
        );

        // Missing subtype keeps the legacy Dspic33Ep default; present unknown
        // strings fail closed to NoPic.
        assert_eq!(
            classify_voltage_controller(None),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("some-unknown-string")),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn unknown_subtype_string_takes_nopic_fail_closed_default() {
        // Present-but-unrecognized subtype is visible at `warn!` and must
        // fail closed to NoPic so a new controller marker never silently routes
        // into a PIC/dsPIC voltage command family. Missing subtype is tested
        // separately because old images may not ship `/etc/subtype`.
        for unknown in ["totally-bogus", "XYZCtrl_BHB99999", "bhb42xxx-no-prefix"] {
            assert_eq!(
                classify_voltage_controller(Some(unknown)),
                VoltageControllerKind::NoPic,
                "unknown subtype {unknown:?} must fail closed to NoPic",
            );
        }
    }

    #[test]
    fn classify_is_case_tolerant_for_bhb_suffix() {
        // The dev-kit ships `BHB42XXX` (uppercase X). Production VNish
        // images have shipped `BHB42xxx` historically; classifier must
        // handle both without re-routing to the dsPIC path.
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42xxx")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("cvctrl_bhb42xxx")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn read_subtype_returns_none_on_windows_host() {
        // Host-test invariant: Linux-only file path, host returns None.
        // This is what allows the table-driven test above to run on the
        // Windows dev box without touching /etc.
        #[cfg(not(target_os = "linux"))]
        assert_eq!(read_subtype(), None);
    }

    #[test]
    fn probe_pic1704_at_0x20_returns_false_on_non_linux_host() {
        // Test-harness invariant: never opens a real I2C bus on Windows.
        #[cfg(not(target_os = "linux"))]
        assert!(!probe_pic1704_at_0x20(0));
    }

    #[test]
    fn classify_with_probe_falls_back_to_dspic_when_probe_misses() {
        // On non-Linux hosts the probe always returns false, so any
        // PIC1704-classified subtype must fall back to Dspic33Ep —
        // proving the no-regression guarantee end-to-end on host CI.
        #[cfg(not(target_os = "linux"))]
        {
            assert_eq!(
                classify_with_probe(Some("BBCtrl_BHB42XXX"), 0),
                VoltageControllerKind::Dspic33Ep,
            );
            assert_eq!(
                classify_with_probe(Some("CVCtrl_BHB42XXX"), 0),
                VoltageControllerKind::Dspic33Ep,
            );
            assert_eq!(
                classify_with_probe(Some("AMLCtrl_BHB42XXX"), 0),
                VoltageControllerKind::Dspic33Ep,
            );
        }
    }

    // -----------------------------------------------------------------
    //  W11.3 marker expansion regression tests (RE2 §2.5 + §6.1).
    //
    //  All CVCtrl-family hashboards report `CVCtrl_BHB42XXX` regardless
    //  of the SKU (S19 / S19i / S19 XP / S19j Pro). The classifier MUST
    //  route every CVCtrl_BHB42XXX subtype to Pic1704 — and the
    //  S19j Pro path must continue to work unchanged.
    // -----------------------------------------------------------------

    #[test]
    fn s19_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19 (Standard) → Cvitek CV183x + PIC1704.
        // Subtype string is the same `CVCtrl_BHB42XXX` family identifier
        // shared by every CVCtrl-class hashboard.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s19i_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19i → Cvitek CV183x + PIC1704.
        // Same subtype family as S19 / S19j Pro / S19 XP.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s19xp_cvctrl_classifies_pic1704() {
        // RE2 §2.5: Antminer S19 XP (Hydra) → Cvitek CV183x + PIC1704.
        // Note RE2 also lists an Amlogic S905 hydro variant of S19 XP;
        // when its subtype string is captured live it will be tested
        // separately. For now we only assert the CVCtrl path.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn s21_aml_classifies_nopic() {
        //  root corruption-prevention guarantee #2 +
        // : S21 Amlogic is NoPic
        // (TAS5782M kernel-managed DAC). The classifier MUST NOT route
        // any plausible S21 subtype string into Pic1704.
        //
        // The known S21 hashboard family identifier candidates from RE2
        // (BHB68xxx, BHB68606, BHB68603) all fall through to the
        // `AMLCtrl_*` catch-all arm → NoPic. Verify the contract.
        for s21_sub in [
            "AMLCtrl_BHB68xxx",
            "AMLCtrl_BHB68606",
            "AMLCtrl_BHB68603",
            "AMLCtrl_S21",
            "AMLCtrl_S21Pro",
            "AMLCtrl_S21XP",
        ] {
            let kind = classify_voltage_controller(Some(s21_sub));
            assert_ne!(
                kind,
                VoltageControllerKind::Pic1704,
                "S21 subtype `{}` MUST NOT classify as Pic1704 — see \
                 corruption-prevention guarantee #2",
                s21_sub,
            );
            assert_eq!(
                kind,
                VoltageControllerKind::NoPic,
                "S21 subtype `{}` should classify as NoPic",
                s21_sub,
            );
        }
    }

    // -----------------------------------------------------------------
    //  W12 T19 expansion regression tests (RE3 §2.5 + §5.1).
    //
    //  RE3 lists ONLY Cvitek CV183x as the T19 carrier (no AM335x BB
    //  or Amlogic alternates). T19 hashboards share the same
    //  `CVCtrl_BHB42XXX` subtype family + APW12 SMBus PSU as the rest
    //  of the BM1362-family CV183x line, so the classifier MUST route
    //  T19 through the existing CVCtrl_BHB42XXX → Pic1704 path.
    // -----------------------------------------------------------------

    #[test]
    fn t19_bhb42_subtype_returns_pic1704_with_probe() {
        // RE3 §2.5: Antminer T19 ships on Cvitek CV183x only with the
        // BM1362-family BHB42XXX hashboard pattern → PIC1704 gate.
        // The subtype-only classification is Pic1704; on host the
        // probe falls back to Dspic33Ep (no /dev/i2c-0).
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        // On non-Linux hosts the probe always returns false, proving
        // the no-regression contract: subtype says Pic1704 but probe
        // misses → Dspic33Ep fallback.
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            classify_with_probe(Some("CVCtrl_BHB42XXX"), 0),
            VoltageControllerKind::Dspic33Ep,
        );
    }

    #[test]
    fn t19_bhb56_subtype_returns_dspic() {
        // Defense-in-depth: if a T19 SKU were ever discovered with a
        // BHB56-family hashboard (RE3 does NOT document this — purely
        // hypothetical), the classifier MUST route it to the existing
        // dsPIC path, not Pic1704. Mirrors the S19k Pro / S21 framed
        // dsPIC contract enforced by the BHB56xxx → Dspic33Ep arm.
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56999")),
            VoltageControllerKind::Dspic33Ep,
        );
    }

    #[test]
    fn t19_unknown_subtype_returns_nopic() {
        // Any unrecognized T19-flavored subtype string must fail closed to
        // NoPic, never silently route to Pic1704 or dsPIC. Until a live T19
        // unit ground-truths a per-SKU subtype string, the only known-safe T19
        // string is the generic `CVCtrl_BHB42XXX` family identifier.
        assert_eq!(
            classify_voltage_controller(Some("T19_unknown")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_T19")),
            VoltageControllerKind::NoPic,
        );
    }

    #[test]
    fn s19jpro_cvctrl_still_pic1704() {
        //  regression — the S19j Pro CV1835 path that landed in
        // 2026-05-09 must remain on the Pic1704 route. If this fails,
        // the W11.3 expansion broke an existing platform.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
    }

    #[test]
    fn classify_with_probe_passes_through_non_pic1704_kinds() {
        // Non-PIC1704 classifications skip the probe entirely.
        assert_eq!(
            classify_with_probe(Some("AMLCtrl_BHB56902"), 0),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_with_probe(Some("AMLCtrl_S21NoPic"), 0),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_with_probe(Some("S9"), 0),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_with_probe(None, 0),
            VoltageControllerKind::Dspic33Ep,
        );
    }

    // -----------------------------------------------------------------
    //  Phase B (2026-05-12) — board-target classification for am3-bb on
    //  S19J_IO_BOARD_V2_0 (LuxOS units have no /etc/subtype).
    // -----------------------------------------------------------------

    #[test]
    fn classify_from_board_target_recognizes_apw12_uart_tunnel_as_dspic() {
        // The .79 unit (AM335x BB on S19J_IO_BOARD_V2_0) uses an APW121215f
        // upstream PSU and fw=0x89 dsPIC hashboard controllers. Stale TOMLs may
        // still use the PSU transport string as their voltage-controller hint.
        for s in [
            "apw12-uart-tunnel",
            "apw12_uart_tunnel",
            "apw-uart-tunnel",
            "  APW12-UART-TUNNEL  ", // case + whitespace tolerant
        ] {
            assert_eq!(
                classify_from_board_target(s),
                Some(VoltageControllerKind::Dspic33Ep),
                "`{}` must classify to Dspic33Ep",
                s
            );
        }
    }

    #[test]
    fn classify_from_board_target_returns_none_for_unknown_strings() {
        // Unrecognized PSU-kind strings give None so the platform falls
        // back to its static default — never a silent misroute.
        for s in ["", "apw12-smbus", "pic1704", "some-future-psu"] {
            assert_eq!(
                classify_from_board_target(s),
                None,
                "`{}` must NOT be recognized by classify_from_board_target",
                s
            );
        }
        assert_eq!(
            classify_from_board_target("dspic33ep-fw89"),
            Some(VoltageControllerKind::Dspic33Ep)
        );
    }

    #[test]
    fn classify_from_board_target_does_not_change_existing_subtype_paths() {
        // Defense-in-depth: the Phase B board-target helper is a separate
        // entry point. It MUST NOT affect what `classify_voltage_controller`
        // or `classify_with_probe` return for the existing /etc/subtype
        // strings — those are byte-identical to pre-Phase-B behavior.
        assert_eq!(
            classify_voltage_controller(Some("CVCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("BBCtrl_BHB42XXX")),
            VoltageControllerKind::Pic1704,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB56902")),
            VoltageControllerKind::Dspic33Ep,
        );
        assert_eq!(
            classify_voltage_controller(Some("AMLCtrl_BHB68xxx")),
            VoltageControllerKind::NoPic,
        );
        assert_eq!(
            classify_voltage_controller(Some("S9")),
            VoltageControllerKind::Pic16f1704,
        );
        assert_eq!(
            classify_voltage_controller(None),
            VoltageControllerKind::Dspic33Ep,
        );
        // And the board-target helper recognizes ONLY the apw12-uart-tunnel
        // family — it must not also start accepting a subtype string.
        assert_eq!(classify_from_board_target("CVCtrl_BHB42XXX"), None);
        assert_eq!(classify_from_board_target("BBCtrl_BHB42XXX"), None);
        assert_eq!(classify_from_board_target("S9"), None);
    }
}
