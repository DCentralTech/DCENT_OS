//! Source-parse breadth pin for hashboard EEPROM write-denylist construction.
//!
//! The runtime guarantee is per I2C service handle: non-S9 paths that can share
//! a bus with AT24C-class hashboard EEPROMs must construct the service with the
//! 0x50..=0x57 write-denylist. This test covers every daemon/mining entry point
//! that creates such a long-running service.

const DAEMON_RS: &str = include_str!("../src/daemon.rs");
const HYBRID_RS: &str = include_str!("../src/s19j_hybrid_mining.rs");
const SERIAL_RS: &str = include_str!("../src/serial_mining.rs");
const AM3_BB_RS: &str = include_str!("../src/am3_bb_mining.rs");
const AMLOGIC_RS: &str = include_str!("../../dcentrald-hal/src/platform/amlogic/mod.rs");

struct ConstructionSite {
    label: &'static str,
    source: &'static str,
    entry_marker: &'static str,
    constructor: &'static str,
    denylist_marker: &'static str,
}

fn assert_denylisted_construction(site: &ConstructionSite) -> Result<(), String> {
    let entry = site.source.find(site.entry_marker).ok_or_else(|| {
        format!(
            "{}: missing entry marker `{}`",
            site.label, site.entry_marker
        )
    })?;
    let body = &site.source[entry..];

    let constructor_pos = body
        .find(site.constructor)
        .ok_or_else(|| format!("{}: missing denylist constructor", site.label))?;
    let denylist_pos = body.find(site.denylist_marker).ok_or_else(|| {
        format!(
            "{}: missing denylist marker `{}`",
            site.label, site.denylist_marker
        )
    })?;

    let distance = constructor_pos.abs_diff(denylist_pos);
    if distance > 512 {
        return Err(format!(
            "{}: constructor and denylist marker are too far apart ({} bytes)",
            site.label, distance
        ));
    }

    Ok(())
}

#[test]
fn non_s9_i2c_construction_sites_register_eeprom_write_denylist() {
    let sites = [
        ConstructionSite {
            label: "daemon_standard_non_s9",
            source: DAEMON_RS,
            entry_marker: "async fn init(",
            constructor: "spawn_i2c_service_no_register_touch_with_denylist",
            denylist_marker: "HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec()",
        },
        ConstructionSite {
            label: "am2_hybrid_phase0",
            source: HYBRID_RS,
            entry_marker: "Phase 0: PSU bring-up",
            constructor: "spawn_i2c_service_no_register_touch_with_denylist",
            denylist_marker: "let am2_eeprom_denylist: Vec<u8> = (0x50u8..=0x57u8).collect()",
        },
        ConstructionSite {
            label: "am2_serial_pic_service",
            source: SERIAL_RS,
            entry_marker: "let bm1362_i2c_service = if",
            constructor: "spawn_i2c_service_no_register_touch_with_denylist",
            denylist_marker: "HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec()",
        },
        ConstructionSite {
            label: "am3_bb_dspic_service",
            source: AM3_BB_RS,
            entry_marker: "let dspic_i2c =",
            constructor: "spawn_i2c_service_no_register_touch_with_denylist",
            denylist_marker: "AM3_BB_HASHBOARD_EEPROM_DENYLIST.to_vec()",
        },
        ConstructionSite {
            label: "am3_aml_protected_i2c0_service",
            source: AMLOGIC_RS,
            entry_marker: "pub fn spawn_amlogic_protected_i2c0_service",
            constructor: "spawn_i2c_service_no_register_touch_with_denylist",
            denylist_marker: "AMLOGIC_EEPROM_DENYLIST.to_vec()",
        },
    ];

    for site in &sites {
        assert_denylisted_construction(site)
            .unwrap_or_else(|err| panic!("EEPROM denylist construction drift: {err}"));
    }
}

#[test]
fn denylist_breadth_helper_rejects_plain_i2c_service_constructor() {
    let site = ConstructionSite {
        label: "negative_control",
        source: "fn run() { let _ = spawn_i2c_service_no_register_touch(0); }",
        entry_marker: "fn run()",
        constructor: "spawn_i2c_service_no_register_touch_with_denylist",
        denylist_marker: "HASHBOARD_EEPROM_WRITE_DENYLIST.to_vec()",
    };

    let err = assert_denylisted_construction(&site)
        .expect_err("negative control must reject a plain no-denylist constructor");
    assert!(
        err.contains("missing denylist constructor"),
        "unexpected negative-control error: {err}"
    );
}
