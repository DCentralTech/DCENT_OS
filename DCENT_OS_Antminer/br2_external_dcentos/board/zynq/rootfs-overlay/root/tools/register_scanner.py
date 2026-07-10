#!/usr/bin/env python3
"""
register_scanner.py - BM1387 ASIC Register Scanner for Antminer S9
===================================================================
Scans all registers (0x00-0xFF) on BM1387 ASIC chips via UART.
Talks directly to hardware via /dev/ttyPS1.

BM1387 Protocol (NOT BM1366/BM1397):
  - NO 0x55/0xAA preamble on commands
  - Read register command: [0x54, chip_addr, reg_addr, CRC5_byte]
  - Response: 7 bytes [reg_addr, 4 bytes data, 2 bytes with CRC5]
  - CRC5 polynomial 0x05, init 0x1F
  - Address interval: 4, max 63 chips per chain

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

# BM1387 command opcodes (NO preamble)
CMD_READ_REGISTER = 0x54
CMD_WRITE_REGISTER = 0x51
CMD_CHAIN_INACTIVE = 0x55
CMD_SET_ADDRESS = 0x41

# Response sizes
READ_RESPONSE_LEN = 7

# Known BM1387 registers from ASIC_REGISTER_BIBLE
KNOWN_REGISTERS = {
    0x00: ("ChipAddress",      "Chip address in chain (interval 4)"),
    0x04: ("HashRate",         "Nonce/hash rate counter"),
    0x08: ("PLLParameter",     "PLL frequency parameter (alt)"),
    0x0C: ("PLL",              "PLL configuration / clock frequency"),
    0x10: ("HashClockCtrl",    "Hash clock control / enable"),
    0x14: ("TicketMask",       "Difficulty ticket mask"),
    0x18: ("MiscControl",      "Miscellaneous control bits"),
    0x1C: ("GeneralI2C",       "General I2C interface"),
    0x20: ("SecurityReg",      "Security / OTP register"),
    0x24: ("ChipNonceOffset",  "Per-chip nonce offset"),
    0x28: ("BaudRate",         "UART baud rate divisor"),
    0x2C: ("ClockOrder",       "Clock domain ordering"),
    0x30: ("FastUARTConfig",   "Fast UART configuration"),
    0x34: ("UARTRelay",        "UART relay / passthrough"),
    0x38: ("TicketMask2",      "Ticket mask 2 / additional filter"),
    0x3C: ("CoreRegister",     "Core register access"),
    0x40: ("TempSensorCtrl",   "Temperature sensor control (candidate)"),
    0x44: ("TempSensorData",   "Temperature sensor data (candidate)"),
    0x48: ("ErrorCounter",     "Error / fault counter"),
    0x4C: ("CoreError",        "Core error flags"),
    0x50: ("CoreEnable0",      "Core enable bits [31:0]"),
    0x54: ("CoreEnable1",      "Core enable bits [63:32]"),
    0x58: ("CoreEnable2",      "Core enable bits [95:64]"),
    0x5C: ("CoreEnable3",      "Core enable bits [113:96]"),
    0x60: ("CoreTest",         "Core test mode"),
    0x7C: ("Version",          "Chip version / revision ID"),
    0x80: ("SweepClockCtrl",   "Sweep clock control"),
    0xA0: ("GoldenNonce",      "Golden nonce output"),
    0xA4: ("ReturnNonce",      "Return nonce register"),
    0xA8: ("NonceMask",        "Nonce range mask"),
    0xFC: ("ChipStatus",       "Chip status / ready flag"),
}

# PLL frequency lookup (common BM1387 PLL values)
PLL_FREQ_MAP = {
    0x00000000: "unknown/disabled",
    0x00680221: "400 MHz",
    0x00700221: "450 MHz",
    0x00680241: "500 MHz",
    0x00700241: "550 MHz",
    0x00680261: "600 MHz",
    0x00700261: "650 MHz",
    0x00680281: "700 MHz",
    0x00700281: "750 MHz",
}


# ============================================================================
# CRC5 for BM1387
# ============================================================================

def crc5_bm1387(data, bit_length=None):
    """
    Calculate CRC5 for BM1387 protocol.
    Polynomial: 0x05 (x^5 + x^2 + 1)
    Init: 0x1F
    Operates bit-by-bit over the input data bytes.
    If bit_length is None, process all bits in data.
    """
    crc = 0x1F  # 5-bit init = all ones
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
            crc = ((crc << 1) | bit) & 0x3F  # 6 bits to capture overflow
            if top_bit ^ ((crc >> 5) & 1):
                crc ^= poly
            crc &= 0x1F
            bit_index += 1
            if bit_index >= bit_length:
                break

    return crc & 0x1F


def build_read_register_cmd(chip_addr, reg_addr):
    """
    Build a BM1387 read register command.
    Format: [0x54, chip_addr, reg_addr, CRC5_byte]
    The CRC5 is computed over the first 3 bytes (24 bits),
    then packed into the low 5 bits of the 4th byte with
    the command length info.
    """
    # Command frame: TYPE(8) ADDR(8) REG(8) = 24 data bits, then CRC5
    cmd_data = bytes([CMD_READ_REGISTER, chip_addr, reg_addr])
    crc = crc5_bm1387(cmd_data)
    # The 4th byte: upper 3 bits = length code (0 for short cmd), lower 5 bits = CRC5
    fourth_byte = (crc & 0x1F)
    return bytes([CMD_READ_REGISTER, chip_addr, reg_addr, fourth_byte])


def parse_read_response(response):
    """
    Parse a 7-byte BM1387 read register response.
    Format: [reg_addr, D3, D2, D1, D0, CRC_hi, CRC_lo]
    Returns (reg_addr, value_u32, crc_ok) or None if invalid.
    """
    if len(response) < READ_RESPONSE_LEN:
        return None

    reg_addr = response[0]
    # Data is big-endian: MSB first
    value = (response[1] << 24) | (response[2] << 16) | (response[3] << 8) | response[4]

    # Verify CRC5 over the response
    # CRC is over first 5 bytes (40 bits)
    resp_crc = response[5] & 0x1F  # CRC5 is in last bytes
    calc_crc = crc5_bm1387(response[:5])
    crc_ok = (calc_crc == resp_crc)

    return (reg_addr, value, crc_ok)


def decode_register(reg_addr, value):
    """Decode a known register value into human-readable form."""
    info = KNOWN_REGISTERS.get(reg_addr)
    name = info[0] if info else "Unknown_0x{:02X}".format(reg_addr)
    desc = info[1] if info else ""
    decoded = ""

    if reg_addr == 0x00:
        # ChipAddress: chip_addr in bits [7:0] typically
        chip_id = (value >> 0) & 0xFF
        decoded = "chip_addr=0x{:02X} (chip #{})".format(chip_id, chip_id // ADDR_INTERVAL)

    elif reg_addr == 0x0C:
        # PLL register
        freq_str = PLL_FREQ_MAP.get(value, "")
        if freq_str:
            decoded = freq_str
        else:
            # Try to decode PLL fields
            fbdiv = (value >> 16) & 0xFF
            refdiv = (value >> 8) & 0x3F
            postdiv1 = (value >> 4) & 0x07
            postdiv2 = value & 0x07
            if refdiv > 0 and postdiv1 > 0 and postdiv2 > 0:
                freq_mhz = (25.0 * fbdiv) / (refdiv * postdiv1 * postdiv2)
                decoded = "{:.0f} MHz (fbdiv={} refdiv={} pd1={} pd2={})".format(
                    freq_mhz, fbdiv, refdiv, postdiv1, postdiv2)
            else:
                decoded = "fbdiv={} refdiv={} postdiv1={} postdiv2={}".format(
                    fbdiv, refdiv, postdiv1, postdiv2)

    elif reg_addr == 0x14:
        # TicketMask
        decoded = "mask=0x{:08X}".format(value)
        if value == 0x0000FFFF:
            decoded += " (diff ~1)"
        elif value == 0x00000000:
            decoded += " (no masking)"

    elif reg_addr == 0x28:
        # BaudRate
        if value > 0:
            baud = 25000000 // value if value else 0
            decoded = "divisor={} (~{} baud)".format(value, baud)

    elif reg_addr == 0x7C:
        # Version
        decoded = "version=0x{:08X}".format(value)
        if value == 0x13871387 or (value & 0xFFFF) == 0x1387:
            decoded += " (BM1387 confirmed)"

    elif reg_addr in (0x40, 0x44):
        # Temperature candidates
        raw = value & 0xFFFF
        # Try common temp encodings
        temp_c_raw = raw / 256.0 if raw > 0 else 0
        decoded = "raw=0x{:04X} (maybe {:.1f}C if /256)".format(raw, temp_c_raw)

    return {
        "name": name,
        "description": desc,
        "decoded": decoded,
    }


# ============================================================================
# UART Hardware Interface
# ============================================================================

class UARTInterface:
    """Direct UART access to BM1387 chain via /dev/ttyPSx."""

    def __init__(self, device=UART_DEFAULT, baud=UART_BAUD, timeout=0.1):
        self.device = device
        self.baud = baud
        self.timeout = timeout
        self.fd = None
        self.fobj = None

    def open(self):
        """Open UART device and configure for BM1387 communication."""
        self.fd = os.open(self.device, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)

        if HAS_TERMIOS:
            # Configure terminal: 115200 8N1, raw mode
            attrs = termios.tcgetattr(self.fd)

            # Input flags: no parity, no flow control
            attrs[0] = 0  # iflag
            attrs[1] = 0  # oflag
            # cflag: 8N1
            attrs[2] = (termios.CS8 | termios.CREAD | termios.CLOCAL)
            # lflag: raw
            attrs[3] = 0
            # cc: VMIN=0, VTIME=1 (0.1s timeout)
            attrs[6][termios.VMIN] = 0
            attrs[6][termios.VTIME] = 1

            # Set baud rate
            baud_const = getattr(termios, 'B115200', 4098)
            attrs[4] = baud_const  # ispeed
            attrs[5] = baud_const  # ospeed

            termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
            termios.tcflush(self.fd, termios.TCIOFLUSH)

        self.fobj = os.fdopen(self.fd, "rb+", buffering=0)

    def close(self):
        """Close UART device."""
        if self.fobj:
            try:
                self.fobj.close()
            except Exception:
                pass
            self.fobj = None
            self.fd = None

    def write(self, data):
        """Write raw bytes to UART."""
        if self.fobj:
            self.fobj.write(data)
            self.fobj.flush()

    def read(self, length, timeout=None):
        """Read up to `length` bytes from UART with timeout."""
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
        """Discard any pending input data."""
        if HAS_TERMIOS and self.fd is not None:
            termios.tcflush(self.fd, termios.TCIFLUSH)
        elif self.fobj:
            # Non-termios flush: read and discard
            try:
                while True:
                    chunk = self.fobj.read(256)
                    if not chunk:
                        break
            except Exception:
                pass

    def read_register(self, chip_addr, reg_addr, retries=2):
        """
        Send a read register command and parse the response.
        Returns (reg_addr, value_u32, crc_ok) or None.
        """
        cmd = build_read_register_cmd(chip_addr, reg_addr)

        for attempt in range(retries + 1):
            self.flush_input()
            self.write(cmd)
            # Wait for response
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
# Mock UART for testing
# ============================================================================

class MockUART:
    """Simulated UART for --test mode. No hardware needed."""

    def __init__(self):
        self.device = "/dev/mock_uart"
        self.baud = 115200
        self._response_queue = []
        self._write_log = []
        self.chip_registers = {}  # (chip_addr, reg_addr) -> value
        self._setup_mock_chips()

    def _setup_mock_chips(self):
        """Set up simulated chip register values."""
        for chip_idx in range(3):  # 3 simulated chips
            addr = chip_idx * ADDR_INTERVAL
            self.chip_registers[(addr, 0x00)] = addr  # ChipAddress
            self.chip_registers[(addr, 0x0C)] = 0x00680261  # PLL 600MHz
            self.chip_registers[(addr, 0x14)] = 0x0000FFFF  # TicketMask
            self.chip_registers[(addr, 0x28)] = 0x00000019  # BaudRate
            self.chip_registers[(addr, 0x7C)] = 0x13871387  # Version
            self.chip_registers[(addr, 0x40)] = 0x00002D00  # Temp ~45C
            self.chip_registers[(addr, 0x44)] = 0x00002F80  # Temp ~47.5C
            self.chip_registers[(addr, 0xFC)] = 0x00000001  # Status OK

    def open(self):
        pass

    def close(self):
        pass

    def write(self, data):
        self._write_log.append(data)

    def read(self, length, timeout=None):
        if self._response_queue:
            return self._response_queue.pop(0)
        return b""

    def flush_input(self):
        pass

    def read_register(self, chip_addr, reg_addr, retries=2):
        """Simulate register read."""
        value = self.chip_registers.get((chip_addr, reg_addr), 0xDEADBEEF)
        crc = crc5_bm1387(bytes([reg_addr,
                                  (value >> 24) & 0xFF,
                                  (value >> 16) & 0xFF,
                                  (value >> 8) & 0xFF,
                                  value & 0xFF]))
        return (reg_addr, value, True)

    def queue_response(self, data):
        """Queue a raw response for the next read."""
        self._response_queue.append(data)


# ============================================================================
# Scanner Logic
# ============================================================================

def scan_registers(uart, chip_addr, reg_start=0x00, reg_end=0xFF, reg_step=4,
                   verbose=False, progress=True):
    """
    Scan a range of registers on a single chip.
    Returns list of dicts with register data.
    """
    results = []
    total = (reg_end - reg_start) // reg_step + 1
    count = 0

    for reg in range(reg_start, reg_end + 1, reg_step):
        count += 1
        if progress and not verbose and (count % 16 == 0 or count == total):
            pct = int(100 * count / total)
            sys.stderr.write("\r  Scanning: {}/{} registers ({}%)".format(count, total, pct))
            sys.stderr.flush()

        result = uart.read_register(chip_addr, reg)

        entry = {
            "register": reg,
            "register_hex": "0x{:02X}".format(reg),
            "chip_addr": chip_addr,
            "responded": result is not None,
            "value": None,
            "value_hex": None,
            "crc_ok": None,
            "known": reg in KNOWN_REGISTERS,
        }

        if result is not None:
            reg_returned, value, crc_ok = result
            entry["value"] = value
            entry["value_hex"] = "0x{:08X}".format(value)
            entry["crc_ok"] = crc_ok

            decoded = decode_register(reg, value)
            entry["name"] = decoded["name"]
            entry["description"] = decoded["description"]
            entry["decoded"] = decoded["decoded"]

            if verbose:
                status = "OK" if crc_ok else "CRC_ERR"
                known_tag = " *" if entry["known"] else ""
                sys.stderr.write("  [0x{:02X}] = 0x{:08X} [{}]{} {}\n".format(
                    reg, value, status, known_tag, decoded.get("decoded", "")))
        else:
            entry["name"] = KNOWN_REGISTERS.get(reg, ("Unknown", ""))[0] if reg in KNOWN_REGISTERS else ""
            entry["description"] = ""
            entry["decoded"] = ""

        results.append(entry)

    if progress and not verbose:
        sys.stderr.write("\r  Scanning: {}/{} registers (100%)\n".format(total, total))
        sys.stderr.flush()

    return results


def scan_all_chips(uart, chip_count, reg_start=0x00, reg_end=0xFF, reg_step=4,
                   verbose=False):
    """Scan registers across all chips in the chain."""
    all_results = {}
    for chip_idx in range(chip_count):
        chip_addr = chip_idx * ADDR_INTERVAL
        sys.stderr.write("Chip #{} (addr=0x{:02X}):\n".format(chip_idx, chip_addr))
        results = scan_registers(uart, chip_addr, reg_start, reg_end, reg_step,
                                 verbose=verbose)
        all_results[chip_addr] = results
    return all_results


def format_table(results, show_empty=False):
    """Format scan results as a text table."""
    lines = []
    lines.append("{:<8} {:<20} {:<12} {:<5} {}".format(
        "REG", "NAME", "VALUE", "CRC", "DECODED"))
    lines.append("-" * 80)

    for entry in results:
        if not entry["responded"] and not show_empty:
            continue

        reg_str = entry["register_hex"]
        name = entry.get("name", "")[:20]
        if entry["responded"]:
            val_str = entry["value_hex"]
            crc_str = "OK" if entry["crc_ok"] else "ERR"
            decoded = entry.get("decoded", "")[:40]
        else:
            val_str = "---"
            crc_str = "---"
            decoded = "(no response)"

        known_marker = "*" if entry["known"] else " "
        lines.append("{}{:<7} {:<20} {:<12} {:<5} {}".format(
            known_marker, reg_str, name, val_str, crc_str, decoded))

    return "\n".join(lines)


def format_summary(results):
    """Generate a summary of the scan."""
    responded = sum(1 for r in results if r["responded"])
    total = len(results)
    crc_errors = sum(1 for r in results if r["responded"] and not r["crc_ok"])
    known_found = sum(1 for r in results if r["responded"] and r["known"])
    unknown_found = sum(1 for r in results if r["responded"] and not r["known"])

    lines = [
        "--- Scan Summary ---",
        "Total registers probed: {}".format(total),
        "Registers that responded: {}".format(responded),
        "Known registers found: {}".format(known_found),
        "Unknown registers found: {}".format(unknown_found),
        "CRC errors: {}".format(crc_errors),
    ]
    return "\n".join(lines)


# ============================================================================
# Self-Tests (--test mode, no hardware required)
# ============================================================================

def run_self_tests():
    """Run comprehensive self-tests. Returns (passed, failed, details)."""
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
        return condition

    # --- Test 1: CRC5 known vectors ---
    # BM1387 CRC5 over [0x54, 0x00, 0x00] should produce a known CRC
    crc1 = crc5_bm1387(bytes([0x54, 0x00, 0x00]))
    test("CRC5: read reg cmd [0x54,0x00,0x00]",
         0 <= crc1 <= 0x1F,
         "CRC5=0x{:02X}".format(crc1))

    # --- Test 2: CRC5 determinism ---
    crc2a = crc5_bm1387(bytes([0x54, 0x04, 0x0C]))
    crc2b = crc5_bm1387(bytes([0x54, 0x04, 0x0C]))
    test("CRC5: deterministic output",
         crc2a == crc2b,
         "crc_a=0x{:02X} crc_b=0x{:02X}".format(crc2a, crc2b))

    # --- Test 3: CRC5 different inputs produce different outputs ---
    crc3a = crc5_bm1387(bytes([0x54, 0x00, 0x00]))
    crc3b = crc5_bm1387(bytes([0x54, 0x00, 0x04]))
    test("CRC5: different input -> different CRC",
         crc3a != crc3b,
         "crc_00=0x{:02X} crc_04=0x{:02X}".format(crc3a, crc3b))

    # --- Test 4: CRC5 range ---
    all_in_range = True
    for i in range(256):
        c = crc5_bm1387(bytes([0x54, 0x00, i]))
        if c < 0 or c > 0x1F:
            all_in_range = False
            break
    test("CRC5: all outputs in 5-bit range [0x00-0x1F]",
         all_in_range)

    # --- Test 5: Build read register command ---
    cmd = build_read_register_cmd(0x00, 0x00)
    test("Build CMD: length is 4 bytes",
         len(cmd) == 4,
         "len={}".format(len(cmd)))

    # --- Test 6: Command opcode ---
    test("Build CMD: opcode is 0x54",
         cmd[0] == CMD_READ_REGISTER,
         "opcode=0x{:02X}".format(cmd[0]))

    # --- Test 7: Command chip address ---
    cmd7 = build_read_register_cmd(0x08, 0x0C)
    test("Build CMD: chip_addr preserved",
         cmd7[1] == 0x08,
         "chip_addr=0x{:02X}".format(cmd7[1]))

    # --- Test 8: Command register address ---
    test("Build CMD: reg_addr preserved",
         cmd7[2] == 0x0C,
         "reg_addr=0x{:02X}".format(cmd7[2]))

    # --- Test 9: Command CRC byte ---
    test("Build CMD: CRC byte in valid range",
         0 <= cmd7[3] <= 0x1F,
         "crc_byte=0x{:02X}".format(cmd7[3]))

    # --- Test 10: Parse response - valid ---
    # Build a synthetic valid response
    reg_a = 0x0C
    val = 0x00680261  # PLL 600MHz
    resp_data = bytes([reg_a,
                       (val >> 24) & 0xFF, (val >> 16) & 0xFF,
                       (val >> 8) & 0xFF, val & 0xFF])
    resp_crc = crc5_bm1387(resp_data)
    full_resp = resp_data + bytes([resp_crc, 0x00])
    parsed = parse_read_response(full_resp)
    test("Parse response: valid 7-byte response",
         parsed is not None and parsed[0] == 0x0C and parsed[1] == 0x00680261,
         "parsed={}".format(parsed))

    # --- Test 11: Parse response - too short ---
    short_resp = parse_read_response(bytes([0x0C, 0x00, 0x68]))
    test("Parse response: rejects short data",
         short_resp is None,
         "result={}".format(short_resp))

    # --- Test 12: Decode known register - ChipAddress ---
    dec12 = decode_register(0x00, 0x00000008)
    test("Decode: ChipAddress 0x08 -> chip #2",
         "chip #2" in dec12["decoded"],
         "decoded='{}'".format(dec12["decoded"]))

    # --- Test 13: Decode known register - PLL ---
    dec13 = decode_register(0x0C, 0x00680261)
    test("Decode: PLL 0x00680261 -> 600 MHz",
         "600" in dec13["decoded"],
         "decoded='{}'".format(dec13["decoded"]))

    # --- Test 14: Decode known register - Version ---
    dec14 = decode_register(0x7C, 0x13871387)
    test("Decode: Version 0x13871387 -> BM1387",
         "BM1387" in dec14["decoded"],
         "decoded='{}'".format(dec14["decoded"]))

    # --- Test 15: Decode unknown register ---
    dec15 = decode_register(0xB0, 0xCAFEBABE)
    test("Decode: unknown register returns name",
         "Unknown" in dec15["name"],
         "name='{}'".format(dec15["name"]))

    # --- Test 16: MockUART read_register ---
    mock = MockUART()
    mock.open()
    result16 = mock.read_register(0x00, 0x7C)
    test("MockUART: read version register",
         result16 is not None and result16[1] == 0x13871387,
         "result={}".format(result16))

    # --- Test 17: MockUART scan ---
    results17 = scan_registers(mock, 0x00, 0x00, 0x0C, 4, verbose=False, progress=False)
    responded = [r for r in results17 if r["responded"]]
    test("MockUART: scan returns results",
         len(responded) > 0,
         "responded={}/{}".format(len(responded), len(results17)))

    # --- Test 18: Format table ---
    table = format_table(results17, show_empty=True)
    test("Format: table output contains header",
         "REG" in table and "VALUE" in table,
         "lines={}".format(len(table.split("\n"))))

    # --- Test 19: Format summary ---
    summary = format_summary(results17)
    test("Format: summary contains counts",
         "responded" in summary.lower() or "Registers" in summary,
         "summary_len={}".format(len(summary)))

    # --- Test 20: Known register lookup ---
    test("Registry: KNOWN_REGISTERS has ChipAddress at 0x00",
         0x00 in KNOWN_REGISTERS and KNOWN_REGISTERS[0x00][0] == "ChipAddress")

    # --- Test 21: Build commands for all chip addresses ---
    cmds_ok = True
    for chip_idx in range(MAX_CHIPS):
        addr = chip_idx * ADDR_INTERVAL
        cmd = build_read_register_cmd(addr & 0xFF, 0x00)
        if len(cmd) != 4 or cmd[0] != 0x54:
            cmds_ok = False
            break
    test("Build CMD: valid for all 63 chip addresses",
         cmds_ok)

    # --- Test 22: CRC5 with all-zero input ---
    crc_z = crc5_bm1387(bytes([0x00, 0x00, 0x00]))
    test("CRC5: all-zero input produces valid CRC",
         0 <= crc_z <= 0x1F,
         "crc=0x{:02X}".format(crc_z))

    # --- Test 23: CRC5 with all-FF input ---
    crc_f = crc5_bm1387(bytes([0xFF, 0xFF, 0xFF]))
    test("CRC5: all-0xFF input produces valid CRC",
         0 <= crc_f <= 0x1F,
         "crc=0x{:02X}".format(crc_f))

    # --- Test 24: JSON output structure ---
    mock2 = MockUART()
    mock2.open()
    results24 = scan_registers(mock2, 0x00, 0x00, 0x04, 4, verbose=False, progress=False)
    json_str = json.dumps(results24)
    json_back = json.loads(json_str)
    test("JSON: scan results serialize/deserialize",
         isinstance(json_back, list) and len(json_back) == len(results24))

    # --- Test 25: TicketMask decode ---
    dec25 = decode_register(0x14, 0x0000FFFF)
    test("Decode: TicketMask 0x0000FFFF -> diff ~1",
         "diff" in dec25["decoded"],
         "decoded='{}'".format(dec25["decoded"]))

    mock.close()
    return passed, failed, tests


# ============================================================================
# CLI
# ============================================================================

def print_help():
    help_text = """
register_scanner.py - BM1387 ASIC Register Scanner
====================================================

Usage:
  register_scanner.py [OPTIONS]

Options:
  --help              Show this help message
  --test              Run self-tests (no hardware required)
  --json              Output results as JSON
  --device PATH       UART device (default: /dev/ttyPS1)
  --chip ADDR         Chip address to scan (hex, e.g. 0x04). Default: 0x00
  --all-chips N       Scan all N chips in chain (addresses 0, 4, 8, ...)
  --reg-start HEX     Start register (default: 0x00)
  --reg-end HEX       End register (default: 0xFF)
  --reg-step N        Register step (default: 4)
  --verbose           Show each register as it's read
  --show-empty        Show registers that didn't respond
  --known-only        Only scan known register addresses

Examples:
  # Self-test (no hardware):
  register_scanner.py --test

  # Scan all registers on chip 0:
  register_scanner.py

  # Scan chip #2 (address 0x08), JSON output:
  register_scanner.py --chip 0x08 --json

  # Scan only known registers on all 63 chips:
  register_scanner.py --all-chips 63 --known-only

  # Scan registers 0x00-0x1F on chip 0:
  register_scanner.py --reg-start 0x00 --reg-end 0x1F
"""
    print(help_text.strip())


def parse_hex_arg(s):
    """Parse a hex or decimal string to int."""
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
        print("register_scanner.py - Self-Test Mode")
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
    show_empty = "--show-empty" in args
    known_only = "--known-only" in args

    device = UART_DEFAULT
    chip_addr = 0x00
    all_chips = None
    reg_start = 0x00
    reg_end = 0xFF
    reg_step = 4

    i = 0
    while i < len(args):
        if args[i] == "--device" and i + 1 < len(args):
            device = args[i + 1]
            i += 2
        elif args[i] == "--chip" and i + 1 < len(args):
            chip_addr = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--all-chips" and i + 1 < len(args):
            all_chips = int(args[i + 1])
            i += 2
        elif args[i] == "--reg-start" and i + 1 < len(args):
            reg_start = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--reg-end" and i + 1 < len(args):
            reg_end = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--reg-step" and i + 1 < len(args):
            reg_step = int(args[i + 1])
            i += 2
        else:
            i += 1

    # If known-only, build register list
    if known_only:
        reg_list = sorted(KNOWN_REGISTERS.keys())
    else:
        reg_list = None

    # Open UART
    uart = UARTInterface(device=device)
    try:
        uart.open()
    except Exception as e:
        sys.stderr.write("ERROR: Cannot open {}: {}\n".format(device, e))
        sys.stderr.write("  (Use --test for self-tests without hardware)\n")
        return 1

    try:
        if all_chips is not None:
            # Scan multiple chips
            if not json_mode:
                sys.stderr.write("Scanning {} chips, registers 0x{:02X}-0x{:02X}\n".format(
                    all_chips, reg_start, reg_end))

            if known_only:
                # Custom scan for known registers only
                all_results = {}
                for chip_idx in range(all_chips):
                    ca = chip_idx * ADDR_INTERVAL
                    chip_results = []
                    for reg in reg_list:
                        result = uart.read_register(ca, reg)
                        entry = {
                            "register": reg,
                            "register_hex": "0x{:02X}".format(reg),
                            "chip_addr": ca,
                            "responded": result is not None,
                            "value": result[1] if result else None,
                            "value_hex": "0x{:08X}".format(result[1]) if result else None,
                            "crc_ok": result[2] if result else None,
                            "known": True,
                        }
                        if result:
                            dec = decode_register(reg, result[1])
                            entry.update({"name": dec["name"], "description": dec["description"],
                                          "decoded": dec["decoded"]})
                        else:
                            entry.update({"name": KNOWN_REGISTERS[reg][0], "description": "", "decoded": ""})
                        chip_results.append(entry)
                    all_results[ca] = chip_results
            else:
                all_results = scan_all_chips(uart, all_chips, reg_start, reg_end, reg_step,
                                             verbose=verbose)

            if json_mode:
                output = {"scan_type": "multi_chip", "chip_count": all_chips,
                          "reg_range": [reg_start, reg_end], "chips": {}}
                for ca, results in all_results.items():
                    output["chips"][str(ca)] = results
                print(json.dumps(output, indent=2))
            else:
                for ca, results in all_results.items():
                    chip_idx = ca // ADDR_INTERVAL
                    print("\n=== Chip #{} (addr=0x{:02X}) ===".format(chip_idx, ca))
                    print(format_table(results, show_empty=show_empty))
                    print(format_summary(results))

        else:
            # Single chip scan
            if not json_mode:
                sys.stderr.write("Scanning chip addr=0x{:02X}, registers 0x{:02X}-0x{:02X}\n".format(
                    chip_addr, reg_start, reg_end))

            if known_only:
                results = []
                for reg in reg_list:
                    result = uart.read_register(chip_addr, reg)
                    entry = {
                        "register": reg,
                        "register_hex": "0x{:02X}".format(reg),
                        "chip_addr": chip_addr,
                        "responded": result is not None,
                        "value": result[1] if result else None,
                        "value_hex": "0x{:08X}".format(result[1]) if result else None,
                        "crc_ok": result[2] if result else None,
                        "known": True,
                    }
                    if result:
                        dec = decode_register(reg, result[1])
                        entry.update({"name": dec["name"], "description": dec["description"],
                                      "decoded": dec["decoded"]})
                    else:
                        entry.update({"name": KNOWN_REGISTERS[reg][0], "description": "", "decoded": ""})
                    results.append(entry)
            else:
                results = scan_registers(uart, chip_addr, reg_start, reg_end, reg_step,
                                         verbose=verbose)

            if json_mode:
                output = {"scan_type": "single_chip", "chip_addr": chip_addr,
                          "reg_range": [reg_start, reg_end], "registers": results}
                print(json.dumps(output, indent=2))
            else:
                print(format_table(results, show_empty=show_empty))
                print()
                print(format_summary(results))

    finally:
        uart.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
