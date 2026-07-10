#!/usr/bin/env python3
"""
temp_finder.py - BM1387 Temperature Register Discovery
========================================================
Discovers which BM1387 register(s) expose the on-chip temperature sensor.

Strategy:
  1. Read all registers (0x00-0xFF) from chip 0 multiple times with delays
  2. Identify registers whose values change over time (temperature drifts)
  3. Flag values in plausible temperature range (20-120C, various encodings)
  4. Compare candidate registers across multiple chips
  5. Cross-reference with known BM1366/BM1397 patterns (0x40 area)

BM1387 has an on-die temperature diode but the register is undocumented.
This tool systematically discovers it.

Part of DCENTos Hacker Shell firmware research tools.
"""

import sys
import os
import struct
import time
import json
import collections

try:
    import fcntl
    import termios
    HAS_TERMIOS = True
except ImportError:
    HAS_TERMIOS = False

# ============================================================================
# BM1387 Constants
# ============================================================================

UART_DEFAULT = "/dev/ttyPS1"
UART_BAUD = 115200
ADDR_INTERVAL = 4
MAX_CHIPS = 63

CMD_READ_REGISTER = 0x54
READ_RESPONSE_LEN = 7

# Known register addresses (to filter OUT as non-temperature)
KNOWN_STATIC_REGS = {
    0x00, 0x14, 0x18, 0x28, 0x2C, 0x30, 0x34, 0x7C, 0xFC,
    0x50, 0x54, 0x58, 0x5C,  # Core enable regs (static unless cores disabled)
}

# Candidate temperature register area (from BM1366 patterns)
TEMP_CANDIDATE_AREA = range(0x38, 0x50)  # 0x38-0x4C is the hot zone

# Temperature plausibility ranges for different encodings
TEMP_ENCODINGS = {
    "raw_direct":       (20, 120),       # Direct value in C
    "div256":           (5120, 30720),    # value / 256 = C (8.8 fixed point)
    "div128":           (2560, 15360),    # value / 128 = C
    "div16":            (320, 1920),      # value / 16 = C (4.4 fixed point)
    "bm1366_style":     (0x1400, 0x7800), # BM1366 temp register pattern
    "adc_10bit":        (50, 450),        # 10-bit ADC, ~0.3C/LSB from 0
    "inverted":         (0xFF00 - 120*256, 0xFF00 - 20*256),  # Inverted
}


# ============================================================================
# CRC5 for BM1387
# ============================================================================

def crc5_bm1387(data, bit_length=None):
    """CRC5 polynomial 0x05, init 0x1F."""
    crc = 0x1F
    poly = 0x05
    if bit_length is None:
        bit_length = len(data) * 8
    bit_index = 0
    for byte in data:
        for i in range(7, -1, -1):
            if bit_index >= bit_length:
                break
            bit = (byte >> i) & 1
            top_bit = (crc >> 4) & 1
            crc = ((crc << 1) | bit) & 0x3F
            if top_bit ^ ((crc >> 5) & 1):
                crc ^= poly
            crc &= 0x1F
            bit_index += 1
            if bit_index >= bit_length:
                break
    return crc & 0x1F


def build_read_register_cmd(chip_addr, reg_addr):
    """Build BM1387 read register command."""
    cmd_data = bytes([CMD_READ_REGISTER, chip_addr, reg_addr])
    crc = crc5_bm1387(cmd_data)
    return bytes([CMD_READ_REGISTER, chip_addr, reg_addr, crc & 0x1F])


def parse_read_response(response):
    """Parse 7-byte response."""
    if len(response) < READ_RESPONSE_LEN:
        return None
    reg_addr = response[0]
    value = (response[1] << 24) | (response[2] << 16) | (response[3] << 8) | response[4]
    resp_crc = response[5] & 0x1F
    calc_crc = crc5_bm1387(response[:5])
    crc_ok = (calc_crc == resp_crc)
    return (reg_addr, value, crc_ok)


# ============================================================================
# UART Interface
# ============================================================================

class UARTInterface:
    """Direct UART to BM1387."""

    def __init__(self, device=UART_DEFAULT, baud=UART_BAUD, timeout=0.1):
        self.device = device
        self.baud = baud
        self.timeout = timeout
        self.fd = None
        self.fobj = None

    def open(self):
        self.fd = os.open(self.device, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        if HAS_TERMIOS:
            attrs = termios.tcgetattr(self.fd)
            attrs[0] = 0
            attrs[1] = 0
            attrs[2] = (termios.CS8 | termios.CREAD | termios.CLOCAL)
            attrs[3] = 0
            attrs[6][termios.VMIN] = 0
            attrs[6][termios.VTIME] = 1
            baud_const = getattr(termios, 'B115200', 4098)
            attrs[4] = baud_const
            attrs[5] = baud_const
            termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
            termios.tcflush(self.fd, termios.TCIOFLUSH)
        self.fobj = os.fdopen(self.fd, "rb+", buffering=0)

    def close(self):
        if self.fobj:
            try:
                self.fobj.close()
            except Exception:
                pass
            self.fobj = None
            self.fd = None

    def write(self, data):
        if self.fobj:
            self.fobj.write(data)
            self.fobj.flush()

    def read(self, length, timeout=None):
        if timeout is None:
            timeout = self.timeout
        if not self.fobj:
            return b""
        result = b""
        deadline = time.time() + timeout
        while len(result) < length and time.time() < deadline:
            try:
                chunk = self.fobj.read(length - len(result))
                if chunk:
                    result += chunk
                else:
                    time.sleep(0.005)
            except (OSError, IOError):
                time.sleep(0.005)
        return result

    def flush_input(self):
        if HAS_TERMIOS and self.fd is not None:
            termios.tcflush(self.fd, termios.TCIFLUSH)

    def read_register(self, chip_addr, reg_addr, retries=2):
        cmd = build_read_register_cmd(chip_addr, reg_addr)
        for attempt in range(retries + 1):
            self.flush_input()
            self.write(cmd)
            time.sleep(0.01)
            resp = self.read(READ_RESPONSE_LEN, timeout=0.05)
            if len(resp) == READ_RESPONSE_LEN:
                parsed = parse_read_response(resp)
                if parsed is not None:
                    return parsed
            if attempt < retries:
                time.sleep(0.02)
        return None


# ============================================================================
# Mock UART with Temperature Simulation
# ============================================================================

class MockUART:
    """Simulated BM1387 with temperature drift for testing."""

    def __init__(self, chip_count=3):
        self.device = "/dev/mock_uart"
        self.chip_count = chip_count
        self._read_count = 0
        self._base_temp = 45.0
        self.chip_registers = {}
        self._setup_chips()

    def _setup_chips(self):
        for idx in range(self.chip_count):
            addr = idx * ADDR_INTERVAL
            # Static registers
            self.chip_registers[(addr, 0x00)] = addr
            self.chip_registers[(addr, 0x0C)] = 0x00680261
            self.chip_registers[(addr, 0x14)] = 0x0000FFFF
            self.chip_registers[(addr, 0x28)] = 0x00000019
            self.chip_registers[(addr, 0x7C)] = 0x13871387
            self.chip_registers[(addr, 0xFC)] = 0x00000001
            # Core enable (static)
            self.chip_registers[(addr, 0x50)] = 0xFFFFFFFF
            self.chip_registers[(addr, 0x54)] = 0xFFFFFFFF
            self.chip_registers[(addr, 0x58)] = 0xFFFFFFFF
            self.chip_registers[(addr, 0x5C)] = 0x0003FFFF

            # Temperature registers (THESE are what we're trying to find)
            # 0x40: temperature control/config (static)
            self.chip_registers[(addr, 0x40)] = 0x00000003
            # 0x44: temperature DATA register (varies!)
            # Encoded as value/256 in lower 16 bits
            # Will be dynamically generated

            # Some other varying registers (noise, counters)
            # 0x04: hash rate counter (changes rapidly)
            # 0xA0: golden nonce (changes)

    def open(self):
        pass

    def close(self):
        pass

    def _get_simulated_temp(self, chip_idx):
        """Get simulated temperature that drifts over time."""
        self._read_count += 1
        # Base temp + chip-specific offset + slow drift + small noise
        base = self._base_temp + chip_idx * 2.5  # Each chip slightly hotter
        drift = (self._read_count % 20) * 0.15   # Slow upward drift
        noise = ((self._read_count * 7 + chip_idx * 13) % 5) * 0.1  # Deterministic noise
        return base + drift + noise

    def read_register(self, chip_addr, reg_addr, retries=2):
        chip_idx = chip_addr // ADDR_INTERVAL
        if chip_idx >= self.chip_count:
            return None

        # Static register
        static_val = self.chip_registers.get((chip_addr, reg_addr))

        if reg_addr == 0x44:
            # Temperature DATA register - varies with reads
            temp_c = self._get_simulated_temp(chip_idx)
            # Encode as value * 256 (8.8 fixed point) in lower 16 bits
            raw_temp = int(temp_c * 256) & 0xFFFF
            value = raw_temp
            return (reg_addr, value, True)

        elif reg_addr == 0x04:
            # Hash rate counter - changes rapidly (NOT temperature)
            value = (self._read_count * 12345 + chip_idx * 67890) & 0xFFFFFFFF
            return (reg_addr, value, True)

        elif reg_addr == 0xA0:
            # Golden nonce - changes rapidly (NOT temperature)
            value = (self._read_count * 54321 + chip_idx * 9876) & 0xFFFFFFFF
            return (reg_addr, value, True)

        elif reg_addr == 0xA4:
            # Return nonce - also changes
            value = (self._read_count * 11111 + chip_idx * 22222) & 0xFFFFFFFF
            return (reg_addr, value, True)

        elif reg_addr == 0x48:
            # Error counter - slowly incrementing
            value = self._read_count // 5
            return (reg_addr, value, True)

        elif static_val is not None:
            return (reg_addr, static_val, True)

        else:
            # Unknown register - return 0
            return (reg_addr, 0x00000000, True)

    def flush_input(self):
        pass

    def write(self, data):
        pass

    def read(self, length, timeout=None):
        return b""


# ============================================================================
# Temperature Analysis Logic
# ============================================================================

def check_temp_plausibility(value):
    """
    Check if a register value could represent temperature in various encodings.
    Returns list of (encoding_name, decoded_temp) for plausible matches.
    """
    matches = []
    v16 = value & 0xFFFF  # Lower 16 bits most likely to hold temp

    # Direct C (unlikely but check)
    if 20 <= v16 <= 120:
        matches.append(("raw_direct", float(v16)))

    # Divide by 256 (8.8 fixed point)
    temp_div256 = v16 / 256.0
    if 20 <= temp_div256 <= 120:
        matches.append(("div256", temp_div256))

    # Divide by 128
    temp_div128 = v16 / 128.0
    if 20 <= temp_div128 <= 120:
        matches.append(("div128", temp_div128))

    # Divide by 16 (4.4 fixed point)
    temp_div16 = v16 / 16.0
    if 20 <= temp_div16 <= 120:
        matches.append(("div16", temp_div16))

    # 10-bit ADC style (0.25C per LSB, offset 0)
    v10 = v16 & 0x3FF
    temp_adc = v10 * 0.25
    if 20 <= temp_adc <= 120:
        matches.append(("adc_10bit", temp_adc))

    # BM1366-style encoding (upper byte * 0.5 + lower/256)
    upper = (v16 >> 8) & 0xFF
    lower = v16 & 0xFF
    temp_1366 = upper * 0.5 + lower / 512.0
    if 20 <= temp_1366 <= 120:
        matches.append(("bm1366_style", temp_1366))

    # Also check full 32-bit for some encodings
    if value != v16:
        full_div256 = (value & 0xFFFF0000) >> 16
        if 20 <= full_div256 / 256.0 <= 120:
            matches.append(("upper16_div256", full_div256 / 256.0))

    return matches


def analyze_register_variance(readings_over_time):
    """
    Analyze a list of readings for a single register.
    Returns dict with variance analysis.
    """
    if not readings_over_time:
        return {"varies": False, "readings": 0}

    values = [r for r in readings_over_time if r is not None]
    if len(values) < 2:
        return {"varies": False, "readings": len(values)}

    unique = len(set(values))
    min_val = min(values)
    max_val = max(values)
    spread = max_val - min_val

    # Calculate average and variance
    avg = sum(values) / len(values)
    variance = sum((v - avg) ** 2 for v in values) / len(values)

    # Coefficient of variation (normalized spread)
    cv = (variance ** 0.5) / avg if avg != 0 else 0

    return {
        "varies": unique > 1,
        "readings": len(values),
        "unique_values": unique,
        "min": min_val,
        "max": max_val,
        "spread": spread,
        "average": avg,
        "variance": variance,
        "cv": cv,
        "values": values,
    }


def classify_varying_register(reg_addr, analysis):
    """
    Classify a varying register as temperature candidate or noise.
    Returns (is_temp_candidate, confidence, reason).
    """
    if not analysis["varies"]:
        return (False, 0.0, "static")

    spread = analysis["spread"]
    cv = analysis["cv"]
    avg = analysis["average"]
    unique = analysis["unique_values"]
    readings = analysis["readings"]

    # Check if values are in temperature-plausible range
    temp_matches = check_temp_plausibility(int(avg))

    # High-frequency changers (counters, nonces) - REJECT
    # They change almost every read and have huge spread
    if unique >= readings * 0.8 and spread > 0x10000:
        return (False, 0.0, "counter/nonce (high variance)")

    if cv > 0.5 and spread > 0x1000:
        return (False, 0.0, "high-variance counter")

    # Temperature characteristics:
    # - Changes slowly (not every read, or by small amounts)
    # - Spread is small relative to value
    # - Values are in plausible temperature range
    score = 0.0
    reasons = []

    if temp_matches:
        score += 0.3
        reasons.append("temp-range match ({})".format(
            ", ".join("{}: {:.1f}C".format(e, t) for e, t in temp_matches[:3])))

    if reg_addr in TEMP_CANDIDATE_AREA:
        score += 0.2
        reasons.append("in candidate area (0x38-0x4C)")

    # Moderate variance (temperature drift)
    if 0.001 < cv < 0.1:
        score += 0.2
        reasons.append("moderate CV={:.4f}".format(cv))

    # Small spread relative to value
    if avg > 0 and spread / avg < 0.1:
        score += 0.15
        reasons.append("small relative spread")

    # Not changing every single read
    if unique < readings * 0.5:
        score += 0.1
        reasons.append("semi-stable ({}/{} unique)".format(unique, readings))
    elif unique < readings * 0.9:
        score += 0.05

    # Known static register - very unlikely temp
    if reg_addr in KNOWN_STATIC_REGS:
        score -= 0.3
        reasons.append("KNOWN STATIC REG (unlikely temp)")

    is_candidate = score >= 0.3
    confidence = min(score, 1.0)
    reason = "; ".join(reasons) if reasons else "no strong indicators"

    return (is_candidate, confidence, reason)


def scan_for_temperature(uart, chip_addr, num_rounds=5, delay_between=2.0,
                         reg_start=0x00, reg_end=0xFF, reg_step=4,
                         verbose=False, progress=True):
    """
    Multi-round register scan to find temperature registers.
    Returns analysis results.
    """
    # Collect readings over time
    all_readings = collections.defaultdict(list)

    for round_num in range(num_rounds):
        if progress:
            sys.stderr.write("  Round {}/{} (chip addr=0x{:02X})...\n".format(
                round_num + 1, num_rounds, chip_addr))

        for reg in range(reg_start, reg_end + 1, reg_step):
            result = uart.read_register(chip_addr, reg)
            if result is not None:
                _, value, crc_ok = result
                if crc_ok:
                    all_readings[reg].append(value)
                else:
                    all_readings[reg].append(None)  # CRC error
            else:
                all_readings[reg].append(None)  # No response

        if round_num < num_rounds - 1:
            if progress:
                sys.stderr.write("    Waiting {:.0f}s for temperature drift...\n".format(delay_between))
            time.sleep(delay_between)

    # Analyze each register
    results = []
    for reg in sorted(all_readings.keys()):
        readings = all_readings[reg]
        analysis = analyze_register_variance(readings)
        is_candidate, confidence, reason = classify_varying_register(reg, analysis)

        entry = {
            "register": reg,
            "register_hex": "0x{:02X}".format(reg),
            "chip_addr": chip_addr,
            "varies": analysis["varies"],
            "unique_values": analysis.get("unique_values", 0),
            "min": analysis.get("min"),
            "max": analysis.get("max"),
            "min_hex": "0x{:08X}".format(analysis["min"]) if analysis.get("min") is not None else None,
            "max_hex": "0x{:08X}".format(analysis["max"]) if analysis.get("max") is not None else None,
            "spread": analysis.get("spread"),
            "average": analysis.get("average"),
            "cv": analysis.get("cv"),
            "is_temp_candidate": is_candidate,
            "confidence": round(confidence, 2),
            "reason": reason,
            "readings": analysis.get("values", []),
        }

        # Add temperature decode attempts for candidates
        if is_candidate and analysis.get("average") is not None:
            entry["temp_decodings"] = check_temp_plausibility(int(analysis["average"]))

        results.append(entry)

        if verbose and analysis["varies"]:
            tag = "*** TEMP CANDIDATE ***" if is_candidate else ""
            sys.stderr.write("  [0x{:02X}] varies: spread={} unique={} confidence={:.2f} {} {}\n".format(
                reg, analysis.get("spread", 0), analysis.get("unique_values", 0),
                confidence, reason[:50], tag))

    return results


def cross_chip_comparison(uart, chip_addrs, candidate_regs, num_reads=3,
                          verbose=False):
    """
    Read candidate registers from multiple chips to see if they
    show similar-but-not-identical values (temperature pattern).
    """
    results = []

    for reg in candidate_regs:
        chip_values = {}
        for chip_addr in chip_addrs:
            values = []
            for _ in range(num_reads):
                result = uart.read_register(chip_addr, reg)
                if result:
                    _, value, crc_ok = result
                    if crc_ok:
                        values.append(value)
                time.sleep(0.1)
            if values:
                avg = sum(values) / len(values)
                chip_values[chip_addr] = {
                    "values": values,
                    "average": avg,
                }

        if len(chip_values) >= 2:
            averages = [cv["average"] for cv in chip_values.values()]
            spread = max(averages) - min(averages)
            overall_avg = sum(averages) / len(averages)

            # Temperature pattern: values are similar but not identical
            # Typically within 10-20C of each other
            similar = spread < overall_avg * 0.3 if overall_avg > 0 else False
            not_identical = spread > 0

            entry = {
                "register": reg,
                "register_hex": "0x{:02X}".format(reg),
                "chips": {},
                "cross_chip_spread": spread,
                "cross_chip_avg": overall_avg,
                "similar_not_identical": similar and not_identical,
                "temp_pattern": similar and not_identical,
            }

            for chip_addr, cv in chip_values.items():
                entry["chips"]["0x{:02X}".format(chip_addr)] = cv

                # Add temp decodings
                temp_matches = check_temp_plausibility(int(cv["average"]))
                if temp_matches:
                    entry["chips"]["0x{:02X}".format(chip_addr)]["temp_decodings"] = temp_matches

            results.append(entry)

            if verbose:
                sys.stderr.write("  [0x{:02X}] cross-chip: spread={:.0f} similar={} pattern={}\n".format(
                    reg, spread, similar, similar and not_identical))

    return results


def format_temp_report(scan_results, cross_results=None):
    """Format temperature discovery results."""
    lines = []
    lines.append("BM1387 Temperature Register Discovery Report")
    lines.append("=" * 65)

    # Candidates
    candidates = [r for r in scan_results if r["is_temp_candidate"]]
    varying = [r for r in scan_results if r["varies"]]
    static = [r for r in scan_results if not r["varies"] and r.get("unique_values", 0) > 0]

    lines.append("Total registers probed: {}".format(len(scan_results)))
    lines.append("Static registers: {}".format(len(static)))
    lines.append("Varying registers: {}".format(len(varying)))
    lines.append("Temperature candidates: {}".format(len(candidates)))
    lines.append("")

    if candidates:
        lines.append("--- Temperature Candidates (ranked by confidence) ---")
        lines.append("{:<8} {:<10} {:<12} {:<12} {:<8} {}".format(
            "REG", "CONF", "AVG", "SPREAD", "UNIQUE", "REASON"))
        lines.append("-" * 65)

        sorted_candidates = sorted(candidates, key=lambda x: -x["confidence"])
        for c in sorted_candidates:
            lines.append("{:<8} {:<10.2f} {:<12} {:<12} {:<8} {}".format(
                c["register_hex"],
                c["confidence"],
                "0x{:08X}".format(int(c["average"])) if c["average"] else "?",
                c.get("spread", 0),
                c.get("unique_values", 0),
                c["reason"][:40],
            ))

            # Show temperature decodings
            if "temp_decodings" in c and c["temp_decodings"]:
                for enc, temp in c["temp_decodings"]:
                    lines.append("         -> {} = {:.1f} C".format(enc, temp))

        lines.append("")

    # Varying but not candidate (noise/counters)
    non_candidates = [r for r in varying if not r["is_temp_candidate"]]
    if non_candidates:
        lines.append("--- Varying Non-Candidates (counters/noise) ---")
        for nc in non_candidates[:10]:  # Show top 10
            lines.append("  [{}] spread={} unique={} reason={}".format(
                nc["register_hex"], nc.get("spread", 0),
                nc.get("unique_values", 0), nc["reason"][:50]))
        if len(non_candidates) > 10:
            lines.append("  ... and {} more".format(len(non_candidates) - 10))
        lines.append("")

    # Cross-chip comparison
    if cross_results:
        lines.append("--- Cross-Chip Comparison ---")
        for cr in cross_results:
            pattern = "TEMP PATTERN" if cr["temp_pattern"] else "not temp"
            lines.append("  [{}] cross-chip spread={:.0f} avg={:.0f} -> {}".format(
                cr["register_hex"], cr["cross_chip_spread"],
                cr["cross_chip_avg"], pattern))
            for chip_key, cv in cr.get("chips", {}).items():
                temp_str = ""
                if "temp_decodings" in cv:
                    temp_str = " -> " + ", ".join(
                        "{}: {:.1f}C".format(e, t) for e, t in cv["temp_decodings"][:2])
                lines.append("    chip {}: avg={:.0f}{}".format(
                    chip_key, cv["average"], temp_str))
        lines.append("")

    # Final recommendation
    lines.append("--- Recommendation ---")
    if candidates:
        best = max(candidates, key=lambda x: x["confidence"])
        lines.append("Most likely temperature register: {} (confidence {:.0f}%)".format(
            best["register_hex"], best["confidence"] * 100))
        if "temp_decodings" in best and best["temp_decodings"]:
            enc, temp = best["temp_decodings"][0]
            lines.append("Likely encoding: {} (current reading: {:.1f} C)".format(enc, temp))
        lines.append("Verify by: 1) Heat the chip and re-run  2) Compare multiple chips")
    else:
        lines.append("No strong temperature candidates found.")
        lines.append("Try: 1) More rounds (--rounds 10)  2) Longer delay (--delay 5)")
        lines.append("     3) Heat the chip first  4) Check registers 0x38-0x4F manually")

    return "\n".join(lines)


# ============================================================================
# Self-Tests
# ============================================================================

def run_self_tests():
    """Run self-tests without hardware."""
    tests = []
    passed = 0
    failed = 0

    def test(name, condition, detail=""):
        nonlocal passed, failed
        status = "PASS" if condition else "FAIL"
        if condition:
            passed += 1
        else:
            failed += 1
        tests.append({"name": name, "status": status, "detail": detail})

    # --- Test 1: CRC5 basic ---
    crc = crc5_bm1387(bytes([0x54, 0x00, 0x44]))
    test("CRC5: valid range",
         0 <= crc <= 0x1F,
         "crc=0x{:02X}".format(crc))

    # --- Test 2: Build read command ---
    cmd = build_read_register_cmd(0x00, 0x44)
    test("Build CMD: 4 bytes, opcode 0x54",
         len(cmd) == 4 and cmd[0] == 0x54)

    # --- Test 3: Parse response ---
    val = 0x00002D00  # ~45C in div256 encoding
    resp = bytes([0x44, 0x00, 0x00, 0x2D, 0x00])
    crc_r = crc5_bm1387(resp)
    full = resp + bytes([crc_r, 0x00])
    parsed = parse_read_response(full)
    test("Parse response: correct value",
         parsed is not None and parsed[1] == 0x00002D00,
         "parsed={}".format(parsed))

    # --- Test 4: Temp plausibility - div256 (45C) ---
    matches = check_temp_plausibility(45 * 256)  # 11520
    encodings = [m[0] for m in matches]
    test("Temp plausibility: div256 detected for 45C",
         "div256" in encodings,
         "matches={}".format(matches))

    # --- Test 5: Temp plausibility - direct C ---
    matches2 = check_temp_plausibility(42)
    encodings2 = [m[0] for m in matches2]
    test("Temp plausibility: raw_direct for 42",
         "raw_direct" in encodings2,
         "matches={}".format(matches2))

    # --- Test 6: Temp plausibility - out of range ---
    matches3 = check_temp_plausibility(0xDEADBEEF)
    test("Temp plausibility: rejects 0xDEADBEEF",
         len(matches3) == 0 or all(m[1] < 20 or m[1] > 120 for m in matches3),
         "matches={}".format(matches3))

    # --- Test 7: Variance analysis - static ---
    analysis_static = analyze_register_variance([100, 100, 100, 100, 100])
    test("Variance: static values detected",
         not analysis_static["varies"],
         "varies={}".format(analysis_static["varies"]))

    # --- Test 8: Variance analysis - varying ---
    analysis_vary = analyze_register_variance([100, 102, 101, 103, 104])
    test("Variance: varying values detected",
         analysis_vary["varies"] and analysis_vary["unique_values"] == 5,
         "unique={}".format(analysis_vary["unique_values"]))

    # --- Test 9: Variance analysis - spread ---
    test("Variance: correct spread",
         analysis_vary["spread"] == 4,
         "spread={}".format(analysis_vary["spread"]))

    # --- Test 10: Classify - counter (high variance) ---
    analysis_counter = analyze_register_variance([1000, 50000, 99000, 200000, 500000])
    is_cand, conf, reason = classify_varying_register(0xA0, analysis_counter)
    test("Classify: counter rejected",
         not is_cand,
         "confidence={:.2f} reason={}".format(conf, reason))

    # --- Test 11: Classify - temperature candidate ---
    # Values around 11520 (45C in div256) with small drift
    temp_readings = [11520, 11525, 11530, 11520, 11535]
    analysis_temp = analyze_register_variance(temp_readings)
    is_cand2, conf2, reason2 = classify_varying_register(0x44, analysis_temp)
    test("Classify: temp candidate at 0x44",
         is_cand2 and conf2 > 0.3,
         "confidence={:.2f} reason={}".format(conf2, reason2))

    # --- Test 12: Classify - static register ---
    analysis_s = analyze_register_variance([0x13871387] * 5)
    is_cand3, conf3, reason3 = classify_varying_register(0x7C, analysis_s)
    test("Classify: static register rejected",
         not is_cand3,
         "reason={}".format(reason3))

    # --- Test 13: MockUART temp simulation ---
    mock = MockUART(chip_count=2)
    r1 = mock.read_register(0x00, 0x44)
    r2 = mock.read_register(0x00, 0x44)
    test("MockUART: temp register returns values",
         r1 is not None and r2 is not None,
         "r1={} r2={}".format(r1, r2))

    # --- Test 14: MockUART temp values differ ---
    vals = []
    for _ in range(5):
        r = mock.read_register(0x00, 0x44)
        if r:
            vals.append(r[1])
    test("MockUART: temp values vary over reads",
         len(set(vals)) > 1,
         "vals={}".format(vals))

    # --- Test 15: MockUART static registers stay static ---
    static_vals = []
    for _ in range(5):
        r = mock.read_register(0x00, 0x7C)
        if r:
            static_vals.append(r[1])
    test("MockUART: version register stays static",
         len(set(static_vals)) == 1 and static_vals[0] == 0x13871387,
         "vals={}".format(static_vals))

    # --- Test 16: Full temp scan on mock ---
    mock2 = MockUART(chip_count=2)
    results = scan_for_temperature(mock2, 0x00, num_rounds=3, delay_between=0,
                                   reg_start=0x40, reg_end=0x4C, reg_step=4,
                                   verbose=False, progress=False)
    candidates = [r for r in results if r["is_temp_candidate"]]
    test("Full scan: finds at least one temp candidate",
         len(candidates) >= 1,
         "candidates={}".format([c["register_hex"] for c in candidates]))

    # --- Test 17: 0x44 is in candidates ---
    cand_regs = [c["register"] for c in candidates]
    test("Full scan: 0x44 identified as candidate",
         0x44 in cand_regs,
         "candidate_regs={}".format(["0x{:02X}".format(r) for r in cand_regs]))

    # --- Test 18: Cross-chip comparison ---
    mock3 = MockUART(chip_count=3)
    cross = cross_chip_comparison(mock3, [0x00, 0x04, 0x08], [0x44],
                                  num_reads=3, verbose=False)
    test("Cross-chip: returns comparison data",
         len(cross) >= 1,
         "results={}".format(len(cross)))

    # --- Test 19: Cross-chip temperature pattern ---
    if cross:
        test("Cross-chip: 0x44 shows temp pattern",
             cross[0].get("temp_pattern", False) or cross[0].get("similar_not_identical", False),
             "pattern={}".format(cross[0].get("temp_pattern")))
    else:
        test("Cross-chip: 0x44 shows temp pattern", False, "no cross results")

    # --- Test 20: Format report ---
    report = format_temp_report(results, cross)
    test("Format: report contains key sections",
         "Temperature" in report and "Candidate" in report,
         "report_lines={}".format(len(report.split("\n"))))

    # --- Test 21: JSON serialization ---
    json_str = json.dumps(results, default=str)
    json_back = json.loads(json_str)
    test("JSON: results serialize correctly",
         isinstance(json_back, list))

    # --- Test 22: Empty readings handled ---
    analysis_empty = analyze_register_variance([])
    test("Variance: empty readings handled",
         not analysis_empty["varies"] and analysis_empty["readings"] == 0)

    # --- Test 23: None readings filtered ---
    analysis_none = analyze_register_variance([None, 100, None, 101, None])
    test("Variance: None readings filtered",
         analysis_none["readings"] == 2,
         "readings={}".format(analysis_none["readings"]))

    # --- Test 24: KNOWN_STATIC_REGS contains expected regs ---
    test("Constants: KNOWN_STATIC_REGS has version reg",
         0x7C in KNOWN_STATIC_REGS and 0x00 in KNOWN_STATIC_REGS)

    # --- Test 25: TEMP_CANDIDATE_AREA covers 0x40-0x4C ---
    test("Constants: TEMP_CANDIDATE_AREA includes 0x40-0x4C",
         0x40 in TEMP_CANDIDATE_AREA and 0x44 in TEMP_CANDIDATE_AREA and 0x48 in TEMP_CANDIDATE_AREA,
         "range=0x{:02X}-0x{:02X}".format(min(TEMP_CANDIDATE_AREA), max(TEMP_CANDIDATE_AREA)))

    return passed, failed, tests


# ============================================================================
# CLI
# ============================================================================

def print_help():
    help_text = """
temp_finder.py - BM1387 Temperature Register Discovery
=========================================================

Usage:
  temp_finder.py [OPTIONS]

Options:
  --help              Show this help message
  --test              Run self-tests (no hardware required)
  --json              Output results as JSON
  --device PATH       UART device (default: /dev/ttyPS1)
  --chip ADDR         Chip address to analyze (hex, default: 0x00)
  --rounds N          Number of scan rounds (default: 5)
  --delay SECONDS     Delay between rounds in seconds (default: 2.0)
  --reg-start HEX     Start register (default: 0x00)
  --reg-end HEX       End register (default: 0xFF)
  --cross-chips N     Also compare across N chips (default: 0 = disabled)
  --candidates-only   Only show temperature candidates
  --verbose           Verbose output during scanning

Strategy:
  1. Reads all registers multiple times with delays between rounds
  2. Identifies registers whose values change (temperature drifts slowly)
  3. Filters out counters/nonces (change too fast/wildly)
  4. Checks values against known temperature encoding patterns
  5. Cross-chip comparison validates (temps similar but not identical)

Known temperature register candidates for BM1387:
  0x40 - Temperature sensor control
  0x44 - Temperature sensor data (most likely)
  0x48 - May be error/temp related

Examples:
  # Self-test (no hardware):
  temp_finder.py --test

  # Quick scan with 3 rounds:
  temp_finder.py --rounds 3

  # Focused scan of candidate area with cross-chip:
  temp_finder.py --reg-start 0x38 --reg-end 0x50 --cross-chips 3 --rounds 10

  # Full scan, 5 second delays, JSON output:
  temp_finder.py --rounds 10 --delay 5 --json

  # Scan specific chip:
  temp_finder.py --chip 0x08 --rounds 5
"""
    print(help_text.strip())


def parse_hex_arg(s):
    s = s.strip()
    if s.startswith("0x") or s.startswith("0X"):
        return int(s, 16)
    return int(s)


def main():
    args = sys.argv[1:]

    if "--help" in args or "-h" in args:
        print_help()
        return 0

    if "--test" in args:
        print("temp_finder.py - Self-Test Mode")
        print("=" * 50)
        passed, failed, tests = run_self_tests()
        for t in tests:
            detail = " ({})".format(t["detail"]) if t["detail"] else ""
            print("  [{}] {}{}".format(t["status"], t["name"], detail))
        print("-" * 50)
        print("Results: {} passed, {} failed out of {}".format(
            passed, failed, passed + failed))
        return 0 if failed == 0 else 1

    # Parse options
    json_mode = "--json" in args
    verbose = "--verbose" in args
    candidates_only = "--candidates-only" in args

    device = UART_DEFAULT
    chip_addr = 0x00
    num_rounds = 5
    delay = 2.0
    reg_start = 0x00
    reg_end = 0xFF
    cross_chips = 0

    i = 0
    while i < len(args):
        if args[i] == "--device" and i + 1 < len(args):
            device = args[i + 1]
            i += 2
        elif args[i] == "--chip" and i + 1 < len(args):
            chip_addr = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--rounds" and i + 1 < len(args):
            num_rounds = int(args[i + 1])
            i += 2
        elif args[i] == "--delay" and i + 1 < len(args):
            delay = float(args[i + 1])
            i += 2
        elif args[i] == "--reg-start" and i + 1 < len(args):
            reg_start = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--reg-end" and i + 1 < len(args):
            reg_end = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--cross-chips" and i + 1 < len(args):
            cross_chips = int(args[i + 1])
            i += 2
        else:
            i += 1

    # Open UART
    uart = UARTInterface(device=device)
    try:
        uart.open()
    except Exception as e:
        sys.stderr.write("ERROR: Cannot open {}: {}\n".format(device, e))
        sys.stderr.write("  (Use --test for self-tests without hardware)\n")
        return 1

    try:
        if not json_mode:
            sys.stderr.write("Temperature register discovery\n")
            sys.stderr.write("  Chip: 0x{:02X}, Registers: 0x{:02X}-0x{:02X}\n".format(
                chip_addr, reg_start, reg_end))
            sys.stderr.write("  Rounds: {}, Delay: {:.1f}s\n".format(num_rounds, delay))

        # Main scan
        scan_results = scan_for_temperature(
            uart, chip_addr, num_rounds=num_rounds, delay_between=delay,
            reg_start=reg_start, reg_end=reg_end, verbose=verbose)

        # Cross-chip comparison
        cross_results = None
        if cross_chips > 0:
            candidates = [r for r in scan_results if r["is_temp_candidate"]]
            candidate_regs = [c["register"] for c in candidates]
            if candidate_regs:
                chip_addrs = [i * ADDR_INTERVAL for i in range(cross_chips)]
                if not json_mode:
                    sys.stderr.write("Cross-chip comparison for {} candidate regs across {} chips...\n".format(
                        len(candidate_regs), cross_chips))
                cross_results = cross_chip_comparison(
                    uart, chip_addrs, candidate_regs, verbose=verbose)

        # Output
        if json_mode:
            output = {
                "chip_addr": chip_addr,
                "chip_addr_hex": "0x{:02X}".format(chip_addr),
                "rounds": num_rounds,
                "delay_seconds": delay,
                "reg_range": [reg_start, reg_end],
            }
            if candidates_only:
                output["candidates"] = [r for r in scan_results if r["is_temp_candidate"]]
            else:
                output["all_registers"] = scan_results

            if cross_results:
                output["cross_chip"] = cross_results

            print(json.dumps(output, indent=2, default=str))
        else:
            if candidates_only:
                filtered = [r for r in scan_results if r["is_temp_candidate"]]
                print(format_temp_report(filtered, cross_results))
            else:
                print(format_temp_report(scan_results, cross_results))

    finally:
        uart.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
