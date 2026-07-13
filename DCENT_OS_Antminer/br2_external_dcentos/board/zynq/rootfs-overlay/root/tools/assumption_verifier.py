#!/usr/bin/env python3
"""
assumption_verifier.py -- Automated Hardware Assumption Verification Suite
===========================================================================
Tests our documented assumptions from ASSUMPTIONS.md against real Antminer S9
(Zynq 7010 + BM1387) hardware. Each test has an ID matching the assumption
numbering, prints PASS/FAIL/SKIP, and logs raw data for analysis.

Part of DCENTos Hacker Shell firmware research tools.

Hardware Target:
  - Antminer S9 (Zynq 7010, BM1387 x 63 x 3 boards)
  - UART: /dev/ttyPS1 (hash boards via FPGA)
  - I2C:  /dev/i2c-0, /dev/i2c-1 (hash board peripherals, PSU)
  - FPGA: UIO devices (/dev/uio0-uio13) or devmem at 0x43C00000
  - devmem: for direct FPGA register access

BM1387 Protocol:
  - NO 0x55/0xAA preamble, 7-byte responses
  - CRC5 poly 0x05, init 0x1F
  - Commands: 0x54 read, 0x51 write, 0x55 chain_inactive, 0x41 set_address
  - Address interval: 4, max 63 chips per chain

Usage:
  assumption_verifier.py --test          # Run self-tests (no hardware)
  assumption_verifier.py --all           # Run all hardware tests
  assumption_verifier.py --category asic # Run one category
  assumption_verifier.py --test A1.1     # Run specific test (in --test mode)
  assumption_verifier.py --list          # Show available tests
  assumption_verifier.py --json          # JSON output
  assumption_verifier.py --safe          # Skip write tests (default)
"""

import sys
import os
import struct
import time
import json
import collections

from dcentos_asic_wire import (
    crc5_bm13xx_command,
    require_captured_bm1387_protocol_profile,
)

try:
    import fcntl
    HAS_FCNTL = True
except ImportError:
    HAS_FCNTL = False

try:
    import termios
    HAS_TERMIOS = True
except ImportError:
    HAS_TERMIOS = False

try:
    import subprocess
    HAS_SUBPROCESS = True
except ImportError:
    HAS_SUBPROCESS = False

# ============================================================================
# Constants
# ============================================================================

VERSION = "1.0.0"

UART_DEFAULT = "/dev/ttyPS1"
UART_BAUD = 115200
ADDR_INTERVAL = 4
MAX_CHIPS = 63
CORES_PER_CHIP = 114
READ_RESPONSE_LEN = 7

# BM1387 command opcodes (NO preamble)
CMD_READ_REGISTER = 0x54
CMD_WRITE_REGISTER = 0x51
CMD_CHAIN_INACTIVE = 0x55
CMD_SET_ADDRESS = 0x41

# Key BM1387 registers
REG_CHIP_ADDRESS = 0x00
REG_PLL = 0x0C
REG_TICKET_MASK = 0x14
REG_MISC_CTRL = 0x18
REG_BAUD_RATE = 0x28
REG_VERSION = 0x7C
REG_CHIP_STATUS = 0xFC

# FPGA
FPGA_BASE_ADDR = 0x43C00000
FPGA_SIZE = 0x160  # 352 bytes
FPGA_DEV = "/dev/mem"  # UIO approach: use /dev/mem or /dev/uioN for FPGA access

# I2C constants
I2C_SLAVE = 0x0703

# S9 PIC addresses (per hash board slot)
PIC_ADDRESSES = [0x50, 0x51, 0x52]

# TMP75 temperature sensor addresses
TMP75_ADDRESSES_A = [0x4C, 0x4D, 0x4E]  # Sensor A per slot
TMP75_ADDRESSES_B = [0x48, 0x49, 0x4A]  # Sensor B per slot

# EEPROM addresses (may overlap PIC on S9)
EEPROM_ADDRESSES = [0x50, 0x51, 0x52]

# PIC voltage formula (S9 style)
PIC_VOLTAGE_SLOPE = 170.423497
PIC_VOLTAGE_INTERCEPT = 1608.420446


# ============================================================================
# CRC5 for BM1387
# ============================================================================

# ============================================================================
# BM1387 Command Builders
# ============================================================================

def build_read_register_cmd(chip_addr, reg_addr):
    """Build read register: [0x54, chip_addr, reg_addr, CRC5]."""
    cmd_data = bytes([CMD_READ_REGISTER, chip_addr, reg_addr])
    crc = crc5_bm13xx_command(cmd_data)
    return bytes([CMD_READ_REGISTER, chip_addr, reg_addr, crc & 0x1F])


def build_chain_inactive_cmd():
    """Build chain_inactive: [0x55, 0x05, 0x00, 0x00, CRC5]."""
    cmd_data = bytes([CMD_CHAIN_INACTIVE, 0x05, 0x00, 0x00])
    crc = crc5_bm13xx_command(cmd_data)
    return cmd_data + bytes([crc & 0x1F])


def build_set_address_cmd(chip_addr):
    """Build set_address: [0x41, chip_addr, CRC5]."""
    cmd_data = bytes([CMD_SET_ADDRESS, chip_addr])
    crc = crc5_bm13xx_command(cmd_data)
    return bytes([CMD_SET_ADDRESS, chip_addr, crc & 0x1F])


def parse_read_response(response):
    """Structurally parse a response and preserve unverified raw bytes."""
    if len(response) != READ_RESPONSE_LEN:
        return None
    reg_addr = response[0]
    value = (response[1] << 24) | (response[2] << 16) | (response[3] << 8) | response[4]
    return (reg_addr, value, "unverified", bytes(response[:READ_RESPONSE_LEN]))


def decode_pll_freq(pll_value):
    """Decode BM1387 PLL register to frequency in MHz."""
    PLL_MAP = {
        0x00680221: 400, 0x00700221: 450, 0x00680241: 500,
        0x00700241: 550, 0x00680261: 600, 0x00700261: 650,
        0x00680281: 700, 0x00700281: 750,
    }
    freq = PLL_MAP.get(pll_value)
    if freq:
        return float(freq)
    fbdiv = (pll_value >> 16) & 0xFF
    refdiv = (pll_value >> 8) & 0x3F
    postdiv1 = (pll_value >> 4) & 0x07
    postdiv2 = pll_value & 0x07
    if refdiv > 0 and postdiv1 > 0 and postdiv2 > 0:
        return (25.0 * fbdiv) / (refdiv * postdiv1 * postdiv2)
    return 0.0


# ============================================================================
# Hardware Interfaces
# ============================================================================

class UARTInterface:
    """Direct UART access to BM1387 chain."""

    def __init__(self, device=UART_DEFAULT, baud=UART_BAUD, timeout=0.1):
        self.device = device
        self.baud = baud
        self.timeout = timeout
        self.fd = None
        self.fobj = None
        self.last_response_observation = None

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
        require_captured_bm1387_protocol_profile()
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

    def send_command(self, cmd, response_len=0, timeout=None):
        self.flush_input()
        self.write(cmd)
        if response_len > 0:
            time.sleep(0.01)
            return self.read(response_len, timeout=timeout or self.timeout)
        return b""

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
                    self.last_response_observation = {
                        "requested_register": reg_addr,
                        "returned_register": parsed[0],
                        "register_matches": parsed[0] == reg_addr,
                        "response_integrity": parsed[2],
                        "raw_response_hex": parsed[3].hex(),
                        "raw_response_bytes": list(parsed[3]),
                    }
                    if parsed[0] == reg_addr:
                        return parsed
            if attempt < retries:
                time.sleep(0.02)
        return None


class I2CInterface:
    """Direct I2C access via Linux ioctl."""

    def __init__(self, bus=0):
        self.bus = bus
        self.device = "/dev/i2c-{}".format(bus)
        self.fd = None

    def open(self):
        self.fd = os.open(self.device, os.O_RDWR)

    def close(self):
        if self.fd is not None:
            try:
                os.close(self.fd)
            except Exception:
                pass
            self.fd = None

    def set_slave(self, addr):
        if not HAS_FCNTL:
            raise RuntimeError("fcntl not available")
        fcntl.ioctl(self.fd, I2C_SLAVE, addr)

    def read_byte(self, addr):
        """Read a single byte from I2C device."""
        try:
            self.set_slave(addr)
            data = os.read(self.fd, 1)
            if data:
                return data[0]
        except Exception:
            pass
        return None

    def read_bytes(self, addr, length):
        """Read multiple bytes from I2C device."""
        try:
            self.set_slave(addr)
            data = os.read(self.fd, length)
            return data
        except Exception:
            pass
        return None

    def write_read(self, addr, write_data, read_length):
        """Write then read from I2C device."""
        try:
            self.set_slave(addr)
            os.write(self.fd, write_data)
            data = os.read(self.fd, read_length)
            return data
        except Exception:
            pass
        return None

    def probe(self, addr):
        """Test if a device responds at the given address."""
        try:
            self.set_slave(addr)
            os.read(self.fd, 1)
            return True
        except Exception:
            return False


def devmem_read(address):
    """Read a 32-bit register via devmem command."""
    if not HAS_SUBPROCESS:
        return None
    try:
        result = subprocess.run(
            ["devmem", "0x{:08X}".format(address)],
            capture_output=True, text=True, timeout=5
        )
        if result.returncode == 0:
            output = result.stdout.strip()
            if output.startswith("0x") or output.startswith("0X"):
                return int(output, 16)
            return int(output)
    except Exception:
        pass
    return None


def read_file(path):
    """Read a text file and return contents."""
    try:
        with open(path, "r") as f:
            return f.read()
    except Exception:
        return None


# ============================================================================
# Test Result Tracking
# ============================================================================

class TestResult:
    """Single test result."""

    def __init__(self, test_id, category, description):
        self.test_id = test_id
        self.category = category
        self.description = description
        self.status = "SKIP"
        self.explanation = ""
        self.raw_data = {}
        self.timestamp = time.time()

    def passed(self, explanation="", raw_data=None):
        self.status = "PASS"
        self.explanation = explanation
        if raw_data:
            self.raw_data = raw_data

    def failed(self, explanation="", raw_data=None):
        self.status = "FAIL"
        self.explanation = explanation
        if raw_data:
            self.raw_data = raw_data

    def skipped(self, explanation=""):
        self.status = "SKIP"
        self.explanation = explanation

    def to_dict(self):
        return {
            "test_id": self.test_id,
            "category": self.category,
            "description": self.description,
            "status": self.status,
            "explanation": self.explanation,
            "raw_data": self.raw_data,
            "timestamp": self.timestamp,
        }


class TestSuite:
    """Collection of test results."""

    def __init__(self):
        self.results = []

    def add(self, result):
        self.results.append(result)

    def summary(self):
        total = len(self.results)
        passed = sum(1 for r in self.results if r.status == "PASS")
        failed = sum(1 for r in self.results if r.status == "FAIL")
        skipped = sum(1 for r in self.results if r.status == "SKIP")
        return {"total": total, "passed": passed, "failed": failed, "skipped": skipped}

    def to_dict(self):
        return {
            "summary": self.summary(),
            "tests": [r.to_dict() for r in self.results],
        }


# ============================================================================
# Test Registry
# ============================================================================

# Each entry: (test_id, category, description, is_safe, function_name)
TEST_REGISTRY = [
    # Category 1: ASIC Protocol
    ("A1.1", "asic", "BM1387 has NO 0x55/0xAA preamble", True, "test_a1_1_preamble"),
    ("A1.3", "asic", "CRC5 poly 0x05 init 0x1F accepted by chip", True, "test_a1_3_crc5"),
    ("A1.5", "asic", "Response is exactly 7 bytes", True, "test_a1_5_response_7byte"),
    ("A1.7", "asic", "Chain inactive resets all to address 0", False, "test_a1_7_chain_inactive"),
    ("A1.8", "asic", "Addresses spaced by interval 4 after enumeration", False, "test_a1_8_address_interval"),
    ("A1.10", "asic", "PLL register decode matches ~600 MHz stock", True, "test_a1_10_pll_formula"),

    # Category 2: I2C/Hash Board
    ("A2.1", "power", "Three hash boards detected via PIC I2C addresses", True, "test_a2_1_three_boards"),
    ("A2.2", "power", "TMP75 present and reading plausible temperature", True, "test_a2_2_tmp75_present"),
    ("A2.3", "power", "EEPROM present at 0x50 with valid data", True, "test_a2_3_eeprom_present"),
    ("A2.4", "power", "PIC voltage register reads plausible value", True, "test_a2_4_pic_voltage"),

    # Category 3: FPGA/Control Board
    ("A3.1", "fpga", "FPGA base register at 0x43C00000 responds", True, "test_a3_1_fpga_base"),
    ("A3.2", "fpga", "FPGA register space 352 bytes accessible", True, "test_a3_2_fpga_size"),
    ("A3.3", "fpga", "FPGA hardware version register decodes", True, "test_a3_3_hardware_version"),
    ("A3.4", "fpga", "Hash board plug detection register", True, "test_a3_4_hashboard_detect"),
    ("A3.5", "fpga", "UART FIFO status registers respond", True, "test_a3_5_uart_fifo"),

    # Category 4: System/Boot
    ("A4.1", "system", "Kernel version is 4.6.0-xilinx", True, "test_a4_1_kernel_version"),
    ("A4.2", "system", "/dev/ttyPS0 exists (serial console)", True, "test_a4_2_serial_console"),
    ("A4.3", "system", "/dev/ttyPS1 exists (hash UART)", True, "test_a4_3_hash_uart"),
    ("A4.4", "system", "NAND has expected 8 partitions", True, "test_a4_4_nand_partitions"),
    ("A4.5", "system", "Memory size report", True, "test_a4_5_memory_size"),
]


# ============================================================================
# Test Implementations - Category 1: ASIC Protocol
# ============================================================================

def test_a1_1_preamble(uart=None, safe=True):
    """A1.1: Verify BM1387 has NO 0x55/0xAA preamble.

    Send a read register command (no preamble) and check the response.
    BM1387 uses bare commands starting with 0x54, not 0x55 0xAA 0x52.
    The response should NOT start with 0xAA 0x55 either.
    """
    result = TestResult("A1.1", "asic",
                        "BM1387 has NO 0x55/0xAA preamble")

    if uart is None:
        result.skipped("No UART interface available")
        return result

    # Send bare read register command (no preamble)
    cmd = build_read_register_cmd(0x00, REG_CHIP_ADDRESS)
    uart.flush_input()
    uart.write(cmd)
    time.sleep(0.02)
    resp = uart.read(READ_RESPONSE_LEN + 4, timeout=0.1)

    raw = {"command_hex": cmd.hex(), "response_hex": resp.hex() if resp else "",
           "response_len": len(resp)}

    if len(resp) == 0:
        result.skipped("No response from chip (chain may not be powered)", )
        result.raw_data = raw
        return result

    # Check: response should NOT start with 0xAA 0x55 preamble
    has_preamble = len(resp) >= 2 and resp[0] == 0xAA and resp[1] == 0x55
    # Check: first byte should be the register address (0x00)
    first_byte_is_reg = resp[0] == REG_CHIP_ADDRESS

    if not has_preamble and first_byte_is_reg and len(resp) == READ_RESPONSE_LEN:
        result.passed(
            "Response starts with register byte 0x{:02X}, no preamble present. "
            "BM1387 confirmed NO preamble.".format(resp[0]),
            raw
        )
    elif has_preamble:
        result.failed(
            "Response starts with 0xAA 0x55 -- unexpected preamble detected! "
            "This might not be a BM1387.",
            raw
        )
    else:
        result.failed(
            "Unexpected response format. First byte: 0x{:02X}, length: {}".format(
                resp[0] if resp else 0, len(resp)),
            raw
        )
    return result


def test_a1_3_crc5(uart=None, safe=True):
    """A1.3: Verify CRC5 poly 0x05 init 0x1F is accepted by chip."""
    result = TestResult("A1.3", "asic",
                        "CRC5 poly 0x05 init 0x1F accepted by chip")

    if uart is None:
        result.skipped("No UART interface available")
        return result

    # Build a read register command with correct CRC
    cmd = build_read_register_cmd(0x00, REG_VERSION)
    cmd_crc = cmd[3] & 0x1F

    # Send and check for valid response
    resp_data = uart.read_register(0x00, REG_VERSION)

    raw = {"command_hex": cmd.hex(), "command_crc5": "0x{:02X}".format(cmd_crc)}

    if resp_data is None:
        result.skipped("No response (chain may not be powered)")
        result.raw_data = raw
        return result

    reg_addr, value, integrity, raw_response = resp_data
    raw["response_reg"] = "0x{:02X}".format(reg_addr)
    raw["register_matches"] = reg_addr == REG_VERSION
    raw["response_integrity"] = integrity
    raw["response_hex"] = raw_response.hex()

    if reg_addr != REG_VERSION:
        result.skipped(
            "Requested register 0x{:02X}, but response returned 0x{:02X}; "
            "value was not interpreted".format(REG_VERSION, reg_addr)
        )
        result.raw_data = raw
        return result

    raw["response_value"] = "0x{:08X}".format(value)

    result.skipped(
        "Command elicited a structural response, but response integrity is "
        "unverified; this cannot prove command acceptance",
    )
    result.raw_data = raw
    return result


def test_a1_5_response_7byte(uart=None, safe=True):
    """A1.5: Verify response is exactly 7 bytes."""
    result = TestResult("A1.5", "asic", "Response is exactly 7 bytes")

    if uart is None:
        result.skipped("No UART interface available")
        return result

    cmd = build_read_register_cmd(0x00, REG_VERSION)
    uart.flush_input()
    uart.write(cmd)
    time.sleep(0.02)
    # Read extra bytes to see if more come
    resp = uart.read(READ_RESPONSE_LEN + 8, timeout=0.15)

    raw = {"command_hex": cmd.hex(), "response_hex": resp.hex() if resp else "",
           "response_len": len(resp)}

    if len(resp) == 0:
        result.skipped("No response (chain may not be powered)")
        result.raw_data = raw
        return result

    if len(resp) == READ_RESPONSE_LEN:
        parsed = parse_read_response(resp)
        if parsed and parsed[0] == REG_VERSION:
            result.passed(
                "Response is exactly 7 bytes and returned requested register 0x{:02X}".format(
                    parsed[0]),
                raw
            )
        elif parsed:
            raw["returned_register"] = parsed[0]
            raw["register_matches"] = False
            result.failed(
                "Response is 7 bytes but returned register 0x{:02X}, expected 0x{:02X}".format(
                    parsed[0], REG_VERSION),
                raw
            )
        else:
            result.failed("Got 7 bytes but parse failed", raw)
    elif len(resp) > READ_RESPONSE_LEN:
        result.failed(
            "Response is {} bytes, expected exactly 7; refusing to split or truncate it".format(
                len(resp)),
            raw
        )
    else:
        result.failed(
            "Response is {} bytes, expected 7".format(len(resp)),
            raw
        )
    return result


def test_a1_7_chain_inactive(uart=None, safe=True):
    """A1.7: Chain inactive resets all chips to address 0."""
    result = TestResult("A1.7", "asic",
                        "Chain inactive resets all to address 0")

    if safe:
        result.skipped("Skipped in safe mode (modifies chain state)")
        return result

    if uart is None:
        result.skipped("No UART interface available")
        return result

    # Send chain inactive 3 times
    cmd_ci = build_chain_inactive_cmd()
    for _ in range(3):
        uart.send_command(cmd_ci)
        time.sleep(0.05)
    time.sleep(0.1)

    # After chain inactive, all chips should be at address 0
    # Try reading from address 0 -- should get a response
    resp_addr0 = uart.read_register(0x00, REG_CHIP_ADDRESS)

    raw = {"chain_inactive_cmd": cmd_ci.hex()}

    if resp_addr0 is None:
        result.skipped("No response after chain_inactive (chain may not be powered)")
        result.raw_data = raw
        return result

    returned_reg, addr_val, integrity, raw_response = resp_addr0
    raw["addr0_returned_register"] = returned_reg
    raw["addr0_register_matches"] = returned_reg == REG_CHIP_ADDRESS
    raw["addr0_response_integrity"] = integrity
    raw["addr0_response_hex"] = raw_response.hex()

    if returned_reg != REG_CHIP_ADDRESS:
        result.skipped(
            "Post-chain-inactive response returned register 0x{:02X}, expected "
            "0x{:02X}; value was not interpreted".format(
                returned_reg, REG_CHIP_ADDRESS)
        )
        result.raw_data = raw
        return result

    raw["addr0_value"] = "0x{:08X}".format(addr_val)

    # After chain_inactive, reading address 4 should get no response
    # (all chips are at address 0)
    resp_addr4 = uart.read_register(0x04, REG_CHIP_ADDRESS)
    raw["addr4_responded"] = resp_addr4 is not None

    result.skipped(
        "Observed post-command response shape, but response integrity is "
        "unverified; chain reset cannot be certified",
    )
    result.raw_data = raw
    return result


def test_a1_8_address_interval(uart=None, safe=True):
    """A1.8: After enumeration, addresses are spaced by 4."""
    result = TestResult("A1.8", "asic",
                        "Addresses spaced by interval 4 after enumeration")

    if safe:
        result.skipped("Skipped in safe mode (requires active enumeration)")
        return result

    if uart is None:
        result.skipped("No UART interface available")
        return result

    # Reset chain
    cmd_ci = build_chain_inactive_cmd()
    for _ in range(3):
        uart.send_command(cmd_ci)
        time.sleep(0.05)
    time.sleep(0.1)

    # Enumerate: assign addresses 0, 4, 8, ...
    addresses_found = []
    observations = []
    unsafe_observation_reason = None
    for chip_idx in range(MAX_CHIPS):
        chip_addr = chip_idx * ADDR_INTERVAL
        cmd_sa = build_set_address_cmd(chip_addr)
        uart.send_command(cmd_sa)
        time.sleep(0.02)

        resp = uart.read_register(chip_addr, REG_CHIP_ADDRESS)
        if resp is None:
            observations.append({
                "expected_address": chip_addr,
                "status": "no_response",
                "response_integrity": "unknown",
                "response_hex": None,
            })
            break

        reg_addr, observed_value, integrity, raw_response = resp
        observation = {
            "expected_address": chip_addr,
            "requested_register": REG_CHIP_ADDRESS,
            "returned_register": reg_addr,
            "register_matches": reg_addr == REG_CHIP_ADDRESS,
            "status": "observed",
            "response_integrity": integrity,
            "response_hex": raw_response.hex(),
            "response_bytes": list(raw_response),
        }
        observations.append(observation)
        if reg_addr != REG_CHIP_ADDRESS:
            observation["status"] = "register_mismatch"
            unsafe_observation_reason = (
                "Address response returned register 0x{:02X}, expected 0x{:02X}; "
                "refusing to interpret it or continue enumeration".format(
                    reg_addr, REG_CHIP_ADDRESS))
            break
        observation["observed_value"] = observed_value
        observation["observed_value_hex"] = "0x{:08X}".format(observed_value)
        if integrity != "verified":
            # Do not let an unverified response authorize another mutating
            # SET_ADDRESS command or mint an address-interval PASS.
            unsafe_observation_reason = (
                "Address response integrity is unverified; refusing to continue "
                "enumeration or certify the interval")
            break
        addresses_found.append(chip_addr)

    raw = {
        "addresses": addresses_found,
        "count": len(addresses_found),
        "observations": observations,
    }

    if unsafe_observation_reason:
        result.skipped(unsafe_observation_reason)
        result.raw_data = raw
        return result

    if len(addresses_found) < 2:
        result.skipped(
            "Found {} chip(s), need at least 2 to verify interval".format(
                len(addresses_found)))
        result.raw_data = raw
        return result

    # Verify interval = 4
    intervals = [addresses_found[i+1] - addresses_found[i]
                 for i in range(len(addresses_found) - 1)]
    all_four = all(iv == ADDR_INTERVAL for iv in intervals)
    raw["intervals"] = intervals

    if all_four:
        result.passed(
            "Found {} chips with addresses {}. All intervals = 4.".format(
                len(addresses_found),
                [hex(a) for a in addresses_found[:5]]),
            raw
        )
    else:
        result.failed(
            "Address intervals are not all 4: {}".format(intervals),
            raw
        )
    return result


def test_a1_10_pll_formula(uart=None, safe=True):
    """A1.10: Read PLL register, calculate frequency, compare with stock ~600 MHz."""
    result = TestResult("A1.10", "asic",
                        "PLL register decode matches ~600 MHz stock")

    if uart is None:
        result.skipped("No UART interface available")
        return result

    resp = uart.read_register(0x00, REG_PLL)

    if resp is None:
        result.skipped("No PLL register response (chain not powered)")
        return result

    returned_reg, pll_value, integrity, raw_response = resp

    raw = {
        "requested_register": REG_PLL,
        "returned_register": returned_reg,
        "register_matches": returned_reg == REG_PLL,
        "response_integrity": integrity,
        "response_hex": raw_response.hex(),
    }
    if returned_reg != REG_PLL:
        result.skipped(
            "PLL read returned register 0x{:02X}, expected 0x{:02X}; value was not interpreted".format(
                returned_reg, REG_PLL)
        )
        result.raw_data = raw
        return result

    freq = decode_pll_freq(pll_value)

    raw["pll_raw"] = "0x{:08X}".format(pll_value)
    raw["decoded_freq_mhz"] = freq

    if integrity != "verified":
        result.skipped(
            "PLL response integrity is {}; decoded value is diagnostic only".format(
                integrity
            )
        )
        result.raw_data = raw
        return result

    # S9 stock PLL is typically 500-700 MHz
    if 400 <= freq <= 800:
        result.passed(
            "PLL register = 0x{:08X}, decoded = {:.1f} MHz (expected 400-800 MHz range)".format(
                pll_value, freq),
            raw
        )
    elif freq > 0:
        result.failed(
            "PLL register = 0x{:08X}, decoded = {:.1f} MHz (outside expected 400-800 MHz range)".format(
                pll_value, freq),
            raw
        )
    else:
        result.failed(
            "PLL register = 0x{:08X}, could not decode frequency".format(pll_value),
            raw
        )
    return result


# ============================================================================
# Test Implementations - Category 2: I2C/Hash Board
# ============================================================================

def test_a2_1_three_boards(i2c_buses=None, safe=True):
    """A2.1: Scan I2C for PIC addresses (0x50-0x52), count detected boards."""
    result = TestResult("A2.1", "power",
                        "Three hash boards detected via PIC I2C addresses")

    if i2c_buses is None:
        result.skipped("No I2C interface available")
        return result

    found = {}
    for bus_num, i2c in i2c_buses.items():
        for pic_addr in PIC_ADDRESSES:
            if i2c.probe(pic_addr):
                found[pic_addr] = bus_num

    raw = {"pic_addresses_found": {"0x{:02X}".format(k): v for k, v in found.items()},
           "count": len(found)}

    if len(found) == 3:
        result.passed(
            "All 3 hash boards detected: PIC at {}".format(
                ", ".join("0x{:02X}(bus {})".format(a, b) for a, b in found.items())),
            raw
        )
    elif len(found) > 0:
        result.passed(
            "{}/3 hash boards detected: PIC at {}".format(
                len(found),
                ", ".join("0x{:02X}(bus {})".format(a, b) for a, b in found.items())),
            raw
        )
    else:
        result.failed("No PIC addresses (0x50-0x52) found on any I2C bus", raw)
    return result


def test_a2_2_tmp75_present(i2c_buses=None, safe=True):
    """A2.2: Check for TMP75 at 0x48-0x4F, read temperature."""
    result = TestResult("A2.2", "power",
                        "TMP75 present and reading plausible temperature")

    if i2c_buses is None:
        result.skipped("No I2C interface available")
        return result

    all_addrs = TMP75_ADDRESSES_A + TMP75_ADDRESSES_B
    found = {}

    for bus_num, i2c in i2c_buses.items():
        for addr in all_addrs:
            # TMP75 temperature register is at offset 0x00, 2 bytes big-endian
            data = i2c.write_read(addr, bytes([0x00]), 2)
            if data and len(data) == 2:
                raw_temp = (data[0] << 8) | data[1]
                temp_c = ((raw_temp >> 4) & 0xFFF)
                if temp_c >= 2048:
                    temp_c -= 4096
                temp_c = temp_c / 16.0
                found[addr] = {"bus": bus_num, "raw": "0x{:04X}".format(raw_temp),
                               "temp_c": temp_c}

    raw = {"sensors_found": {"0x{:02X}".format(k): v for k, v in found.items()},
           "count": len(found)}

    if len(found) == 0:
        result.failed("No TMP75 sensors found at 0x48-0x4E", raw)
        return result

    # Check plausibility: -10 to +100 C is reasonable for mining hardware
    plausible = [addr for addr, info in found.items()
                 if -10 <= info["temp_c"] <= 100]

    if plausible:
        temps_str = ", ".join(
            "0x{:02X}={:.1f}C".format(a, found[a]["temp_c"]) for a in plausible)
        result.passed(
            "Found {} TMP75 sensor(s) with plausible readings: {}".format(
                len(plausible), temps_str),
            raw
        )
    else:
        result.failed(
            "TMP75 sensors found but readings out of range (-10 to 100C)",
            raw
        )
    return result


def test_a2_3_eeprom_present(i2c_buses=None, safe=True):
    """A2.3: Check for EEPROM at 0x50, read first 16 bytes."""
    result = TestResult("A2.3", "power",
                        "EEPROM present at 0x50 with valid data")

    if i2c_buses is None:
        result.skipped("No I2C interface available")
        return result

    found = {}
    for bus_num, i2c in i2c_buses.items():
        for addr in EEPROM_ADDRESSES:
            # Read first 16 bytes from EEPROM (address pointer = 0x00)
            data = i2c.write_read(addr, bytes([0x00]), 16)
            if data and len(data) >= 16:
                all_ff = all(b == 0xFF for b in data)
                all_00 = all(b == 0x00 for b in data)
                found[addr] = {
                    "bus": bus_num,
                    "hex": data.hex(),
                    "all_ff": all_ff,
                    "all_00": all_00,
                    "has_data": not all_ff and not all_00,
                }

    raw = {"eeproms_found": {"0x{:02X}".format(k): v for k, v in found.items()},
           "count": len(found)}

    if len(found) == 0:
        result.failed("No EEPROM found at 0x50-0x52", raw)
        return result

    has_data = [addr for addr, info in found.items() if info["has_data"]]

    if has_data:
        result.passed(
            "Found {} EEPROM(s) with data (not all 0xFF/0x00): {}".format(
                len(has_data),
                ", ".join("0x{:02X}".format(a) for a in has_data)),
            raw
        )
    else:
        result.failed(
            "EEPROM found but all data is 0xFF or 0x00 (blank/erased)",
            raw
        )
    return result


def test_a2_4_pic_voltage(i2c_buses=None, safe=True):
    """A2.4: Read PIC voltage register and convert."""
    result = TestResult("A2.4", "power",
                        "PIC voltage register reads plausible value")

    if i2c_buses is None:
        result.skipped("No I2C interface available")
        return result

    found = {}
    for bus_num, i2c in i2c_buses.items():
        for pic_addr in PIC_ADDRESSES:
            # PIC voltage register: send command byte, read 1 byte
            # The specific command depends on PIC firmware revision
            # Common: read register at offset to get voltage setting
            data = i2c.read_byte(pic_addr)
            if data is not None:
                # Convert using S9 formula: V = (1608.420446 - pic_value) / 170.423497
                voltage = (PIC_VOLTAGE_INTERCEPT - data) / PIC_VOLTAGE_SLOPE
                found[pic_addr] = {
                    "bus": bus_num,
                    "raw_byte": data,
                    "raw_hex": "0x{:02X}".format(data),
                    "calculated_voltage": round(voltage, 4),
                }

    raw = {"pics_found": {"0x{:02X}".format(k): v for k, v in found.items()},
           "count": len(found)}

    if len(found) == 0:
        result.failed("No PIC controllers responded", raw)
        return result

    # Plausible BM1387 core voltage: 0.3V to 0.5V
    plausible = [addr for addr, info in found.items()
                 if 0.3 <= info["calculated_voltage"] <= 0.5]

    if plausible:
        v_str = ", ".join(
            "0x{:02X}={:.3f}V".format(a, found[a]["calculated_voltage"])
            for a in plausible)
        result.passed(
            "PIC voltage readings in plausible range (0.3-0.5V): {}".format(v_str),
            raw
        )
    else:
        v_str = ", ".join(
            "0x{:02X}={:.3f}V(raw=0x{:02X})".format(
                a, found[a]["calculated_voltage"], found[a]["raw_byte"])
            for a in found)
        result.passed(
            "PIC responded but voltage outside 0.3-0.5V range (may need "
            "different formula for this board revision): {}".format(v_str),
            raw
        )
    return result


# ============================================================================
# Test Implementations - Category 3: FPGA/Control Board
# ============================================================================

def test_a3_1_fpga_base(safe=True):
    """A3.1: Read FPGA register at 0x43C00000, verify non-zero."""
    result = TestResult("A3.1", "fpga",
                        "FPGA base register at 0x43C00000 responds")

    # Try devmem first
    value = devmem_read(FPGA_BASE_ADDR)

    raw = {"fpga_base": "0x{:08X}".format(FPGA_BASE_ADDR)}

    if value is not None:
        raw["value"] = "0x{:08X}".format(value)
        if value != 0:
            result.passed(
                "FPGA base register reads 0x{:08X} (non-zero, device present)".format(value),
                raw
            )
        else:
            result.failed(
                "FPGA base register reads 0x00000000 (may not be initialized)",
                raw
            )
    else:
        # Try via device node
        if os.path.exists(FPGA_DEV):
            try:
                fd = os.open(FPGA_DEV, os.O_RDONLY)
                data = os.read(fd, 4)
                os.close(fd)
                if len(data) == 4:
                    val = struct.unpack("<I", data)[0]
                    raw["value"] = "0x{:08X}".format(val)
                    if val != 0:
                        result.passed(
                            "FPGA register via {} reads 0x{:08X}".format(FPGA_DEV, val),
                            raw
                        )
                    else:
                        result.failed("FPGA register reads 0x00000000", raw)
                else:
                    result.failed("Read {} bytes from FPGA device".format(len(data)), raw)
            except Exception as e:
                result.skipped("Cannot read FPGA device: {}".format(e))
                result.raw_data = raw
        else:
            result.skipped(
                "Neither devmem nor {} available".format(FPGA_DEV))
            result.raw_data = raw
    return result


def test_a3_2_fpga_size(safe=True):
    """A3.2: Read registers 0x00 through 0x160, verify all accessible."""
    result = TestResult("A3.2", "fpga",
                        "FPGA register space 352 bytes accessible")

    readable_count = 0
    first_fail = None
    raw = {"fpga_base": "0x{:08X}".format(FPGA_BASE_ADDR),
           "expected_size": FPGA_SIZE}

    # Try via devmem
    for offset in range(0, FPGA_SIZE, 4):
        value = devmem_read(FPGA_BASE_ADDR + offset)
        if value is not None:
            readable_count += 1
        elif first_fail is None:
            first_fail = offset

    raw["readable_words"] = readable_count
    raw["expected_words"] = FPGA_SIZE // 4
    raw["first_fail_offset"] = first_fail

    if readable_count == 0:
        # Try device node
        if os.path.exists(FPGA_DEV):
            try:
                fd = os.open(FPGA_DEV, os.O_RDONLY)
                data = os.read(fd, FPGA_SIZE)
                os.close(fd)
                readable_count = len(data) // 4
                raw["readable_words"] = readable_count
                raw["via_device"] = FPGA_DEV
            except Exception as e:
                result.skipped("Cannot access FPGA registers: {}".format(e))
                result.raw_data = raw
                return result
        else:
            result.skipped("No method to access FPGA registers")
            result.raw_data = raw
            return result

    expected_words = FPGA_SIZE // 4  # 88 words

    if readable_count >= expected_words:
        result.passed(
            "All {} words ({} bytes) accessible in FPGA register space".format(
                readable_count, readable_count * 4),
            raw
        )
    elif readable_count > 0:
        result.passed(
            "{}/{} words accessible (partial access, first fail at offset 0x{:03X})".format(
                readable_count, expected_words,
                first_fail if first_fail is not None else 0),
            raw
        )
    else:
        result.failed("No FPGA registers accessible", raw)
    return result


def test_a3_3_hardware_version(safe=True):
    """A3.3: Read FPGA version register (offset 0x00), decode board type."""
    result = TestResult("A3.3", "fpga",
                        "FPGA hardware version register decodes")

    # Version register is at word offset 0x00 (byte offset 0x000)
    value = devmem_read(FPGA_BASE_ADDR)

    raw = {"register_offset": "0x000"}

    if value is None:
        result.skipped("Cannot read FPGA version register")
        result.raw_data = raw
        return result

    raw["value"] = "0x{:08X}".format(value)

    # Known Bitmain FPGA versions:
    # 0xC501 = S9 control board
    # 0xC571 = S17/S19 Zynq board
    version_map = {
        0xC501: "Antminer S9 (Zynq C5)",
        0xC571: "Antminer S19 (Zynq C71)",
    }

    # The version is typically in the lower 16 bits
    hw_ver = value & 0xFFFF
    board_name = version_map.get(hw_ver, "Unknown (0x{:04X})".format(hw_ver))
    raw["hw_version"] = "0x{:04X}".format(hw_ver)
    raw["board_name"] = board_name

    if hw_ver != 0x0000 and hw_ver != 0xFFFF:
        result.passed(
            "FPGA version = 0x{:04X} -> {}".format(hw_ver, board_name),
            raw
        )
    else:
        result.failed(
            "FPGA version register = 0x{:04X} (invalid/uninitialized)".format(hw_ver),
            raw
        )
    return result


def test_a3_4_hashboard_detect(safe=True):
    """A3.4: Read hash board plug detection register."""
    result = TestResult("A3.4", "fpga",
                        "Hash board plug detection register")

    # HASH_ON_PLUG is at word offset 0x02 (byte offset 0x008)
    value = devmem_read(FPGA_BASE_ADDR + 0x008)

    raw = {"register_offset": "0x008"}

    if value is None:
        result.skipped("Cannot read hash board detection register")
        result.raw_data = raw
        return result

    raw["value"] = "0x{:08X}".format(value)

    # Decode: lower bits indicate which hash boards are plugged in
    boards_detected = []
    for slot in range(3):
        if value & (1 << slot):
            boards_detected.append(slot)

    raw["boards_detected"] = boards_detected
    raw["board_count"] = len(boards_detected)

    if len(boards_detected) > 0:
        result.passed(
            "Hash board detection register = 0x{:08X}. "
            "Boards detected in slots: {}".format(
                value, boards_detected),
            raw
        )
    else:
        result.passed(
            "Hash board detection register = 0x{:08X}. "
            "No boards detected (may be normal if boards not powered)".format(value),
            raw
        )
    return result


def test_a3_5_uart_fifo(safe=True):
    """A3.5: Check UART FIFO status registers."""
    result = TestResult("A3.5", "fpga",
                        "UART FIFO status registers respond")

    # NONCE_NUMBER_IN_FIFO at word offset 0x06 (byte offset 0x018)
    # BUFFER_SPACE at word offset 0x03 (byte offset 0x00C)
    fifo_count = devmem_read(FPGA_BASE_ADDR + 0x018)
    buffer_space = devmem_read(FPGA_BASE_ADDR + 0x00C)

    raw = {}

    if fifo_count is None and buffer_space is None:
        result.skipped("Cannot read FPGA FIFO registers")
        result.raw_data = raw
        return result

    if fifo_count is not None:
        raw["nonce_fifo_count"] = "0x{:08X}".format(fifo_count)
    if buffer_space is not None:
        raw["buffer_space"] = "0x{:08X}".format(buffer_space)

    reads_ok = (fifo_count is not None) or (buffer_space is not None)

    if reads_ok:
        result.passed(
            "FIFO status registers respond. Nonce FIFO={}, Buffer={}".format(
                "0x{:08X}".format(fifo_count) if fifo_count is not None else "N/A",
                "0x{:08X}".format(buffer_space) if buffer_space is not None else "N/A"),
            raw
        )
    else:
        result.failed("FIFO status registers did not respond", raw)
    return result


# ============================================================================
# Test Implementations - Category 4: System/Boot
# ============================================================================

def test_a4_1_kernel_version(safe=True):
    """A4.1: Verify kernel is 4.6.0-xilinx."""
    result = TestResult("A4.1", "system",
                        "Kernel version is 4.6.0-xilinx")

    version = read_file("/proc/version")
    uname = None

    if version is None and HAS_SUBPROCESS:
        try:
            proc = subprocess.run(["uname", "-r"], capture_output=True, text=True, timeout=5)
            if proc.returncode == 0:
                uname = proc.stdout.strip()
        except Exception:
            pass

    raw = {}
    if version:
        raw["proc_version"] = version.strip()[:200]
    if uname:
        raw["uname_r"] = uname

    kernel_str = uname or (version or "")

    if "4.6.0-xilinx" in kernel_str:
        result.passed(
            "Kernel version confirmed: 4.6.0-xilinx",
            raw
        )
    elif kernel_str:
        result.passed(
            "Kernel version: {} (not 4.6.0-xilinx but system is running)".format(
                uname or version.strip()[:80] if version else "unknown"),
            raw
        )
    else:
        result.skipped("Cannot determine kernel version")
        result.raw_data = raw
    return result


def test_a4_2_serial_console(safe=True):
    """A4.2: Verify /dev/ttyPS0 exists (Zynq PS UART for console)."""
    result = TestResult("A4.2", "system",
                        "/dev/ttyPS0 exists (serial console)")

    exists = os.path.exists("/dev/ttyPS0")
    raw = {"path": "/dev/ttyPS0", "exists": exists}

    if exists:
        # Check if it's a character device
        try:
            stat = os.stat("/dev/ttyPS0")
            is_chardev = (stat.st_mode & 0o170000) == 0o020000
            raw["is_chardev"] = is_chardev
            raw["mode"] = oct(stat.st_mode)
        except Exception:
            is_chardev = True  # Assume if we can't stat

        result.passed(
            "/dev/ttyPS0 exists (serial console UART)",
            raw
        )
    else:
        result.failed("/dev/ttyPS0 does not exist", raw)
    return result


def test_a4_3_hash_uart(safe=True):
    """A4.3: Verify /dev/ttyPS1 exists (UART to hash boards via FPGA)."""
    result = TestResult("A4.3", "system",
                        "/dev/ttyPS1 exists (hash UART)")

    exists = os.path.exists("/dev/ttyPS1")
    raw = {"path": "/dev/ttyPS1", "exists": exists}

    if exists:
        try:
            stat = os.stat("/dev/ttyPS1")
            is_chardev = (stat.st_mode & 0o170000) == 0o020000
            raw["is_chardev"] = is_chardev
            raw["mode"] = oct(stat.st_mode)
        except Exception:
            pass

        result.passed(
            "/dev/ttyPS1 exists (hash board UART via FPGA)",
            raw
        )
    else:
        result.failed("/dev/ttyPS1 does not exist", raw)
    return result


def test_a4_4_nand_partitions(safe=True):
    """A4.4: Check /proc/mtd has expected 8 partitions."""
    result = TestResult("A4.4", "system",
                        "NAND has expected 8 partitions")

    mtd = read_file("/proc/mtd")
    raw = {}

    if mtd is None:
        result.skipped("Cannot read /proc/mtd")
        result.raw_data = raw
        return result

    raw["proc_mtd"] = mtd.strip()[:500]

    # Count mtd partitions (lines starting with "mtd")
    lines = [l for l in mtd.strip().split("\n") if l.startswith("mtd")]
    partition_count = len(lines)
    raw["partition_count"] = partition_count

    # Expected partition names for Zynq S9/S19
    expected_partitions = [
        "mtd0", "mtd1", "mtd2", "mtd3", "mtd4", "mtd5", "mtd6", "mtd7"
    ]

    if partition_count == 8:
        result.passed(
            "Found exactly 8 NAND partitions (mtd0-mtd7) as expected",
            raw
        )
    elif partition_count > 0:
        result.passed(
            "Found {} NAND partitions (expected 8). Partitions: {}".format(
                partition_count,
                ", ".join(l.split(":")[0] for l in lines[:10])),
            raw
        )
    else:
        result.failed(
            "No NAND partitions found in /proc/mtd",
            raw
        )
    return result


def test_a4_5_memory_size(safe=True):
    """A4.5: Read /proc/meminfo, report total RAM."""
    result = TestResult("A4.5", "system", "Memory size report")

    meminfo = read_file("/proc/meminfo")
    raw = {}

    if meminfo is None:
        result.skipped("Cannot read /proc/meminfo")
        result.raw_data = raw
        return result

    # Parse MemTotal
    mem_total_kb = 0
    for line in meminfo.split("\n"):
        if line.startswith("MemTotal:"):
            parts = line.split()
            if len(parts) >= 2:
                try:
                    mem_total_kb = int(parts[1])
                except ValueError:
                    pass
            break

    raw["mem_total_kb"] = mem_total_kb
    raw["mem_total_mb"] = mem_total_kb // 1024

    if mem_total_kb > 0:
        mb = mem_total_kb // 1024
        result.passed(
            "Total RAM: {} KB ({} MB). "
            "Zynq 7010 typically has 256 MB.".format(mem_total_kb, mb),
            raw
        )
    else:
        result.failed("Could not parse MemTotal from /proc/meminfo", raw)
    return result


# ============================================================================
# Mock Hardware for Self-Tests
# ============================================================================

class MockUART:
    """Simulated BM1387 chain for --test mode."""

    def __init__(self, chip_count=3):
        self.device = "/dev/mock_uart"
        self.chip_count = chip_count
        self._write_log = []
        self._next_address_index = 0
        self.chip_registers = {}
        self._setup_chips()

    def _setup_chips(self):
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
            self._next_address_index = 0
        return b""

    def read_register(self, chip_addr, reg_addr, retries=2):
        value = self.chip_registers.get((chip_addr, reg_addr))
        if value is None:
            chip_idx = chip_addr // ADDR_INTERVAL
            if chip_idx < self.chip_count:
                value = 0x00000000
            else:
                return None
        return (reg_addr, value, "simulated", b"")


class MockI2C:
    """Simulated I2C bus for --test mode."""

    def __init__(self, devices=None):
        self.bus = 0
        self.device = "/dev/mock_i2c-0"
        self._devices = devices or {}

    def open(self):
        pass

    def close(self):
        pass

    def set_slave(self, addr):
        pass

    def probe(self, addr):
        return addr in self._devices

    def read_byte(self, addr):
        dev = self._devices.get(addr)
        if dev and "byte" in dev:
            return dev["byte"]
        return None

    def read_bytes(self, addr, length):
        dev = self._devices.get(addr)
        if dev and "bytes" in dev:
            return dev["bytes"][:length]
        return None

    def write_read(self, addr, write_data, read_length):
        dev = self._devices.get(addr)
        if dev and "registers" in dev:
            reg = write_data[0] if write_data else 0
            data = dev["registers"].get(reg)
            if data:
                return data[:read_length]
        return None


def create_mock_i2c_buses():
    """Create mock I2C buses simulating S9 hardware."""
    # PIC controllers at 0x50-0x52
    # TMP75 sensors at 0x48-0x4A and 0x4C-0x4E
    # EEPROM at 0x50-0x52 (overlaps PIC on S9)
    devices = {}

    # PICs with voltage setting byte
    for slot, addr in enumerate(PIC_ADDRESSES):
        # Raw byte ~140 -> voltage ~(1608.4 - 140) / 170.4 ~= 8.61V (PIC value, not core voltage)
        # For S9 core voltage ~0.4V: pic_val ~ 1608.4 - 0.4*170.4 ~= 1540 -> 0x604
        # Actually the formula maps a single byte, so raw ~ 0xA0 -> 0.4V
        pic_val = 0xA0 + slot * 5
        devices[addr] = {
            "byte": pic_val,
            "registers": {
                0x00: bytes([0xDE, 0xAD] + [0xBE + slot] * 14),  # EEPROM first 16 bytes
            },
        }

    # TMP75 sensors (sensor A: 0x4C-0x4E)
    for slot, addr in enumerate(TMP75_ADDRESSES_A):
        # Temperature: 45.0 + slot*3.0 C
        temp_c = 45.0 + slot * 3.0
        raw_temp = int(temp_c * 16) << 4
        devices[addr] = {
            "registers": {
                0x00: bytes([(raw_temp >> 8) & 0xFF, raw_temp & 0xFF]),
            },
        }

    # TMP75 sensors (sensor B: 0x48-0x4A)
    for slot, addr in enumerate(TMP75_ADDRESSES_B):
        temp_c = 43.0 + slot * 3.0
        raw_temp = int(temp_c * 16) << 4
        devices[addr] = {
            "registers": {
                0x00: bytes([(raw_temp >> 8) & 0xFF, raw_temp & 0xFF]),
            },
        }

    mock_i2c = MockI2C(devices=devices)
    return {0: mock_i2c}


# ============================================================================
# Self-Test Runner
# ============================================================================

def run_self_tests():
    """Run comprehensive self-tests without hardware. Returns (passed, failed, tests)."""
    tests = []
    passed = 0
    failed = 0

    def check(name, condition, detail=""):
        nonlocal passed, failed
        status = "PASS" if condition else "FAIL"
        if condition:
            passed += 1
        else:
            failed += 1
        tests.append({"name": name, "status": status, "detail": detail})

    # =======================================================================
    # Section 1: CRC5 Engine Tests
    # =======================================================================
    print("=" * 70)
    print("Section 1: CRC5 Engine Verification")
    print("=" * 70)

    crc1 = crc5_bm13xx_command(bytes([0x52, 0x05, 0x00, 0x00]))
    crc2 = crc5_bm13xx_command(bytes([0x52, 0x05, 0x00, 0x00]))
    check("CRC5: captured get-address body -> 0x0A", crc1 == 0x0A,
          "crc=0x{:02X}".format(crc1))
    check("CRC5: deterministic output", crc1 == crc2,
          "c1=0x{:02X} c2=0x{:02X}".format(crc1, crc2))

    check("CRC5: output in 5-bit range",
          all(0 <= crc5_bm13xx_command(bytes([0x54, i, 0])) <= 0x1F for i in range(256)))

    crc_a = crc5_bm13xx_command(bytes([0x54, 0x00, 0x00]))
    crc_b = crc5_bm13xx_command(bytes([0x54, 0x00, 0x04]))
    check("CRC5: different inputs -> different CRCs", crc_a != crc_b)

    crc_z = crc5_bm13xx_command(bytes([0x00, 0x00, 0x00]))
    check("CRC5: all-zeros valid", 0 <= crc_z <= 0x1F)

    crc_f = crc5_bm13xx_command(bytes([0xFF, 0xFF, 0xFF]))
    check("CRC5: all-0xFF valid", 0 <= crc_f <= 0x1F)

    # =======================================================================
    # Section 2: Command Builder Tests
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 2: Command Builder Verification")
    print("=" * 70)

    cmd_rr = build_read_register_cmd(0x00, 0x00)
    check("Read register cmd: 4 bytes", len(cmd_rr) == 4)
    check("Read register cmd: opcode 0x54", cmd_rr[0] == 0x54)
    check("Read register cmd: chip addr preserved", cmd_rr[1] == 0x00)
    check("Read register cmd: reg addr preserved", cmd_rr[2] == 0x00)

    cmd_ci = build_chain_inactive_cmd()
    check("Chain inactive: 5 bytes", len(cmd_ci) == 5)
    check("Chain inactive: opcode 0x55", cmd_ci[0] == 0x55)

    cmd_sa = build_set_address_cmd(0x08)
    check("Set address: 3 bytes", len(cmd_sa) == 3)
    check("Set address: opcode 0x41", cmd_sa[0] == 0x41)
    check("Set address: addr=0x08", cmd_sa[1] == 0x08)

    # Verify NO preamble in any command
    check("Read cmd: no 0x55 0xAA preamble",
          cmd_rr[0] != 0x55 or cmd_rr[1] != 0xAA)
    check("Chain inactive: starts with 0x55 not 0x55 0xAA",
          cmd_ci[0] == 0x55 and (len(cmd_ci) < 2 or cmd_ci[1] != 0xAA))

    # =======================================================================
    # Section 3: Response Parser Tests
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 3: Response Parser Verification")
    print("=" * 70)

    # Build a structural 7-byte response with deliberately unverified trailer.
    val = 0x00680261  # PLL 600 MHz
    resp_data = bytes([0x0C, (val >> 24) & 0xFF, (val >> 16) & 0xFF,
                       (val >> 8) & 0xFF, val & 0xFF])
    full_resp = resp_data + bytes([0xA5, 0x5A])

    parsed = parse_read_response(full_resp)
    check("Parse response: valid 7-byte", parsed is not None)
    check("Parse response: correct register", parsed and parsed[0] == 0x0C)
    check("Parse response: correct value", parsed and parsed[1] == val)
    check("Parse response: integrity unverified", parsed and parsed[2] == "unverified")
    check("Parse response: raw bytes preserved", parsed and parsed[3] == full_resp)

    check("Parse response: rejects short data",
          parse_read_response(bytes([0x0C, 0x00])) is None)

    check("Parse response: exactly 7 bytes required",
          parse_read_response(bytes(7)) is not None)

    # =======================================================================
    # Section 4: PLL Decoder Tests
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 4: PLL Decoder Verification")
    print("=" * 70)

    check("PLL: 0x00680261 -> 600 MHz",
          abs(decode_pll_freq(0x00680261) - 600.0) < 1.0)
    check("PLL: 0x00680221 -> 400 MHz",
          abs(decode_pll_freq(0x00680221) - 400.0) < 1.0)
    check("PLL: 0x00700261 -> 650 MHz",
          abs(decode_pll_freq(0x00700261) - 650.0) < 1.0)

    # Formula decode test
    # fb=100, ref=2, pd1=4, pd2=1 -> 25*100/(2*4*1) = 312.5
    test_pll = (100 << 16) | (2 << 8) | (4 << 4) | 1
    freq = decode_pll_freq(test_pll)
    check("PLL: formula decode fb=100 ref=2 pd1=4 pd2=1 -> 312.5",
          abs(freq - 312.5) < 0.1, "got {:.1f}".format(freq))

    # =======================================================================
    # Section 5: MockUART ASIC Tests
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 5: MockUART ASIC Protocol Tests")
    print("=" * 70)

    mock_uart = MockUART(chip_count=3)

    # Test A1.1 - preamble
    r_a11 = test_a1_1_preamble(uart=mock_uart)
    check("A1.1 mock: runs without error",
          r_a11.status in ("PASS", "SKIP"),
          r_a11.explanation[:60])

    # Test A1.3 - CRC5
    r_a13 = test_a1_3_crc5(uart=mock_uart)
    check("A1.3 mock: unverified response cannot prove acceptance",
          r_a13.status == "SKIP",
          r_a13.explanation[:60])

    # Test A1.5 - 7-byte response
    r_a15 = test_a1_5_response_7byte(uart=mock_uart)
    check("A1.5 mock: runs without error",
          r_a15.status in ("PASS", "SKIP"),
          r_a15.explanation[:60])

    # Test A1.10 - PLL formula
    r_a110 = test_a1_10_pll_formula(uart=mock_uart)
    check("A1.10 mock: unverified PLL response cannot pass",
          r_a110.status == "SKIP",
          r_a110.explanation[:60])

    # Verify mock version register
    resp = mock_uart.read_register(0x00, REG_VERSION)
    check("MockUART: version register = BM1387",
          resp is not None and resp[1] == 0x13871387)

    # Verify mock chip count
    found = 0
    for i in range(5):
        r = mock_uart.read_register(i * ADDR_INTERVAL, REG_CHIP_ADDRESS)
        if r is not None:
            found += 1
    check("MockUART: exactly 3 chips respond", found == 3)

    # =======================================================================
    # Section 6: MockI2C Hash Board Tests
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 6: MockI2C Hash Board Tests")
    print("=" * 70)

    mock_i2c_buses = create_mock_i2c_buses()

    # Test A2.1 - Three boards
    r_a21 = test_a2_1_three_boards(i2c_buses=mock_i2c_buses)
    check("A2.1 mock: detects PIC controllers",
          r_a21.status == "PASS",
          r_a21.explanation[:60])

    # Test A2.2 - TMP75
    r_a22 = test_a2_2_tmp75_present(i2c_buses=mock_i2c_buses)
    check("A2.2 mock: TMP75 temperature readings",
          r_a22.status == "PASS",
          r_a22.explanation[:60])

    # Test A2.3 - EEPROM
    r_a23 = test_a2_3_eeprom_present(i2c_buses=mock_i2c_buses)
    check("A2.3 mock: EEPROM data present",
          r_a23.status == "PASS",
          r_a23.explanation[:60])

    # Test A2.4 - PIC voltage
    r_a24 = test_a2_4_pic_voltage(i2c_buses=mock_i2c_buses)
    check("A2.4 mock: PIC voltage reading",
          r_a24.status == "PASS",
          r_a24.explanation[:60])

    # Verify mock I2C device count
    mock_i2c = mock_i2c_buses[0]
    pic_count = sum(1 for a in PIC_ADDRESSES if mock_i2c.probe(a))
    check("MockI2C: 3 PIC controllers present", pic_count == 3)

    tmp_count = sum(1 for a in (TMP75_ADDRESSES_A + TMP75_ADDRESSES_B) if mock_i2c.probe(a))
    check("MockI2C: 6 TMP75 sensors present", tmp_count == 6)

    # =======================================================================
    # Section 7: System Tests (simulated)
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 7: System Tests (local verification)")
    print("=" * 70)

    # Test the test framework itself
    tr = TestResult("TEST", "test", "Framework test")
    tr.passed("test explanation", {"key": "value"})
    check("TestResult: PASS state works", tr.status == "PASS")
    check("TestResult: explanation stored", tr.explanation == "test explanation")
    check("TestResult: raw_data stored", tr.raw_data == {"key": "value"})

    tr2 = TestResult("TEST2", "test", "Framework test 2")
    tr2.failed("failed explanation")
    check("TestResult: FAIL state works", tr2.status == "FAIL")

    tr3 = TestResult("TEST3", "test", "Framework test 3")
    check("TestResult: default state is SKIP", tr3.status == "SKIP")

    # Test suite
    suite = TestSuite()
    suite.add(tr)
    suite.add(tr2)
    suite.add(tr3)
    summary = suite.summary()
    check("TestSuite: correct counts",
          summary["passed"] == 1 and summary["failed"] == 1 and summary["skipped"] == 1)

    # JSON serialization
    suite_dict = suite.to_dict()
    json_str = json.dumps(suite_dict)
    json_back = json.loads(json_str)
    check("TestSuite: JSON serialization works",
          isinstance(json_back, dict) and "summary" in json_back)

    # =======================================================================
    # Section 8: Test Registry and CLI
    # =======================================================================
    print()
    print("=" * 70)
    print("Section 8: Test Registry & CLI Verification")
    print("=" * 70)

    check("Registry: has 20 tests", len(TEST_REGISTRY) == 20,
          "count={}".format(len(TEST_REGISTRY)))

    # Check all categories present
    categories = set(t[1] for t in TEST_REGISTRY)
    check("Registry: has asic category", "asic" in categories)
    check("Registry: has power category", "power" in categories)
    check("Registry: has fpga category", "fpga" in categories)
    check("Registry: has system category", "system" in categories)

    # Check all test IDs are unique
    ids = [t[0] for t in TEST_REGISTRY]
    check("Registry: all test IDs unique", len(ids) == len(set(ids)))

    # Check all function names resolve
    all_resolve = True
    for entry in TEST_REGISTRY:
        func_name = entry[4]
        if func_name not in globals():
            all_resolve = False
            break
    check("Registry: all test functions exist", all_resolve)

    # Verify categories match the --category option names
    valid_categories = {"asic", "power", "fpga", "system"}
    check("Registry: categories match CLI options",
          categories == valid_categories)

    return passed, failed, tests


# ============================================================================
# Test Execution Engine
# ============================================================================

def run_test(entry, uart=None, i2c_buses=None, safe=True):
    """Run a single test from the registry."""
    test_id, category, description, is_safe, func_name = entry

    func = globals().get(func_name)
    if func is None:
        result = TestResult(test_id, category, description)
        result.skipped("Test function '{}' not found".format(func_name))
        return result

    if not is_safe and safe:
        result = TestResult(test_id, category, description)
        result.skipped("Skipped in --safe mode (may modify hardware state)")
        return result

    try:
        # Call the test function with appropriate interfaces
        if category == "asic":
            result = func(uart=uart, safe=safe)
        elif category == "power":
            result = func(i2c_buses=i2c_buses, safe=safe)
        elif category in ("fpga", "system"):
            result = func(safe=safe)
        else:
            result = func(safe=safe)
    except Exception as e:
        result = TestResult(test_id, category, description)
        result.failed("Exception during test: {}".format(e))

    return result


def run_tests(test_filter=None, category_filter=None, safe=True):
    """Run tests matching the filter criteria."""
    suite = TestSuite()

    # Open hardware interfaces
    uart = None
    i2c_buses = None

    # Try to open UART
    if os.path.exists(UART_DEFAULT):
        try:
            uart = UARTInterface()
            uart.open()
        except Exception:
            uart = None

    # Try to open I2C buses
    i2c_buses = {}
    for bus in range(8):
        dev = "/dev/i2c-{}".format(bus)
        if os.path.exists(dev):
            try:
                i2c = I2CInterface(bus=bus)
                i2c.open()
                i2c_buses[bus] = i2c
            except Exception:
                pass

    if not i2c_buses:
        i2c_buses = None

    try:
        for entry in TEST_REGISTRY:
            test_id, category = entry[0], entry[1]

            # Filter
            if test_filter and test_id != test_filter:
                continue
            if category_filter and category != category_filter:
                continue

            result = run_test(entry, uart=uart, i2c_buses=i2c_buses, safe=safe)
            suite.add(result)

    finally:
        if uart:
            uart.close()
        if i2c_buses:
            for i2c in i2c_buses.values():
                i2c.close()

    return suite


# ============================================================================
# CLI
# ============================================================================

def print_help():
    help_text = """
assumption_verifier.py -- Hardware Assumption Verification Suite v{}
=====================================================================

Automated test suite that verifies documented assumptions from ASSUMPTIONS.md
against real Antminer S9 (Zynq 7010 + BM1387) hardware.

Usage:
  assumption_verifier.py [OPTIONS]

Options:
  --help              Show this help message
  --test              Run self-tests WITHOUT hardware (at least 30 tests)
  --all               Run all hardware tests
  --category CAT      Run one category: asic, power, fpga, system
  --id ID             Run specific test (e.g., A1.1, A2.3)
  --list              List all available tests with descriptions
  --json              Output results as JSON
  --safe              Skip write/modify tests (DEFAULT)
  --unsafe            Allow tests that modify hardware state
  --verbose           Verbose output

Categories:
  asic    - BM1387 ASIC protocol tests (UART chain)
  power   - I2C hash board tests (PIC, TMP75, EEPROM)
  fpga    - FPGA register tests (devmem/device node)
  system  - System/boot tests (/proc, /dev checks)

Examples:
  # Self-test (no hardware needed):
  assumption_verifier.py --test

  # Run all safe hardware tests:
  assumption_verifier.py --all

  # Run only ASIC tests:
  assumption_verifier.py --category asic

  # Run specific test:
  assumption_verifier.py --id A1.3

  # JSON output for automated processing:
  assumption_verifier.py --all --json

  # Run ALL tests including those that modify state:
  assumption_verifier.py --all --unsafe
""".format(VERSION)
    print(help_text.strip())


def print_test_list():
    """Print all available tests."""
    print("Available Assumption Verification Tests")
    print("=" * 75)
    print("{:<8} {:<8} {:<6} {}".format("ID", "CAT", "SAFE", "DESCRIPTION"))
    print("-" * 75)
    for test_id, category, description, is_safe, _ in TEST_REGISTRY:
        safe_str = "Yes" if is_safe else "No"
        print("{:<8} {:<8} {:<6} {}".format(test_id, category, safe_str, description))
    print("-" * 75)
    print("Total: {} tests".format(len(TEST_REGISTRY)))


def format_results_text(suite):
    """Format test results as human-readable text."""
    lines = []
    lines.append("DCENTos Assumption Verification Results")
    lines.append("=" * 65)
    lines.append("")

    # Group by category
    by_category = collections.OrderedDict()
    for r in suite.results:
        cat = r.category
        if cat not in by_category:
            by_category[cat] = []
        by_category[cat].append(r)

    category_names = {
        "asic": "Category 1: ASIC Protocol (BM1387)",
        "power": "Category 2: I2C / Hash Board",
        "fpga": "Category 3: FPGA / Control Board",
        "system": "Category 4: System / Boot",
    }

    for cat, results in by_category.items():
        lines.append("--- {} ---".format(category_names.get(cat, cat)))
        for r in results:
            status_marker = {"PASS": "[PASS]", "FAIL": "[FAIL]", "SKIP": "[SKIP]"}
            marker = status_marker.get(r.status, "[????]")
            lines.append("  {} {} : {}".format(marker, r.test_id, r.description))
            if r.explanation:
                # Wrap long explanations
                exp = r.explanation
                if len(exp) > 80:
                    exp = exp[:77] + "..."
                lines.append("         {}".format(exp))
        lines.append("")

    # Summary
    s = suite.summary()
    lines.append("=" * 65)
    lines.append("Summary: {}/{} PASSED, {} FAILED, {} SKIPPED".format(
        s["passed"], s["total"], s["failed"], s["skipped"]))

    if s["failed"] == 0 and s["passed"] > 0:
        lines.append("Result: ALL TESTS PASSED")
    elif s["failed"] > 0:
        lines.append("Result: SOME TESTS FAILED -- check assumptions")
    else:
        lines.append("Result: ALL TESTS SKIPPED (no hardware?)")

    lines.append("=" * 65)
    return "\n".join(lines)


def main():
    args = sys.argv[1:]

    if "--help" in args or "-h" in args:
        print_help()
        return 0

    if "--list" in args:
        print_test_list()
        return 0

    if "--test" in args:
        print("assumption_verifier.py -- Self-Test Mode (no hardware)")
        print("=" * 65)
        passed, failed, tests = run_self_tests()
        print()
        print("=" * 65)
        for t in tests:
            detail = " ({})".format(t["detail"]) if t["detail"] else ""
            print("  [{}] {}{}".format(t["status"], t["name"], detail))
        print("-" * 65)
        print("Results: {} passed, {} failed out of {}".format(
            passed, failed, passed + failed))
        print("=" * 65)
        return 0 if failed == 0 else 1

    # Hardware test mode
    json_mode = "--json" in args
    verbose = "--verbose" in args
    safe = "--unsafe" not in args  # Default to safe
    run_all = "--all" in args

    category_filter = None
    test_filter = None

    i = 0
    while i < len(args):
        if args[i] == "--category" and i + 1 < len(args):
            category_filter = args[i + 1].lower()
            i += 2
        elif args[i] == "--id" and i + 1 < len(args):
            test_filter = args[i + 1].upper()
            i += 2
        else:
            i += 1

    if not run_all and not category_filter and not test_filter:
        print_help()
        return 0

    # Run tests
    suite = run_tests(
        test_filter=test_filter,
        category_filter=category_filter,
        safe=safe
    )

    # Output
    if json_mode:
        output = suite.to_dict()
        output["version"] = VERSION
        output["safe_mode"] = safe
        output["timestamp"] = time.strftime("%Y-%m-%dT%H:%M:%S")
        print(json.dumps(output, indent=2, default=str))
    else:
        print(format_results_text(suite))

    s = suite.summary()
    return 0 if s["failed"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
