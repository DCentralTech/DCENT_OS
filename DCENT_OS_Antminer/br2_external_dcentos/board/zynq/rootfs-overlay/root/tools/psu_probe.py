#!/usr/bin/env python3
"""
psu_probe.py - APW PSU Characterization via I2C/PMBus
======================================================
Probe Antminer APW PSUs (APW3++, APW7, APW12) via I2C PMBus protocol.
Direct hardware access via Linux I2C ioctl on /dev/i2c-X.

PMBus Protocol:
  - I2C_SLAVE ioctl (0x0703) to set target address
  - I2C_SMBUS ioctl (0x0720) for read/write transactions
  - Linear16 encoding: value = mantissa * 2^exponent
  - APW12 typically at 0x58, APW3++ at 0x00 (broadcast)

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
    HAS_FCNTL = True
except ImportError:
    HAS_FCNTL = False

try:
    import array
    HAS_ARRAY = True
except ImportError:
    HAS_ARRAY = False

# ============================================================================
# I2C / PMBus Constants
# ============================================================================

I2C_SLAVE = 0x0703
I2C_SMBUS = 0x0720

# SMBus transaction types
I2C_SMBUS_READ = 0
I2C_SMBUS_WRITE = 1
I2C_SMBUS_BYTE_DATA = 2
I2C_SMBUS_WORD_DATA = 3
I2C_SMBUS_BLOCK_DATA = 5

# Default I2C bus and addresses
I2C_BUS_DEFAULT = 1
I2C_DEV_TEMPLATE = "/dev/i2c-{}"

# Known APW PSU addresses
APW_ADDRESSES = {
    0x58: "APW12 (primary)",
    0x59: "APW12 (secondary)",
    0x5A: "APW12 (tertiary)",
    0x00: "APW3++ broadcast",
    0x40: "APW7 (primary)",
    0x41: "APW7 (secondary)",
    0x50: "Generic PMBus",
    0x51: "Generic PMBus alt",
}

# PMBus standard commands
PMBUS_COMMANDS = {
    0x01: ("OPERATION",          "byte",    "Operation control"),
    0x02: ("ON_OFF_CONFIG",      "byte",    "On/Off configuration"),
    0x03: ("CLEAR_FAULTS",       "send",    "Clear all faults"),
    0x10: ("WRITE_PROTECT",      "byte",    "Write protect"),
    0x19: ("CAPABILITY",         "byte",    "Device capability"),
    0x20: ("VOUT_MODE",          "byte",    "Output voltage mode"),
    0x21: ("VOUT_COMMAND",       "word",    "Output voltage setpoint"),
    0x24: ("VOUT_MAX",           "word",    "Maximum output voltage"),
    0x25: ("VOUT_MARGIN_HIGH",   "word",    "Voltage margin high"),
    0x26: ("VOUT_MARGIN_LOW",    "word",    "Voltage margin low"),
    0x40: ("VOUT_OV_FAULT_LIMIT", "word",   "OV fault limit"),
    0x44: ("VOUT_UV_FAULT_LIMIT", "word",   "UV fault limit"),
    0x46: ("IOUT_OC_FAULT_LIMIT", "word",   "OC fault limit"),
    0x4F: ("OT_FAULT_LIMIT",    "word",    "Over-temp fault limit"),
    0x51: ("OT_WARN_LIMIT",     "word",    "Over-temp warning limit"),
    0x79: ("STATUS_WORD",        "word",    "Status word (fault summary)"),
    0x7A: ("STATUS_VOUT",        "byte",    "Output voltage status"),
    0x7B: ("STATUS_IOUT",        "byte",    "Output current status"),
    0x7C: ("STATUS_INPUT",       "byte",    "Input status"),
    0x7D: ("STATUS_TEMPERATURE", "byte",    "Temperature status"),
    0x7E: ("STATUS_CML",         "byte",    "Communication status"),
    0x80: ("STATUS_MFR_SPECIFIC", "byte",   "Manufacturer specific status"),
    0x88: ("READ_VIN",           "word",    "Input voltage"),
    0x89: ("READ_IIN",           "word",    "Input current"),
    0x8B: ("READ_VOUT",          "word",    "Output voltage"),
    0x8C: ("READ_IOUT",          "word",    "Output current"),
    0x8D: ("READ_TEMPERATURE_1", "word",    "Temperature sensor 1"),
    0x8E: ("READ_TEMPERATURE_2", "word",    "Temperature sensor 2"),
    0x8F: ("READ_TEMPERATURE_3", "word",    "Temperature sensor 3"),
    0x90: ("READ_FAN_SPEED_1",  "word",    "Fan speed 1 (RPM)"),
    0x91: ("READ_FAN_SPEED_2",  "word",    "Fan speed 2 (RPM)"),
    0x96: ("READ_POUT",          "word",    "Output power"),
    0x97: ("READ_PIN",           "word",    "Input power"),
    0x98: ("PMBUS_REVISION",     "byte",    "PMBus revision"),
    0x99: ("MFR_ID",             "block",   "Manufacturer ID"),
    0x9A: ("MFR_MODEL",          "block",   "Manufacturer model"),
    0x9B: ("MFR_REVISION",       "block",   "Manufacturer revision"),
    0x9C: ("MFR_LOCATION",       "block",   "Manufacturing location"),
    0x9D: ("MFR_DATE",           "block",   "Manufacturing date"),
    0x9E: ("MFR_SERIAL",         "block",   "Serial number"),
    0xAD: ("IC_DEVICE_ID",       "block",   "IC device identifier"),
    0xAE: ("IC_DEVICE_REV",      "block",   "IC device revision"),
}

# Key read commands for quick status
KEY_COMMANDS = [0x88, 0x89, 0x8B, 0x8C, 0x8D, 0x90, 0x96, 0x97, 0x79]

# Status word bit definitions
STATUS_WORD_BITS = {
    0:  "NONE_OF_THE_ABOVE",
    1:  "CML_FAULT",
    2:  "TEMPERATURE_FAULT",
    3:  "VIN_UV_FAULT",
    4:  "IOUT_OC_FAULT",
    5:  "VOUT_OV_FAULT",
    6:  "UNIT_OFF",
    7:  "BUSY",
    8:  "UNKNOWN_FAULT",
    9:  "OTHER_FAULT",
    10: "FAN_FAULT",
    11: "POWER_GOOD_NEGATED",
    12: "MFR_SPECIFIC_FAULT",
    13: "INPUT_FAULT",
    14: "IOUT_POUT_FAULT",
    15: "VOUT_FAULT",
}


# ============================================================================
# PMBus Linear16 Encoding
# ============================================================================

def linear16_to_float(raw_word):
    """
    Decode PMBus Linear16 (Linear Data Format) value.
    Format: [15:11] = signed exponent (5-bit two's complement)
            [10:0]  = unsigned mantissa (11 bits)
    Value = mantissa * 2^exponent

    Per PMBus spec, the mantissa is unsigned for measurement commands
    (READ_VIN, READ_VOUT, READ_IOUT, etc.). The exponent is always
    signed (two's complement 5-bit).
    """
    if raw_word is None:
        return None

    # Extract exponent (signed 5-bit two's complement, bits 15:11)
    exponent = (raw_word >> 11) & 0x1F
    if exponent >= 16:
        exponent -= 32  # Sign extend 5-bit to Python int

    # Extract mantissa (unsigned 11-bit, bits 10:0)
    mantissa = raw_word & 0x7FF

    return mantissa * (2.0 ** exponent)


def float_to_linear16(value, exponent=None):
    """
    Encode a float to PMBus Linear16.
    If exponent is None, auto-select the best exponent to maximize precision.
    Mantissa is unsigned 11-bit (0-2047).
    """
    if exponent is None:
        # Auto-select exponent: find smallest exponent where mantissa fits in 11 bits
        for exp in range(-16, 16):
            m = value / (2.0 ** exp)
            if 0 <= m <= 2047:
                exponent = exp
                break
        if exponent is None:
            exponent = 0  # fallback

    mantissa = int(round(value / (2.0 ** exponent)))
    if mantissa > 2047:
        mantissa = 2047
    elif mantissa < 0:
        mantissa = 0

    exp_bits = exponent & 0x1F
    mant_bits = mantissa & 0x7FF
    return (exp_bits << 11) | mant_bits


def linear11_to_float(raw_word):
    """
    Decode PMBus Linear11 (used for VOUT in some modes).
    Same as Linear16 but mantissa is unsigned.
    """
    exponent = (raw_word >> 11) & 0x1F
    if exponent >= 16:
        exponent -= 32
    mantissa = raw_word & 0x7FF
    return mantissa * (2.0 ** exponent)


def decode_vout_direct(raw_word, vout_mode_exp=-8):
    """
    Decode VOUT in DIRECT mode (Linear16 with separate exponent from VOUT_MODE).
    VOUT_MODE byte: [7:5]=mode, [4:0]=signed exponent
    mode 0 = Linear, mode 1 = VID, mode 2 = Direct
    """
    return raw_word * (2.0 ** vout_mode_exp)


def decode_status_word(raw_word):
    """Decode PMBus STATUS_WORD into list of active faults."""
    faults = []
    for bit, name in STATUS_WORD_BITS.items():
        if raw_word & (1 << bit):
            faults.append(name)
    return faults


# ============================================================================
# I2C Hardware Interface
# ============================================================================

class I2CInterface:
    """Direct I2C access via Linux ioctl."""

    def __init__(self, bus=I2C_BUS_DEFAULT):
        self.bus = bus
        self.device = I2C_DEV_TEMPLATE.format(bus)
        self.fd = None
        self.current_addr = None

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
        """Set I2C slave address via ioctl."""
        if not HAS_FCNTL:
            raise RuntimeError("fcntl not available")
        fcntl.ioctl(self.fd, I2C_SLAVE, addr)
        self.current_addr = addr

    def smbus_read_byte_data(self, addr, cmd):
        """Read a single byte from PMBus command register."""
        self.set_slave(addr)
        # Use I2C_SMBUS ioctl
        # struct i2c_smbus_ioctl_data { read_write, command, size, data }
        # We build this as a ctypes-free approach using struct + fcntl
        if HAS_ARRAY:
            data = array.array('B', [0] * 34)  # i2c_smbus_data union (34 bytes)
            # Pack: read_write(1B) + command(1B) + size(4B) + pointer(8B)
            # Simplified: write command byte, read 1 byte back
            try:
                os.write(self.fd, bytes([cmd]))
                result = os.read(self.fd, 1)
                if result:
                    return result[0]
            except Exception:
                pass
        return None

    def smbus_read_word_data(self, addr, cmd):
        """Read a 16-bit word from PMBus command register."""
        self.set_slave(addr)
        try:
            os.write(self.fd, bytes([cmd]))
            result = os.read(self.fd, 2)
            if result and len(result) == 2:
                # PMBus words are little-endian
                return result[0] | (result[1] << 8)
        except Exception:
            pass
        return None

    def smbus_read_block_data(self, addr, cmd, max_len=32):
        """Read a block of data from PMBus command register."""
        self.set_slave(addr)
        try:
            os.write(self.fd, bytes([cmd]))
            result = os.read(self.fd, max_len + 1)
            if result and len(result) > 0:
                # First byte is length in SMBus block read
                block_len = result[0]
                if block_len <= max_len and len(result) > block_len:
                    return result[1:block_len + 1]
                else:
                    return result
        except Exception:
            pass
        return None

    def pmbus_read(self, addr, cmd_code):
        """Read a PMBus register using appropriate transaction type."""
        cmd_info = PMBUS_COMMANDS.get(cmd_code)
        if cmd_info is None:
            # Try as word
            return self.smbus_read_word_data(addr, cmd_code)

        name, dtype, desc = cmd_info
        if dtype == "byte":
            return self.smbus_read_byte_data(addr, cmd_code)
        elif dtype == "word":
            return self.smbus_read_word_data(addr, cmd_code)
        elif dtype == "block":
            return self.smbus_read_block_data(addr, cmd_code)
        elif dtype == "send":
            return None  # Send-only commands
        return None

    def scan_bus(self, addr_start=0x08, addr_end=0x77):
        """Scan I2C bus for responding devices."""
        found = []
        for addr in range(addr_start, addr_end + 1):
            try:
                self.set_slave(addr)
                os.read(self.fd, 1)
                found.append(addr)
            except Exception:
                pass
        return found


# ============================================================================
# Mock I2C for Testing
# ============================================================================

class MockI2C:
    """Simulated I2C/PMBus for --test mode."""

    def __init__(self):
        self.bus = 1
        self.device = "/dev/mock_i2c-1"
        self.current_addr = None
        self._registers = {}
        self._setup_mock_psu()

    def _setup_mock_psu(self):
        """Set up simulated APW12 PSU at 0x58."""
        addr = 0x58
        # Input: 220V AC rectified ~ 310V DC bus, 5A
        self._registers[(addr, 0x88)] = self._encode_linear16(220.0)   # VIN
        self._registers[(addr, 0x89)] = self._encode_linear16(5.2)     # IIN
        # Output: 12.1V at 125A (hash board supply)
        self._registers[(addr, 0x8B)] = self._encode_linear16(12.1)    # VOUT
        self._registers[(addr, 0x8C)] = self._encode_linear16(125.0)   # IOUT
        # Temperature: 42C
        self._registers[(addr, 0x8D)] = self._encode_linear16(42.0)    # TEMP1
        self._registers[(addr, 0x8E)] = self._encode_linear16(38.0)    # TEMP2
        # Fan: 4500 RPM
        self._registers[(addr, 0x90)] = self._encode_linear16(4500.0)  # FAN1
        # Power
        self._registers[(addr, 0x96)] = self._encode_linear16(1512.5)  # POUT
        self._registers[(addr, 0x97)] = self._encode_linear16(1580.0)  # PIN
        # Status: all good (no faults)
        self._registers[(addr, 0x79)] = 0x0000  # STATUS_WORD
        self._registers[(addr, 0x7A)] = 0x00    # STATUS_VOUT
        self._registers[(addr, 0x7B)] = 0x00    # STATUS_IOUT
        self._registers[(addr, 0x7C)] = 0x00    # STATUS_INPUT
        self._registers[(addr, 0x7D)] = 0x00    # STATUS_TEMPERATURE
        # VOUT_MODE: Linear mode, exponent = -8
        self._registers[(addr, 0x20)] = 0x18  # mode=0, exp=-8 (0x18 = 0b11000 = -8 two's comp 5-bit)
        # PMBus revision
        self._registers[(addr, 0x98)] = 0x22  # PMBus 1.2
        # Manufacturer info (as block data)
        self._registers[(addr, 0x99)] = b"BITMAIN"     # MFR_ID
        self._registers[(addr, 0x9A)] = b"APW12"       # MFR_MODEL
        self._registers[(addr, 0x9B)] = b"REV1.0"      # MFR_REVISION
        self._registers[(addr, 0x9E)] = b"SN2024001"   # MFR_SERIAL

        # Second simulated device at 0x40
        addr2 = 0x40
        self._registers[(addr2, 0x88)] = self._encode_linear16(218.0)
        self._registers[(addr2, 0x8B)] = self._encode_linear16(12.05)
        self._registers[(addr2, 0x8C)] = self._encode_linear16(60.0)

    def _encode_linear16(self, value, exponent=-5):
        """Encode a float as Linear16."""
        mantissa = int(round(value / (2.0 ** exponent)))
        if mantissa > 1023:
            mantissa = 1023
        if mantissa < -1024:
            mantissa = -1024
        exp_bits = exponent & 0x1F
        mant_bits = mantissa & 0x7FF
        return (exp_bits << 11) | mant_bits

    def open(self):
        pass

    def close(self):
        pass

    def set_slave(self, addr):
        self.current_addr = addr

    def smbus_read_byte_data(self, addr, cmd):
        val = self._registers.get((addr, cmd))
        if val is not None and isinstance(val, int):
            return val & 0xFF
        return None

    def smbus_read_word_data(self, addr, cmd):
        val = self._registers.get((addr, cmd))
        if val is not None and isinstance(val, int):
            return val & 0xFFFF
        return None

    def smbus_read_block_data(self, addr, cmd, max_len=32):
        val = self._registers.get((addr, cmd))
        if val is not None and isinstance(val, bytes):
            return val
        return None

    def pmbus_read(self, addr, cmd_code):
        cmd_info = PMBUS_COMMANDS.get(cmd_code)
        if cmd_info is None:
            return self.smbus_read_word_data(addr, cmd_code)
        name, dtype, desc = cmd_info
        if dtype == "byte":
            return self.smbus_read_byte_data(addr, cmd_code)
        elif dtype == "word":
            return self.smbus_read_word_data(addr, cmd_code)
        elif dtype == "block":
            return self.smbus_read_block_data(addr, cmd_code)
        return None

    def scan_bus(self, addr_start=0x08, addr_end=0x77):
        found = []
        for addr in range(addr_start, addr_end + 1):
            # Check if any registers exist for this address
            for key in self._registers:
                if key[0] == addr:
                    found.append(addr)
                    break
        return found


# ============================================================================
# PSU Probe Logic
# ============================================================================

def probe_psu_address(i2c, addr, verbose=False):
    """
    Read all key PMBus registers from a PSU at given address.
    Returns dict of readings.
    """
    readings = collections.OrderedDict()

    # First try to get VOUT_MODE for voltage decoding
    vout_mode = i2c.pmbus_read(addr, 0x20)
    vout_exp = -8  # default
    if vout_mode is not None and isinstance(vout_mode, int):
        mode = (vout_mode >> 5) & 0x07
        exp_raw = vout_mode & 0x1F
        if exp_raw >= 16:
            exp_raw -= 32
        vout_exp = exp_raw
        readings["vout_mode"] = {
            "raw": vout_mode,
            "mode": mode,
            "mode_str": ["Linear", "VID", "Direct"][mode] if mode < 3 else "Unknown",
            "exponent": vout_exp,
        }

    # Read all standard commands
    for cmd_code in sorted(PMBUS_COMMANDS.keys()):
        name, dtype, desc = PMBUS_COMMANDS[cmd_code]

        raw = i2c.pmbus_read(addr, cmd_code)
        if raw is None:
            continue

        entry = {
            "command": cmd_code,
            "command_hex": "0x{:02X}".format(cmd_code),
            "name": name,
            "description": desc,
            "raw": raw if not isinstance(raw, bytes) else raw.hex() if hasattr(raw, 'hex') else str(raw),
        }

        if dtype == "word" and isinstance(raw, int):
            entry["raw_hex"] = "0x{:04X}".format(raw)

            # Decode based on command
            if cmd_code == 0x79:  # STATUS_WORD
                entry["faults"] = decode_status_word(raw)
                entry["all_clear"] = len(entry["faults"]) == 0 or entry["faults"] == ["NONE_OF_THE_ABOVE"]
            elif cmd_code in (0x88, 0x89, 0x8C, 0x8D, 0x8E, 0x8F, 0x90, 0x91, 0x96, 0x97):
                # Linear16 encoded values
                decoded = linear16_to_float(raw)
                entry["value"] = round(decoded, 3) if decoded else None
                if cmd_code == 0x88:
                    entry["unit"] = "V"
                elif cmd_code == 0x89:
                    entry["unit"] = "A"
                elif cmd_code == 0x8B:
                    entry["unit"] = "V"
                elif cmd_code == 0x8C:
                    entry["unit"] = "A"
                elif cmd_code in (0x8D, 0x8E, 0x8F):
                    entry["unit"] = "C"
                elif cmd_code in (0x90, 0x91):
                    entry["unit"] = "RPM"
                elif cmd_code in (0x96, 0x97):
                    entry["unit"] = "W"
            elif cmd_code == 0x8B:  # VOUT - may use VOUT_MODE exponent
                decoded = decode_vout_direct(raw, vout_exp)
                entry["value"] = round(decoded, 3) if decoded else None
                entry["unit"] = "V"

        elif dtype == "byte" and isinstance(raw, int):
            entry["raw_hex"] = "0x{:02X}".format(raw)

        elif dtype == "block" and isinstance(raw, (bytes, bytearray)):
            try:
                entry["text"] = raw.decode("ascii", errors="replace").strip("\x00")
            except Exception:
                entry["text"] = str(raw)

        readings[name] = entry

        if verbose:
            val_str = ""
            if "value" in entry and entry["value"] is not None:
                val_str = " = {:.3f} {}".format(entry["value"], entry.get("unit", ""))
            elif "text" in entry:
                val_str = ' = "{}"'.format(entry["text"])
            sys.stderr.write("  [0x{:02X}] {}: 0x{}{}\n".format(
                cmd_code, name,
                entry.get("raw_hex", str(raw))[-4:] if isinstance(raw, int) else str(raw),
                val_str))

    return readings


def format_psu_report(addr, readings, addr_label=""):
    """Format PSU probe results as human-readable text."""
    lines = []
    label = addr_label or APW_ADDRESSES.get(addr, "Unknown")
    lines.append("PSU at I2C address 0x{:02X} ({})".format(addr, label))
    lines.append("=" * 60)

    # Manufacturer info
    mfr_fields = ["MFR_ID", "MFR_MODEL", "MFR_REVISION", "MFR_SERIAL", "MFR_DATE"]
    has_mfr = False
    for field in mfr_fields:
        if field in readings and "text" in readings[field]:
            if not has_mfr:
                lines.append("--- Identification ---")
                has_mfr = True
            lines.append("  {}: {}".format(field, readings[field]["text"]))

    # Key measurements
    lines.append("")
    lines.append("--- Measurements ---")

    measurement_cmds = [
        (0x88, "Input Voltage",  "V"),
        (0x89, "Input Current",  "A"),
        (0x8B, "Output Voltage", "V"),
        (0x8C, "Output Current", "A"),
        (0x96, "Output Power",   "W"),
        (0x97, "Input Power",    "W"),
        (0x8D, "Temperature 1",  "C"),
        (0x8E, "Temperature 2",  "C"),
        (0x90, "Fan Speed 1",    "RPM"),
    ]

    for cmd, label, unit in measurement_cmds:
        name = PMBUS_COMMANDS.get(cmd, ("",))[0]
        if name in readings:
            entry = readings[name]
            if "value" in entry and entry["value"] is not None:
                lines.append("  {:<20} {:>10.3f} {}".format(label + ":", entry["value"], unit))
            else:
                lines.append("  {:<20} {:>10} (raw: {})".format(
                    label + ":", "?", entry.get("raw_hex", "?")))
        else:
            lines.append("  {:<20} {:>10}".format(label + ":", "(not available)"))

    # Efficiency
    pin_entry = readings.get("READ_PIN")
    pout_entry = readings.get("READ_POUT")
    if pin_entry and pout_entry:
        pin_val = pin_entry.get("value")
        pout_val = pout_entry.get("value")
        if pin_val and pout_val and pin_val > 0:
            eff = (pout_val / pin_val) * 100
            lines.append("  {:<20} {:>9.1f}%".format("Efficiency:", eff))

    # Status
    lines.append("")
    lines.append("--- Status ---")
    status_entry = readings.get("STATUS_WORD")
    if status_entry:
        if status_entry.get("all_clear"):
            lines.append("  Status: ALL CLEAR (no faults)")
        else:
            faults = status_entry.get("faults", [])
            lines.append("  Status: FAULTS DETECTED")
            for f in faults:
                if f != "NONE_OF_THE_ABOVE":
                    lines.append("    - {}".format(f))
    else:
        lines.append("  Status: (could not read)")

    return "\n".join(lines)


def scan_for_psus(i2c, addr_start=0x10, addr_end=0x60, verbose=False):
    """Scan I2C bus for PSU devices."""
    found = []

    if verbose:
        sys.stderr.write("Scanning I2C bus for PSU devices (0x{:02X}-0x{:02X})...\n".format(
            addr_start, addr_end))

    for addr in range(addr_start, addr_end + 1):
        try:
            # Try to read a common PMBus register
            result = i2c.pmbus_read(addr, 0x98)  # PMBUS_REVISION
            if result is not None:
                label = APW_ADDRESSES.get(addr, "Unknown device")
                found.append({"address": addr, "address_hex": "0x{:02X}".format(addr),
                              "label": label, "pmbus_rev": result})
                if verbose:
                    sys.stderr.write("  Found device at 0x{:02X}: {}\n".format(addr, label))
                continue

            # Also try READ_VIN as fallback
            result = i2c.pmbus_read(addr, 0x88)
            if result is not None:
                label = APW_ADDRESSES.get(addr, "Unknown device")
                found.append({"address": addr, "address_hex": "0x{:02X}".format(addr),
                              "label": label})
                if verbose:
                    sys.stderr.write("  Found device at 0x{:02X}: {}\n".format(addr, label))
        except Exception:
            pass

    return found


def watchdog_test(i2c, addr, verbose=False):
    """
    Watchdog/timeout characterization (READ ONLY - safe).
    Tests how the PSU responds to rapid status polling.
    Does NOT write any registers.

    Returns timing data about PSU responsiveness.
    """
    results = {
        "address": addr,
        "address_hex": "0x{:02X}".format(addr),
        "test": "watchdog_characterization",
        "warning": "READ-ONLY test, no writes performed",
        "timings": [],
    }

    # Test 1: Single read latency
    times = []
    for i in range(10):
        t0 = time.time()
        result = i2c.pmbus_read(addr, 0x79)  # STATUS_WORD
        t1 = time.time()
        if result is not None:
            times.append(t1 - t0)

    if times:
        results["single_read_ms"] = {
            "min": round(min(times) * 1000, 2),
            "max": round(max(times) * 1000, 2),
            "avg": round(sum(times) / len(times) * 1000, 2),
            "count": len(times),
        }

    # Test 2: Burst read (rapid consecutive reads)
    burst_ok = 0
    burst_fail = 0
    t_start = time.time()
    for i in range(50):
        result = i2c.pmbus_read(addr, 0x88)  # READ_VIN
        if result is not None:
            burst_ok += 1
        else:
            burst_fail += 1
    t_end = time.time()

    results["burst_read"] = {
        "total": 50,
        "success": burst_ok,
        "failed": burst_fail,
        "total_time_ms": round((t_end - t_start) * 1000, 2),
        "reads_per_second": round(50.0 / (t_end - t_start), 1) if (t_end - t_start) > 0 else 0,
    }

    # Test 3: Status stability over time
    status_values = []
    for i in range(5):
        result = i2c.pmbus_read(addr, 0x79)
        if result is not None:
            status_values.append(result)
        time.sleep(0.1)

    results["status_stability"] = {
        "readings": len(status_values),
        "unique_values": len(set(status_values)),
        "stable": len(set(status_values)) <= 1,
        "values": ["0x{:04X}".format(v) for v in status_values],
    }

    if verbose:
        sys.stderr.write("Watchdog test results:\n")
        if "single_read_ms" in results:
            sys.stderr.write("  Single read: avg={:.2f}ms\n".format(
                results["single_read_ms"]["avg"]))
        sys.stderr.write("  Burst: {}/{} OK, {:.1f} reads/sec\n".format(
            burst_ok, 50, results["burst_read"]["reads_per_second"]))
        sys.stderr.write("  Status stable: {}\n".format(
            results["status_stability"]["stable"]))

    return results


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

    # --- Test 1: Linear16 decode - positive ---
    # exponent=-5 (0x1B=27, signed=-5), mantissa=220*32=7040
    # But let's use a known encoding:
    # 220V with exp=-5: mantissa = 220 / 2^-5 = 220*32 = 7040
    # raw = (0x1B << 11) | 7040 = (27 << 11) | 7040
    raw_220v = (0x1B << 11) | (7040 & 0x7FF)
    decoded_220 = linear16_to_float(raw_220v)
    test("Linear16 decode: 220V",
         decoded_220 is not None and abs(decoded_220 - 220.0) < 1.0,
         "decoded={:.3f}".format(decoded_220 if decoded_220 else 0))

    # --- Test 2: Linear16 decode - small value ---
    # 12.1V with exp=-8: mantissa = 12.1 * 256 = 3097.6 -> 3098 = 0xC1A
    # But 11 bits max = 2047, so need different exponent
    # exp=-5: mantissa = 12.1 * 32 = 387.2 -> 387
    raw_12v = (0x1B << 11) | (387 & 0x7FF)
    decoded_12 = linear16_to_float(raw_12v)
    test("Linear16 decode: ~12V",
         decoded_12 is not None and abs(decoded_12 - 12.09375) < 1.0,
         "decoded={:.3f}".format(decoded_12 if decoded_12 else 0))

    # --- Test 3: Linear16 decode - temperature ---
    # 42C with exp=-5: mantissa = 42 * 32 = 1344
    raw_42c = (0x1B << 11) | (1344 & 0x7FF)
    decoded_42 = linear16_to_float(raw_42c)
    test("Linear16 decode: 42C temperature",
         decoded_42 is not None and abs(decoded_42 - 42.0) < 1.0,
         "decoded={:.3f}".format(decoded_42 if decoded_42 else 0))

    # --- Test 4: Linear16 decode - zero ---
    decoded_zero = linear16_to_float(0x0000)
    test("Linear16 decode: zero",
         decoded_zero == 0.0,
         "decoded={}".format(decoded_zero))

    # --- Test 5: Linear16 encode roundtrip ---
    encoded = float_to_linear16(42.0, exponent=-8)
    decoded_back = linear16_to_float(encoded)
    test("Linear16 roundtrip: 42.0 -> encode -> decode",
         decoded_back is not None and abs(decoded_back - 42.0) < 0.1,
         "encoded=0x{:04X} decoded={:.3f}".format(encoded, decoded_back or 0))

    # --- Test 6: Linear16 negative exponent ---
    # exp=-10 (0x16=22, signed=-10), mantissa = 100
    raw_neg = (22 << 11) | 100
    decoded_neg = linear16_to_float(raw_neg)
    expected = 100 * (2.0 ** -10)
    test("Linear16 decode: negative exponent",
         decoded_neg is not None and abs(decoded_neg - expected) < 0.001,
         "decoded={:.6f} expected={:.6f}".format(decoded_neg or 0, expected))

    # --- Test 7: Status word decode - no faults ---
    faults_clear = decode_status_word(0x0000)
    test("Status decode: no faults",
         len(faults_clear) == 0,
         "faults={}".format(faults_clear))

    # --- Test 8: Status word decode - OV fault ---
    faults_ov = decode_status_word(1 << 5)
    test("Status decode: VOUT_OV_FAULT",
         "VOUT_OV_FAULT" in faults_ov,
         "faults={}".format(faults_ov))

    # --- Test 9: Status word decode - multiple faults ---
    faults_multi = decode_status_word((1 << 2) | (1 << 4) | (1 << 10))
    test("Status decode: multiple faults",
         "TEMPERATURE_FAULT" in faults_multi and "IOUT_OC_FAULT" in faults_multi and "FAN_FAULT" in faults_multi,
         "faults={}".format(faults_multi))

    # --- Test 10: MockI2C bus scan ---
    mock = MockI2C()
    mock.open()
    found = mock.scan_bus(0x08, 0x77)
    test("MockI2C: bus scan finds devices",
         len(found) >= 1,
         "found={} devices".format(len(found)))

    # --- Test 11: MockI2C read VIN ---
    vin_raw = mock.pmbus_read(0x58, 0x88)
    test("MockI2C: read VIN raw",
         vin_raw is not None and isinstance(vin_raw, int),
         "raw=0x{:04X}".format(vin_raw or 0))

    # --- Test 12: MockI2C decode VIN ---
    vin_decoded = linear16_to_float(vin_raw) if vin_raw else None
    test("MockI2C: VIN decodes to ~220V",
         vin_decoded is not None and 200 < vin_decoded < 250,
         "vin={:.1f}V".format(vin_decoded or 0))

    # --- Test 13: MockI2C read VOUT ---
    vout_raw = mock.pmbus_read(0x58, 0x8B)
    vout_decoded = linear16_to_float(vout_raw) if vout_raw else None
    test("MockI2C: VOUT decodes to ~12V",
         vout_decoded is not None and 10 < vout_decoded < 15,
         "vout={:.3f}V".format(vout_decoded or 0))

    # --- Test 14: MockI2C read temperature ---
    temp_raw = mock.pmbus_read(0x58, 0x8D)
    temp_decoded = linear16_to_float(temp_raw) if temp_raw else None
    test("MockI2C: temperature decodes to ~42C",
         temp_decoded is not None and 30 < temp_decoded < 60,
         "temp={:.1f}C".format(temp_decoded or 0))

    # --- Test 15: MockI2C status word ---
    status = mock.pmbus_read(0x58, 0x79)
    test("MockI2C: status word = no faults",
         status == 0x0000,
         "status=0x{:04X}".format(status or 0))

    # --- Test 16: MockI2C block data ---
    mfr_model = mock.pmbus_read(0x58, 0x9A)
    test("MockI2C: MFR_MODEL reads as bytes",
         mfr_model is not None and isinstance(mfr_model, bytes),
         "model={}".format(mfr_model))

    # --- Test 17: MockI2C model decode ---
    model_str = mfr_model.decode("ascii") if isinstance(mfr_model, bytes) else ""
    test("MockI2C: MFR_MODEL = APW12",
         "APW12" in model_str,
         "model='{}'".format(model_str))

    # --- Test 18: Full probe ---
    readings = probe_psu_address(mock, 0x58, verbose=False)
    test("Full probe: returns readings dict",
         isinstance(readings, dict) and len(readings) > 0,
         "readings_count={}".format(len(readings)))

    # --- Test 19: Format report ---
    report = format_psu_report(0x58, readings)
    test("Format report: contains voltage info",
         "Voltage" in report or "voltage" in report.lower(),
         "report_lines={}".format(len(report.split("\n"))))

    # --- Test 20: PSU scan ---
    found2 = scan_for_psus(mock, 0x10, 0x60, verbose=False)
    test("PSU scan: finds APW12 at 0x58",
         any(d["address"] == 0x58 for d in found2),
         "found={}".format(len(found2)))

    # --- Test 21: Watchdog test ---
    wd_results = watchdog_test(mock, 0x58, verbose=False)
    test("Watchdog test: returns results",
         "burst_read" in wd_results and "status_stability" in wd_results,
         "keys={}".format(list(wd_results.keys())))

    # --- Test 22: JSON serialization ---
    json_str = json.dumps(readings, default=str)
    json_back = json.loads(json_str)
    test("JSON: readings serialize/deserialize",
         isinstance(json_back, dict))

    # --- Test 23: PMBUS_COMMANDS completeness ---
    test("PMBUS_COMMANDS: has all key read commands",
         all(c in PMBUS_COMMANDS for c in KEY_COMMANDS),
         "key_commands={}".format(KEY_COMMANDS))

    # --- Test 24: APW_ADDRESSES lookup ---
    test("APW_ADDRESSES: has APW12 at 0x58",
         0x58 in APW_ADDRESSES and "APW12" in APW_ADDRESSES[0x58])

    # --- Test 25: Linear16 edge case - max mantissa ---
    raw_max = (0x00 << 11) | 0x7FF  # exp=0, mantissa=2047
    decoded_max = linear16_to_float(raw_max)
    test("Linear16: max mantissa decodes correctly",
         decoded_max == 2047.0,
         "decoded={}".format(decoded_max))

    mock.close()
    return passed, failed, tests


# ============================================================================
# CLI
# ============================================================================

def print_help():
    help_text = """
psu_probe.py - APW PSU Characterization via I2C/PMBus
=======================================================

Usage:
  psu_probe.py [OPTIONS]

Options:
  --help              Show this help message
  --test              Run self-tests (no hardware required)
  --json              Output results as JSON
  --bus N             I2C bus number (default: 1 -> /dev/i2c-1)
  --addr HEX          PSU I2C address (default: 0x58 for APW12)
  --scan              Scan bus for PMBus devices first
  --scan-range LO-HI  Address range to scan (default: 0x10-0x60)
  --all               Probe all found PSU addresses
  --watchdog          Run watchdog/timing characterization (READ ONLY)
  --verbose           Verbose output

PSU Models:
  APW12:    Address 0x58 (primary), 0x59/0x5A (secondary/tertiary)
  APW7:     Address 0x40/0x41
  APW3++:   Address 0x00 (broadcast)

Examples:
  # Self-test (no hardware):
  psu_probe.py --test

  # Probe default APW12 at 0x58:
  psu_probe.py

  # Scan bus and probe all PSUs:
  psu_probe.py --scan --all

  # Probe specific address, JSON output:
  psu_probe.py --addr 0x40 --json

  # Watchdog characterization:
  psu_probe.py --addr 0x58 --watchdog

  # Use different I2C bus:
  psu_probe.py --bus 0 --addr 0x58
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
        print("psu_probe.py - Self-Test Mode")
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
    do_scan = "--scan" in args
    do_all = "--all" in args
    do_watchdog = "--watchdog" in args

    bus = I2C_BUS_DEFAULT
    addr = 0x58
    scan_lo = 0x10
    scan_hi = 0x60

    i = 0
    while i < len(args):
        if args[i] == "--bus" and i + 1 < len(args):
            bus = int(args[i + 1])
            i += 2
        elif args[i] == "--addr" and i + 1 < len(args):
            addr = parse_hex_arg(args[i + 1])
            i += 2
        elif args[i] == "--scan-range" and i + 1 < len(args):
            parts = args[i + 1].split("-")
            scan_lo = parse_hex_arg(parts[0])
            scan_hi = parse_hex_arg(parts[1]) if len(parts) > 1 else scan_lo
            i += 2
        else:
            i += 1

    # Open I2C
    i2c = I2CInterface(bus=bus)
    try:
        i2c.open()
    except Exception as e:
        sys.stderr.write("ERROR: Cannot open /dev/i2c-{}: {}\n".format(bus, e))
        sys.stderr.write("  (Use --test for self-tests without hardware)\n")
        return 1

    try:
        addresses_to_probe = []

        if do_scan or do_all:
            # Scan for devices
            found = scan_for_psus(i2c, scan_lo, scan_hi, verbose=verbose)
            if not json_mode:
                sys.stderr.write("Found {} PMBus device(s)\n".format(len(found)))

            if do_all:
                addresses_to_probe = [d["address"] for d in found]
            elif found:
                addresses_to_probe = [found[0]["address"]]
            else:
                if not json_mode:
                    sys.stderr.write("No PMBus devices found.\n")
                if json_mode:
                    print(json.dumps({"scan": found, "error": "no devices found"}))
                return 1
        else:
            addresses_to_probe = [addr]

        all_outputs = {}

        for probe_addr in addresses_to_probe:
            if not json_mode:
                sys.stderr.write("Probing PSU at 0x{:02X}...\n".format(probe_addr))

            readings = probe_psu_address(i2c, probe_addr, verbose=verbose)

            if do_watchdog:
                wd_results = watchdog_test(i2c, probe_addr, verbose=verbose)
                readings["_watchdog"] = wd_results

            all_outputs[probe_addr] = readings

            if not json_mode:
                print()
                print(format_psu_report(probe_addr, readings))
                if do_watchdog:
                    print("\n--- Watchdog Test ---")
                    wd = readings["_watchdog"]
                    if "single_read_ms" in wd:
                        print("  Read latency: avg={:.2f}ms min={:.2f}ms max={:.2f}ms".format(
                            wd["single_read_ms"]["avg"],
                            wd["single_read_ms"]["min"],
                            wd["single_read_ms"]["max"]))
                    br = wd.get("burst_read", {})
                    print("  Burst read: {}/{} OK ({:.1f} reads/sec)".format(
                        br.get("success", 0), br.get("total", 0),
                        br.get("reads_per_second", 0)))
                    ss = wd.get("status_stability", {})
                    print("  Status stable: {}".format(ss.get("stable", "?")))
                print()

        if json_mode:
            output = {}
            for probe_addr, readings in all_outputs.items():
                key = "0x{:02X}".format(probe_addr)
                output[key] = readings
            print(json.dumps(output, indent=2, default=str))

    finally:
        i2c.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
