#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportTier {
    Validated,
    Experimental,
    Planned,
    Blind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPicTypeHint {
    Pic16,
    DsPic,
    NoPic,
}

/// Strongly-typed model enum for variants with full hardware detail.
///
/// The string-keyed [`lookup_model`] remains the primary API for legacy
/// callers; this enum is the forward-looking form that enables exhaustive
/// pattern matches in drivers that need platform-specific codepaths
/// (e.g. MiscCtrl triple-write, PIC framing, PSU protocol dispatch).
///
/// Phase 2 Agent D (2026-04-20): only `S19jProAm2` is populated so far.
/// Additional variants will be introduced as each platform is validated
/// end-to-end; until then, drivers should keep consuming [`ModelSpec`]
/// via the string key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// Antminer S19j Pro on am2 X19 control board (Zynq + BM1362 + dsPIC33).
    /// Live probe: 203.0.113.139.
    S19jProAm2,
}

impl Model {
    /// Canonical string key used with [`lookup_model`].
    pub const fn model_key(self) -> &'static str {
        match self {
            Self::S19jProAm2 => "s19jproam2",
        }
    }

    /// bos_platform value as reported by `/etc/bos_platform` on BraiinsOS.
    pub const fn platform_key(self) -> &'static str {
        match self {
            Self::S19jProAm2 => "zynq-bm3-am2",
        }
    }

    /// Board family (matches Buildroot board dir / sysupgrade prefix family).
    pub const fn board_family(self) -> &'static str {
        match self {
            Self::S19jProAm2 => "am2",
        }
    }

    /// Sysupgrade board target — MUST match the tarball prefix exactly
    /// (; wrong prefix = brick).
    pub const fn board_target(self) -> &'static str {
        match self {
            Self::S19jProAm2 => "am2-s19j",
        }
    }

    /// Lookup the matching [`ModelSpec`] (infallible for enum variants).
    pub fn spec(self) -> ModelSpec {
        // SAFETY: every enum variant has a registered ModelSpec entry.
        lookup_model(self.model_key()).expect("Model variant has registered ModelSpec")
    }
}

/// Detect the [`Model`] from a BraiinsOS `bos_platform` string plus an
/// observed ASIC chip ID (as returned by the GetAddress broadcast).
///
/// `zynq-bm3-am2` alone is ambiguous — the S19 Pro (BM1398) and S19j Pro
/// (BM1362) share the am2 control board. We disambiguate via `chip_id`:
///   - 0x1362 → `S19jProAm2`
///   - 0x1398 → S19 Pro (no enum variant yet; returns `None` for Phase 2 D)
///
/// Returns `None` when the (platform, chip) pair isn't a known enum variant.
/// Callers should fall back to [`lookup_model`] with the configured model
/// string in that case.
pub fn detect_model_from_platform(bos_platform: &str, chip_id: Option<u16>) -> Option<Model> {
    match (bos_platform.trim(), chip_id) {
        ("zynq-bm3-am2", Some(0x1362)) => Some(Model::S19jProAm2),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProfile {
    X17S17,
    X17T17,
    X17S17Plus,
    X17T17Plus,
    X17S17e,
    X17T17e,
}

impl RuntimeProfile {
    pub fn key(self) -> &'static str {
        match self {
            Self::X17S17 => "x17-s17-dspic-48",
            Self::X17T17 => "x17-t17-pic16-30",
            Self::X17S17Plus => "x17-s17plus-pic16-65",
            Self::X17T17Plus => "x17-t17plus-pic16-44",
            Self::X17S17e => "x17-s17e-dspic-planned",
            Self::X17T17e => "x17-t17e-pic16-planned",
        }
    }

    pub fn chips_per_chain(self) -> Option<u8> {
        match self {
            Self::X17S17 => Some(48),
            Self::X17T17 => Some(30),
            Self::X17S17Plus => Some(65),
            Self::X17T17Plus => Some(44),
            Self::X17S17e | Self::X17T17e => None,
        }
    }

    pub fn pic_type_hint(self) -> ModelPicTypeHint {
        match self {
            Self::X17S17 | Self::X17S17e => ModelPicTypeHint::DsPic,
            Self::X17T17 | Self::X17S17Plus | Self::X17T17Plus | Self::X17T17e => {
                ModelPicTypeHint::Pic16
            }
        }
    }

    pub fn pic_addrs_hint(self) -> Option<&'static [u8]> {
        match self.pic_type_hint() {
            ModelPicTypeHint::Pic16 => Some(X17_PIC_ADDRS),
            ModelPicTypeHint::DsPic | ModelPicTypeHint::NoPic => None,
        }
    }
}

const X17_PIC_ADDRS: &[u8] = &[0x50, 0x51, 0x52];

/// Thermal threshold bundle (degrees Celsius).
///
/// Semantics mirror the S9/S19 thermal controller:
///   - `target_c`  — PID setpoint
///   - `hot_c`     — push fans to max (honors PWM cap)
///   - `dangerous_c` — PCB/board danger line; throttle frequency
///   - `dangerous_internal_c` — die/chip danger line; disable hash boards
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThermalThresholds {
    pub target_c: u8,
    pub hot_c: u8,
    pub dangerous_c: u8,
    pub dangerous_internal_c: u8,
}

/// Extended, fully-populated hardware constants for a validated model variant.
///
/// This is the Phase-2 forward-looking form: one struct per `Model` enum
/// variant with every hardware constant a driver could need, verified
/// against a live probe. Keeps the legacy string-keyed [`ModelSpec`] system
/// untouched so existing call sites are unaffected.
///
/// Lookup via [`Model::extended_spec`] or [`lookup_extended_spec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelExtendedSpec {
    /// Canonical string key (matches [`Model::model_key`]).
    pub model_key: &'static str,
    /// Hardware chain count (populated boards are a runtime fact).
    pub chain_count: u8,
    /// ASIC chips per populated chain.
    pub chips_per_chain: u8,
    /// SHA-256 cores per ASIC chip (e.g. 114 for BM1387, 4 for BM1362).
    pub cores_per_chip: u16,
    /// Rated full-unit hashrate in TH/s (per Bitmain datasheet).
    pub rated_hashrate_th: u16,
    /// Rated full-unit wall power in watts.
    pub rated_power_w: u16,
    /// Default mining frequency in MHz (conservative pre-autotune).
    pub default_freq_mhz: u16,
    /// Default shared-rail voltage in millivolts.
    pub default_voltage_mv: u16,
    /// Hard voltage floor in millivolts.
    pub voltage_min_mv: u16,
    /// Hard voltage ceiling in millivolts.
    pub voltage_max_mv: u16,
    /// Minimum legal mining frequency in MHz.
    pub freq_min_mhz: u16,
    /// Maximum legal mining frequency in MHz.
    pub freq_max_mhz: u16,
    /// UART baud used during ASIC enumeration / init phase.
    pub init_baud: u32,
    /// UART baud used during sustained mining (post-MiscCtrl upgrade).
    pub mining_baud: u32,
    /// Thermal control thresholds.
    pub thermal: ThermalThresholds,
    /// Fan channel count on the control board.
    pub fan_count: u8,
    /// Default industrial-profile PWM cap (home/heater profiles override
    /// via config down to ~30 —).
    pub fan_pwm_cap_industrial: u8,
    /// PIC I2C bus addresses (empty slice for NoPic platforms).
    pub pic_addrs: &'static [u8],
    /// Primary expected PIC firmware byte (e.g. 0x89 for S19j Pro am2).
    pub pic_fw_byte_expected: u8,
    /// Additional PIC firmware bytes considered compatible.
    pub pic_fw_bytes_secondary: &'static [u8],
    /// bos_platform key (e.g. `zynq-bm3-am2`).
    pub platform_key: &'static str,
    /// Board family string (e.g. `am2`) — MUST match Buildroot / sysupgrade.
    pub board_family: &'static str,
    /// Sysupgrade target prefix (e.g. `am2-s19j`). Wrong prefix = brick
    ///.
    pub board_target: &'static str,
}

/// PIC firmware bytes considered compatible with the S19j Pro am2 driver.
/// Primary is 0x89 (observed on .139 — BraiinsOS+ 26.04-plus). 0x88/0xB9/0xFE
/// are additional S19/S17+ am2 PIC firmware variants seen across the fleet.
const S19JPRO_AM2_PIC_FW_SECONDARY: &[u8] = &[0x88, 0xB9, 0xFE];

/// PIC I2C addresses for S19j Pro am2 (dsPIC33 on am2 X19 control board).
/// Source: live probe of 203.0.113.139 (see
/// ).
const S19JPRO_AM2_PIC_ADDRS: &[u8] = &[0x20, 0x21, 0x22];

/// S19j Pro am2 ModelExtendedSpec — verified on 203.0.113.139 (2026-04-20).
pub const S19JPRO_AM2_SPEC: ModelExtendedSpec = ModelExtendedSpec {
    model_key: "s19jproam2",
    chain_count: 3,
    chips_per_chain: 126,
    cores_per_chip: 4,
    rated_hashrate_th: 104,
    rated_power_w: 3068,
    default_freq_mhz: 545,
    default_voltage_mv: 13_700,
    voltage_min_mv: 11_960,
    voltage_max_mv: 15_200,
    freq_min_mhz: 50,
    freq_max_mhz: 1300,
    init_baud: 115_200,
    mining_baud: 3_125_000,
    thermal: ThermalThresholds {
        target_c: 60,
        hot_c: 80,
        dangerous_c: 90,
        dangerous_internal_c: 110,
    },
    fan_count: 4,
    // Industrial-profile default. Home/heater users MUST override to 30 via
    // config. The driver itself never hardcodes
    // above config — this value is only the upper bound for the industrial
    // profile.
    fan_pwm_cap_industrial: 80,
    pic_addrs: S19JPRO_AM2_PIC_ADDRS,
    pic_fw_byte_expected: 0x89,
    pic_fw_bytes_secondary: S19JPRO_AM2_PIC_FW_SECONDARY,
    platform_key: "zynq-bm3-am2",
    board_family: "am2",
    board_target: "am2-s19j",
};

impl Model {
    /// Lookup the fully-populated hardware spec for this variant.
    pub const fn extended_spec(self) -> ModelExtendedSpec {
        match self {
            Self::S19jProAm2 => S19JPRO_AM2_SPEC,
        }
    }
}

/// String-keyed lookup for [`ModelExtendedSpec`]. Returns `None` for keys
/// that don't (yet) have a full Phase-2 spec.
pub fn lookup_extended_spec(model: &str) -> Option<ModelExtendedSpec> {
    let normalized = normalize_model_token(model);
    match normalized.as_str() {
        "s19jproam2" => Some(S19JPRO_AM2_SPEC),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub model_key: &'static str,
    pub family_key: &'static str,
    pub chip_label: &'static str,
    pub chip_id: Option<u16>,
    pub chips_per_chain_hint: Option<u8>,
    pub pic_type_hint: Option<ModelPicTypeHint>,
    pub pic_addrs_hint: Option<&'static [u8]>,
    pub support_tier: SupportTier,
}

fn normalize_model_token(model: &str) -> String {
    model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .collect()
}

/// TD-003 expanded Antminer platforms must stay management-only until their
/// promotion gates are complete. This is intentionally narrower than
/// `SupportTier::Experimental`: some experimental entries have live mining
/// evidence, while these specific models are scaffolding/preparation lanes.
pub fn td003_management_only_model(model: &str) -> Option<&'static str> {
    match normalize_model_token(model).as_str() {
        "s15" | "antminers15" => Some("Antminer S15"),
        "t15" | "antminert15" => Some("Antminer T15"),
        "s17" | "s17pro" | "antminers17" | "antminers17pro" => Some("Antminer S17 / S17 Pro"),
        "s17+" | "s17plus" | "antminers17+" | "antminers17plus" => Some("Antminer S17+"),
        "t17" | "antminert17" => Some("Antminer T17"),
        "t17+" | "t17plus" | "antminert17+" | "antminert17plus" => Some("Antminer T17+"),
        "s17e" | "antminers17e" => Some("Antminer S17e"),
        "t17e" | "antminert17e" => Some("Antminer T17e"),
        "t19" | "antminert19" => Some("Antminer T19"),
        "s19xp" | "antminers19xp" => Some("Antminer S19 XP"),
        _ => None,
    }
}

/// Same TD-003 gate as [`td003_management_only_model`], but keyed by the
/// baked `/etc/dcentos/board_target` marker. This is a runtime defense-in-depth
/// layer: a stale or missing `[mining].model` must not let an in-development
/// board stamp reach voltage, PIC/PMBus, ASIC init, or hash dispatch paths.
pub fn td003_management_only_board_target(board_target: &str) -> Option<&'static str> {
    match normalize_model_token(board_target).as_str() {
        "am1s15" => Some("Antminer S15"),
        "am1t15" => Some("Antminer T15"),
        "am2s17plus" => Some("Antminer S17+"),
        "am2s17" | "am2s17p" | "am2s17pro" | "am1s17" => Some("Antminer S17 / S17 Pro"),
        "x17s17edspicplanned" => Some("Antminer S17e"),
        "x17t17" | "am2t17" => Some("Antminer T17"),
        "am2t17plus" => Some("Antminer T17+"),
        "x17t17epic16planned" => Some("Antminer T17e"),
        "am2t19" | "x19t19" | "cv183xt19" | "cv1835t19" | "am3t19" => Some("Antminer T19"),
        "am2s19xp" | "am3s19xp" | "amlogicxps19" | "cv1835s19xp" | "cv183xs19xp" => {
            Some("Antminer S19 XP")
        }
        _ => None,
    }
}

pub fn lookup_model(model: &str) -> Option<ModelSpec> {
    let normalized = normalize_model_token(model);

    let spec = match normalized.as_str() {
        "s9" => ModelSpec {
            model_key: "s9",
            family_key: "bm1387",
            chip_label: "BM1387",
            chip_id: Some(0x1387),
            chips_per_chain_hint: Some(63),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Validated,
        },
        "s9+" | "s9plus" => ModelSpec {
            model_key: "s9+",
            family_key: "bm1387",
            chip_label: "BM1387P",
            chip_id: Some(0x1387),
            chips_per_chain_hint: Some(84),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "t9" => ModelSpec {
            model_key: "t9",
            family_key: "bm1387",
            chip_label: "BM1387",
            chip_id: Some(0x1387),
            chips_per_chain_hint: Some(57),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "t9+" | "t9plus" => ModelSpec {
            model_key: "t9+",
            family_key: "bm1387",
            chip_label: "BM1387",
            chip_id: Some(0x1387),
            chips_per_chain_hint: Some(18),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "s15" | "t15" => ModelSpec {
            model_key: "s15",
            family_key: "bm1391",
            chip_label: "BM1391",
            chip_id: None,
            chips_per_chain_hint: None,
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Planned,
        },
        "s17" | "s17pro" => ModelSpec {
            model_key: "s17",
            family_key: "bm1397",
            chip_label: "BM1397",
            chip_id: Some(0x1397),
            chips_per_chain_hint: Some(48),
            pic_type_hint: Some(ModelPicTypeHint::DsPic),
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "t17" => ModelSpec {
            model_key: "t17",
            family_key: "bm1397",
            chip_label: "BM1397",
            chip_id: Some(0x1397),
            chips_per_chain_hint: Some(30),
            pic_type_hint: Some(ModelPicTypeHint::Pic16),
            pic_addrs_hint: Some(X17_PIC_ADDRS),
            support_tier: SupportTier::Experimental,
        },
        "s17+" | "s17plus" => ModelSpec {
            model_key: "s17+",
            family_key: "bm1396",
            chip_label: "BM1396",
            chip_id: Some(0x1396),
            chips_per_chain_hint: Some(65),
            pic_type_hint: Some(ModelPicTypeHint::Pic16),
            pic_addrs_hint: Some(X17_PIC_ADDRS),
            support_tier: SupportTier::Experimental,
        },
        "t17+" | "t17plus" => ModelSpec {
            model_key: "t17+",
            family_key: "bm1396",
            chip_label: "BM1396",
            chip_id: Some(0x1396),
            chips_per_chain_hint: Some(44),
            pic_type_hint: Some(ModelPicTypeHint::Pic16),
            pic_addrs_hint: Some(X17_PIC_ADDRS),
            support_tier: SupportTier::Experimental,
        },
        "s17e" => ModelSpec {
            model_key: "s17e",
            family_key: "bm1397",
            chip_label: "BM1397",
            chip_id: None,
            chips_per_chain_hint: None,
            pic_type_hint: Some(ModelPicTypeHint::DsPic),
            pic_addrs_hint: None,
            support_tier: SupportTier::Planned,
        },
        "t17e" => ModelSpec {
            model_key: "t17e",
            family_key: "bm1397",
            chip_label: "BM1397",
            chip_id: None,
            chips_per_chain_hint: None,
            pic_type_hint: Some(ModelPicTypeHint::Pic16),
            pic_addrs_hint: Some(X17_PIC_ADDRS),
            support_tier: SupportTier::Planned,
        },
        "s19" => ModelSpec {
            model_key: "s19",
            family_key: "bm1398",
            chip_label: "BM1398",
            chip_id: Some(0x1398),
            // NOMINAL, not RE-confirmed. The plain S19 (e.g. 95T) is a different
            // sub-variant than the S19 Pro (110T = 3x114) and its per-chain count is
            // not repo-confirmed; skus.conf lists 3x114 but flags it as borrowed
            // ("rides proven am2-s19pro image") and "binning enumerated at runtime".
            // So this hint only seeds the expected-count display and is superseded
            // the moment the chain enumerates — the two sources deliberately differ
            // until a live S19 confirms one. (SupportTier::Experimental below.)
            chips_per_chain_hint: Some(76),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "s19pro" => ModelSpec {
            model_key: "s19pro",
            family_key: "bm1398",
            chip_label: "BM1398",
            chip_id: Some(0x1398),
            chips_per_chain_hint: Some(114),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "t19" => ModelSpec {
            model_key: "t19",
            family_key: "bm1398",
            chip_label: "BM1398",
            chip_id: Some(0x1398),
            chips_per_chain_hint: None,
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "s19j" => ModelSpec {
            model_key: "s19j",
            family_key: "bm1398",
            chip_label: "BM1398",
            chip_id: Some(0x1398),
            chips_per_chain_hint: None,
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        "s19jpro" | "s19jpro+" | "s19jproplus" => ModelSpec {
            model_key: "s19jpro",
            family_key: "bm1362",
            chip_label: "BM1362",
            chip_id: Some(0x1362),
            chips_per_chain_hint: Some(126),
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Experimental,
        },
        // S19j Pro on am2 X19 control board (Zynq + BM1362 + dsPIC33 @ 0x20-0x22).
        // Dedicated entry so `Model::S19jProAm2::spec()` has a registered ModelSpec
        // and the string key resolves unambiguously. Full hardware constants live
        // in `S19JPRO_AM2_SPEC` (ModelExtendedSpec).
        "s19jproam2" => ModelSpec {
            model_key: "s19jproam2",
            family_key: "bm1362",
            chip_label: "BM1362",
            chip_id: Some(0x1362),
            chips_per_chain_hint: Some(126),
            pic_type_hint: Some(ModelPicTypeHint::DsPic),
            pic_addrs_hint: Some(S19JPRO_AM2_PIC_ADDRS),
            support_tier: SupportTier::Experimental,
        },
        "s19xp" => ModelSpec {
            model_key: "s19xp",
            family_key: "bm1366",
            chip_label: "BM1366",
            chip_id: Some(0x1366),
            chips_per_chain_hint: Some(110),
            // Live-confirmed BHB56xxx hashboards are NoPic (BraiinsOS+
            // model-list shows only the NoPic-class entry for this model).
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "s19k" | "s19kpro" => ModelSpec {
            model_key: "s19k",
            family_key: "bm1366",
            chip_label: "BM1366",
            chip_id: Some(0x1366),
            chips_per_chain_hint: Some(77),
            // S19K Pro is NoPic per `/etc/bosminer.toml` model
            // "Antminer S19K Pro NoPic" on .78 (BraiinsOS+ 25.07-plus,
            // 2026-04-29). Voltage via TAS5782M kernel-managed.
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "s21" => ModelSpec {
            model_key: "s21",
            family_key: "bm1368",
            chip_label: "BM1368",
            chip_id: Some(0x1368),
            chips_per_chain_hint: Some(108),
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "t21" => ModelSpec {
            model_key: "t21",
            family_key: "bm1368",
            chip_label: "BM1368",
            chip_id: Some(0x1368),
            chips_per_chain_hint: Some(108),
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "s21pro" | "s21+" | "s21plus" => ModelSpec {
            model_key: "s21pro",
            family_key: "bm1370",
            chip_label: "BM1370",
            chip_id: Some(0x1370),
            chips_per_chain_hint: Some(65),
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "s21xp" => ModelSpec {
            model_key: "s21xp",
            family_key: "bm1370",
            chip_label: "BM1370",
            chip_id: Some(0x1370),
            chips_per_chain_hint: Some(230),
            pic_type_hint: Some(ModelPicTypeHint::NoPic),
            pic_addrs_hint: Some(&[]),
            support_tier: SupportTier::Experimental,
        },
        "t23" | "s23" => ModelSpec {
            model_key: "s23",
            family_key: "bm13xx-next",
            chip_label: "BM13??",
            chip_id: None,
            chips_per_chain_hint: None,
            pic_type_hint: None,
            pic_addrs_hint: None,
            support_tier: SupportTier::Blind,
        },
        _ => return None,
    };

    Some(spec)
}

pub fn model_chip_id(model: &str) -> Option<u16> {
    lookup_model(model).and_then(|spec| spec.chip_id)
}

pub fn model_family_key(model: &str) -> Option<&'static str> {
    lookup_model(model).map(|spec| spec.family_key)
}

pub fn model_key(model: &str) -> Option<&'static str> {
    lookup_model(model).map(|spec| spec.model_key)
}

pub fn model_chip_label(model: &str) -> Option<&'static str> {
    lookup_model(model).map(|spec| spec.chip_label)
}

/// Reconcile the ASIC chip label (e.g. `"BM1387"`) from a known
/// `/etc/dcentos/board_target` marker.
///
/// P1-1 (D-12): once the control board / board family is identified — either
/// from the baked `/etc/dcentos/board_target` overlay file or post-enumeration
/// — the ASIC silicon is no longer ambiguous. An `am1-s9` board is BM1387, an
/// `am2-s19pro` is BM1398, an `am3-s21` is BM1368, etc. Callers use this to
/// replace a placeholder `"Auto-detect"` `chip_type` with the real chip on the
/// REST / CGMiner / Prometheus surfaces, so fleet tools (pyasic / asic-rs /
/// Foreman) can classify the unit instead of seeing `"Antminer (Auto-detect)"`.
///
/// The accepted markers are the exact strings the Buildroot post-build hooks
/// write to `/etc/dcentos/board_target` (plus a few pyasic-friendly spelling
/// aliases). Returns `None` for any marker that does NOT pin a single chip
/// family — a bare `am2` / `am3-aml` / `am3-bb` with no model suffix,
/// `unknown`, or empty. Truth-contract: never fabricate a chip on an
/// unidentified board; the caller keeps its honest `"Auto-detect"` fallback in
/// that case.
pub fn board_target_chip_label(board_target: &str) -> Option<&'static str> {
    // `normalize_model_token` lower-cases and strips non-alphanumeric bytes
    // (keeping '+'), so e.g. "am1-s9" → "am1s9", "am3-s19jpro-aml" →
    // "am3s19jproaml".
    let marker = normalize_model_token(board_target);
    let label = match marker.as_str() {
        // am1 — Zynq S9 (BM1387).
        "am1s9" => "BM1387",
        // am1 — Zynq S15 (BM1391, 7nm). Scaffold-tier target (see skus.conf
        // `am1-s15`, chip_id 0x1391); the chip LABEL resolves here for
        // detection/identity so an S15 board_target is not mislabeled
        // "Auto-detect", while the mining path stays scaffold-gated. Mirrors
        // lookup_model("s15") = BM1391 (the other resolution path).
        "am1s15" | "am1t15" | "s15" | "t15" => "BM1391",
        // am2 — Zynq X17/X19 control boards. The bare "am2" family is shared
        // by BM1398 (S19/S19 Pro) and BM1362 (S19j Pro), so only the
        // model-suffixed board_target pins the chip.
        "am2s19pro" | "am2s19" | "am2t19" | "x19t19" => "BM1398",
        "am2s19j" | "am2s19jpro" | "am2s19jprozynq" | "am2s19jproxil25" | "am2xil25" => "BM1362",
        "am2s19xp" => "BM1366",
        "am2s17" | "am2s17p" | "am2s17pro" | "am2t17" => "BM1397",
        "am2s17plus" | "am2t17plus" => "BM1396",
        "x17s17edspicplanned" | "x17t17epic16planned" => "BM1397",
        // am3-aml — Amlogic A113D. Disambiguated by the model suffix.
        "am3s21" | "am3t21" => "BM1368",
        "am3s21pro" | "am3s21xp" => "BM1370",
        "am3s19k" | "am3s19kpro" | "am3s19xp" => "BM1366",
        "am3s19jproaml" => "BM1362",
        // am3-bb — BeagleBone S19j Pro (BM1362).
        "am3bbs19jpro" => "BM1362",
        // cv1835 — Cvitek S19j Pro / S19 XP / T19 carriers.
        "cv1835s19jpro" => "BM1362",
        "cv1835s19xp" | "cv183xs19xp" => "BM1366",
        "cv1835t19" | "cv183xt19" => "BM1398",
        _ => return None,
    };
    Some(label)
}

// NOTE: `model_pic_type_hint` is defined below at line ~601 — it consults
// the RuntimeProfile path (covers X17S17 → DsPic etc) AND falls through to
// the lookup_model spec.pic_type_hint we added in Phase A.2. That single
// implementation handles both old runtime-profile-based models and the
// new spec-based BM136x family (S19K Pro NoPic, S21, etc.).

pub fn model_runtime_profile(model: &str) -> Option<RuntimeProfile> {
    let normalized = normalize_model_token(model);

    match normalized.as_str() {
        "s17" => Some(RuntimeProfile::X17S17),
        "t17" => Some(RuntimeProfile::X17T17),
        "s17+" | "s17plus" => Some(RuntimeProfile::X17S17Plus),
        "t17+" | "t17plus" => Some(RuntimeProfile::X17T17Plus),
        "s17e" => Some(RuntimeProfile::X17S17e),
        "t17e" => Some(RuntimeProfile::X17T17e),
        _ => None,
    }
}

pub fn model_chip_count_hint(model: &str) -> Option<u8> {
    model_runtime_profile(model)
        .and_then(RuntimeProfile::chips_per_chain)
        .or_else(|| lookup_model(model).and_then(|spec| spec.chips_per_chain_hint))
}

pub fn model_pic_type_hint(model: &str) -> Option<ModelPicTypeHint> {
    model_runtime_profile(model)
        .map(RuntimeProfile::pic_type_hint)
        .or_else(|| lookup_model(model).and_then(|spec| spec.pic_type_hint))
}

pub fn model_pic_addrs_hint(model: &str) -> Option<&'static [u8]> {
    model_runtime_profile(model)
        .and_then(RuntimeProfile::pic_addrs_hint)
        .or_else(|| lookup_model(model).and_then(|spec| spec.pic_addrs_hint))
}

#[cfg(test)]
mod tests {
    use super::{
        detect_model_from_platform, lookup_extended_spec, lookup_model, model_chip_count_hint,
        model_chip_id, model_chip_label, model_family_key, model_key, model_pic_addrs_hint,
        model_pic_type_hint, model_runtime_profile, td003_management_only_board_target,
        td003_management_only_model, Model, ModelPicTypeHint, RuntimeProfile, SupportTier,
    };

    #[test]
    fn distinguishes_s19j_from_s19j_pro() {
        assert_eq!(model_chip_id("s19j"), Some(0x1398));
        assert_eq!(model_chip_id("s19jpro"), Some(0x1362));
    }

    /// P1-1 (D-12): the S9 `.100` audit found `chip_type` surfacing as
    /// "Auto-detect" (and CGMiner `Type` as "Antminer (Auto-detect)") even
    /// though the board was already identified as `am1-s9`. Once the board is
    /// known the ASIC is pinned, so `board_target_chip_label` must reconcile
    /// the real chip from `/etc/dcentos/board_target` — and downstream the
    /// chip label must resolve to a real model name, NOT "Auto-detect".
    #[test]
    fn board_target_reconciles_chip_label_so_auto_detect_never_surfaces() {
        use super::board_target_chip_label;
        use dcentrald_asic::drivers::MinerProfile;

        // The exact bug: am1-s9 must yield BM1387 / "Antminer S9".
        assert_eq!(board_target_chip_label("am1-s9"), Some("BM1387"));
        // am1-s15: skus.conf declares it BM1391 and lookup_model("s15") agrees;
        // the board_target path was MISSING it (returned None -> "Auto-detect"),
        // unlike every other target SKU. Cross-source pin so it stays resolved.
        assert_eq!(board_target_chip_label("am1-s15"), Some("BM1391"));
        let s9 = MinerProfile::for_chip(0x1387).expect("S9 profile registered");
        assert_eq!(
            s9.name, "Antminer S9",
            "am1-s9 → BM1387 → 'Antminer S9' (never 'Auto-detect')"
        );

        // Every other identified board_target also pins its silicon — these
        // are the exact strings the Buildroot post-build hooks write to
        // /etc/dcentos/board_target.
        assert_eq!(board_target_chip_label("am2-s19pro"), Some("BM1398"));
        assert_eq!(board_target_chip_label("am2-s19j"), Some("BM1362"));
        assert_eq!(board_target_chip_label("am2-s19jpro-zynq"), Some("BM1362"));
        assert_eq!(
            board_target_chip_label("am2-s19jpro-xil"),
            Some("BM1362")
        );
        assert_eq!(board_target_chip_label("am1-t15"), Some("BM1391"));
        assert_eq!(board_target_chip_label("am2-s17p"), Some("BM1397"));
        assert_eq!(board_target_chip_label("am2-s17plus"), Some("BM1396"));
        assert_eq!(board_target_chip_label("am2-t17"), Some("BM1397"));
        assert_eq!(board_target_chip_label("am2-t17plus"), Some("BM1396"));
        assert_eq!(
            board_target_chip_label("x17-s17e-dspic-planned"),
            Some("BM1397")
        );
        assert_eq!(
            board_target_chip_label("x17-t17e-pic16-planned"),
            Some("BM1397")
        );
        assert_eq!(board_target_chip_label("am3-s21"), Some("BM1368"));
        assert_eq!(board_target_chip_label("am3-t21"), Some("BM1368"));
        assert_eq!(board_target_chip_label("am3-s21pro"), Some("BM1370"));
        assert_eq!(board_target_chip_label("am3-s21xp"), Some("BM1370"));
        assert_eq!(board_target_chip_label("am3-s19k"), Some("BM1366"));
        assert_eq!(board_target_chip_label("am3-s19xp"), Some("BM1366"));
        assert_eq!(board_target_chip_label("am3-s19jpro-aml"), Some("BM1362"));
        assert_eq!(board_target_chip_label("am3-bb-s19jpro"), Some("BM1362"));
        assert_eq!(board_target_chip_label("cv1835-s19jpro"), Some("BM1362"));
        assert_eq!(board_target_chip_label("cv1835-s19xp"), Some("BM1366"));
        assert_eq!(board_target_chip_label("cv1835-t19"), Some("BM1398"));

        // Each reconciled chip label resolves to a real Antminer model name.
        for (label, chip_id, model) in [
            ("BM1398", 0x1398u16, "Antminer S19 Pro"),
            ("BM1362", 0x1362, "Antminer S19j Pro"),
            ("BM1368", 0x1368, "Antminer S21"),
        ] {
            let p = MinerProfile::for_chip(chip_id)
                .unwrap_or_else(|| panic!("{label} profile registered"));
            assert_eq!(p.name, model);
        }

        // Ambiguous / unidentified markers must NOT fabricate a chip — the
        // caller keeps its honest "Auto-detect" fallback (truth-contract).
        assert_eq!(board_target_chip_label("am3-aml"), None);
        assert_eq!(board_target_chip_label("am2"), None);
        assert_eq!(board_target_chip_label("am3-bb"), None);
        assert_eq!(board_target_chip_label("unknown"), None);
        assert_eq!(board_target_chip_label(""), None);
    }

    #[test]
    fn normalizes_requested_safe_aliases() {
        assert_eq!(model_chip_id("T9+"), Some(0x1387));
        assert_eq!(model_chip_id("s21_plus"), Some(0x1370));
        assert_eq!(model_family_key("S19_Pro"), Some("bm1398"));
        assert_eq!(model_key("s19k_pro"), Some("s19k"));
        assert_eq!(model_key("T9+"), Some("t9+"));
        assert_eq!(model_key("S9_plus"), Some("s9+"));
        assert_eq!(model_chip_count_hint("s9"), Some(63));
        assert_eq!(model_chip_count_hint("s9+"), Some(84));
        assert_eq!(model_chip_count_hint("t9"), Some(57));
        assert_eq!(model_chip_count_hint("t9+"), Some(18));
        assert_eq!(model_chip_count_hint("t17+"), Some(44));
        assert_eq!(model_chip_count_hint("s17+"), Some(65));
        assert_eq!(model_chip_count_hint("s21xp"), Some(230));
        assert_eq!(model_pic_type_hint("t17"), Some(ModelPicTypeHint::Pic16));
        assert_eq!(model_pic_type_hint("s17"), Some(ModelPicTypeHint::DsPic));
        assert_eq!(
            model_pic_addrs_hint("t17+").expect("x17 pic addrs")[0],
            0x50
        );
        assert_eq!(
            model_runtime_profile("S17+"),
            Some(RuntimeProfile::X17S17Plus)
        );
        assert_eq!(
            model_runtime_profile("t17e").expect("t17e profile").key(),
            "x17-t17e-pic16-planned"
        );
    }

    #[test]
    fn model_spec_chip_ids_match_skus_conf_for_all_20_target_skus() {
        // Cross-source pin: the daemon's ModelSpec chip_id (which drives
        // registry.detect() -> driver dispatch) MUST match skus.conf (the
        // acceptance-harness 20-SKU source of truth) for every confirmed target, or a
        // board would dispatch to the wrong driver at first-light. S15 is the ONE
        // intentional exception: chip_id stays None (SupportTier::Planned
        // scaffold-gate — dispatch gated until confirmed) while its label is BM1391.
        let id = |key: &str| lookup_model(key).expect("model spec").chip_id;
        assert_eq!(id("s9"), Some(0x1387)); // skus.conf am1-s9
        assert_eq!(id("t15"), None); // skus.conf am1-t15 scaffold
        assert_eq!(id("s17"), Some(0x1397)); // am2-s17p
        assert_eq!(id("s17pro"), Some(0x1397)); // am2-s17p
        assert_eq!(id("s17+"), Some(0x1396)); // am2-s17plus fail-closed BM1396
        assert_eq!(id("t17"), Some(0x1397)); // am2-t17
        assert_eq!(id("t17+"), Some(0x1396)); // am2-t17plus fail-closed BM1396
        assert_eq!(id("s17e"), None); // planned BM1397 scaffold
        assert_eq!(id("t17e"), None); // planned BM1397 scaffold
        assert_eq!(id("s19"), Some(0x1398)); // am2-s19
        assert_eq!(id("s19pro"), Some(0x1398)); // am2-s19pro
        assert_eq!(id("s19jpro"), Some(0x1362)); // am2-s19jpro-zynq
        assert_eq!(id("s19kpro"), Some(0x1366)); // am3-s19k
        assert_eq!(id("t19"), Some(0x1398)); // am2-t19
        assert_eq!(id("s19xp"), Some(0x1366)); // am3-s19xp
        assert_eq!(id("s21"), Some(0x1368)); // am3-s21
        assert_eq!(id("t21"), Some(0x1368)); // am3-t21
        assert_eq!(id("s21pro"), Some(0x1370)); // am3-s21pro
        assert_eq!(id("s21xp"), Some(0x1370)); // am3-s21xp dedicated BM1370 geometry
                                               // S15: intentional scaffold-gate - chip_id None (dispatch gated), label known.
        assert_eq!(id("s15"), None);
        assert_eq!(lookup_model("s15").expect("s15 spec").chip_label, "BM1391");
        assert_eq!(lookup_model("t15").expect("t15 spec").chip_label, "BM1391");
        assert_eq!(
            lookup_model("s17e").expect("s17e spec").chip_label,
            "BM1397"
        );
        assert_eq!(
            lookup_model("t17e").expect("t17e spec").chip_label,
            "BM1397"
        );
    }

    #[test]
    fn bm1396_s17plus_t17plus_are_identified_but_not_driver_promoted() {
        use dcentrald_asic::drivers::ChipRegistry;

        for model in ["s17+", "s17plus", "t17+", "t17plus"] {
            let spec = lookup_model(model).expect("BM1396 plus-family model");
            assert_eq!(spec.chip_label, "BM1396");
            assert_eq!(spec.chip_id, Some(0x1396));
            assert_eq!(spec.support_tier, SupportTier::Experimental);
        }

        assert_eq!(model_chip_count_hint("s17+"), Some(65));
        assert_eq!(model_chip_count_hint("t17+"), Some(44));
        assert_eq!(model_pic_type_hint("s17+"), Some(ModelPicTypeHint::Pic16));
        assert_eq!(model_pic_type_hint("t17+"), Some(ModelPicTypeHint::Pic16));
        assert!(
            ChipRegistry::production().detect(0x1396).is_none(),
            "BM1396 is identified for S17+/T17+, but remains fail-closed until a live enumerate bench gate"
        );
    }

    #[test]
    fn live_skus_conf_agrees_with_board_target_chip_label_for_every_target() {
        // Authoritative cross-source gate: read the LIVE skus.conf (the
        // acceptance-harness 20-SKU source of truth) and verify the daemon's
        // board_target_chip_label resolves each board_target to the SAME chip.
        // Unlike the hardcoded pins above, this catches a divergence introduced on
        // EITHER side (a skus.conf edit OR a model.rs edit) — the exact class of the
        // am1-s15 gap. skus.conf columns: SKU|board_target|arch|chip|chip_id|...
        use super::board_target_chip_label;
        let skus = include_str!("../../../scripts/hw-acceptance/skus.conf");
        let mut checked = 0;
        for line in skus.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('|').collect();
            assert!(cols.len() >= 5, "malformed skus.conf line: {line}");
            let board_target = cols[1].trim();
            let chip = cols[3].trim();
            assert_eq!(
                board_target_chip_label(board_target),
                Some(chip),
                "skus.conf declares {board_target} = {chip}, but board_target_chip_label disagrees"
            );
            checked += 1;
        }
        assert_eq!(
            checked, 20,
            "skus.conf must list exactly the 20 target SKUs; cross-checked {checked}"
        );
    }

    #[test]
    fn keeps_planned_families_non_runnable() {
        let s15 = lookup_model("s15").expect("s15 spec");
        assert_eq!(s15.chip_label, "BM1391");
        assert_eq!(s15.chip_id, None);
        assert_eq!(s15.chips_per_chain_hint, None);
        assert_eq!(s15.pic_type_hint, None);
        assert_eq!(s15.support_tier, SupportTier::Planned);

        let t17e = lookup_model("t17e").expect("t17e spec");
        assert_eq!(t17e.model_key, "t17e");
        assert_eq!(t17e.chip_label, "BM1397");
        assert_eq!(t17e.chip_id, None);
        assert_eq!(t17e.chips_per_chain_hint, None);
        assert_eq!(t17e.pic_type_hint, Some(ModelPicTypeHint::Pic16));
        assert_eq!(t17e.support_tier, SupportTier::Planned);
    }

    #[test]
    fn td003_management_only_gate_is_exact_to_expanded_matrix() {
        for model in [
            "s15",
            "Antminer S15",
            "t15",
            "Antminer T15",
            "s17",
            "S17 Pro",
            "Antminer S17",
            "s17+",
            "Antminer S17+",
            "t17",
            "Antminer T17",
            "t17+",
            "Antminer T17+",
            "s17e",
            "Antminer S17e",
            "t17e",
            "Antminer T17e",
            "t19",
            "Antminer T19",
            "s19xp",
            "Antminer S19 XP",
        ] {
            assert!(
                td003_management_only_model(model).is_some(),
                "{model} must stay management-only until platform promotion"
            );
        }

        for model in ["s9", "s19jproam2", "s19jpro", "s19pro", "s21", "s19k"] {
            assert_eq!(
                td003_management_only_model(model),
                None,
                "{model} must not be caught by the TD-003 exact gate"
            );
        }
    }

    #[test]
    fn td003_board_target_gate_covers_in_development_markers() {
        for (marker, label) in [
            ("am1-s15", "Antminer S15"),
            ("am1-t15", "Antminer T15"),
            ("am2-s17p", "Antminer S17 / S17 Pro"),
            ("am2-s17pro", "Antminer S17 / S17 Pro"),
            ("am2-s17plus", "Antminer S17+"),
            ("am2-s17", "Antminer S17 / S17 Pro"),
            ("am1-s17", "Antminer S17 / S17 Pro"),
            ("x17-s17e-dspic-planned", "Antminer S17e"),
            ("x17-t17", "Antminer T17"),
            ("am2-t17", "Antminer T17"),
            ("am2-t17plus", "Antminer T17+"),
            ("x17-t17e-pic16-planned", "Antminer T17e"),
            ("am2-t19", "Antminer T19"),
            ("cv183x-t19", "Antminer T19"),
            ("cv1835-t19", "Antminer T19"),
            ("am3-t19", "Antminer T19"),
            ("am2-s19xp", "Antminer S19 XP"),
            ("am3-s19xp", "Antminer S19 XP"),
            ("cv1835-s19xp", "Antminer S19 XP"),
        ] {
            assert_eq!(
                td003_management_only_board_target(marker),
                Some(label),
                "{marker} must stay management-only until platform promotion"
            );
        }
    }

    #[test]
    fn td003_board_target_gate_does_not_block_promoted_or_other_known_markers() {
        for marker in [
            "",
            "am1-s9",
            "am2-s19j",
            "am2-s19jpro",
            "am2-s19pro",
            "am3-bb-s19jpro",
            "am3-s19k",
            "am3-s21",
            "am3-s21pro",
            "am3-s21xp",
            "am3-t21",
            "cv1835-s19jpro",
            "bcb100-s19jpro-lab",
        ] {
            assert_eq!(
                td003_management_only_board_target(marker),
                None,
                "{marker:?} must not be caught by the TD-003 board-target gate"
            );
        }
    }

    #[test]
    fn exposes_future_family_as_blind() {
        let s23 = lookup_model("s23").expect("s23 spec");
        assert_eq!(s23.chip_label, "BM13??");
        assert_eq!(s23.chip_id, None);
        assert_eq!(s23.support_tier, SupportTier::Blind);
        assert_eq!(model_chip_label("t23"), Some("BM13??"));
    }

    #[test]
    fn s19jpro_am2_variant_registers_string_key_and_extended_spec() {
        // Enum variant → ModelSpec round-trip.
        let m = Model::S19jProAm2;
        assert_eq!(m.model_key(), "s19jproam2");
        assert_eq!(m.platform_key(), "zynq-bm3-am2");
        assert_eq!(m.board_family(), "am2");
        assert_eq!(m.board_target(), "am2-s19j");

        let spec = lookup_model("s19jproam2").expect("s19jproam2 ModelSpec registered");
        assert_eq!(spec.chip_id, Some(0x1362));
        assert_eq!(spec.chips_per_chain_hint, Some(126));
        assert_eq!(spec.pic_type_hint, Some(ModelPicTypeHint::DsPic));
        assert_eq!(
            spec.pic_addrs_hint.expect("am2 pic addrs"),
            &[0x20, 0x21, 0x22]
        );

        // Extended spec content sanity.
        let ext = m.extended_spec();
        assert_eq!(ext.chain_count, 3);
        assert_eq!(ext.chips_per_chain, 126);
        assert_eq!(ext.cores_per_chip, 4);
        assert_eq!(ext.rated_hashrate_th, 104);
        assert_eq!(ext.rated_power_w, 3068);
        assert_eq!(ext.default_voltage_mv, 13_700);
        assert_eq!(ext.voltage_min_mv, 11_960);
        assert_eq!(ext.voltage_max_mv, 15_200);
        assert_eq!(ext.mining_baud, 3_125_000);
        assert_eq!(ext.init_baud, 115_200);
        assert_eq!(ext.thermal.target_c, 60);
        assert_eq!(ext.thermal.dangerous_internal_c, 110);
        assert_eq!(ext.fan_count, 4);
        assert_eq!(ext.fan_pwm_cap_industrial, 80);
        assert_eq!(ext.pic_fw_byte_expected, 0x89);
        assert_eq!(ext.board_target, "am2-s19j");

        // String-keyed extended lookup returns the same constant.
        assert_eq!(lookup_extended_spec("s19jproam2"), Some(ext));
        assert_eq!(lookup_extended_spec("s19jproam2"), Some(m.extended_spec()));
    }

    #[test]
    fn detect_model_disambiguates_am2_by_chip_id() {
        // zynq-bm3-am2 is shared by S19 Pro (BM1398) and S19j Pro (BM1362).
        // Only the BM1362 pair maps to a concrete enum variant today.
        assert_eq!(
            detect_model_from_platform("zynq-bm3-am2", Some(0x1362)),
            Some(Model::S19jProAm2)
        );
        assert_eq!(
            detect_model_from_platform("zynq-bm3-am2", Some(0x1398)),
            None
        );
        assert_eq!(detect_model_from_platform("zynq-bm3-am2", None), None);
        assert_eq!(
            detect_model_from_platform("zynq-bm1-s9", Some(0x1387)),
            None
        );
        // Whitespace tolerated (bos_platform often includes a trailing newline).
        assert_eq!(
            detect_model_from_platform("  zynq-bm3-am2\n", Some(0x1362)),
            Some(Model::S19jProAm2)
        );
    }
}
