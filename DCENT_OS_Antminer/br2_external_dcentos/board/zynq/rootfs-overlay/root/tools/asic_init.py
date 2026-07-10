#!/usr/bin/env python3
"""
asic_init.py - DCENTos ASIC Initialization Tool for Antminer S9
===============================================================

Complete cold-boot initialization of BM1387 ASIC chips via FPGA UART FIFOs
and PIC voltage controllers. Performs the full sequence discovered through
live probing on DCENTos Hacker Shell.

Hardware: Antminer S9 (Zynq C55/C71 control board, BM1387 ASICs)
FPGA:     Braiins s9io v1.0.2 (200 MHz internal clock)
OS:       DCENTos Hacker Shell (Buildroot, ARM Linux)

Do not use this tool on AM2/XIL S17/S19/S19j Pro targets. Their fan block is
UIO-bound and their chain/PIC bring-up is different from the S9 BM1387 path.

Usage:
  python3 asic_init.py --chain all --enumerate-only --verbose
  python3 asic_init.py --chain 7 --voltage 8.5 --baud 1562500 --fan 100
  python3 asic_init.py --chain all --voltage 9.0

Author: D-Central Technologies (DCENTos Project)
License: GPL-3.0
"""

import argparse
import mmap
import os
import signal
import struct
import sys
import threading
import time

# ---------------------------------------------------------------------------
# FPGA Register Map (Braiins s9io v1.0.2, verified live)
# ---------------------------------------------------------------------------

# Chain base addresses (v1.0.2 actual, NOT the v0.2 docs)
CHAIN_BASE = {
    6: 0x43C00000,
    7: 0x43C10000,
    8: 0x43C20000,
}

# Common register offsets (base + 0x0000)
REG_VERSION     = 0x00   # R   - FPGA IP version
REG_BUILD_ID    = 0x04   # R   - Build timestamp
REG_CTRL        = 0x08   # RW  - Control: [4]=BM139X, [3]=ENABLE, [2:1]=MIDSTATE, [0]=ERR_CLR
REG_STAT        = 0x0C   # R   - Status (unused in v1.0.2)
REG_BAUD        = 0x10   # RW  - Baud rate divisor
REG_WORK_TIME   = 0x14   # RW  - Inter-work delay
REG_ERR_COUNTER = 0x18   # R   - Cumulative CRC error count

# CMD FIFO block offsets (base + 0x1000)
CMD_RX_FIFO     = 0x1000  # R   - ASIC command response data (2 words/response)
CMD_TX_FIFO     = 0x1004  # W   - ASIC command data (auto-CRC5 by FPGA)
CMD_CTRL_REG    = 0x1008  # RW  - [2]=IRQ_EN, [1]=RST_TX, [0]=RST_RX
CMD_STAT_REG    = 0x100C  # R   - [4]=IRQ_PEND, [3]=TX_FULL, [2]=TX_EMPTY, [1]=RX_FULL, [0]=RX_EMPTY

# Work RX block offsets (base + 0x2000)
WORK_RX_CTRL    = 0x2008  # RW  - [0]=RST_RX_FIFO
WORK_RX_STAT    = 0x200C  # R

# Work TX block offsets (base + 0x3000)
WORK_TX_CTRL    = 0x3008  # RW  - [1]=RST_TX_FIFO
WORK_TX_STAT    = 0x300C  # R

# Fan control registers (custom Braiins fan controller IP)
FAN_BASE        = 0x42800000
FAN0_RPS_OFF    = 0x00   # R   - Fan 0 tach (RPS), often reads 0 on S9
FAN1_RPS_OFF    = 0x04   # R   - Fan 1 tach (RPS), confirmed working
FAN_PWM0_OFF    = 0x10   # RW  - PWM channel 0, 7-bit (0-127)
FAN_PWM1_OFF    = 0x14   # RW  - PWM channel 1, 7-bit (0-127)

# GPIO registers
GPIO_INPUT      = 0x41200000  # R   - Hash board plug detect (bits 5-7)
GPIO_OUTPUT     = 0x41210000  # RW  - Reset lines (bits 9-11)

# FPGA clock: 200 MHz (100 MHz FCLK doubled by PL PLL)
FPGA_CLK_HZ     = 200_000_000

# Expected FPGA version
EXPECTED_VERSION = 0x00901002  # s9io v1.0.2 for Antminer S9


def am2_target_present():
    """Detect AM2/XIL targets where this S9 devmem tool must not run."""
    for path in ("/etc/dcentos/board_family", "/etc/dcentos/board_target"):
        try:
            with open(path, "r", encoding="utf-8") as fh:
                value = fh.read().strip()
            if value.startswith("am2-") or value == "zynq-bm3-am2":
                return True
        except OSError:
            pass

    try:
        names = []
        for entry in os.listdir("/sys/class/uio"):
            name_path = os.path.join("/sys/class/uio", entry, "name")
            try:
                with open(name_path, "r", encoding="utf-8") as fh:
                    names.append(fh.read().strip())
            except OSError:
                pass
        return "fan-control" in names and "board-control" in names
    except OSError:
        return False

# CMD_STAT_REG bit positions
STAT_RX_EMPTY   = (1 << 0)
STAT_RX_FULL    = (1 << 1)
STAT_TX_EMPTY   = (1 << 2)
STAT_TX_FULL    = (1 << 3)
STAT_IRQ_PEND   = (1 << 4)

# ---------------------------------------------------------------------------
# CMD_TX_FIFO Command Encoding (LSB-first packed)
#
#   Bytes [B0, B1, B2, B3] -> word = B0 | (B1<<8) | (B2<<16) | (B3<<24)
# ---------------------------------------------------------------------------

CMD_GET_ADDRESS   = 0x00000554   # GetAddress broadcast: [0x54, 0x05, 0x00, 0x00]
CMD_CHAIN_INACTIVE = 0x00000555  # Chain Inactive bcast: [0x55, 0x05, 0x00, 0x00]

def cmd_set_chip_address(addr):
    """SetChipAddress: [0x41, 0x05, addr, 0x00]"""
    return (addr << 16) | 0x0541

def cmd_read_reg(chip_addr, reg_addr):
    """Read register (unicast): [0x44, 0x05, chip, reg]"""
    return (reg_addr << 24) | (chip_addr << 16) | 0x0544

def cmd_read_reg_broadcast(reg_addr):
    """Read register (broadcast): [0x54, 0x05, 0x00, reg]"""
    return (reg_addr << 24) | 0x0554

# ---------------------------------------------------------------------------
# PIC Microcontroller Protocol (I2C)
# ---------------------------------------------------------------------------

# PIC I2C addresses (verified: 0x50 + (chain - 1))
PIC_ADDR = {
    6: 0x55,
    7: 0x56,
    8: 0x57,
}

# PIC commands (header: 0x55 0xAA)
PIC_JUMP_FROM_LOADER  = [0x55, 0xAA, 0x06]  # Jump from bootloader to app
PIC_RESET             = [0x55, 0xAA, 0x07]  # Reset PIC
PIC_SET_VOLTAGE       = [0x55, 0xAA, 0x10]  # + pic_val byte
PIC_GET_BOARD_ID      = [0x55, 0xAA, 0x13]  # Read board ID
PIC_ENABLE_VOLTAGE    = [0x55, 0xAA, 0x15]  # + 0x01 (enable) or 0x00 (disable)
PIC_HEARTBEAT         = [0x55, 0xAA, 0x16]  # Send heartbeat (every 1s)
PIC_GET_VERSION       = [0x55, 0xAA, 0x17]  # Read firmware version
PIC_GET_VOLTAGE       = [0x55, 0xAA, 0x18]  # Read voltage readback

# Voltage conversion (verified: pic_val=75 -> 9.0V)
VOLTAGE_A = 1608.420446
VOLTAGE_B = 170.423497

I2C_SLAVE = 0x0703  # ioctl I2C_SLAVE

# ---------------------------------------------------------------------------
# Memory-Mapped Register Access
# ---------------------------------------------------------------------------

class MmapRegion:
    """Memory-mapped /dev/mem region for FPGA register access."""

    PAGE_SIZE = 4096

    def __init__(self, phys_addr, size=PAGE_SIZE):
        self.phys_addr = phys_addr
        self.size = size
        self.fd = None
        self.mm = None
        self._open()

    def _open(self):
        self.fd = os.open("/dev/mem", os.O_RDWR | os.O_SYNC)
        # Align to page boundary
        page_offset = self.phys_addr & (self.PAGE_SIZE - 1)
        page_base = self.phys_addr & ~(self.PAGE_SIZE - 1)
        map_size = self.size + page_offset
        # Round up to page size
        map_size = ((map_size + self.PAGE_SIZE - 1) // self.PAGE_SIZE) * self.PAGE_SIZE
        self.mm = mmap.mmap(self.fd, map_size, mmap.MAP_SHARED,
                            mmap.PROT_READ | mmap.PROT_WRITE,
                            offset=page_base)
        self._page_offset = page_offset

    def read32(self, offset):
        """Read a 32-bit register at the given offset from the base address."""
        pos = self._page_offset + offset
        self.mm.seek(pos)
        data = self.mm.read(4)
        return struct.unpack("<I", data)[0]

    def write32(self, offset, value):
        """Write a 32-bit value to the register at the given offset."""
        pos = self._page_offset + offset
        self.mm.seek(pos)
        self.mm.write(struct.pack("<I", value & 0xFFFFFFFF))

    def close(self):
        if self.mm:
            self.mm.close()
            self.mm = None
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None


class FPGAAccess:
    """Manages all FPGA register access via /dev/mem mmaps."""

    def __init__(self):
        self._regions = {}

    def _get_region(self, phys_addr):
        """Get or create an mmap region for the given page-aligned address."""
        page_base = phys_addr & ~(MmapRegion.PAGE_SIZE - 1)
        if page_base not in self._regions:
            self._regions[page_base] = MmapRegion(page_base)
        return self._regions[page_base], phys_addr - page_base

    def read32(self, addr):
        """Read 32-bit register at absolute physical address."""
        region, offset = self._get_region(addr)
        return region.read32(offset)

    def write32(self, addr, value):
        """Write 32-bit register at absolute physical address."""
        region, offset = self._get_region(addr)
        region.write32(offset, value)

    def close(self):
        for region in self._regions.values():
            region.close()
        self._regions.clear()


# ---------------------------------------------------------------------------
# I2C PIC Communication
# ---------------------------------------------------------------------------

class PICI2C:
    """PIC voltage controller communication via /dev/i2c-0."""

    def __init__(self, bus=0):
        self.bus = bus
        self.fd = None
        self._current_addr = None

    def open(self):
        dev = "/dev/i2c-%d" % self.bus
        self.fd = os.open(dev, os.O_RDWR)

    def close(self):
        if self.fd is not None:
            os.close(self.fd)
            self.fd = None

    def _set_slave(self, addr):
        """Set the I2C slave address via ioctl."""
        if self._current_addr != addr:
            import fcntl
            fcntl.ioctl(self.fd, I2C_SLAVE, addr)
            self._current_addr = addr

    def write(self, addr, data):
        """Write bytes to PIC at given I2C address."""
        self._set_slave(addr)
        os.write(self.fd, bytes(data))

    def read(self, addr, length):
        """Read bytes from PIC at given I2C address."""
        self._set_slave(addr)
        return os.read(self.fd, length)

    def write_read(self, addr, write_data, read_length, delay=0.05):
        """Write command, optional delay, then read response."""
        self.write(addr, write_data)
        time.sleep(delay)
        return self.read(addr, read_length)


# ---------------------------------------------------------------------------
# Voltage Conversion
# ---------------------------------------------------------------------------

def voltage_to_pic(voltage_v):
    """Convert voltage (V) to PIC register value."""
    pic_val = int(round(VOLTAGE_A - VOLTAGE_B * voltage_v))
    if pic_val < 0:
        pic_val = 0
    if pic_val > 255:
        pic_val = 255
    return pic_val

def pic_to_voltage(pic_val):
    """Convert PIC register value to voltage (V)."""
    return (VOLTAGE_A - pic_val) / VOLTAGE_B

# ---------------------------------------------------------------------------
# Baud Rate Conversion
# ---------------------------------------------------------------------------

def baud_to_reg(baud):
    """Convert baud rate to FPGA BAUD_REG value (200 MHz clock).
    Uses known-good values from BraiinsOS for standard baud rates."""
    KNOWN_BAUD_REGS = {
        115200:  0x6C,   # 108 -> 114,679 baud (BraiinsOS verified)
        1562500: 0x07,   # 7   -> 1,562,500 baud (operational speed)
        3125000: 0x03,   # 3   -> 3,125,000 baud (maximum)
    }
    if baud in KNOWN_BAUD_REGS:
        return KNOWN_BAUD_REGS[baud]
    return (FPGA_CLK_HZ // (16 * baud)) - 1

def reg_to_baud(reg_val):
    """Convert FPGA BAUD_REG value to actual baud rate."""
    return FPGA_CLK_HZ // (16 * (reg_val + 1))

# ---------------------------------------------------------------------------
# ASIC Initialization Engine
# ---------------------------------------------------------------------------

class ASICInitializer:
    """Complete ASIC cold-boot initialization for Antminer S9."""

    def __init__(self, args):
        self.args = args
        self.fpga = FPGAAccess()
        self.i2c = PICI2C(bus=0)
        self.heartbeat_threads = {}
        self.heartbeat_stop = threading.Event()
        self.chain_results = {}
        self._shutdown = False

    def log(self, msg, level="INFO"):
        """Print log message with timestamp."""
        ts = time.strftime("%H:%M:%S")
        prefix = {"INFO": "[+]", "WARN": "[!]", "ERROR": "[-]", "DEBUG": "[.]"}
        pfx = prefix.get(level, "[?]")
        if level == "DEBUG" and not self.args.verbose:
            return
        print("%s %s %s" % (ts, pfx, msg))

    def run(self):
        """Execute the full initialization sequence."""
        try:
            self._print_banner()
            self._parse_chains()

            # Step 1: Verify FPGA
            self._verify_fpga()

            # Step 2: Set fans to max for safety
            self._set_fans(self.args.fan if self.args.fan is not None else 127)

            # Step 3: Read GPIO state
            self._read_gpio()

            # Step 4: Reset FPGA FIFOs for each chain
            for chain in self.chains:
                self._reset_fifos(chain)

            if self.args.enumerate_only:
                # Enumerate-only mode: skip voltage, just read current state
                self.log("=== ENUMERATE-ONLY MODE (no voltage enable) ===")
                for chain in self.chains:
                    self._read_chain_status(chain)
                    self._try_enumerate(chain)
                self._display_results()
                return

            # Step 5: Open I2C and init PICs
            self.i2c.open()
            for chain in self.chains:
                self._init_pic(chain)

            # Step 6: Release hash board resets
            self._release_resets()

            # Step 7: Wait for chips to boot
            self.log("Waiting 2s for ASIC chips to boot...")
            time.sleep(2.0)

            # Step 8: Set baud rate
            for chain in self.chains:
                self._set_baud(chain, self.args.baud)

            # Step 9: Enumerate chips
            for chain in self.chains:
                self._enumerate_chips(chain)

            # Step 10: Display results
            self._display_results()

            # Step 11: Keep heartbeat running until Ctrl+C
            self.log("Heartbeat running. Press Ctrl+C to shut down.")
            while not self._shutdown:
                time.sleep(0.5)

        except KeyboardInterrupt:
            self.log("\nCtrl+C received, shutting down...", "WARN")
        except Exception as e:
            self.log("Fatal error: %s" % str(e), "ERROR")
            import traceback
            traceback.print_exc()
        finally:
            self._cleanup()

    def _print_banner(self):
        print("=" * 64)
        print("  DCENTos ASIC Initializer - Antminer S9 BM1387")
        print("  D-Central Technologies | asic_init.py v1.0")
        print("=" * 64)
        print()

    def _parse_chains(self):
        """Parse --chain argument into list of chain IDs."""
        if self.args.chain == "all":
            self.chains = [6, 7, 8]
        else:
            c = int(self.args.chain)
            if c not in (6, 7, 8):
                self.log("Invalid chain %d. Must be 6, 7, 8, or 'all'." % c, "ERROR")
                sys.exit(1)
            self.chains = [c]
        self.log("Target chains: %s" % self.chains)

    # ---- FPGA Verification ----

    def _verify_fpga(self):
        """Verify FPGA is present and running expected version."""
        self.log("Verifying FPGA...")
        for chain in self.chains:
            base = CHAIN_BASE[chain]
            version = self.fpga.read32(base + REG_VERSION)
            build_id = self.fpga.read32(base + REG_BUILD_ID)
            self.log("  Chain %d: VERSION=0x%08X BUILD_ID=0x%08X" %
                     (chain, version, build_id), "DEBUG")
            if version != EXPECTED_VERSION:
                self.log("  Chain %d: Unexpected FPGA version 0x%08X (expected 0x%08X)" %
                         (chain, version, EXPECTED_VERSION), "WARN")
            else:
                self.log("  Chain %d: FPGA s9io v1.0.2 confirmed" % chain)

    # ---- Fan Control ----

    def _set_fans(self, pwm):
        """Set fan PWM on both channels (0-127)."""
        pwm = max(0, min(127, pwm))
        self.log("Setting fans to PWM=%d (%d%%)" % (pwm, pwm * 100 // 127))

        # Map fan base to mmap
        self.fpga.write32(FAN_BASE + FAN_PWM0_OFF, pwm)
        self.fpga.write32(FAN_BASE + FAN_PWM1_OFF, pwm)

        # Read back fan speed (allow settle time for accurate reading)
        time.sleep(0.5)
        fan0_rps = self.fpga.read32(FAN_BASE + FAN0_RPS_OFF) & 0x7F
        fan1_rps = self.fpga.read32(FAN_BASE + FAN1_RPS_OFF) & 0x7F
        fan0_rpm = fan0_rps * 60
        fan1_rpm = fan1_rps * 60
        self.log("  Fan 0: %d RPS (%d RPM)" % (fan0_rps, fan0_rpm), "DEBUG")
        self.log("  Fan 1: %d RPS (%d RPM)" % (fan1_rps, fan1_rpm))
        self.fan_rpm = max(fan0_rpm, fan1_rpm)

    def _read_fan_rpm(self):
        """Read current fan RPM."""
        fan0_rps = self.fpga.read32(FAN_BASE + FAN0_RPS_OFF) & 0x7F
        fan1_rps = self.fpga.read32(FAN_BASE + FAN1_RPS_OFF) & 0x7F
        # Return whichever is non-zero (FAN0 often reads 0 on S9)
        rps = fan1_rps if fan1_rps > 0 else fan0_rps
        return rps * 60

    # ---- GPIO ----

    def _read_gpio(self):
        """Read GPIO input (board detect) and output (reset state)."""
        gpio_in = self.fpga.read32(GPIO_INPUT)
        gpio_out = self.fpga.read32(GPIO_OUTPUT)

        self.log("GPIO Input:  0x%08X" % gpio_in)
        self.log("GPIO Output: 0x%08X" % gpio_out)

        # Check board presence (bits 5-7)
        for chain in self.chains:
            bit = {6: 5, 7: 6, 8: 7}[chain]
            present = bool(gpio_in & (1 << bit))
            status = "PRESENT" if present else "NOT DETECTED"
            self.log("  Chain %d (J%d): %s" % (chain, chain, status))
            if not present:
                self.log("  WARNING: Hash board on J%d not detected! "
                         "Check connector." % chain, "WARN")

        # Check reset state (bits 9-11)
        for chain in self.chains:
            bit = {6: 9, 7: 10, 8: 11}[chain]
            released = bool(gpio_out & (1 << bit))
            state = "RELEASED" if released else "ASSERTED"
            self.log("  Chain %d reset: %s" % (chain, state), "DEBUG")

    # ---- FIFO Reset ----

    def _reset_fifos(self, chain):
        """Reset all FIFOs for a chain and clear error counter."""
        base = CHAIN_BASE[chain]
        self.log("Chain %d: Resetting FIFOs..." % chain)

        # 1. Disable chain
        self.fpga.write32(base + REG_CTRL, 0x00000000)
        time.sleep(0.001)

        # 2. Reset CMD FIFOs (bit 0=RST_RX, bit 1=RST_TX)
        self.fpga.write32(base + CMD_CTRL_REG, 0x00000003)
        time.sleep(0.001)
        self.fpga.write32(base + CMD_CTRL_REG, 0x00000000)

        # 3. Reset Work RX FIFO
        self.fpga.write32(base + WORK_RX_CTRL, 0x00000001)
        time.sleep(0.001)
        self.fpga.write32(base + WORK_RX_CTRL, 0x00000000)

        # 4. Reset Work TX FIFO
        self.fpga.write32(base + WORK_TX_CTRL, 0x00000002)
        time.sleep(0.001)
        self.fpga.write32(base + WORK_TX_CTRL, 0x00000000)

        # 5. Clear error counter (CTRL_REG bit 0)
        self.fpga.write32(base + REG_CTRL, 0x00000001)
        time.sleep(0.001)
        self.fpga.write32(base + REG_CTRL, 0x00000000)

        # 6. Set baud to 115200 (initial enumeration speed)
        baud_reg = baud_to_reg(115200)
        self.fpga.write32(base + REG_BAUD, baud_reg)

        # 7. Enable chain (CTRL bit 3, BM1387 mode = bit 4 clear)
        self.fpga.write32(base + REG_CTRL, 0x00000008)

        # 8. Verify FIFO state
        stat = self.fpga.read32(base + CMD_STAT_REG)
        rx_empty = bool(stat & STAT_RX_EMPTY)
        tx_empty = bool(stat & STAT_TX_EMPTY)
        err = self.fpga.read32(base + REG_ERR_COUNTER)

        self.log("  CMD_STAT=0x%02X (RX_EMPTY=%d TX_EMPTY=%d) ERR=%d" %
                 (stat, rx_empty, tx_empty, err), "DEBUG")

        if not rx_empty:
            self.log("  WARNING: RX FIFO not empty after reset!", "WARN")
        if not tx_empty:
            self.log("  WARNING: TX FIFO not empty after reset!", "WARN")
        if err != 0:
            self.log("  WARNING: Error counter not zero after clear: %d" % err, "WARN")

        self.log("  Chain %d FIFOs reset, baud=115200, core ENABLED" % chain)

    # ---- Chain Status (enumerate-only mode) ----

    def _read_chain_status(self, chain):
        """Read and display current chain status without modifying state."""
        base = CHAIN_BASE[chain]
        version = self.fpga.read32(base + REG_VERSION)
        build_id = self.fpga.read32(base + REG_BUILD_ID)
        ctrl = self.fpga.read32(base + REG_CTRL)
        baud_reg = self.fpga.read32(base + REG_BAUD)
        err = self.fpga.read32(base + REG_ERR_COUNTER)
        cmd_stat = self.fpga.read32(base + CMD_STAT_REG)

        actual_baud = reg_to_baud(baud_reg) if baud_reg > 0 else 0
        enabled = bool(ctrl & 0x08)
        rx_empty = bool(cmd_stat & STAT_RX_EMPTY)
        tx_empty = bool(cmd_stat & STAT_TX_EMPTY)

        self.log("Chain %d Status:" % chain)
        self.log("  VERSION:     0x%08X" % version)
        self.log("  BUILD_ID:    0x%08X" % build_id)
        self.log("  CTRL:        0x%08X (ENABLED=%s)" % (ctrl, enabled))
        self.log("  BAUD_REG:    0x%04X (%d baud)" % (baud_reg, actual_baud))
        self.log("  ERR_COUNTER: %d" % err)
        self.log("  CMD_STAT:    0x%02X (RX_EMPTY=%d TX_EMPTY=%d)" %
                 (cmd_stat, rx_empty, tx_empty))

    def _try_enumerate(self, chain):
        """Try to enumerate chips (may fail if boards are unpowered)."""
        base = CHAIN_BASE[chain]

        # Check if RX FIFO has stale data
        stat = self.fpga.read32(base + CMD_STAT_REG)
        if not (stat & STAT_RX_EMPTY):
            self.log("  Chain %d: RX FIFO has stale data (already reset above)" % chain,
                     "DEBUG")

        # Send GetAddress broadcast
        self.log("  Chain %d: Sending GetAddress broadcast..." % chain)
        self.fpga.write32(base + CMD_TX_FIFO, CMD_GET_ADDRESS)

        # Wait for responses
        time.sleep(0.5)

        # Read responses
        chip_count = 0
        chip_id = None
        stat = self.fpga.read32(base + CMD_STAT_REG)

        if stat & STAT_RX_EMPTY:
            self.log("  Chain %d: No response (boards unpowered or no chips)" % chain)
            self.chain_results[chain] = {
                "chips": 0,
                "chip_id": None,
                "crc_errors": self.fpga.read32(base + REG_ERR_COUNTER),
                "voltage": None,
                "baud": reg_to_baud(self.fpga.read32(base + REG_BAUD)),
            }
            return

        # Read response pairs
        max_reads = 256  # Safety limit (128 response pairs max)
        while max_reads > 0:
            stat = self.fpga.read32(base + CMD_STAT_REG)
            if stat & STAT_RX_EMPTY:
                break
            w0 = self.fpga.read32(base + CMD_RX_FIFO)
            stat = self.fpga.read32(base + CMD_STAT_REG)
            if stat & STAT_RX_EMPTY:
                # Odd word - incomplete response
                self.log("  Chain %d: Incomplete response (odd word)" % chain, "WARN")
                break
            w1 = self.fpga.read32(base + CMD_RX_FIFO)

            # Decode: W0 = register data (ChipID), W1 = metadata
            # ChipID is in W0 bytes 0-1 (LSB-first): 0x00908713 -> ID=0x1387
            cid = (w0 & 0xFF) << 8 | ((w0 >> 8) & 0xFF)
            if chip_id is None:
                chip_id = cid
            chip_count += 1
            max_reads -= 2

            if self.args.verbose and chip_count <= 5:
                self.log("    Chip %d: W0=0x%08X W1=0x%08X (ID=0x%04X)" %
                         (chip_count, w0, w1, cid), "DEBUG")

        if self.args.verbose and chip_count > 5:
            self.log("    ... (%d more chips)" % (chip_count - 5), "DEBUG")

        err = self.fpga.read32(base + REG_ERR_COUNTER)
        self.log("  Chain %d: %d chips detected, ChipID=0x%04X, CRC errors=%d" %
                 (chain, chip_count, chip_id or 0, err))

        self.chain_results[chain] = {
            "chips": chip_count,
            "chip_id": chip_id,
            "crc_errors": err,
            "voltage": None,
            "baud": reg_to_baud(self.fpga.read32(base + REG_BAUD)),
        }

    # ---- PIC Initialization ----

    def _init_pic(self, chain):
        """Initialize PIC voltage controller for a chain."""
        pic_addr = PIC_ADDR[chain]
        self.log("Chain %d: Initializing PIC at I2C 0x%02X..." % (chain, pic_addr))

        # Step 1: Jump from bootloader to application
        self.log("  Sending JUMP_FROM_LOADER_TO_APP...", "DEBUG")
        try:
            self.i2c.write(pic_addr, PIC_JUMP_FROM_LOADER)
        except OSError as e:
            self.log("  I2C write failed: %s" % e, "ERROR")
            return False
        time.sleep(1.0)

        # Step 2: Read PIC firmware version
        self.log("  Reading PIC firmware version...", "DEBUG")
        try:
            self.i2c.write(pic_addr, PIC_GET_VERSION)
            time.sleep(0.05)
            ver_data = self.i2c.read(pic_addr, 1)
            ver = ver_data[0]
            self.log("  PIC firmware version: 0x%02X" % ver)
            if ver == 0xCC:
                self.log("  WARNING: PIC still in bootloader mode (0xCC)! "
                         "Jump may have failed.", "WARN")
            elif ver != 0x03:
                self.log("  NOTE: Expected version 0x03, got 0x%02X" % ver, "WARN")
        except OSError as e:
            self.log("  PIC version read failed: %s" % e, "WARN")

        # Step 3: Set voltage
        pic_val = voltage_to_pic(self.args.voltage)
        actual_v = pic_to_voltage(pic_val)
        self.log("  Setting voltage: %.2fV (PIC val=%d, actual=%.3fV)" %
                 (self.args.voltage, pic_val, actual_v))
        try:
            self.i2c.write(pic_addr, PIC_SET_VOLTAGE + [pic_val])
        except OSError as e:
            self.log("  Voltage set failed: %s" % e, "ERROR")
            return False
        time.sleep(0.3)

        # Step 4: Enable voltage
        self.log("  Enabling voltage output...")
        try:
            self.i2c.write(pic_addr, PIC_ENABLE_VOLTAGE + [0x01])
        except OSError as e:
            self.log("  Voltage enable failed: %s" % e, "ERROR")
            return False
        time.sleep(0.5)

        # Step 5: Verify voltage readback
        try:
            self.i2c.write(pic_addr, PIC_GET_VOLTAGE)
            time.sleep(0.05)
            v_data = self.i2c.read(pic_addr, 1)
            readback_val = v_data[0]
            readback_v = pic_to_voltage(readback_val)
            self.log("  Voltage readback: PIC val=%d -> %.3fV" %
                     (readback_val, readback_v))
        except OSError as e:
            self.log("  Voltage readback failed: %s" % e, "WARN")
            readback_v = actual_v

        # Step 6: Start heartbeat thread
        self._start_heartbeat(chain, pic_addr)
        self.log("  Chain %d PIC initialized, voltage ENABLED" % chain)

        # Store voltage for results
        if chain not in self.chain_results:
            self.chain_results[chain] = {}
        self.chain_results[chain]["voltage"] = readback_v

        return True

    def _start_heartbeat(self, chain, pic_addr):
        """Start a background thread sending PIC heartbeat every 1 second."""
        def heartbeat_loop():
            self.log("  Chain %d heartbeat thread started" % chain, "DEBUG")
            while not self.heartbeat_stop.is_set():
                try:
                    self.i2c.write(pic_addr, PIC_HEARTBEAT)
                except OSError:
                    pass  # Ignore transient I2C errors in heartbeat
                self.heartbeat_stop.wait(1.0)
            self.log("  Chain %d heartbeat thread stopped" % chain, "DEBUG")

        t = threading.Thread(target=heartbeat_loop, daemon=True,
                             name="heartbeat-chain%d" % chain)
        t.start()
        self.heartbeat_threads[chain] = t

    # ---- Hash Board Reset ----

    def _release_resets(self):
        """Release hash board resets via AXI GPIO output."""
        self.log("Releasing hash board resets (GPIO=0x00000E00)...")
        self.fpga.write32(GPIO_OUTPUT, 0x00000E00)
        time.sleep(0.1)

        # Verify
        gpio_out = self.fpga.read32(GPIO_OUTPUT)
        self.log("  GPIO Output: 0x%08X" % gpio_out, "DEBUG")

    # ---- Baud Rate ----

    def _set_baud(self, chain, baud):
        """Set FPGA baud rate for a chain."""
        base = CHAIN_BASE[chain]
        baud_reg = baud_to_reg(baud)
        actual = reg_to_baud(baud_reg)
        self.log("Chain %d: Setting baud %d (REG=0x%04X, actual=%d)" %
                 (chain, baud, baud_reg, actual))
        self.fpga.write32(base + REG_BAUD, baud_reg)

    # ---- Chip Enumeration ----

    def _enumerate_chips(self, chain):
        """Enumerate BM1387 chips on a chain via GetAddress broadcast."""
        base = CHAIN_BASE[chain]
        self.log("Chain %d: Enumerating chips..." % chain)

        # Ensure FIFO is clean (reset was done earlier, but re-check)
        stat = self.fpga.read32(base + CMD_STAT_REG)
        if not (stat & STAT_RX_EMPTY):
            self.log("  Flushing stale RX data...", "DEBUG")
            self.fpga.write32(base + CMD_CTRL_REG, 0x00000001)  # RST_RX
            time.sleep(0.001)
            self.fpga.write32(base + CMD_CTRL_REG, 0x00000000)

        # Wait for TX ready
        stat = self.fpga.read32(base + CMD_STAT_REG)
        if not (stat & STAT_TX_EMPTY):
            self.log("  WARNING: TX FIFO not empty!", "WARN")

        # Send GetAddress broadcast
        self.fpga.write32(base + CMD_TX_FIFO, CMD_GET_ADDRESS)
        self.log("  Sent GetAddress (0x%08X), waiting for responses..." %
                 CMD_GET_ADDRESS, "DEBUG")

        # Wait for responses to arrive
        time.sleep(1.0)

        # Read response pairs
        chip_count = 0
        chip_id = None
        max_reads = 256  # 128 pairs max

        while max_reads > 0:
            stat = self.fpga.read32(base + CMD_STAT_REG)
            if stat & STAT_RX_EMPTY:
                break

            w0 = self.fpga.read32(base + CMD_RX_FIFO)

            stat = self.fpga.read32(base + CMD_STAT_REG)
            if stat & STAT_RX_EMPTY:
                self.log("  Incomplete response pair (odd word)", "WARN")
                break

            w1 = self.fpga.read32(base + CMD_RX_FIFO)

            # Decode chip ID from W0:
            # W0 = 0x00908713 -> bytes LSB first: 0x13, 0x87, 0x90, 0x00
            # ChipID big-endian = 0x1387 (byte0 << 8 | byte1)
            byte0 = w0 & 0xFF
            byte1 = (w0 >> 8) & 0xFF
            cid = (byte0 << 8) | byte1

            if chip_id is None:
                chip_id = cid

            chip_count += 1
            max_reads -= 2

            if self.args.verbose:
                chip_addr_byte = (w0 >> 24) & 0xFF
                crc5 = (w1 >> 24) & 0xFF
                if chip_count <= 10:
                    self.log("    Chip %2d: W0=0x%08X W1=0x%08X ID=0x%04X addr=0x%02X CRC5=0x%02X" %
                             (chip_count, w0, w1, cid, chip_addr_byte, crc5), "DEBUG")

        if self.args.verbose and chip_count > 10:
            self.log("    ... (%d more chips)" % (chip_count - 10), "DEBUG")

        err = self.fpga.read32(base + REG_ERR_COUNTER)
        baud_reg = self.fpga.read32(base + REG_BAUD)
        actual_baud = reg_to_baud(baud_reg)

        self.log("  Chain %d: %d chips found, ChipID=0x%04X, CRC errors=%d" %
                 (chain, chip_count, chip_id or 0, err))

        self.chain_results[chain] = self.chain_results.get(chain, {})
        self.chain_results[chain].update({
            "chips": chip_count,
            "chip_id": chip_id,
            "crc_errors": err,
            "baud": actual_baud,
        })
        if "voltage" not in self.chain_results[chain]:
            self.chain_results[chain]["voltage"] = None

    # ---- Results Display ----

    def _display_results(self):
        """Display final results summary."""
        print()
        print("=" * 64)
        print("  INITIALIZATION RESULTS")
        print("=" * 64)
        print()

        total_chips = 0
        total_errors = 0

        fmt = "  %-10s %-8s %-10s %-10s %-10s %-10s"
        print(fmt % ("Chain", "Chips", "ChipID", "CRC Err", "Voltage", "Baud"))
        print(fmt % ("-----", "-----", "------", "-------", "-------", "----"))

        for chain in sorted(self.chain_results.keys()):
            r = self.chain_results[chain]
            chips = r.get("chips", 0)
            cid = "0x%04X" % r["chip_id"] if r.get("chip_id") else "N/A"
            err = r.get("crc_errors", 0)
            volt = "%.2fV" % r["voltage"] if r.get("voltage") else "N/A"
            baud = "%d" % r["baud"] if r.get("baud") else "N/A"

            print(fmt % ("Chain %d" % chain, chips, cid, err, volt, baud))
            total_chips += chips
            total_errors += err

        print()
        print("  Total chips: %d" % total_chips)
        print("  Total CRC errors: %d" % total_errors)

        fan_rpm = self._read_fan_rpm()
        print("  Fan RPM: %d" % fan_rpm)

        # GPIO state
        gpio_in = self.fpga.read32(GPIO_INPUT)
        boards = []
        for c in [6, 7, 8]:
            bit = {6: 5, 7: 6, 8: 7}[c]
            if gpio_in & (1 << bit):
                boards.append("J%d" % c)
        print("  Boards detected: %s" % ", ".join(boards) if boards else "  Boards detected: NONE")

        print()
        if total_chips == 189:
            print("  STATUS: ALL 189 CHIPS DETECTED -- FULL SYSTEM OK")
        elif total_chips > 0:
            print("  STATUS: %d/189 chips detected (%.1f%%)" %
                  (total_chips, total_chips * 100.0 / 189))
        else:
            print("  STATUS: No chips detected")
            if self.args.enumerate_only:
                print("  (Boards are unpowered -- use without --enumerate-only to power up)")
        print()

    # ---- Cleanup ----

    def _cleanup(self):
        """Clean shutdown: disable voltage, stop heartbeat, restore fans."""
        self.log("Cleaning up...")

        # Stop heartbeat threads
        self.heartbeat_stop.set()
        for chain, t in self.heartbeat_threads.items():
            t.join(timeout=2.0)

        # Disable voltage on each chain (if we enabled it)
        if not self.args.enumerate_only:
            for chain in self.chains:
                pic_addr = PIC_ADDR[chain]
                self.log("  Chain %d: Disabling voltage..." % chain)
                try:
                    self.i2c.write(pic_addr, PIC_ENABLE_VOLTAGE + [0x00])
                except OSError as e:
                    self.log("  Chain %d voltage disable failed: %s" % (chain, e), "WARN")

        # Set fans back to low (but not zero -- maintain some cooling)
        self.log("  Setting fans to low speed (PWM=30)...")
        try:
            self.fpga.write32(FAN_BASE + FAN_PWM0_OFF, 30)
            self.fpga.write32(FAN_BASE + FAN_PWM1_OFF, 30)
        except Exception:
            pass

        # Close I2C
        self.i2c.close()

        # Close FPGA mmaps
        self.fpga.close()

        self.log("Shutdown complete.")


# ---------------------------------------------------------------------------
# Signal Handler
# ---------------------------------------------------------------------------

_initializer = None

def signal_handler(signum, frame):
    global _initializer
    if _initializer:
        _initializer._shutdown = True

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    global _initializer

    parser = argparse.ArgumentParser(
        description="DCENTos ASIC Initializer - Antminer S9 BM1387 cold-boot tool",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Safe probe -- reads FIFO status, no voltage enable:
  python3 asic_init.py --chain all --enumerate-only --verbose

  # Full init -- power boards, enumerate at 115200:
  python3 asic_init.py --chain all --voltage 9.0

  # Single chain, undervolted, fast baud:
  python3 asic_init.py --chain 7 --voltage 8.5 --baud 1562500

  # Set S9 fans to 50%% only:
  python3 asic_init.py --chain all --enumerate-only --fan 64
""")

    parser.add_argument("--chain", type=str, default="all",
                        help="Chain to init: 6, 7, 8, or 'all' (default: all)")
    parser.add_argument("--voltage", type=float, default=9.0,
                        help="Hash board voltage in V (default: 9.0)")
    parser.add_argument("--baud", type=int, default=115200,
                        choices=[115200, 1562500],
                        help="UART baud rate (default: 115200)")
    parser.add_argument("--enumerate-only", action="store_true",
                        help="Skip voltage enable, just read FIFO status + GPIO")
    parser.add_argument("--fan", type=int, default=None,
                        help="S9/am1 fan PWM value 0-127 (default: 127 for safety)")
    parser.add_argument("--verbose", "-v", action="store_true",
                        help="Enable verbose/debug output")

    args = parser.parse_args()

    if am2_target_present():
        print(
            "ERROR: AM2/XIL target detected. asic_init.py is S9/BM1387-only; "
            "use dcentrald AM2 paths for fan and chain control.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Validate voltage range
    if args.voltage < 7.0 or args.voltage > 10.0:
        print("ERROR: Voltage %.2f out of safe range (7.0-10.0V)" % args.voltage)
        sys.exit(1)

    # Validate fan PWM
    if args.fan is not None and (args.fan < 0 or args.fan > 127):
        print("ERROR: Fan PWM must be 0-127")
        sys.exit(1)

    # Set up signal handler
    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    # Run initializer
    _initializer = ASICInitializer(args)
    _initializer.run()


if __name__ == "__main__":
    main()
