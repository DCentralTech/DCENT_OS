#![cfg(feature = "sim-hal")]

use std::path::Path;

use dcentrald_asic::drivers::ChipRegistry;
use dcentrald_hal::chain_backend::Bm1397PlusChainBackend;
use dcentrald_hal::fpga_chain::FpgaChain;
use dcentrald_hal::i2c::I2cPicFirmware;
use dcentrald_hal::platform::sim::{SimModel, SimPlatform, TraceEvent};
use serde_json::Value;

const S19PRO_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s19pro/init_sequence.jsonl");
const S9_VECTOR: &str = include_str!("../../dcentrald-re-catalog/vectors/s9/init_sequence.jsonl");
const S17_VECTOR: &str = include_str!("../../dcentrald-re-catalog/vectors/s17/init_sequence.jsonl");
const S17PRO_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s17pro/init_sequence.jsonl");
const T17_VECTOR: &str = include_str!("../../dcentrald-re-catalog/vectors/t17/init_sequence.jsonl");
const S17_ENUMERATION_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s17/enumeration.jsonl");
const S19PRO_PSU_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s19pro/psu_handshake.jsonl");
const S19JPRO_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s19jpro/init_sequence.jsonl");
const S19XP_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s19xp/init_sequence.jsonl");
const S19KPRO_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s19kpro/init_sequence.jsonl");
const S21_VECTOR: &str = include_str!("../../dcentrald-re-catalog/vectors/s21/init_sequence.jsonl");
const S21PRO_VECTOR: &str =
    include_str!("../../dcentrald-re-catalog/vectors/s21pro/init_sequence.jsonl");

fn parse_vector(contents: &str) -> (Value, Vec<TraceEvent>) {
    let mut lines = contents.lines();
    let header = serde_json::from_str(lines.next().expect("provenance header"))
        .expect("valid provenance JSON");
    let events = lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid trace event JSON"))
        .collect();
    (header, events)
}

fn assert_init_vector_with_strictness(
    vector: &str,
    slug: &str,
    model: SimModel,
    expected_open_core_writes: Option<u32>,
    expected_strictness: &str,
) {
    let (header, expected) = parse_vector(vector);
    assert_eq!(header["schema"], "dcent-init-trace-v1");
    assert_eq!(header["model"], slug);
    assert_eq!(header["strictness"], expected_strictness);

    let repository_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let provenance = header["provenance"].as_array().expect("provenance array");
    assert!(provenance.len() >= 2);
    for source in provenance {
        let source = source.as_str().expect("provenance path");
        assert!(
            repository_root.join(source).exists(),
            "golden source does not exist: {source}"
        );
    }

    let evidence = dcentrald_re_catalog::model_evidence(slug).expect("model catalog row");
    let registry = ChipRegistry::production();
    let driver = registry
        .detect(evidence.chip_id)
        .expect("production driver");
    let mut chain = FpgaChain::open_sim_for_model(0, model).expect("sim chain");
    let chip_count =
        u8::try_from(evidence.chips_per_chain.expect("known chip count")).expect("u8 chip count");
    driver
        .init_chain(
            &mut chain,
            chip_count,
            evidence.default_frequency_mhz.expect("known frequency"),
        )
        .expect("production init");
    if let Some(expected) = expected_open_core_writes {
        assert_eq!(
            driver
                .send_open_core_work(&mut chain, chip_count)
                .expect("open-core path"),
            expected
        );
    }

    let actual = chain.drain_sim_trace().expect("sim trace");
    assert_eq!(actual, expected, "ordered init byte trace drifted");
}

fn assert_exact_init_vector(
    vector: &str,
    slug: &str,
    model: SimModel,
    expected_open_core_writes: Option<u32>,
) {
    assert_init_vector_with_strictness(vector, slug, model, expected_open_core_writes, "exact");
}

#[test]
fn s19pro_init_snapshot_tracks_the_simulator_without_claiming_vendor_exactness() {
    let (header, _) = parse_vector(S19PRO_VECTOR);
    assert_eq!(header["maturity"], "experimental");
    assert!(header["evidence_boundary"]
        .as_str()
        .is_some_and(|boundary| boundary.contains("not a vendor-exact")));
    assert_init_vector_with_strictness(
        S19PRO_VECTOR,
        "s19pro",
        SimModel::S19Pro,
        Some(0),
        "implementation_snapshot",
    );
}

#[test]
fn s9_init_is_an_exact_ordered_byte_match() {
    assert_exact_init_vector(S9_VECTOR, "s9", SimModel::S9, None);
}

#[test]
fn s17_init_is_an_exact_ordered_byte_match() {
    assert_exact_init_vector(S17_VECTOR, "s17", SimModel::S17, Some(0));
}

#[test]
fn s17pro_init_is_an_exact_ordered_byte_match() {
    assert_exact_init_vector(S17PRO_VECTOR, "s17pro", SimModel::S17Pro, Some(0));
}

#[test]
fn t17_init_is_an_exact_ordered_byte_match() {
    assert_exact_init_vector(T17_VECTOR, "t17", SimModel::T17, Some(0));
}

#[test]
fn s9_open_core_uses_all_114_activation_slots_and_enters_mining_mode() {
    let evidence = dcentrald_re_catalog::model_evidence("s9").expect("S9 catalog row");
    let registry = ChipRegistry::production();
    let driver = registry.detect(0x1387).expect("production BM1387 driver");
    let mut chain = FpgaChain::open_sim_for_model(0, SimModel::S9).expect("S9 sim chain");
    driver
        .init_chain(
            &mut chain,
            evidence.chips_per_chain.expect("S9 count") as u8,
            evidence.default_frequency_mhz.expect("S9 frequency"),
        )
        .expect("S9 init");
    let _ = chain.drain_sim_trace().expect("discard init trace");
    chain
        .set_sim_nonce_policy(dcentrald_hal::platform::sim::SimNoncePolicy::Valid)
        .expect("valid activation nonce policy");

    let activated = driver
        .send_open_core_work(&mut chain, 63)
        .expect("production S9 open-core");
    assert_eq!(activated, 114);
    let trace = chain.drain_sim_trace().expect("open-core trace");
    let work: Vec<&Vec<u8>> = trace
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Work { bytes, .. } => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(work.len(), 114);
    assert!(work.iter().all(|bytes| bytes.len() == 36 * 4));
    assert!(work.iter().all(|bytes| &bytes[4..8] == [0xff; 4]));
    assert!(trace.iter().any(|event| matches!(
        event,
        TraceEvent::Command { bytes, .. }
            if bytes == &[0x58, 0x09, 0x00, 0x1c]
    )));
    assert!(trace.iter().any(|event| matches!(
        event,
        TraceEvent::Command { bytes, .. }
            if bytes == &[0x00, 0x20, 0x01, 0x80]
    )));
}

#[test]
fn s17_enumeration_is_an_exact_saleae_frame_match() {
    let mut lines = S17_ENUMERATION_VECTOR.lines();
    let header: Value =
        serde_json::from_str(lines.next().expect("capture header")).expect("valid capture header");
    let command: Value =
        serde_json::from_str(lines.next().expect("capture command")).expect("valid command");
    let response: Value =
        serde_json::from_str(lines.next().expect("capture response")).expect("valid response");
    assert_eq!(header["strictness"], "exact");
    assert_eq!(
        command["bytes"],
        serde_json::json!([85, 170, 82, 5, 0, 0, 10])
    );

    let captured: Vec<u8> = response["bytes"]
        .as_array()
        .expect("captured response bytes")
        .iter()
        .map(|byte| byte.as_u64().expect("byte") as u8)
        .collect();
    assert_eq!(&captured[..2], &[0xaa, 0x55]);
    let expected_body = &captured[2..];
    let expected_count = response["occurrences_per_command"]
        .as_u64()
        .expect("capture count") as usize;

    let platform = SimPlatform::new(SimModel::S17);
    let backend = platform
        .open_bm1397plus_backend(0)
        .expect("S17 simulated backend");
    backend
        .send_get_address_bm1397plus()
        .expect("enumeration request");
    let actual = backend.read_all_responses(0).expect("enumeration replies");
    assert_eq!(actual.len(), expected_count);
    assert!(actual.iter().all(|body| body == expected_body));
}

#[test]
fn s19pro_dspic_service_handshake_matches_golden_bytes() {
    let (_header, expected) = parse_vector(S19PRO_PSU_VECTOR);
    let platform = SimPlatform::new(SimModel::S19Pro);
    let service = platform.open_i2c_service(0).expect("simulated I2C service");
    let version = service
        .write_read(0x20, &[0x55, 0xaa, 0x17], 1)
        .expect("dsPIC firmware version");
    assert_eq!(version, [0x89]);
    service
        .heartbeat(0x20, I2cPicFirmware::Stock)
        .expect("dsPIC heartbeat");
    service
        .set_voltage_mv(0x20, 13_800)
        .expect("dsPIC set voltage");
    service
        .write_byte_by_byte(0x20, &[0x55, 0xaa, 0x15, 0x01])
        .expect("dsPIC enable voltage");

    let actual = platform.drain_i2c_trace().expect("I2C trace");
    assert_eq!(actual, expected);
}

fn assert_structural_init_vector(
    vector: &str,
    slug: &str,
    model: SimModel,
    chip_count: u8,
    frequency_mhz: u16,
) {
    let (header, expected) = parse_vector(vector);
    assert_eq!(header["model"], slug);
    assert_eq!(header["strictness"], "structural");
    let evidence = dcentrald_re_catalog::model_evidence(slug).expect("model catalog row");
    let registry = ChipRegistry::production();
    let driver = registry
        .detect(evidence.chip_id)
        .expect("production driver");
    let mut chain = FpgaChain::open_sim_for_model(0, model).expect("sim chain");
    driver
        .init_chain(&mut chain, chip_count, frequency_mhz)
        .expect("production init");
    let actual = chain.drain_sim_trace().expect("sim trace");

    let mut cursor = 0;
    for event in expected {
        let Some(relative) = actual[cursor..].iter().position(|actual| actual == &event) else {
            panic!("structural event missing after index {cursor}: {event:?}");
        };
        cursor += relative + 1;
    }
}

#[test]
fn s19jpro_init_contains_provenance_backed_structural_sequence() {
    assert_structural_init_vector(S19JPRO_VECTOR, "s19jpro", SimModel::S19jPro, 126, 545);
}

#[test]
fn s19xp_init_contains_provenance_backed_structural_sequence() {
    assert_structural_init_vector(S19XP_VECTOR, "s19xp", SimModel::S19Xp, 110, 675);
}

#[test]
fn s19kpro_init_contains_provenance_backed_structural_sequence() {
    assert_structural_init_vector(S19KPRO_VECTOR, "s19kpro", SimModel::S19kPro, 77, 670);
}

#[test]
fn s21_init_contains_provenance_backed_structural_sequence() {
    assert_structural_init_vector(S21_VECTOR, "s21", SimModel::S21, 108, 525);
}

#[test]
fn s21pro_init_contains_provenance_backed_structural_sequence() {
    assert_structural_init_vector(S21PRO_VECTOR, "s21pro", SimModel::S21Pro, 65, 525);
}
