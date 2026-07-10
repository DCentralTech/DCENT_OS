#!/usr/bin/env python3
"""
asic_enumerator.py - BM1387 ASIC Chain Discovery & Initialization
==================================================================
Full chain enumeration for BM1387 chips on Antminer S9.
Discovers, addresses, and characterizes all chips in a hash chain.

BM1387 Protocol:
  - chain_inactive (0x55): Reset all chips to address 0
  - set_address (0x41): Assign sequential addresses (interval 4)
  - read_register (0x54): Verify chip responds at assigned address
  - 63 chips max per chain, 114 cores per chip
  - NO 0x55/0xAA preamble, 7-byte responses

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
CORES_PER_CHIP = 114

# Command opcodes (NO preamble)
CMD_READ_REGISTER = 0x54
CMD_WRITE_REGISTER = 0x51
CMD_CHAIN_INACTIVE = 0x55
CMD_SET_ADDRESS = 0x41

READ_RESPONSE_LEN = 7

# Key registers
REG_CHIP_ADDRESS = 0x00
REG_PLL = 0x0C
REG_TICKET_MASK = 0x14
REG_MISC_CTRL = 0x18
REG_BAUD_RATE = 0x28
REG_VERSION = 0x7C
REG_CHIP_STATUS = 0xFC

# PLL decode table
PLL_FREQ_MAP = {
    0x00680221: 400,
    0x00700221: 450,
    0x00680241: 500,
    0x00700241: 550,
    0x00680261: 600,
    0x00700261: 650,
    0x00680281: 700,
    0x00700281: 750,
}


# ============================================================================
# CRC5 for BM1387
# ============================================================================

def crc5_bm1387(data, bit_length=None):
    """CRC5 polynomial 0x05, init 0x1F, bit-by-bit."""
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


# ============================================================================
# BM1387 Command Builders
# ============================================================================

def build_read_register_cmd(chip_addr, reg_addr):
    """Build read register command: [0x54, chip_addr, reg_addr, CRC5]."""
    cmd_data = bytes([CMD_READ_REGISTER, chip_addr, reg_addr])
    crc = crc5_bm1387(cmd_data)
    return bytes([CMD_READ_REGISTER, chip_addr, reg_addr, crc & 0x1F])


def build_chain_inactive_cmd():
    """
    Build chain_inactive command.
    This resets all chips to address 0 and puts them in inactive state.
    Format: [0x55, 0x05, 0x00, 0x00, CRC5_byte]
    The 0x55 command with specific payload to deactivate chain.
    """
    # BM1387 chain_inactive: 5-byte command
    # [CMD=0x55, LEN=0x05, 0x00, 0x00, CRC5]
    cmd_data = bytes([CMD_CHAIN_INACTIVE, 0x05, 0x00, 0x00])
    crc = crc5_bm1387(cmd_data)
    return cmd_data + bytes([crc & 0x1F])


def build_set_address_cmd(chip_addr):
    """
    Build set_address command to assign an address to the next unaddressed chip.
    Format: [0x41, chip_addr, CRC5_byte]
    Each call addresses one chip; chips respond in chain order.
    """
    cmd_data = bytes([CMD_SET_ADDRESS, chip_addr])
    crc = crc5_bm1387(cmd_data)
    return bytes([CMD_SET_ADDRESS, chip_addr, crc & 0x1F])


def build_write_register_cmd(chip_addr, reg_addr, value):
    """
    Build write register command.
    Format: [0x51, chip_addr, reg_addr, V3, V2, V1, V0, CRC5]
    """
    v3 = (value >> 24) & 0xFF
    v2 = (value >> 16) & 0xFF
    v1 = (value >> 8) & 0xFF
    v0 = value & 0xFF
    cmd_data = bytes([CMD_WRITE_REGISTER, chip_addr, reg_addr, v3, v2, v1, v0])
    crc = crc5_bm1387(cmd_data)
    return cmd_data + bytes([crc & 0x1F])


def parse_read_response(response):
    """Parse 7-byte response: [reg, D3, D2, D1, D0, CRC_hi, CRC_lo]."""
    if len(response) < READ_RESPONSE_LEN:
        return None
    reg_addr = response[0]
    value = (response[1] << 24) | (response[2] << 16) | (response[3] << 8) | response[4]
    resp_crc = response[5] & 0x1F
    calc_crc = crc5_bm1387(response[:5])
    crc_ok = (calc_crc == resp_crc)
    return (reg_addr, value, crc_ok)


def decode_pll_freq(pll_value):
    """Decode PLL register to frequency in MHz."""
    freq = PLL_FREQ_MAP.get(pll_value)
    if freq:
        return freq
    # Try formula decode
    fbdiv = (pll_value >> 16) & 0xFF
    refdiv = (pll_value >> 8) & 0x3F
    postdiv1 = (pll_value >> 4) & 0x07
    postdiv2 = pll_value & 0x07
    if refdiv > 0 and postdiv1 > 0 and postdiv2 > 0:
        return (25.0 * fbdiv) / (refdiv * postdiv1 * postdiv2)
    return 0


def decode_version(version_value):
    """Decode version register to chip model string."""
    low16 = version_value & 0xFFFF
    if low16 == 0x1387:
        return "BM1387"
    elif low16 == 0x1366:
        return "BM1366"
    elif low16 == 0x1397:
        return "BM1397"
    else:
        return "Unknown(0x{:04X})".format(low16)


# ============================================================================
# UART Hardware Interface
# ============================================================================

class UARTInterface:
    """Direct UART access to BM1387 chain."""

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
        elif self.fobj:
            try:
                while True:
                    chunk = self.fobj.read(256)
                    if not chunk:
                        break
            except Exception:
                pass

    def send_command(self, cmd, response_len=0, timeout=None):
        """Send command and optionally read response."""
        self.flush_input()
        self.write(cmd)
        if response_len > 0:
            time.sleep(0.01)
            return self.read(response_len, timeout=timeout or self.timeout)
        return b""

    def read_register(self, chip_addr, reg_addr, retries=2):
        """Read a register. Returns (reg, value, crc_ok) or None."""
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
# Mock UART for Testing
# ============================================================================

class MockUART:
    """Simulated BM1387 chain for --test mode."""

    def __init__(self, chip_count=3):
        self.device = "/dev/mock_uart"
        self.chip_count = chip_count
        self._write_log = []
        self._chips_addressed = False
        self._next_address_index = 0
        self.chip_registers = {}
        self._setup_chips()

    def _setup_chips(self):
        """Initialize simulated chip state."""
        for idx in range(self.chip_count):
            addr = idx * ADDR_INTERVAL
            self.chip_registers[(addr, REG_CHIP_ADDRESS)] = addr
            self.chip_registers[(addr, REG_PLL)] = 0x00680261  # 600 MHz
            self.chip_registers[(addr, REG_TICKET_MASK)] = 0x0000FFFF
            self.chip_registers[(addr, REG_MISC_CTRL)] = 0x00000040
            self.chip_registers[(addr, REG_BAUD_RATE)] = 0x00000019
            self.chip_registers[(addr, REG_VERSION)] = 0x13871387
            self.chip_registers[(addr, REG_CHIP_STATUS)] = 0x00000001

    def open(self):
        pass

    def close(self):
        pass

    def write(self, data):
        self._write_log.append(data)

    def read(self, length, timeout=None):
        return b""

    def flush_input(self):
        pass

    def send_command(self, cmd, response_len=0, timeout=None):
        self._write_log.append(cmd)
        if cmd[0] == CMD_CHAIN_INACTIVE:
            self._chips_addressed = False
            self._next_address_index = 0
        elif cmd[0] == CMD_SET_ADDRESS and len(cmd) >= 3:
            assigned_addr = cmd[1]
            if self._next_address_index < self.chip_count:
                self._next_address_index += 1
        return b""

    def read_register(self, chip_addr, reg_addr, retries=2):
        """Simulate register read."""
        value = self.chip_registers.get((chip_addr, reg_addr))
        if value is None:
            # Only respond if chip exists
            chip_idx = chip_addr // ADDR_INTERVAL
            if chip_idx < self.chip_count:
                value = 0x00000000
            else:
                return None
        return (reg_addr, value, True)


# ============================================================================
# Enumeration Logic
# ============================================================================

def enumerate_chain_active(uart, max_chips=MAX_CHIPS, verbose=False):
    """
    Full active enumeration:
    1. Send chain_inactive to reset all addresses
    2. Assign sequential addresses (0, 4, 8, ...)
    3. Verify each chip responds
    4. Read PLL and version registers
    Returns list of chip info dicts.
    """
    chips = []

    # Step 1: Chain inactive
    if verbose:
        sys.stderr.write("Step 1: Sending chain_inactive...\n")
    cmd_inactive = build_chain_inactive_cmd()
    uart.send_command(cmd_inactive)
    time.sleep(0.1)

    # Send it 3 times for reliability
    uart.send_command(cmd_inactive)
    time.sleep(0.05)
    uart.send_command(cmd_inactive)
    time.sleep(0.1)

    if verbose:
        sys.stderr.write("Step 2: Assigning addresses...\n")

    # Step 2: Set addresses sequentially
    for chip_idx in range(max_chips):
        chip_addr = chip_idx * ADDR_INTERVAL

        cmd_addr = build_set_address_cmd(chip_addr)
        uart.send_command(cmd_addr)
        time.sleep(0.02)

        # Step 3: Verify by reading ChipAddress register
        result = uart.read_register(chip_addr, REG_CHIP_ADDRESS)
        if result is None:
            if verbose:
                sys.stderr.write("  Chip #{}: no response at addr 0x{:02X} - end of chain\n".format(
                    chip_idx, chip_addr))
            break

        reg_returned, value, crc_ok = result
        if not crc_ok:
            if verbose:
                sys.stderr.write("  Chip #{}: CRC error at addr 0x{:02X}\n".format(
                    chip_idx, chip_addr))
            # Still count it but flag the error
            pass

        chip_info = {
            "index": chip_idx,
            "address": chip_addr,
            "address_hex": "0x{:02X}".format(chip_addr),
            "chip_addr_reg": value,
            "address_verified": (value & 0xFF) == chip_addr if crc_ok else False,
            "crc_ok": crc_ok,
            "pll_raw": None,
            "pll_freq_mhz": None,
            "version_raw": None,
            "version_str": None,
            "status": None,
        }

        # Step 4: Read PLL
        pll_result = uart.read_register(chip_addr, REG_PLL)
        if pll_result:
            _, pll_val, pll_crc = pll_result
            chip_info["pll_raw"] = pll_val
            chip_info["pll_raw_hex"] = "0x{:08X}".format(pll_val)
            chip_info["pll_freq_mhz"] = decode_pll_freq(pll_val)

        # Step 5: Read version
        ver_result = uart.read_register(chip_addr, REG_VERSION)
        if ver_result:
            _, ver_val, ver_crc = ver_result
            chip_info["version_raw"] = ver_val
            chip_info["version_raw_hex"] = "0x{:08X}".format(ver_val)
            chip_info["version_str"] = decode_version(ver_val)

        # Read status
        stat_result = uart.read_register(chip_addr, REG_CHIP_STATUS)
        if stat_result:
            _, stat_val, _ = stat_result
            chip_info["status"] = stat_val
            chip_info["status_hex"] = "0x{:08X}".format(stat_val)

        chips.append(chip_info)

        if verbose:
            sys.stderr.write("  Chip #{}: addr=0x{:02X} verified={} pll={} version={}\n".format(
                chip_idx, chip_addr, chip_info["address_verified"],
                chip_info.get("pll_freq_mhz", "?"),
                chip_info.get("version_str", "?")))

    return chips


def enumerate_chain_passive(uart, max_chips=MAX_CHIPS, verbose=False):
    """
    Passive scan: Don't reset addresses, just probe existing chain.
    Read register 0x00 at each possible address to find responding chips.
    """
    chips = []

    if verbose:
        sys.stderr.write("Passive scan: probing addresses 0x00 to 0x{:02X}...\n".format(
            (max_chips - 1) * ADDR_INTERVAL))

    for chip_idx in range(max_chips):
        chip_addr = chip_idx * ADDR_INTERVAL

        result = uart.read_register(chip_addr, REG_CHIP_ADDRESS)
        if result is None:
            continue

        reg_returned, value, crc_ok = result

        chip_info = {
            "index": chip_idx,
            "address": chip_addr,
            "address_hex": "0x{:02X}".format(chip_addr),
            "chip_addr_reg": value,
            "crc_ok": crc_ok,
        }

        # Read PLL
        pll_result = uart.read_register(chip_addr, REG_PLL)
        if pll_result:
            _, pll_val, _ = pll_result
            chip_info["pll_raw"] = pll_val
            chip_info["pll_raw_hex"] = "0x{:08X}".format(pll_val)
            chip_info["pll_freq_mhz"] = decode_pll_freq(pll_val)

        # Read version
        ver_result = uart.read_register(chip_addr, REG_VERSION)
        if ver_result:
            _, ver_val, _ = ver_result
            chip_info["version_raw"] = ver_val
            chip_info["version_raw_hex"] = "0x{:08X}".format(ver_val)
            chip_info["version_str"] = decode_version(ver_val)

        chips.append(chip_info)

        if verbose:
            sys.stderr.write("  Found chip at addr=0x{:02X}, version={}\n".format(
                chip_addr, chip_info.get("version_str", "?")))

    return chips


def format_chain_report(chips, scan_type="active"):
    """Format chain enumeration results as text table."""
    lines = []
    lines.append("BM1387 Chain Enumeration Report ({} scan)".format(scan_type))
    lines.append("=" * 75)
    lines.append("Chips found: {}".format(len(chips)))
    lines.append("Total cores: {} (@ {} per chip)".format(
        len(chips) * CORES_PER_CHIP, CORES_PER_CHIP))
    lines.append("")

    if not chips:
        lines.append("No chips detected.")
        return "\n".join(lines)

    # Table header
    lines.append("{:<6} {:<8} {:<10} {:<10} {:<12} {:<10} {}".format(
        "#", "ADDR", "VERIFIED", "PLL(MHz)", "VERSION", "STATUS", "NOTES"))
    lines.append("-" * 75)

    freq_counts = collections.Counter()
    version_counts = collections.Counter()
    crc_errors = 0

    for chip in chips:
        idx = chip["index"]
        addr = chip["address_hex"]
        verified = "Yes" if chip.get("address_verified") else ("N/A" if "address_verified" not in chip else "No")
        pll = "{:.0f}".format(chip["pll_freq_mhz"]) if chip.get("pll_freq_mhz") else "?"
        version = chip.get("version_str", "?")
        status = chip.get("status_hex", "?")
        notes = ""

        if not chip.get("crc_ok"):
            notes += "CRC_ERR "
            crc_errors += 1
        if chip.get("address_verified") is False:
            notes += "ADDR_MISMATCH "

        if chip.get("pll_freq_mhz"):
            freq_counts[chip["pll_freq_mhz"]] += 1
        if chip.get("version_str"):
            version_counts[chip["version_str"]] += 1

        lines.append("{:<6} {:<8} {:<10} {:<10} {:<12} {:<10} {}".format(
            idx, addr, verified, pll, version, status, notes.strip()))

    lines.append("-" * 75)

    # Summary
    lines.append("")
    lines.append("--- Chain Summary ---")
    lines.append("Total chips: {}".format(len(chips)))

    if freq_counts:
        freq_summary = ", ".join("{:.0f} MHz x{}".format(f, c) for f, c in sorted(freq_counts.items()))
        lines.append("PLL frequencies: {}".format(freq_summary))

    if version_counts:
        ver_summary = ", ".join("{} x{}".format(v, c) for v, c in sorted(version_counts.items()))
        lines.append("Chip versions: {}".format(ver_summary))

    if crc_errors:
        lines.append("CRC errors: {} (communication issues)".format(crc_errors))

    # Hashrate estimate
    total_ghps = 0
    for chip in chips:
        if chip.get("pll_freq_mhz"):
            # BM1387: each core does ~1 hash per clock cycle per pipeline stage
            # Rough estimate: cores * freq_mhz = MH/s per chip
            ghps_chip = (CORES_PER_CHIP * chip["pll_freq_mhz"]) / 1000.0
            total_ghps += ghps_chip

    if total_ghps > 0:
        lines.append("Estimated hashrate: {:.1f} GH/s ({:.2f} TH/s)".format(
            total_ghps, total_ghps / 1000.0))

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

    # --- Test 1: CRC5 determinism ---
    c1 = crc5_bm1387(bytes([0x54, 0x00, 0x00]))
    c2 = crc5_bm1387(bytes([0x54, 0x00, 0x00]))
    test("CRC5: deterministic", c1 == c2, "c1=0x{:02X} c2=0x{:02X}".format(c1, c2))

    # --- Test 2: CRC5 range ---
    all_ok = all(0 <= crc5_bm1387(bytes([0x54, i, 0])) <= 0x1F for i in range(256))
    test("CRC5: all outputs in 5-bit range", all_ok)

    # --- Test 3: Build chain_inactive command ---
    cmd_ci = build_chain_inactive_cmd()
    test("chain_inactive: starts with 0x55",
         cmd_ci[0] == CMD_CHAIN_INACTIVE,
         "cmd={}".format(cmd_ci.hex()))

    # --- Test 4: chain_inactive command length ---
    test("chain_inactive: 5 bytes",
         len(cmd_ci) == 5,
         "len={}".format(len(cmd_ci)))

    # --- Test 5: Build set_address command ---
    cmd_sa = build_set_address_cmd(0x08)
    test("set_address: starts with 0x41",
         cmd_sa[0] == CMD_SET_ADDRESS,
         "cmd={}".format(cmd_sa.hex()))

    # --- Test 6: set_address contains chip addr ---
    test("set_address: chip addr in byte 1",
         cmd_sa[1] == 0x08,
         "byte1=0x{:02X}".format(cmd_sa[1]))

    # --- Test 7: set_address command length ---
    test("set_address: 3 bytes",
         len(cmd_sa) == 3,
         "len={}".format(len(cmd_sa)))

    # --- Test 8: Build read register command ---
    cmd_rr = build_read_register_cmd(0x04, 0x0C)
    test("read_register: 4 bytes, opcode 0x54",
         len(cmd_rr) == 4 and cmd_rr[0] == 0x54,
         "cmd={}".format(cmd_rr.hex()))

    # --- Test 9: Build write register command ---
    cmd_wr = build_write_register_cmd(0x00, 0x14, 0x0000FFFF)
    test("write_register: 8 bytes, opcode 0x51",
         len(cmd_wr) == 8 and cmd_wr[0] == 0x51,
         "cmd={}".format(cmd_wr.hex()))

    # --- Test 10: Write register value encoding ---
    # Value 0xAABBCCDD should be in bytes 3-6
    cmd_wr2 = build_write_register_cmd(0x00, 0x14, 0xAABBCCDD)
    test("write_register: value big-endian",
         cmd_wr2[3] == 0xAA and cmd_wr2[4] == 0xBB and cmd_wr2[5] == 0xCC and cmd_wr2[6] == 0xDD,
         "bytes={:02X}{:02X}{:02X}{:02X}".format(cmd_wr2[3], cmd_wr2[4], cmd_wr2[5], cmd_wr2[6]))

    # --- Test 11: Parse response ---
    val = 0x00680261
    resp_data = bytes([0x0C, (val >> 24) & 0xFF, (val >> 16) & 0xFF,
                       (val >> 8) & 0xFF, val & 0xFF])
    resp_crc = crc5_bm1387(resp_data)
    full_resp = resp_data + bytes([resp_crc, 0x00])
    parsed = parse_read_response(full_resp)
    test("parse_response: valid PLL response",
         parsed is not None and parsed[0] == 0x0C and parsed[1] == val,
         "parsed={}".format(parsed))

    # --- Test 12: PLL decode ---
    freq = decode_pll_freq(0x00680261)
    test("PLL decode: 0x00680261 -> 600 MHz",
         freq == 600,
         "freq={}".format(freq))

    # --- Test 13: PLL decode formula ---
    freq2 = decode_pll_freq(0x00500141)
    test("PLL decode: formula fallback works",
         isinstance(freq2, (int, float)),
         "freq={}".format(freq2))

    # --- Test 14: Version decode BM1387 ---
    ver = decode_version(0x13871387)
    test("Version decode: 0x13871387 -> BM1387",
         ver == "BM1387",
         "ver={}".format(ver))

    # --- Test 15: Version decode unknown ---
    ver2 = decode_version(0x12341234)
    test("Version decode: unknown -> labeled unknown",
         "Unknown" in ver2,
         "ver={}".format(ver2))

    # --- Test 16: MockUART enumeration ---
    mock = MockUART(chip_count=3)
    chips = enumerate_chain_active(mock, max_chips=5, verbose=False)
    test("MockUART active enum: finds 3 chips",
         len(chips) == 3,
         "found={}".format(len(chips)))

    # --- Test 17: MockUART chip addresses ---
    addrs = [c["address"] for c in chips]
    test("MockUART: addresses are 0,4,8",
         addrs == [0, 4, 8],
         "addrs={}".format(addrs))

    # --- Test 18: MockUART PLL values ---
    plls = [c.get("pll_freq_mhz") for c in chips]
    test("MockUART: all PLLs are 600 MHz",
         all(p == 600 for p in plls),
         "plls={}".format(plls))

    # --- Test 19: MockUART versions ---
    vers = [c.get("version_str") for c in chips]
    test("MockUART: all versions are BM1387",
         all(v == "BM1387" for v in vers),
         "vers={}".format(vers))

    # --- Test 20: Passive scan ---
    mock2 = MockUART(chip_count=5)
    chips2 = enumerate_chain_passive(mock2, max_chips=8, verbose=False)
    test("MockUART passive scan: finds 5 chips",
         len(chips2) == 5,
         "found={}".format(len(chips2)))

    # --- Test 21: Format report ---
    report = format_chain_report(chips, "active")
    test("Format report: contains chip count",
         "3" in report and "BM1387" in report,
         "report_lines={}".format(len(report.split("\n"))))

    # --- Test 22: Format report empty chain ---
    report_empty = format_chain_report([], "active")
    test("Format report: handles empty chain",
         "No chips" in report_empty or "0" in report_empty)

    # --- Test 23: JSON serialization ---
    json_str = json.dumps(chips)
    json_back = json.loads(json_str)
    test("JSON: chip list serializes/deserializes",
         isinstance(json_back, list) and len(json_back) == 3)

    # --- Test 24: Address interval correctness ---
    all_correct = True
    for i in range(MAX_CHIPS):
        expected = i * ADDR_INTERVAL
        cmd = build_set_address_cmd(expected & 0xFF)
        if cmd[1] != (expected & 0xFF):
            all_correct = False
            break
    test("Address interval: all 63 addresses valid",
         all_correct)

    # --- Test 25: Hashrate estimate in report ---
    report3 = format_chain_report(chips, "active")
    test("Report: contains hashrate estimate",
         "hashrate" in report3.lower() or "GH/s" in report3,
         "has_hashrate={}".format("GH/s" in report3))

    return passed, failed, tests


# ============================================================================
# CLI
# ============================================================================

def print_help():
    help_text = """
asic_enumerator.py - BM1387 ASIC Chain Discovery & Initialization
===================================================================

Usage:
  asic_enumerator.py [OPTIONS]

Options:
  --help              Show this help message
  --test              Run self-tests (no hardware required)
  --json              Output results as JSON
  --device PATH       UART device (default: /dev/ttyPS1)
  --max-chips N       Maximum chips to scan (default: 63)
  --passive           Passive scan only (don't reset chain addresses)
  --active            Active enumeration (reset + re-address, DEFAULT)
  --verbose           Verbose output during enumeration

Modes:
  Active (default):   Reset chain, assign addresses, verify all chips.
                      WARNING: This will re-address the chain!
  Passive:            Just probe existing addresses without changes.
                      Safe to run while miner is hashing.

Examples:
  # Self-test (no hardware):
  asic_enumerator.py --test

  # Active enumeration (re-addresses chain):
  asic_enumerator.py --active

  # Passive scan of existing chain:
  asic_enumerator.py --passive

  # JSON output, max 10 chips:
  asic_enumerator.py --passive --max-chips 10 --json

  # Verbose active enumeration:
  asic_enumerator.py --active --verbose
"""
    print(help_text.strip())


def main():
    args = sys.argv[1:]

    if "--help" in args or "-h" in args:
        print_help()
        return 0

    if "--test" in args:
        print("asic_enumerator.py - Self-Test Mode")
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
    passive = "--passive" in args
    device = UART_DEFAULT
    max_chips = MAX_CHIPS

    i = 0
    while i < len(args):
        if args[i] == "--device" and i + 1 < len(args):
            device = args[i + 1]
            i += 2
        elif args[i] == "--max-chips" and i + 1 < len(args):
            max_chips = int(args[i + 1])
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
        if passive:
            if not json_mode:
                sys.stderr.write("Passive scan (not modifying chain)...\n")
            chips = enumerate_chain_passive(uart, max_chips=max_chips, verbose=verbose)
        else:
            if not json_mode:
                sys.stderr.write("Active enumeration (will re-address chain!)...\n")
            chips = enumerate_chain_active(uart, max_chips=max_chips, verbose=verbose)

        scan_type = "passive" if passive else "active"

        if json_mode:
            output = {
                "scan_type": scan_type,
                "device": device,
                "chip_count": len(chips),
                "total_cores": len(chips) * CORES_PER_CHIP,
                "max_chips_scanned": max_chips,
                "chips": chips,
            }
            # Add hashrate estimate
            total_ghps = 0
            for chip in chips:
                if chip.get("pll_freq_mhz"):
                    total_ghps += (CORES_PER_CHIP * chip["pll_freq_mhz"]) / 1000.0
            output["estimated_hashrate_ghs"] = round(total_ghps, 1)
            output["estimated_hashrate_ths"] = round(total_ghps / 1000.0, 3)
            print(json.dumps(output, indent=2))
        else:
            print(format_chain_report(chips, scan_type))

    finally:
        uart.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
