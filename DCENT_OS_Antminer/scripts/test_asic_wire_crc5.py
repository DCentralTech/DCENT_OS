#!/usr/bin/env python3
"""Offline tests for the capture-backed ASIC command CRC5 contract."""

import ast
import importlib.util
import json
import pathlib
import sys

import pytest


ROOT = pathlib.Path(__file__).resolve().parents[1]
CANONICAL = ROOT / "tools/asic-wire/python/dcentos_asic_wire.py"
CONTRACT = ROOT / "contracts/asic-wire/v1/bm13xx-command-crc5.json"
RESPONSE_CONTRACT = ROOT / "contracts/asic-wire/v1/bm13xx-response-crc5.json"
STAGED = (
    ROOT / "overlay/root/tools/dcentos_asic_wire.py",
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/dcentos_asic_wire.py",
)
CONSUMERS = (
    ROOT / "overlay/root/tools/asic_enumerator.py",
    ROOT / "overlay/root/tools/register_scanner.py",
    ROOT / "overlay/root/tools/temp_finder.py",
    ROOT / "overlay/root/tools/board_health.py",
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/asic_enumerator.py",
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/register_scanner.py",
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/temp_finder.py",
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py",
)
RESPONSE_PARSERS = tuple(path for path in CONSUMERS if path.name != "board_health.py")
PAIRED_CONSUMERS = (
    (
        ROOT / "overlay/root/tools/asic_enumerator.py",
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/asic_enumerator.py",
    ),
    (
        ROOT / "overlay/root/tools/register_scanner.py",
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/register_scanner.py",
    ),
    (
        ROOT / "overlay/root/tools/temp_finder.py",
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/temp_finder.py",
    ),
)
TEMP_FINDERS = PAIRED_CONSUMERS[2]
ENUMERATORS = PAIRED_CONSUMERS[0]
REGISTER_SCANNERS = PAIRED_CONSUMERS[1]
HARDWARE_UART_CONSUMERS = RESPONSE_PARSERS


def load_module(path, name):
    sys.path.insert(0, str(path.parent))
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.path.pop(0)


WIRE = load_module(CANONICAL, "dcentos_asic_wire_canonical_test")
VECTORS = json.loads(CONTRACT.read_text(encoding="utf-8"))
RESPONSE_VECTORS = json.loads(RESPONSE_CONTRACT.read_text(encoding="utf-8"))


@pytest.mark.parametrize(
    "vector", VECTORS["positive_vectors"], ids=lambda item: item["id"]
)
def test_capture_backed_positive_vectors(vector):
    body = bytes.fromhex(vector["body_hex"])
    trailer = int(vector["trailer_hex"], 16)
    frame = bytes.fromhex(vector["full_frame_hex"])

    assert frame[:2] == bytes.fromhex("55aa")
    assert frame[2:-1] == body
    assert frame[-1] == trailer
    assert WIRE.crc5_bm13xx_command(body) == trailer
    assert vector["direction"] == "host-to-asic"
    assert any(item["kind"] == "independent-sniff" for item in vector["evidence"])


@pytest.mark.parametrize(
    "vector", VECTORS["negative_vectors"], ids=lambda item: item["id"]
)
def test_response_trailers_are_not_promoted_to_command_crc(vector):
    body = bytes.fromhex(vector["body_hex"])
    observed = int(vector["observed_trailer_hex"], 16)
    frame = bytes.fromhex(vector["full_frame_hex"])

    assert vector["direction"] == "asic-to-host"
    assert frame[:2] == bytes.fromhex("aa55")
    assert frame[2:-1] == body
    assert frame[-1] == observed
    assert WIRE.crc5_bm13xx_command(body) != observed


@pytest.mark.parametrize(
    "vector", RESPONSE_VECTORS["positive_vectors"], ids=lambda item: item["id"]
)
def test_capture_backed_response_crc_vectors(vector):
    payload = bytes.fromhex(vector["payload_hex"])
    trailer = int(vector["trailer_hex"], 16)
    frame = bytes.fromhex(vector["full_frame_hex"])
    is_job_response = vector["response_kind"] == "job"

    assert frame[:2] == bytes.fromhex("aa55")
    assert frame[2:-1] == payload
    assert frame[-1] == trailer
    assert bool(trailer & 0x80) is is_job_response
    assert WIRE.crc5_bm13xx_response(payload, is_job_response=is_job_response) == (
        trailer & 0x1F
    )


def test_response_contract_has_independent_algorithm_and_bm1362_payload_variation():
    algorithm = RESPONSE_VECTORS["algorithm"]
    assessment = RESPONSE_VECTORS["corpus_assessment"]
    bm1362_payloads = {
        vector["payload_hex"]
        for vector in RESPONSE_VECTORS["positive_vectors"]
        if vector["chip_id_hex"] == "1362"
    }

    assert algorithm["id"] == "bm13xx-asic-response-crc5"
    assert algorithm["polynomial_hex"] == "0d"
    assert "generic polynomial helper" in algorithm["transition"]
    assert algorithm["initial_value_by_response_kind"] == {
        "command_or_register": "03",
        "job": "1b",
    }
    assert len(bm1362_payloads) == assessment["bm1362_distinct_payload_vectors"]
    assert assessment["am3_bb_distinct_payload_vectors"] == 1
    assert "Measured hardware identity" in assessment["does_not_support"]
    assert (
        "unique-chip identity or chip-count proof from repeated unassigned AM3-BB replies"
        in assessment["does_not_support"]
    )

    for reference_key in ("reference", "integration_reference"):
        assert (ROOT.parent.parent / algorithm[reference_key]["path"]).is_file()
    for vector in RESPONSE_VECTORS["positive_vectors"]:
        for evidence in vector["evidence"]:
            assert (ROOT.parent.parent / evidence["path"]).is_file()


def test_response_crc_api_requires_explicit_direction_kind():
    payload = bytes.fromhex("136203000000")
    assert WIRE.crc5_bm13xx_response(payload, is_job_response=False) == 0x0D
    assert WIRE.crc5_bm13xx_response(payload, is_job_response=True) != 0x0D

    with pytest.raises(TypeError):
        WIRE.crc5_bm13xx_response(payload, is_job_response=0)
    with pytest.raises(TypeError):
        WIRE.crc5_bm13xx_response("136203000000", is_job_response=False)


def test_bit_length_is_explicit_and_strictly_bounded():
    payload = bytes.fromhex("52050000")
    assert WIRE.crc5_bm13xx_command(payload, bit_length=32) == 0x0A
    assert WIRE.crc5_bm13xx_command(payload, bit_length=0) == 0x1F

    with pytest.raises(ValueError):
        WIRE.crc5_bm13xx_command(payload, bit_length=-1)
    with pytest.raises(ValueError):
        WIRE.crc5_bm13xx_command(payload, bit_length=33)
    with pytest.raises(TypeError):
        WIRE.crc5_bm13xx_command(payload, bit_length=True)
    with pytest.raises(TypeError):
        WIRE.crc5_bm13xx_command(payload, bit_length=3.5)
    with pytest.raises(TypeError):
        WIRE.crc5_bm13xx_command(4)


def test_preamble_is_outside_command_crc_coverage():
    body = bytes.fromhex("52050000")
    assert WIRE.crc5_bm13xx_command(body) == 0x0A
    assert WIRE.crc5_bm13xx_command(bytes.fromhex("55aa") + body) != 0x0A


def test_staging_copies_are_byte_identical():
    expected = CANONICAL.read_bytes()
    for staged in STAGED:
        assert staged.read_bytes() == expected


@pytest.mark.parametrize("legacy,buildroot", PAIRED_CONSUMERS)
def test_paired_consumers_are_byte_identical(legacy, buildroot):
    assert legacy.read_bytes() == buildroot.read_bytes()


@pytest.mark.parametrize(
    "path", CONSUMERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_consumers_import_named_command_crc_without_local_copy(path):
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    local_functions = {
        node.name for node in ast.walk(tree) if isinstance(node, ast.FunctionDef)
    }
    assert "crc5_bm1387" not in local_functions

    imported = False
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.module == "dcentos_asic_wire":
            imported = any(alias.name == "crc5_bm13xx_command" for alias in node.names)
    assert imported


@pytest.mark.parametrize(
    "path", RESPONSE_PARSERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_response_parsers_never_call_command_crc(path):
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    parser = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "parse_read_response"
    )
    calls = {
        node.func.id
        for node in ast.walk(parser)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
    }
    assert "crc5_bm13xx_command" not in calls


@pytest.mark.parametrize(
    "path", RESPONSE_PARSERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_response_parsers_preserve_raw_bytes_as_unverified(path):
    module = load_module(path, "consumer_{}".format(abs(hash(str(path)))))
    raw = bytes.fromhex("0c00680261a55a")
    parsed = module.parse_read_response(raw)
    assert parsed[0] == 0x0C
    assert parsed[1] == 0x00680261
    assert parsed[2] == "unverified"
    assert parsed[3] == raw
    assert module.parse_read_response(raw[:-1]) is None
    assert module.parse_read_response(raw + b"\x00") is None


def test_assumption_receipt_cannot_pass_on_unverified_response():
    path = (
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py"
    )
    module = load_module(path, "assumption_verifier_receipt_test")
    result = module.test_a1_3_crc5(module.MockUART(chip_count=1), safe=True)
    assert result.status == "SKIP"
    assert "cannot prove" in result.explanation


def test_address_interval_assumption_cannot_pass_on_unverified_response():
    path = (
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py"
    )
    module = load_module(path, "assumption_verifier_interval_test")
    result = module.test_a1_8_address_interval(
        module.MockUART(chip_count=3), safe=False
    )
    assert result.status == "SKIP"
    assert "certify" in result.explanation
    assert result.raw_data["observations"]
    assert result.raw_data["observations"][0]["response_integrity"] == "simulated"
    assert "response_bytes" in result.raw_data["observations"][0]


@pytest.mark.parametrize(
    "path", TEMP_FINDERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_temperature_results_retain_unverified_integrity_and_raw_bytes(
    path, monkeypatch
):
    module = load_module(path, "temp_integrity_{}".format(abs(hash(str(path)))))
    monkeypatch.setattr(module.time, "sleep", lambda _seconds: None)

    class ObservedUart:
        def __init__(self):
            self.counter = 0

        def read_register(self, chip_addr, reg_addr):
            self.counter += 1
            value = (45 + chip_addr + self.counter) * 256
            raw = bytes([reg_addr, 0, 0, (value >> 8) & 0xFF, value & 0xFF, 0xA5, 0x5A])
            return reg_addr, value, "unverified", raw

    uart = ObservedUart()
    scan = module.scan_for_temperature(
        uart,
        0,
        num_rounds=2,
        delay_between=0,
        reg_start=0x44,
        reg_end=0x44,
        reg_step=1,
        progress=False,
    )
    assert scan[0]["response_integrity"] == "unverified"
    assert scan[0]["verified"] is False
    assert scan[0]["evidence_status"] == "structural_candidate"
    assert "confidence" not in scan[0]
    assert len(scan[0]["response_observations"]) == 2
    assert all(scan[0]["raw_responses_hex"])
    assert all(isinstance(item, list) for item in scan[0]["raw_responses_bytes"])

    cross = module.cross_chip_comparison(uart, [0, 4], [0x44], num_reads=2)
    assert cross[0]["response_integrity"] == "unverified"
    assert cross[0]["verified"] is False
    assert cross[0]["evidence_status"] == "structural_candidate"
    for chip in cross[0]["chips"].values():
        assert chip["response_integrity"] == "unverified"
        assert all(chip["raw_responses_hex"])
        assert all(isinstance(item, list) for item in chip["raw_responses_bytes"])

    encoded = json.dumps({"scan": scan, "cross_chip": cross})
    assert '"response_integrity": "unverified"' in encoded
    report = module.format_temp_report(scan, cross)
    assert "Response integrity: unverified (values are observational)" in report
    assert "most likely" not in report.lower()
    assert "confidence" not in report.lower()


@pytest.mark.parametrize(
    "path", TEMP_FINDERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_temperature_consumers_do_not_interpret_mismatched_registers(path, monkeypatch):
    module = load_module(path, "temp_mismatch_{}".format(abs(hash(str(path)))))
    monkeypatch.setattr(module.time, "sleep", lambda _seconds: None)
    raw = bytes.fromhex("7c00002d00a55a")

    class MismatchedUart:
        def read_register(self, chip_addr, reg_addr):
            return 0x7C, 0x00002D00, "unverified", raw

    scan = module.scan_for_temperature(
        MismatchedUart(),
        0,
        num_rounds=1,
        delay_between=0,
        reg_start=0x44,
        reg_end=0x44,
        reg_step=1,
        progress=False,
    )
    assert scan[0]["readings"] == []
    assert scan[0]["average"] is None
    assert scan[0]["is_temp_candidate"] is False
    assert scan[0]["evidence_status"] == "register_mismatch"
    assert scan[0]["response_observations"][0]["status"] == "register_mismatch"
    assert scan[0]["raw_responses_hex"] == [raw.hex()]

    cross = module.cross_chip_comparison(MismatchedUart(), [0, 4], [0x44], num_reads=1)
    assert cross[0]["cross_chip_avg"] is None
    assert cross[0]["temp_pattern"] is False
    assert cross[0]["evidence_status"] == "register_mismatch"
    for chip in cross[0]["chips"].values():
        assert chip["values"] == []
        assert chip["average"] is None
        assert chip["evidence_status"] == "register_mismatch"
        assert chip["raw_responses_hex"] == [raw.hex()]


@pytest.mark.parametrize(
    "path", REGISTER_SCANNERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_register_scanner_does_not_decode_mismatched_register(path):
    module = load_module(path, "scanner_mismatch_{}".format(abs(hash(str(path)))))
    raw = bytes.fromhex("7c00680261a55a")

    class MismatchedUart:
        def read_register(self, chip_addr, reg_addr):
            return 0x7C, 0x00680261, "unverified", raw

    entry = module.scan_registers(
        MismatchedUart(),
        0,
        reg_start=0x0C,
        reg_end=0x0C,
        reg_step=1,
        progress=False,
    )[0]
    assert entry["returned_register"] == 0x7C
    assert entry["value"] is None
    assert entry["decoded"] == ""
    assert entry["evidence_status"] == "register_mismatch"
    assert entry["raw_response_hex"] == raw.hex()
    assert entry["raw_response_bytes"] == list(raw)


@pytest.mark.parametrize(
    "path", ENUMERATORS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_enumerator_does_not_interpret_mismatched_address_register(path, monkeypatch):
    module = load_module(path, "enumerator_mismatch_{}".format(abs(hash(str(path)))))
    monkeypatch.setattr(module.time, "sleep", lambda _seconds: None)
    raw = bytes.fromhex("7c00000000a55a")

    class MismatchedUart:
        def send_command(self, _command):
            return b""

        def read_register(self, chip_addr, reg_addr):
            return 0x7C, 0, "unverified", raw

    active = module.enumerate_chain_active(MismatchedUart(), max_chips=2)
    passive = module.enumerate_chain_passive(MismatchedUart(), max_chips=1)
    for result in (active[0], passive[0]):
        assert result["evidence_status"] == "register_mismatch"
        assert result["requested_register"] == module.REG_CHIP_ADDRESS
        assert result["returned_register"] == 0x7C
        assert "chip_addr_reg" not in result
        assert result["raw_response_hex"] == raw.hex()


def test_assumption_verifier_rejects_oversized_and_mismatched_responses():
    path = (
        ROOT
        / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py"
    )
    module = load_module(path, "assumption_verifier_strict_response_test")

    class OversizedUart:
        def flush_input(self):
            pass

        def write(self, _command):
            pass

        def read(self, _length, timeout=None):
            return bytes.fromhex("7c00680261a55a00")

    oversized = module.test_a1_5_response_7byte(OversizedUart(), safe=True)
    assert oversized.status == "FAIL"
    assert "exactly 7" in oversized.explanation

    class MismatchedUart:
        def read_register(self, chip_addr, reg_addr):
            return 0x7C, 0x00680261, "verified", bytes.fromhex("7c00680261a55a")

    pll = module.test_a1_10_pll_formula(MismatchedUart(), safe=True)
    assert pll.status == "SKIP"
    assert pll.raw_data["register_matches"] is False
    assert "pll_raw" not in pll.raw_data


@pytest.mark.parametrize(
    "path", HARDWARE_UART_CONSUMERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_hardware_uart_send_is_fail_closed_without_captured_profile(path):
    module = load_module(path, "send_guard_{}".format(abs(hash(str(path)))))
    uart = module.UARTInterface()
    with pytest.raises(RuntimeError, match="not capture-validated"):
        uart.write(b"\x00")


def test_board_health_enumeration_is_fail_closed_without_captured_profile():
    path = ROOT / "overlay/root/tools/board_health.py"
    module = load_module(path, "board_health_send_guard")
    with pytest.raises(RuntimeError, match="not capture-validated"):
        module.enumerate_asic_chain(6, hw_devmem=lambda _addr: 0)


@pytest.mark.parametrize(
    "path", HARDWARE_UART_CONSUMERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_uart_rejects_wrong_register_and_preserves_raw_observation(path, monkeypatch):
    module = load_module(path, "register_match_{}".format(abs(hash(str(path)))))
    monkeypatch.setattr(module.time, "sleep", lambda _seconds: None)
    uart = module.UARTInterface()
    requested = 0x0C
    mismatched_raw = bytes.fromhex("7c00680261a55a")
    uart.flush_input = lambda: None
    uart.write = lambda _data: None
    uart.read = lambda _length, timeout=None: mismatched_raw

    assert uart.read_register(0, requested, retries=0) is None
    assert uart.last_response_observation == {
        "requested_register": requested,
        "returned_register": 0x7C,
        "register_matches": False,
        "response_integrity": "unverified",
        "raw_response_hex": mismatched_raw.hex(),
        "raw_response_bytes": list(mismatched_raw),
    }


@pytest.mark.parametrize(
    "path", TEMP_FINDERS, ids=lambda path: str(path.relative_to(ROOT))
)
def test_temperature_source_has_no_authoritative_ranking_words(path):
    source = path.read_text(encoding="utf-8").lower()
    assert "most likely" not in source
    assert "confidence" not in source
