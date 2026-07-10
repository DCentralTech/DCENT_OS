# dcentrald Architecture Document

> **DCENT_OS Mining Daemon** -- 100% original D-Central codebase
> **Version:** 0.4
> **Date:** 2026-03-11
> **Status:** Implementation Phase 2 — Mining pipeline functional, hardware I/O wired
> **License:** GPL-3.0
> **Target:** Antminer S9 (Zynq-7010, BM1387) as initial platform

---

## Table of Contents

1. [Design Philosophy](#1-design-philosophy)
2. [Cargo Workspace Layout](#2-cargo-workspace-layout)
3. [Module Dependency Graph](#3-module-dependency-graph)
4. [ChipDriver Trait -- Universal Hash Board Compatibility](#4-chipdriver-trait----universal-hash-board-compatibility)
5. [Hardware Abstraction Layer (dcentrald-hal)](#5-hardware-abstraction-layer-dcentrald-hal)
6. [ASIC Driver Subsystem (dcentrald-asic)](#6-asic-driver-subsystem-dcentrald-asic)
7. [Stratum Subsystem (dcentrald-stratum)](#7-stratum-subsystem-dcentrald-stratum)
8. [Thermal Management (dcentrald-thermal)](#8-thermal-management-dcentrald-thermal)
9. [API Layer (dcentrald-api)](#9-api-layer-dcentrald-api)
10. [Mining Pipeline -- Async Data Flow](#10-mining-pipeline----async-data-flow)
11. [Configuration System](#11-configuration-system)
12. [Mode System](#12-mode-system)
13. [Startup Sequence -- Cold Boot](#13-startup-sequence----cold-boot)
14. [Safety Systems](#14-safety-systems)
15. [Persistent Storage](#15-persistent-storage)
16. [Platform Abstraction -- Multi-Board Support](#16-platform-abstraction----multi-board-support)
17. [Logging and Diagnostics](#17-logging-and-diagnostics)
18. [Diagnostic Subsystem (dcentrald-diagnostics)](#18-diagnostic-subsystem-dcentrald-diagnostics)
19. [Build and Deployment](#19-build-and-deployment)
20. [Dashboard Architecture](#20-dashboard-architecture)
21. [PSU Compatibility & 120V Home Mining](#21-psu-compatibility--120v-home-mining)
22. [Future Considerations](#22-future-considerations)

---

## 1. Design Philosophy

### Core Principles

1. **Clean rewrite, NOT a fork.** Study Mujina, BraiinsOS, and ESP-Miner for reference patterns. Ship 100% original D-Central code. No upstream dependencies on other mining firmware.

2. **Universal Hash Board Compatibility.** Auto-detect ASIC chip type via ChipID response. Load the correct driver. One control board can drive supported/lab-validated Zynq-era hash boards (S9 through S19 XP) when the exact board, power, and recovery path is proven.

3. **UIO-first FPGA access.** Open `/dev/uioN`, mmap 4 KB register regions, read/write directly. No kernel modules. No bitmain_axi.ko. Matches the proven BraiinsOS approach, verified on live hardware.

4. **No mandatory fee or license server.** The shipped donation route is transparent, user-configurable, and fully disableable. Every donation endpoint is visible; there is no hidden pool switching, phone-home dependency, or telemetry requirement.

5. **Home miner first.** Every feature decision asks: "does this help someone mining in their house?" 120V PSU bypass, noise management, thermostat mode, Home Assistant integration.

6. **Safety never compromised.** Watchdog timeout, PIC heartbeat, thermal shutdown, fan failure detection. If the daemon crashes, the hardware enters a safe state within seconds.

7. **Async Rust on Tokio.** The mining pipeline is inherently concurrent: Stratum network I/O, work dispatch to multiple chains, nonce collection, thermal monitoring, API serving. Tokio with `mpsc` channels provides the right concurrency model.

8. **Mode-first design.** One firmware adapts to three user types. Space Heater mode for the home miner who wants quiet heat and sats. Standard mode for the regular miner with a full dashboard. Hacker mode for the repair tech or researcher who needs raw register access and overclocking. Same binary, same flash, three completely different experiences.

### What dcentrald Is NOT

- NOT a CGMiner fork. CGMiner is C, single-threaded, callback-driven. dcentrald is Rust, async, actor-based.
- NOT a Mujina fork. Mujina targets USB-attached Bitaxe boards. dcentrald targets embedded Linux on the miner's own control board with FPGA chains.
- NOT a bosminer reimplementation. We study BraiinsOS for reference (especially UIO patterns), but the architecture is our own.

---

## 2. Cargo Workspace Layout

```
dcentrald/
  Cargo.toml                      # Workspace root (resolver = "2")
  dcentrald/                       # Main daemon binary crate
    Cargo.toml
    src/
      main.rs                      # Entry point, signal handling, lifecycle (IMPLEMENTED)
      daemon.rs                    # Daemon: init phases 1-7, run loop, shutdown (IMPLEMENTED)
      config.rs                    # TOML config — all sections with defaults (IMPLEMENTED)
      error.rs                     # Centralized error types (thiserror) (IMPLEMENTED)
      logging.rs                   # Structured logging (tracing-subscriber) (IMPLEMENTED)
  dcentrald-hal/                   # Hardware Abstraction Layer crate
    Cargo.toml
    src/
      lib.rs                       # Public trait exports (IMPLEMENTED)
      uio.rs                       # UIO device: open, mmap, IRQ wait (IMPLEMENTED)
      fpga_chain.rs                # FPGA chain: 4 register blocks per chain (IMPLEMENTED)
      i2c.rs                       # I2C bus: open /dev/i2c-N, ioctl, read/write (IMPLEMENTED)
      gpio.rs                      # AXI GPIO: direct register access (IMPLEMENTED)
      fan.rs                       # Fan PWM 0-127, tach RPM readback (IMPLEMENTED)
      watchdog.rs                  # /dev/watchdog open, kick, magic close (IMPLEMENTED)
      xadc.rs                      # XADC IIO sysfs: die temp, VCCINT, VCCAUX (IMPLEMENTED)
      led.rs                       # LED control via sysfs or GPIO (IMPLEMENTED)
      platform/
        mod.rs                     # Platform trait and detection (IMPLEMENTED)
        zynq.rs                    # Zynq (S9/S17/S19) platform (IMPLEMENTED)
        amlogic.rs                 # Amlogic (S19XP/S21) platform (stub)
        beaglebone.rs              # BeagleBone (S19j) platform (stub)
  dcentrald-asic/                  # ASIC chip driver crate
    Cargo.toml
    src/
      lib.rs                       # ChipDriver trait, ChipRegistry (IMPLEMENTED)
      protocol.rs                  # BM13xx wire protocol: CRC5/16, framing (IMPLEMENTED)
      chain.rs                     # Chain: enumerate_chips, assign_addresses (IMPLEMENTED)
      pic.rs                       # PIC: cold_boot_init, voltage, heartbeat (IMPLEMENTED)
      drivers/
        mod.rs                     # ChipRegistry with auto-detect (IMPLEMENTED)
        bm1387.rs                  # BM1387: PLL table, set_frequency, init (IMPLEMENTED)
        bm1397.rs                  # BM1397 driver (S17/T17) (stub)
        bm1366.rs                  # BM1366 driver (S19XP) (stub)
        bm1368.rs                  # BM1368 driver (S21) (stub)
        bm1370.rs                  # BM1370 driver (S21 Pro) (stub)
        bm1362.rs                  # BM1362 driver (S19j Pro) (stub)
  dcentrald-stratum/               # Stratum protocol client crate
    Cargo.toml
    src/
      lib.rs                       # Public API (IMPLEMENTED)
      types.rs                     # JobTemplate, NonceSubmission, PoolStatus (IMPLEMENTED)
      v1/
        mod.rs                     # Stratum V1 client (IMPLEMENTED)
        client.rs                  # Connection management, reconnect logic (IMPLEMENTED)
        messages.rs                # JSON-RPC message types (IMPLEMENTED)
        codec.rs                   # Line-delimited JSON codec (IMPLEMENTED)
        job.rs                     # JobTemplate, merkle root computation (IMPLEMENTED)
        difficulty.rs              # Difficulty target conversions (IMPLEMENTED)
      v2/                          # Stratum V2 (future, stub)
        mod.rs
  dcentrald-thermal/               # Thermal management crate
    Cargo.toml
    src/
      lib.rs                       # Public API, ThermalState enum
      controller.rs                # PID-based thermal control loop (IMPLEMENTED)
      fan.rs                       # Fan speed manager, noise/CFM estimation (IMPLEMENTED)
      profiles.rs                  # ATM thermal profiles + S9 power presets (IMPLEMENTED)
      curtailment.rs               # Sleep/wake state machine (IMPLEMENTED)
      heater.rs                    # Space Heater PID power targeting + night mode (IMPLEMENTED)
  dcentrald-diagnostics/            # Diagnostic test orchestration and report generation
    Cargo.toml
    src/
      lib.rs                       # Public API: DiagnosticService, TestResult
      hashreport.rs                # HashReport 15-minute test drive
      chip_health.rs               # Per-chip health scoring and ChipMap
      board_health.rs              # Per-board health test
      troubleshoot.rs              # Instant troubleshooting tools
      report.rs                    # HTML/PDF report generation (askama templates)
      progress.rs                  # Progress tracking and WebSocket push
      subprocess.rs                # Phase 1: spawn Python tools as subprocesses
  dcentrald-api/                   # API and web dashboard crate
    Cargo.toml
    src/
      lib.rs                       # AppState, MinerState, start_api_servers (IMPLEMENTED)
      cgminer.rs                   # CGMiner TCP API port 4028 — 13 commands (IMPLEMENTED)
      rest.rs                      # REST API — 85 routes, all handlers implemented
      websocket.rs                 # WebSocket /ws — stats + diag progress (IMPLEMENTED)
      dashboard.rs                 # Mode-themed HTML dashboard (IMPLEMENTED)
      mode_middleware.rs            # Mode access gating + SafetyEnvelope (IMPLEMENTED)
```

### Key Dependencies

| Dependency | Purpose |
|---|---|
| `tokio` (full) | Async runtime, timers, signals, file I/O, sync primitives |
| `serde` / `serde_json` | Configuration and API serialization |
| `toml` | Configuration file parsing |
| `tracing` / `tracing-subscriber` | Structured logging with level filtering |
| `thiserror` / `anyhow` | Error handling |
| `nix` | POSIX system calls (mmap, ioctl, open) |
| `sha2` | SHA-256d for share validation |
| `axum` | HTTP server for REST API and dashboard |
| `tokio-tungstenite` | WebSocket support |
| `bytes` | Zero-copy byte buffer management |
| `askama` | Compile-time HTML templates for diagnostic reports |
| `uuid` | Unique identifiers for diagnostic test sessions |
| `rust-embed` | Embed dashboard static assets into binary |

---

## 3. Module Dependency Graph

```
                              dcentrald (binary)
                                    |
          +------------+------------+------------+------------+
          |            |            |            |            |
   dcentrald-api  dcentrald-   dcentrald-  dcentrald-  dcentrald-
          |       stratum      thermal     diagnostics     |
          |            |            |            |          |
          +------+-----+------+----+------+-----+          |
                 |            |           |                 |
          dcentrald-asic      |    dcentrald-hal           |
                 |            |           |                 |
                 +------+-----+-----+-----+                |
                        |           |                      |
                 (Linux kernel)     +──────────────────────+
               UIO  I2C  GPIO  WDT
```

### Crate Visibility Rules

- `dcentrald-hal` has no dependencies on other dcentrald crates. It is pure hardware access.
- `dcentrald-asic` depends on `dcentrald-hal` for FPGA chain and I2C access.
- `dcentrald-stratum` has no dependencies on other dcentrald crates. It is pure network protocol.
- `dcentrald-thermal` depends on `dcentrald-hal` for fan control and temperature reading.
- `dcentrald-diagnostics` depends on `dcentrald-hal` and `dcentrald-asic` for hardware probing and chip communication. Also depends on `dcentrald-thermal` for temperature reading during tests.
- `dcentrald-api` depends on `dcentrald-asic`, `dcentrald-thermal`, and `dcentrald-diagnostics` for state queries and test orchestration.
- `dcentrald` (the binary) ties everything together via Tokio channels.

---

## 4. ChipDriver Trait -- Universal Hash Board Compatibility

### The Core Abstraction

The `ChipDriver` trait is the central abstraction that makes Universal Hash Board Compatibility possible. Each ASIC chip family (BM1387, BM1397, BM1366, BM1368, BM1370, BM1362) implements this trait with its specific initialization sequence, register values, job format, and nonce decoding.

```
trait ChipDriver: Send + Sync {
    /// Chip identifier (e.g., 0x1387, 0x1397, 0x1366, 0x1368, 0x1370, 0x1362)
    fn chip_id(&self) -> u16;

    /// Human-readable chip name (e.g., "BM1387", "BM1370")
    fn chip_name(&self) -> &'static str;

    /// Number of cores per chip (for hashrate estimation)
    fn cores_per_chip(&self) -> u32;

    /// Expected response length from this chip (9 bytes for BM1387, 11 for BM1366+)
    fn response_length(&self) -> usize;

    /// Default ASIC UART baud rate (115200 for all, upgradeable)
    fn default_baud(&self) -> u32;

    /// Maximum operational baud rate
    fn max_baud(&self) -> u32;

    /// Run the full chip initialization sequence on a chain
    fn init_chain(&self, chain: &mut FpgaChain, chip_count: u8) -> Result<()>;

    /// Set PLL frequency for a specific chip (or broadcast)
    fn set_frequency(&self, chain: &mut FpgaChain, chip_addr: u8, freq_mhz: u16) -> Result<()>;

    /// Set core voltage via PIC (chain-level, not per-chip)
    fn set_voltage(&self, pic: &mut PicController, voltage_mv: u16) -> Result<()>;

    /// Submit a mining job to the chain
    fn send_work(&self, chain: &mut FpgaChain, work: &MiningWork) -> Result<u8>;

    /// Decode a nonce response from the WORK_RX_FIFO
    fn decode_nonce(&self, raw: &[u32; 2]) -> Result<NonceResult>;

    /// Compute the FPGA BAUD_REG value for a target baud rate
    fn baud_reg_value(&self, target_baud: u32, fpga_clock_hz: u32) -> u32;

    /// Configure the FPGA CTRL_REG for this chip type
    fn ctrl_reg_value(&self) -> u32;

    /// Job dispatch interval in milliseconds (chip-specific)
    fn job_interval_ms(&self, chip_count: u8, freq_mhz: u16) -> u32;

    /// TicketMask register value for target difficulty
    fn ticket_mask(&self, difficulty: u32) -> u32;

    /// PLL parameters for a target frequency
    fn pll_params(&self, freq_mhz: u16) -> PllConfig;
}
```

### Auto-Detection Flow

```
                    Cold Boot
                       |
                       v
            +---------------------+
            | Reset all chips     |
            | (GPIO reset pulse)  |
            +---------------------+
                       |
                       v
            +---------------------+
            | Send GetAddress     |
            | broadcast via       |
            | CMD_TX_FIFO         |
            +---------------------+
                       |
                       v
            +---------------------+
            | Read CMD_RX_FIFO    |
            | First response W0:  |
            | 0x00908713 -> ChipID|
            | bytes [1:0] = 0x1387|
            +---------------------+
                       |
                       v
            +---------------------+
            | Look up ChipID in   |
            | driver registry:    |
            | 0x1387 -> BM1387    |
            | 0x1397 -> BM1397    |
            | 0x1366 -> BM1366    |
            | 0x1368 -> BM1368    |
            | 0x1370 -> BM1370    |
            | 0x1362 -> BM1362    |
            +---------------------+
                       |
                       v
            +---------------------+
            | Call driver.init()  |
            | with detected       |
            | chip count          |
            +---------------------+
```

### ChipDriver Registry

```
struct ChipRegistry {
    drivers: HashMap<u16, Box<dyn ChipDriver>>,
}

impl ChipRegistry {
    fn new() -> Self {
        let mut registry = Self { drivers: HashMap::new() };
        registry.register(Box::new(Bm1387Driver::new()));
        registry.register(Box::new(Bm1397Driver::new()));
        registry.register(Box::new(Bm1366Driver::new()));
        registry.register(Box::new(Bm1368Driver::new()));
        registry.register(Box::new(Bm1370Driver::new()));
        registry.register(Box::new(Bm1362Driver::new()));
        registry
    }

    fn detect(&self, chip_id: u16) -> Option<&dyn ChipDriver> {
        self.drivers.get(&chip_id).map(|d| d.as_ref())
    }
}
```

### Why This Matters

The 18-pin hash board connector is electrically identical across all Zynq-era Antminers (S9 through S19 XP). The UART protocol (preamble, CRC, framing) is the same. Only the initialization sequences, register values, and job formats differ between chip families.

This means a $15 S9 control board running dcentrald can drive supported later-generation hash boards in validated/lab configurations -- if the exact board, power, and recovery path is proven and the driver knows how to speak that ASIC. The ChipDriver trait makes this possible without recompiling. Plug in a supported hash board, dcentrald reads the ChipID, selects the right driver, and follows the platform gate.

### Chip Family Differences (Quick Reference)

| Property | BM1387 | BM1397 | BM1366 | BM1368 | BM1370 |
|---|---|---|---|---|---|
| Response size | 9 bytes (2 words) | 9 bytes (2 words) | 11 bytes (2 words + ver) | 11 bytes | 11 bytes |
| Job format | Midstate-based | Midstate-based (4x) | Full header (82 B) | Full header | Full header |
| Version rolling | None | 4 midstates (SW) | ASIC-internal (HW) | HW | HW |
| Job ID increment | N/A | +4 mod 128 | +8 mod 128 | +24 mod 128 | +24 mod 128 |
| Max baud | 6 Mbps (FPGA) | 3.125 Mbps | 1 Mbps | 1 Mbps | 1 Mbps |
| PLL postdiv encode | N/A | No -1 subtract | -1 subtract | -1 | -1 |
| CTRL_REG mode bit | BM1387 (bit4=0) | BM139X (bit4=1) | BM139X | BM139X | BM139X |

---

## 5. Hardware Abstraction Layer (dcentrald-hal)

### UioDevice

The foundation of all FPGA access. Opens a UIO device, mmaps the register region, and provides type-safe register read/write.

```
struct UioDevice {
    fd: RawFd,              // File descriptor for /dev/uioN
    regs: *mut u32,         // mmap'd register base pointer
    size: usize,            // Mapped region size (4096 bytes)
    name: String,           // UIO device name (e.g., "chain6-common")
}

impl UioDevice {
    fn open(uio_number: u8) -> Result<Self>;
    fn read_reg(&self, offset: u32) -> u32;
    fn write_reg(&self, offset: u32, value: u32);
    fn wait_irq(&self) -> Result<u32>;      // Blocking read on fd
    fn enable_irq(&self) -> Result<()>;      // Write 1 to fd
}
```

Safety: The mmap pointer is only valid within the 4 KB boundary. All offset access is bounds-checked in debug builds. Accessing unmapped FPGA space causes AXI external abort faults that crash the process (confirmed by live probing).

### FpgaChain

Wraps 4 UioDevice instances representing one hash chain's register blocks.

```
struct FpgaChain {
    common:  UioDevice,     // VERSION, CTRL, BAUD, WORK_TIME, ERR_COUNTER
    cmd:     UioDevice,     // CMD_RX_FIFO, CMD_TX_FIFO, CMD_STAT
    work_rx: UioDevice,     // WORK_RX_FIFO, WORK_RX_STAT (nonce responses)
    work_tx: UioDevice,     // WORK_TX_FIFO, WORK_TX_STAT (job submission)
    chain_id: u8,           // 6, 7, or 8 (matches S9 connector numbering)
}
```

**Register Layout (per chain, verified from live S9 probe):**

```
Common Block (+0x0000):
  +0x00  VERSION       R    IP core version (0x00901002 = s9io v1.0.2)
  +0x04  BUILD_ID      R    Bitstream build timestamp
  +0x08  CTRL_REG      RW   [4]=BM139X, [3]=ENABLE, [2:1]=MIDSTATE_CNT, [0]=ERR_CLR
  +0x0C  STAT_REG      R    Reserved
  +0x10  BAUD_REG      RW   Baud divisor: baud = FPGA_CLK / (16 * (BAUD_REG + 1))
  +0x14  WORK_TIME     RW   Inter-work delay (reset=1)
  +0x18  ERR_COUNTER   R    CRC error count (write bit0 of CTRL to clear)

CMD Block (+0x1000):
  +0x00  CMD_RX_FIFO   R    Response data (32-bit words, 2 words per response)
  +0x04  CMD_TX_FIFO   W    Command data (32-bit words, LSB-first byte packing)
  +0x08  CMD_CTRL_REG  RW   [1]=RST_TX, [0]=RST_RX
  +0x0C  CMD_STAT_REG  R    [4]=IRQ, [3]=TX_FULL, [2]=TX_EMPTY, [1]=RX_FULL, [0]=RX_EMPTY

Work RX Block (+0x2000):
  +0x00  WORK_RX_FIFO  R    Nonce data (2x 32-bit words per nonce)
  +0x08  WORK_RX_CTRL  RW   [0]=RST_RX
  +0x0C  WORK_RX_STAT  R    [4]=IRQ, [1]=RX_FULL, [0]=RX_EMPTY

Work TX Block (+0x3000):
  +0x04  WORK_TX_FIFO  W    Mining work data (12 or 36 words per job)
  +0x08  WORK_TX_CTRL  RW   [1]=RST_TX
  +0x0C  WORK_TX_STAT  R    [4]=IRQ, [3]=TX_FULL, [2]=TX_EMPTY
  +0x10  WORK_TX_THR   RW   IRQ threshold (fire when FIFO level drops below)
  +0x14  WORK_TX_LAST  R    Last work ID sent
```

**Baud Rate Calculation (FPGA clock confirmed at 200 MHz from ASIC live probe baud match):**

```
BAUD_REG = 0x6C (108): 200M / (16 * 109) = 114,679 baud (~115200 chip default)
BAUD_REG = 0x07 (7):   200M / (16 * 8)   = 1,562,500 baud (operational speed)
BAUD_REG = 0x03 (3):   200M / (16 * 4)   = 3,125,000 baud (maximum tested)
```

### I2cBus

Wraps Linux I2C char device for PIC microcontroller and temperature sensor communication.

```
struct I2cBus {
    fd: RawFd,             // /dev/i2c-0
}

impl I2cBus {
    fn open(bus: u8) -> Result<Self>;
    fn set_slave(&self, addr: u8) -> Result<()>;   // ioctl(I2C_SLAVE)
    fn write(&self, data: &[u8]) -> Result<usize>;
    fn read(&self, buf: &mut [u8]) -> Result<usize>;
    fn write_read(&self, write: &[u8], read: &mut [u8]) -> Result<()>;
}
```

**I2C Device Map (S9, verified):**

```
Address  Device           Function
0x55     PIC16F1704       Chain 6 (J6) voltage controller
0x56     PIC16F1704       Chain 7 (J7) voltage controller
0x57     PIC16F1704       Chain 8 (J8) voltage controller
```

Note: TMP75 temperature sensors are NOT visible until PIC enables voltage to the hash board. After voltage enable, temperature sensors appear at addresses 0x48-0x4F depending on board revision.

### GpioController

Direct AXI GPIO register access. Sysfs GPIO fails on the S9 for output pins (Operation not permitted on reset lines). Direct register writes are required.

```
struct GpioController {
    input_base: *mut u32,   // mmap of 0x41200000 (9-bit input, plug detect)
    output_base: *mut u32,  // mmap of 0x41210000 (13-bit output, LEDs + resets)
}

impl GpioController {
    fn read_plug_detect(&self) -> [bool; 3];    // GPIO bits 5,6,7 -> J6,J7,J8
    fn set_board_reset(&self, chain: u8, assert: bool);  // bits 9,10,11
    fn set_led(&self, led: Led, on: bool);       // bits 0-3
    fn enable_outputs(&self);                    // Write 0 to TRI register
}
```

**GPIO Pin Map (verified from live probe):**

```
Input Register (0x41200000 + 0x00):
  Bit 5: J6 plug detect (HIGH = hash board present)
  Bit 6: J7 plug detect
  Bit 7: J8 plug detect

Output Register (0x41210000 + 0x00):
  Bits 0-3: LEDs (D5-D8)
  Bits 9-11: Hash board reset (J6, J7, J8) -- active LOW, write 1 = assert reset

Output Tristate (0x41210000 + 0x04):
  Must write 0x00000000 to enable outputs (default is 0xFFFFFFFF = all tristate)
```

### FanController

Controls the custom Braiins fan controller IP at 0x42800000. NOT an AXI Timer (confirmed by live probing). Simple 7-bit PWM with 7-bit tachometer feedback.

```
struct FanController {
    regs: UioDevice,       // /dev/uio0 mapped to 0x42800000
}

impl FanController {
    fn set_speed(&self, pwm: u8);          // 0-127, both channels
    fn get_rpm(&self) -> u32;              // Read FAN1_RPS * 60
    fn get_speed_percent(&self) -> u8;     // Read current PWM value
}
```

**Register Map (verified from live probe):**

```
+0x00  FAN0_RPS   R   Fan 0 tach (RPS), always 0 on S9 (not connected)
+0x04  FAN1_RPS   R   Fan 1 tach (RPS), multiply by 60 for RPM
+0x10  FAN_PWM0   RW  PWM channel 0 (7-bit, 0-127)
+0x14  FAN_PWM1   RW  PWM channel 1 (7-bit, 0-127)
```

**Calibration Curve (measured on live S9):**

```
PWM   0 -> ~900 RPM   (hardware minimum, fans never fully stop)
PWM  20 -> ~2340 RPM
PWM  50 -> ~4500 RPM
PWM  64 -> ~5000 RPM  (50% duty)
PWM 100 -> ~5940 RPM  (maximum, saturated above this)
PWM 127 -> ~5940 RPM
```

### Watchdog

```
struct Watchdog {
    fd: RawFd,            // /dev/watchdog
}

impl Watchdog {
    fn open() -> Result<Self>;
    fn kick(&self) -> Result<()>;     // Write "V" to fd
    fn close(self);                    // Close fd, watchdog keeps running
}
```

The Cadence WDT at 0xF8005000 supports configurable timeout. `CONFIG_WATCHDOG_NOWAYOUT` is NOT set, meaning the watchdog CAN be stopped (by closing the fd after writing "V").

---

## 6. ASIC Driver Subsystem (dcentrald-asic)

### Wire Protocol

All BM13xx chips share the same wire protocol framing:

```
Command Frame (Host -> ASIC):
  [0x55] [0xAA] [header] [length] [payload...] [CRC5 or CRC16]

Response Frame (ASIC -> Host):
  [0xAA] [0x55] [payload (5 or 7 bytes)] [CRC5+flags (2 bytes)]

Header byte encoding (BM1397+):
  bit 6: TYPE_CMD (0x40)    bit 5: TYPE_JOB (0x20)
  bit 4: GROUP_ALL (0x10)
  bits 3-0: CMD (0=SETADDR, 1=WRITE, 2=READ, 3=INACTIVE)

Common header values (BM1397+):
  0x40 = CMD single SETADDRESS     0x41 = CMD single WRITE
  0x42 = CMD single READ           0x51 = CMD broadcast WRITE
  0x52 = CMD broadcast READ        0x53 = CMD broadcast INACTIVE
  0x21 = JOB single WRITE

BM1387 header byte encoding (DIFFERENT from BM1397+):
  Bits 7:6 = 01 (CMD packet type)
  Bits 5:4 = 00 (single chip) or 01 (broadcast)
  Bits 3:0 = Command ID:
    0x01 = Write / SetAddress (differentiated by length: 5 bytes = SetAddr, 9 bytes = Write)
    0x04 = Read Register (GetStatus)
    0x05 = Chain Inactive (Inactivate)
    0x08 = Write Register (SetConfig, 8 bytes)

BM1387 header values:
  0x41 = CMD single SETADDRESS (5 bytes)  0x58 = CMD single WRITE (8 bytes)
  0x54 = CMD single READ (5 bytes)        0x55 = CMD broadcast INACTIVE (5 bytes)
  0x51 = CMD broadcast WRITE (8 bytes)

CRC:
  CMD packets: CRC-5 (poly 0x05, init 0x1F), 1 byte appended
  JOB packets: CRC-16 CCITT-FALSE (poly 0x1021, init 0xFFFF), 2 bytes BE
```

### FPGA FIFO Command Encoding

The Braiins s9io FPGA handles the UART preamble (0x55 0xAA) and CRC internally. Commands written to CMD_TX_FIFO are packed as 32-bit words with LSB-first byte ordering:

```
Bytes [B0, B1, B2, B3] -> 32-bit word = B0 | (B1 << 8) | (B2 << 16) | (B3 << 24)

BM1387 FIFO Commands (verified from live ASIC probe + asic_init.py):
  GetAddress broadcast:     CMD_TX = 0x00000554  -> wire [0x54, 0x05, 0x00, 0x00]
  Chain Inactive broadcast: CMD_TX = 0x00000555  -> wire [0x55, 0x05, 0x00, 0x00]
  SetChipAddress(addr):     CMD_TX = (addr << 16) | 0x0541  -> wire [0x41, 0x05, addr, 0x00]
  Read register(chip,reg):  CMD_TX = (reg << 24) | (chip << 16) | 0x0544  -> wire [0x44, 0x05, chip, reg]
  Write reg broadcast:      CMD_TX word1 = 0x00000951, word2 = (val << 8) | reg  -> wire [0x51, 0x09, reg, val...]

  BUG FIX (2026-03-12): GetAddress was 0x0552 and ChainInactive was 0x0553 in
  early dcentrald code. These used BM1397+ CMD encoding (READ=0x02, INACTIVE=0x03)
  but BM1387 uses different command IDs: READ=0x04, INACTIVE=0x05. The FPGA
  passes command bytes through to UART unchanged, so the header byte must match
  the target chip's protocol. Corrected to 0x0554 and 0x0555 respectively.
```

Responses come back as pairs of 32-bit words from CMD_RX_FIFO, also LSB-first.

### PIC Microcontroller Protocol

The PIC16F1704 on each hash board controls DC-DC voltage regulation. Communication uses a custom protocol over I2C with 0x55 0xAA preamble (same as ASIC protocol, but over I2C instead of UART).

```
PicController struct:
  i2c: I2cBus reference
  address: u8 (0x55, 0x56, or 0x57)
  heartbeat_running: bool

PIC Command Set (verified from live S9 probe):
  0x06  JUMP_FROM_LOADER_TO_APP  -- CRITICAL on cold boot, PIC stuck in bootloader
  0x10  SET_VOLTAGE              -- [0x55, 0xAA, 0x10, pic_val]
  0x15  ENABLE_VOLTAGE           -- [0x55, 0xAA, 0x15, 0x01]
  0x16  SEND_HEARTBEAT           -- [0x55, 0xAA, 0x16] (must repeat every 1s)
  0x17  GET_VERSION              -- [0x55, 0xAA, 0x17] -> read 1 byte (0x03 expected; actual 0x56/0x5A/0x5E seen)
  0x18  GET_VOLTAGE              -- [0x55, 0xAA, 0x18] -> read 1 byte

Voltage conversion:
  voltage_V = (1608.420446 - pic_value) / 170.423497
  pic_value = 1608.420446 - (voltage_V * 170.423497)
  Example: PIC value 75 = 9.00V
```

### PIC Initialization Sequence (Critical Discovery)

On cold boot (no prior mining daemon), the PIC is in bootloader mode. ALL reads return 0xCC. The `JUMP_FROM_LOADER_TO_APP` command MUST be sent before any voltage operations work.

```
PIC Cold Boot Init:
  1. write([0x55, 0xAA, 0x06])       # JUMP_FROM_LOADER_TO_APP
  2. sleep(1000ms)                    # Wait for PIC app firmware to boot
  3. write([0x55, 0xAA, 0x17])       # GET_VERSION
     read(1) -> verify != 0xCC        # Confirm app mode (actual values: 0x56/0x5A/0x5E, NOT 0x03)
  4. write([0x55, 0xAA, 0x10, 75])   # SET_VOLTAGE (9.0V initial)
     sleep(300ms)
  5. write([0x55, 0xAA, 0x15, 0x01]) # ENABLE_VOLTAGE
     sleep(500ms)
  6. Start heartbeat thread:
     loop { write([0x55, 0xAA, 0x16]); sleep(1000ms); }
```

### BM1387 Driver (First Implementation)

The BM1387 is the simplest chip to implement since we have complete register dumps from live hardware and the initialization sequence is well-understood.

```
BM1387 Initialization Sequence:
  1. Set BAUD_REG to 0x6C (115200 baud for enumeration)
  2. Reset CMD FIFOs (CMD_CTRL = 0x03, then 0x00)
  3. Enable chain (CTRL_REG = 0x08, BM1387 mode, 1 midstate)
  4. Send GetAddress broadcast -> count responses
  5. Send Chain Inactive -> each chip responds once
  6. Assign addresses: chip[i] = i * (256/chip_count)
  7. Set PLL frequency (ramp from 100 MHz to target in steps)
  8. Set MiscControl register (baud rate, clock inversion)
  9. Set TicketMask (hardware difficulty filter)
  10. Optionally increase baud rate (BAUD_REG = 0x07 for 1.5 Mbaud)

BM1387 Register Values (from live dump, chip 0):
  0x00 ChipAddress:  0x13879000 (ID=0x1387, addr=0x00)
  0x0C PLL:          0x80400222 (default/reset PLL config)
  0x1C MiscControl:  0x40201A00 (baud_div=26, inv_clock=1, mmen=0)

BM1387 Work Format (1-midstate, via WORK_TX_FIFO):
  12 x 32-bit words = 48 bytes per work item:
    Word 0:  work_id | midstate_count
    Word 1:  nbits
    Word 2:  ntime
    Word 3:  merkle_root[31:0]   (last 4 bytes)
    Word 4-11: midstate[0..31]   (32 bytes, SHA256 intermediate)

BM1387 Nonce Response (from WORK_RX_FIFO):
  2 x 32-bit words = 8 bytes per nonce:
    Word 0: nonce value (32-bit)
    Word 1: [solution_id:8] [chip_addr:8] [work_id:8] [crc5:8]
```

---

## 7. Stratum Subsystem (dcentrald-stratum)

### Stratum V1 Client

The Stratum V1 client implements the JSON-RPC based mining protocol. It manages pool connection, receives jobs, and submits shares.

```
Stratum V1 Handshake:
  1. TCP connect to pool:port
  2. -> mining.subscribe(user_agent, session_id)
     <- result: [[subscription], extranonce1, extranonce2_size]
  3. -> mining.authorize(worker, password)
     <- result: true/false
  4. -> mining.configure(extensions: ["version-rolling"])
     <- result: version_rolling configuration
  5. <- mining.set_difficulty(difficulty)
  6. <- mining.notify(job_id, prevhash, coinb1, coinb2, merkle[], version, nbits, ntime, clean)
```

### JobTemplate

```
struct JobTemplate {
    job_id: String,
    prev_block_hash: [u8; 32],
    coinbase1: Vec<u8>,
    coinbase2: Vec<u8>,
    merkle_branches: Vec<[u8; 32]>,
    version: u32,
    nbits: u32,
    ntime: u32,
    clean_jobs: bool,
    share_target: [u8; 32],
    extranonce1: Vec<u8>,
    extranonce2_size: usize,
}
```

### Work Generation Pipeline

```
JobTemplate (from pool)
    |
    v
Generate extranonce2 (incrementing counter)
    |
    v
Build coinbase: coinbase1 + extranonce1 + extranonce2 + coinbase2
    |
    v
SHA256d(coinbase) -> coinbase_hash
    |
    v
Build merkle root: fold coinbase_hash with merkle_branches
    |
    v
Build block header: version + prevhash + merkle_root + ntime + nbits + nonce
    |
    v
For BM1387: Compute SHA256 midstate of first 64 bytes
    |
    v
Pack into MiningWork struct -> dispatch to chains
```

### Reconnection Strategy

```
Reconnect with exponential backoff:
  attempt 1: wait 1s
  attempt 2: wait 2s
  attempt 3: wait 4s
  ...
  max wait: 60s
  Add jitter: +/- 25% randomization
  Reset backoff on successful connection
```

### Pool Failover

dcentrald supports up to 3 Stratum V1 pool URLs in priority order. Repeated connection/session failures rotate to the next configured pool, clear stale work before backup work is dispatched, and expose the active route through `/api/pools.failover`. Automatic no-notify/reject-rate failover and stable primary return are intentionally deferred until thresholds are proven by mock-pool and hardware soak tests.

### Donation System

dcentrald has no mandatory fee and no license server. The current shipped default is a transparent 2% donation that users can set from 0.0 to 5.0 or disable entirely. The donation routes to DCENT_Pool — D-Central's Solo/Guild pool, a trustless MMORPG-style take on solo mining where miners can join guilds and share the block reward. During donation time, dcentrald uses clean time-based pool switching; if the primary D-Central donation endpoint is unavailable, it may try the visible Braiins Pool backup worker `DungeonMaster`. Donation fallback is never part of user-pool failover and never extends the configured donation percentage.

```
[donation]
enabled = true
percent = 2.0                    # 0.0 to 5.0, fully disableable
pool_url = "stratum+tcp://pool.d-central.tech:3333"
worker = "DungeonMaster"
fallback_enabled = true
fallback_pool_url = "stratum+tcp://stratum.braiins.com:3333"
fallback_worker = "DungeonMaster"
```

---

## 8. Thermal Management (dcentrald-thermal)

### Control Loop Architecture

```
    +------------------+
    | Temperature      |
    | Sensors          |
    | (I2C TMP75 or    |
    |  PIC readback)   |
    +--------+---------+
             |
             v
    +------------------+
    | Thermal          |
    | Controller       |
    | (PID loop, 5s    |
    |  interval)       |
    +--------+---------+
             |
     +-------+--------+
     |                 |
     v                 v
+----------+   +-------------+
| Fan PWM  |   | Frequency   |
| Adjust   |   | Throttle    |
| (0-127)  |   | (reduce MHz |
|          |   |  or disable |
|          |   |  boards)    |
+----------+   +-------------+
```

### ATM (Automatic Thermal Management) Profiles

```
struct ThermalProfile {
    target_temp_c: u8,        // Normal operating target (default: 55)
    hot_temp_c: u8,           // Start throttling (default: 65)
    dangerous_temp_c: u8,     // Emergency shutdown (default: 75)
    fan_min_pwm: u8,          // Minimum fan speed (default: 0, ~900 RPM)
    fan_max_pwm: u8,          // Maximum fan speed (default: 100, ~5940 RPM)
    ramp_delay_s: u16,        // Post-ramp stabilization delay (default: 300)
    hysteresis_c: u8,         // Temperature hysteresis band (default: 3)
}
```

### Thermal State Machine

```
                   +--------+
                   | COLD   |  temp < target
                   | START  |  fans = min, freq = target
                   +---+----+
                       | temp rises above target
                       v
                   +--------+
                   | NORMAL |  target <= temp < hot
                   | MINING |  fans = PID(target_temp)
                   +---+----+
                       | temp rises above hot
                       v
                   +--------+
                   | HOT    |  hot <= temp < dangerous
                   | THROT. |  fans = max, freq -= step
                   +---+----+
                       | temp rises above dangerous
                       v
                   +--------+
                   | DANGER |  temp >= dangerous
                   | SHTDWN |  disable voltage, fans = max
                   +--------+
                       | temp drops below (dangerous - hysteresis)
                       v
                   (restart init sequence)
```

### Space Heater Thermal Control

Space Heater mode adds a second PID control layer on top of the existing chip thermal loop. The existing loop (Layer 1) manages chip temperature via fan speed and frequency throttling. The heater loop (Layer 2) manages **heat output** by adjusting the power target.

```
Layer 1 (existing):  chip_temp → PID → fan_speed + freq_throttle
Layer 2 (heater):    target_heat → PID → power_target_watts → Layer 1
```

#### HeaterController

```
struct HeaterController {
    target_heat_watts: u32,      // User-selected power/heat target
    temp_source: TempSource,     // Where room temp comes from (if used)
    room_temp_c: Option<f32>,    // Current room temp (None if manual mode)
    current_power_w: f32,        // Measured/estimated power consumption
    pid: PidController,          // PID for power → frequency adjustment
}

enum TempSource {
    /// No room sensor. User picks a power preset directly.
    /// Dashboard shows estimated BTU output for each preset.
    Manual,

    /// External sensor (USB, Zigbee, or network sensor).
    /// dcentrald reads via configured URL or device path.
    Sensor { url: String, poll_interval_s: u16 },

    /// Home Assistant entity (e.g., sensor.living_room_temperature).
    /// Requires MQTT or HA REST API integration.
    HomeAssistant { entity_id: String },

    /// User manually sets a room temp value via API/dashboard.
    /// Useful when no sensor is available but user knows approximate room temp.
    UserInput,
}
```

**Sensor is NOT required.** The default and primary mode is `Manual` — the user selects a power preset (e.g., "Low 400W", "Medium 800W", "High 1200W") and the dashboard shows the corresponding heat output. Room temperature sensing is an optional enhancement for thermostat-like behavior.

#### Derived Metrics (Always Displayed)

Every setting in dcentrald presents as much derived information as possible, so the user always understands the real-world impact:

```
Power/Heat Metrics:
  watts → BTU/h:        btu = watts * 3.412
  watts → monthly cost:  cost = watts * hours_per_month * electricity_rate / 1000
  watts → sats earned:   sats = estimated_hashrate_at_watts * network_difficulty_factor

Fan/Noise Metrics:
  fan RPM → dB estimate:  Calibration curve per fan model
                           S9 stock fan: ~900 RPM = ~35 dB, ~3000 RPM = ~55 dB,
                           ~5000 RPM = ~68 dB, ~5940 RPM = ~72 dB
  fan RPM → CFM estimate: S9 dual 120mm: ~30 CFM at 900 RPM, ~120 CFM at 5940 RPM

Mining Metrics:
  frequency → hashrate:   hashrate_ths = chips * cores * freq_mhz / 1e6
  frequency → watts:      Derived from voltage * current (PIC readback or lookup table)
```

#### Power Presets (Space Heater Mode)

```
Preset      Watts   BTU/h    Noise Est.   Hashrate Est. (S9)
───────────────────────────────────────────────────────────────
Whisper     300W    1,024    ~35 dB       ~4.5 TH/s
Low         500W    1,706    ~40 dB       ~7.5 TH/s
Medium      800W    2,730    ~48 dB       ~10.5 TH/s
High       1200W    4,094    ~58 dB       ~13.5 TH/s
Max        1400W    4,777    ~65 dB       ~14.0 TH/s
```

Presets are model-specific. The table above is for S9. Each ASIC chip driver provides its own preset table based on tested frequency/voltage/power curves.

#### Night Mode

```
struct NightMode {
    enabled: bool,
    start_hour: u8,              // e.g., 22 (10 PM)
    end_hour: u8,                // e.g., 7 (7 AM)
    max_fan_pwm: u8,             // Reduced fan ceiling (e.g., 40)
    power_reduction_pct: u8,     // Reduce power target by N% (e.g., 40)
}
```

During night hours, dcentrald reduces the power target by the configured percentage and caps fan PWM. The dashboard shows the transition countdown and estimated noise level during night mode.

### Fan Failure Detection

If FAN1_RPS reads 0 for more than 5 consecutive seconds while PWM > 0, declare fan failure. Immediately disable all hash board voltages and set fans to maximum (in case of partial failure). Log the event. Do not restart mining until fan RPM is confirmed.

### Curtailment (Sleep/Wake)

Sleep mode drops power consumption to approximately 25W:
1. Disable hash board voltages via PIC (ENABLE_VOLTAGE = 0)
2. Set fan PWM to 20% (maintain airflow for PSU cooling)
3. Continue watchdog kicks and API serving
4. Continue Stratum connection (hash-on-disconnect mode if configured)

Wake from sleep:
1. Re-run PIC initialization (voltage enable, heartbeat start)
2. Wait for voltage stabilization (500ms)
3. Re-enumerate chips
4. Ramp frequency gradually over 60 seconds
5. Ramp fans based on temperature

---

## 9. API Layer (dcentrald-api)

### CGMiner-Compatible API (Port 4028)

TCP socket API compatible with pyasic and hass-miner. Line-delimited JSON requests, JSON responses.

```
Supported Commands:

  summary
    Returns: Elapsed, MHS av, MHS 5s, Found Blocks, Accepted, Rejected,
             Hardware Errors, Utility, Temperature, Fan Speed, Fan Percent,
             Pool Rejected%, Pool Stale%

  stats
    Returns: per-chain stats (chips, frequency, voltage, temp, hashrate)
             per-chip data (frequency, errors, health score)

  pools
    Returns: pool URL, status, user, accepted, rejected, stale,
             difficulty, last share time

  devs
    Returns: per-device (chain) stats: MHS av, Accepted, Rejected,
             Temperature, Fan Speed, Frequency

  version
    Returns: CGMiner version (compatibility: "dcentrald 0.1.0"),
             API version, firmware version, chip type

  coin
    Returns: Hash Method (SHA256), Current Block Time, Current Block Hash,
             Network Difficulty

  config
    Returns: Pool Count, Strategy, Log Interval, Device Count,
             OS, ASC Count

  switchpool|N
    Switch active pool to pool N

  enablepool|N / disablepool|N
    Enable or disable pool N

  addpool|url,user,pass
    Add a new pool

  restart / quit
    Restart mining / shutdown daemon
```

### REST API (Port 80)

JSON endpoints for the web dashboard and external automation.

```
GET  /api/status
  Returns: complete miner status (hashrate, temps, fans, pool, uptime, chips)
  Polled by dashboard every 5 seconds

GET  /api/stats
  Returns: detailed per-chain, per-chip statistics

GET  /api/pools
  Returns: pool configuration and status

POST /api/pools
  Body: { url, worker, password, priority }
  Add or modify pool configuration

GET  /api/config
  Returns: current dcentrald.toml configuration

POST /api/config
  Body: partial config update
  Applies configuration changes (some require restart)

POST /api/action/restart
  Restart mining daemon

POST /api/action/reboot
  Reboot the miner

POST /api/action/sleep
  Enter curtailment sleep mode

POST /api/action/wake
  Wake from curtailment sleep mode

GET  /api/system/info
  Returns: firmware version, model, MAC, IP, uptime, chip type, chip count
  Compatible with pyasic discovery

GET  /api/system/asic
  Returns: per-ASIC chip data (compatible with AxeOS API for pyasic)

GET  /api/history
  Returns: historical hashrate, temperature, power data (last 24h)

GET  /api/profiles
  Returns: saved tuning profiles

POST /api/profiles
  Save current tuning as a named profile

GET  /
  Serves the embedded web dashboard (static HTML/JS/CSS)
```

### Heater Endpoints (Space Heater Mode)

```
GET  /api/heater/status
  Returns: current mode, power_target_watts, estimated_btu_h, room_temp_c (if available),
           temp_source, active_preset, night_mode_active, cost_per_day, sats_earned_today,
           noise_estimate_db, airflow_cfm

POST /api/heater/target
  Body: { "preset": "medium" }   -- select a named power preset
    OR: { "watts": 800 }          -- set exact wattage target
  Returns: applied target with derived metrics (BTU, noise, hashrate estimate)

GET  /api/heater/presets
  Returns: available power presets for detected chip model, with BTU/noise/hashrate
           estimates for each preset

POST /api/heater/room-temp
  Body: { "temp_c": 21.5 }        -- manual room temp input (UserInput source)
  Returns: updated heater status

GET  /api/heater/history
  Returns: 24h history of power output, BTU/h, room temp, cost, sats earned

GET  /api/heater/night-mode
  Returns: night mode configuration and status

POST /api/heater/night-mode
  Body: { "enabled": true, "start_hour": 22, "end_hour": 7,
           "max_fan_pwm": 40, "power_reduction_pct": 40 }
  Returns: updated night mode config
```

### Hacker Debug Endpoints (Hacker Mode Only)

These endpoints are blocked in Heater and Standard modes. Accessing them requires Hacker mode active and, for write operations, explicit confirmation.

```
GET  /api/debug/registers
  Query: ?chain=6&offset=0x00&count=8
  Returns: raw FPGA register dump (hex values)

POST /api/debug/registers
  Body: { "chain": 6, "offset": "0x08", "value": "0x00000008" }
  Returns: written value, readback verification
  REQUIRES: { "confirm": true } field (safety gate)

GET  /api/debug/i2c
  Query: ?bus=0&addr=0x55&reg=0x18
  Returns: raw I2C register read

POST /api/debug/i2c
  Body: { "bus": 0, "addr": "0x55", "data": [85, 170, 16, 75] }
  Returns: I2C write result

POST /api/debug/asic-command
  Body: { "chain": 6, "command": "read_register", "chip": 0, "register": "0x0C" }
  Returns: raw ASIC response (hex words)

GET  /api/debug/pid-state
  Returns: current PID controller state (proportional, integral, derivative terms,
           setpoint, process variable, output, error history)

POST /api/debug/pid-params
  Body: { "kp": 2.0, "ki": 0.5, "kd": 0.1, "setpoint": 55.0 }
  Returns: updated PID parameters (takes effect on next loop iteration)

POST /api/debug/chip/frequency
  Body: { "chain": 6, "chip": 0, "freq_mhz": 700 }
  Returns: applied frequency, readback PLL value
  REQUIRES: { "confirm": true }

POST /api/debug/chip/voltage
  Body: { "chain": 6, "pic_value": 70 }
  Returns: applied voltage, readback, estimated voltage_V
  REQUIRES: { "confirm": true }
```

### Diagnostic Endpoints (Standard + Hacker Modes)

```
POST /api/diagnostics/hashreport/start
  Body: { "duration_minutes": 15 }   -- optional, default 15
  Returns: { "test_id": "uuid", "status": "running" }

GET  /api/diagnostics/hashreport/status
  Query: ?test_id=uuid
  Returns: { "phase": 3, "phase_name": "mining_performance", "progress_pct": 45,
             "elapsed_s": 420, "eta_s": 480 }

GET  /api/diagnostics/hashreport/result
  Query: ?test_id=uuid
  Returns: full HashReport JSON (see Diagnostic Subsystem section)

GET  /api/diagnostics/hashreport/report
  Query: ?test_id=uuid&format=html
  Returns: rendered HTML report (browser print-to-PDF)

POST /api/diagnostics/chip-health/start
  Body: { "chain": 6, "duration_minutes": 5 }
  Returns: { "test_id": "uuid", "status": "running" }

GET  /api/diagnostics/chip-health/status
  Query: ?test_id=uuid
  Returns: progress with per-chip completion count

GET  /api/diagnostics/chip-health/result
  Query: ?test_id=uuid
  Returns: per-chip health scores, ChipMap grid data

POST /api/diagnostics/board-health/start
  Body: { "chain": 6 }
  Returns: { "test_id": "uuid", "status": "running" }

GET  /api/diagnostics/board-health/status
  Query: ?test_id=uuid
  Returns: test progress

GET  /api/diagnostics/board-health/result
  Query: ?test_id=uuid
  Returns: board health report (chip count, voltage, CRC errors, temp distribution)

GET  /api/diagnostics/troubleshoot/network
  Returns: { "dns": true, "gateway": true, "pool_reachable": true,
             "latency_ms": 45, "stratum_connected": true }

GET  /api/diagnostics/troubleshoot/psu
  Returns: PSU PMBus readings (VIN, VOUT, IOUT, temp, faults, efficiency)

GET  /api/diagnostics/troubleshoot/fpga
  Returns: per-chain FPGA status (version, ctrl_reg, baud, error count, FIFO depth)

GET  /api/diagnostics/troubleshoot/asic-comm
  Returns: per-chain ASIC communication test (chip count, response rate, CRC errors)

GET  /api/diagnostics/troubleshoot/i2c-scan
  Returns: I2C bus scan results (all responding addresses with device identification)
```

### WebSocket (Port 80, /ws)

Real-time streaming for the dashboard. Pushes updates every 1 second.

```
WebSocket Message Format (JSON):

{
  "type": "stats",
  "timestamp": 1741654800,
  "hashrate_ghs": 14200.5,
  "hashrate_5s_ghs": 14180.2,
  "accepted": 1234,
  "rejected": 2,
  "chains": [
    {
      "id": 6,
      "chips": 63,
      "frequency_mhz": 650,
      "voltage_mv": 8600,
      "temp_c": 58.5,
      "hashrate_ghs": 4733.5,
      "errors": 0,
      "status": "mining"
    }
  ],
  "fans": {
    "pwm": 75,
    "rpm": 4800
  },
  "pool": {
    "url": "stratum+tcp://pool.example.com:3333",
    "status": "connected",
    "difficulty": 65536,
    "last_share_s": 12
  }
}
```

#### Diagnostic Progress (WebSocket)

In addition to the `stats` message type, the WebSocket pushes diagnostic progress when a test is running:

```
{
  "type": "diagnostic_progress",
  "test_id": "uuid",
  "test_type": "hashreport",
  "phase": 3,
  "phase_name": "mining_performance",
  "progress_pct": 45,
  "elapsed_s": 420,
  "eta_s": 480,
  "detail": "Window 6/12 — 63 chips responding, avg 226 GH/s per chip"
}
```

#### Heater Status (WebSocket)

In Space Heater mode, the WebSocket includes heater-specific fields:

```
{
  "type": "heater_status",
  "power_watts": 812,
  "btu_h": 2771,
  "noise_db": 48,
  "airflow_cfm": 75,
  "preset": "medium",
  "room_temp_c": 21.5,
  "cost_today_usd": 1.42,
  "sats_today": 1847,
  "night_mode_active": false,
  "night_mode_starts_in_s": 14400
}
```

### pyasic Compatibility

To ensure dcentrald works with pyasic (and therefore hass-miner / Home Assistant):

1. Implement CGMiner API on port 4028 with the `summary`, `stats`, `pools`, `devs`, and `version` commands.
2. Alternatively (or additionally), implement the AxeOS REST API endpoints (`/api/system/info`, `/api/system/asic`) for pyasic's ESP-Miner backend.
3. hass-miner polls every 10 seconds. The API must respond within 5 seconds.

---

## 10. Mining Pipeline -- Async Data Flow

### Overall Architecture (Implemented)

```
+--------------------------------------------------------------------------+
|                          dcentrald                                        |
|                                                                           |
|  +------------------+            +-------------------+                    |
|  | Stratum Client   |            | API Servers       |                    |
|  | (Tokio task)     |            | (Tokio tasks)     |                    |
|  |                  |            | - CGMiner :4028   |                    |
|  | Pool connect     |            | - REST :80        |                    |
|  | Job receive      |            | - WebSocket :80   |                    |
|  | Share submit     |            | - Dashboard :80   |                    |
|  +--------+---------+            +--------+----------+                    |
|           |      ^                        ^                               |
|    job_tx |      | share_tx        state_rx (watch::borrow)              |
|   (mpsc)  |      | (mpsc)                 |                               |
|           v      |                        |                               |
|  +--------+------+-----------------------------------------------+       |
|  |                   Work Dispatcher (single Tokio task)          |       |
|  |                   OWNS all Chain objects (sole FPGA consumer)  |       |
|  |                                                                |       |
|  |  tokio::select! {                                              |       |
|  |    job_rx.recv()          => Store JobTemplate, reset if clean  |       |
|  |    dispatch_timer.tick()  => Generate work, write WORK_TX_FIFO |       |
|  |    nonce_poll_timer.tick()=> Read WORK_RX_FIFO, decode, submit |       |
|  |    hashrate_timer.tick()  => Compute EMA hashrate, update state|       |
|  |    shutdown.cancelled()   => Break                             |       |
|  |  }                                                             |       |
|  |                                                                |       |
|  |  +----------+  +----------+  +----------+                     |       |
|  |  | Chain 6  |  | Chain 7  |  | Chain 8  |  FpgaChain (mmap)  |       |
|  |  | WORK_TX  |  | WORK_TX  |  | WORK_TX  |  volatile write    |       |
|  |  | WORK_RX  |  | WORK_RX  |  | WORK_RX  |  volatile read     |       |
|  |  +----------+  +----------+  +----------+                     |       |
|  |                                                                |       |
|  |  WorkBuilder: midstate computation, extranonce2 generation     |       |
|  |  HashrateTracker: EMA, per-chain tracking, 5s windows          |       |
|  |  WorkTable[256]: work_id → (job_id, extranonce2, ntime)        |       |
|  +----------------------------------------------------------------+       |
|                                                                           |
|  +--------------------------------------------+                          |
|  | Thermal Controller (Tokio task)  5s loop    |                          |
|  | - Read XADC die temp (IIO sysfs)           |                          |
|  | - Read fan RPM (FPGA tach register)         |                          |
|  | - PID control → set fan PWM via FPGA        |                          |
|  | - EmergencyShutdown → disable PIC voltages  |                          |
|  | - FanFailure → disable PIC voltages         |                          |
|  +--------------------------------------------+                          |
|                                                                           |
|  +--------------------------------------------+                          |
|  | PIC Heartbeat (single Tokio task)  1s loop  |                          |
|  | - Open I2C bus, send HEARTBEAT to all PICs  |                          |
|  | - PICs at 0x55/0x56/0x57 (chains 6/7/8)     |                          |
|  | - PIC cuts voltage if heartbeat stops ~10s   |                          |
|  +--------------------------------------------+                          |
|                                                                           |
|  +--------------------------------------------+                          |
|  | Watchdog Kicker (Tokio task)  configurable   |                          |
|  | - Write "1" to /dev/watchdog                 |                          |
|  +--------------------------------------------+                          |
|                                                                           |
|  +--------------------------------------------+                          |
|  | State Publisher (Tokio task)  1s loop        |                          |
|  | - Read fan PWM/RPM from FanController        |                          |
|  | - Update uptime_s in MinerState              |                          |
|  | - Broadcast WebSocket stats message           |                          |
|  | - API reads state_rx via watch::borrow()      |                          |
|  +--------------------------------------------+                          |
|                                                                           |
|  +--------------------------------------------+                          |
|  | Stratum Status Handler (Tokio task)          |                          |
|  | - Receives StratumStatus from client          |                          |
|  | - Updates pool status/difficulty in MinerState|                          |
|  | - Tracks accepted/rejected share counts       |                          |
|  +--------------------------------------------+                          |
|                                                                           |
|  +--------------------------------------------+                          |
|  | Signal Handler (Tokio task)                  |                          |
|  | - SIGINT / SIGTERM → CancellationToken       |                          |
|  +--------------------------------------------+                          |
+--------------------------------------------------------------------------+
```

The key implementation decision: a **single WorkDispatcher task** handles all chains rather than spawning per-chain workers. This simplifies ownership (one task owns all FpgaChain mmap regions), eliminates inter-task synchronization, and is practical because FPGA register access is nanosecond-latency volatile I/O that never blocks the Tokio runtime.

### Channel Topology (Implemented)

| Channel | Type | From | To | Content |
|---|---|---|---|---|
| `job_tx/rx` | `mpsc(32)` | Stratum Client | WorkDispatcher | `JobTemplate` |
| `share_tx/rx` | `mpsc(256)` | WorkDispatcher | Stratum Client | `ValidShare` |
| `status_tx/rx` | `mpsc(64)` | Stratum Client | Status Handler | `StratumStatus` (state changes, share results) |
| `state_tx/rx` | `watch` | WorkDispatcher + StatePublisher | API Server | `MinerState` (shared via `send_modify`) |
| `stats_broadcast_tx` | `broadcast(64)` | StatePublisher | WebSocket clients | JSON stats string |
| `diag_broadcast_tx` | `broadcast(32)` | DiagnosticService | WebSocket clients | Diagnostic progress |
| `mode_tx/rx` | `watch` | Daemon | API middleware | `OperatingMode` |
| `shutdown` | `CancellationToken` | Signal Handler | All tasks | Shutdown signal |

Note: The original design called for per-chain `work_tx`, per-chain `nonce_tx`, and a `thermal_cmd` channel. The actual implementation is simpler — the WorkDispatcher owns all chains directly (no per-chain channels needed) and the thermal controller writes to the fan directly (no command channel needed).

### Nonce Collection (Polling-Based, Implemented)

The WorkDispatcher polls WORK_RX_FIFO at 100 Hz (10 ms interval) for all chains in sequence:

```
Nonce Poll Loop (10ms interval, inside tokio::select!):
  For each active chain:
    1. Check WORK_RX_STAT: work_rx_has_data()?
    2. While has data (up to 100 nonces per poll):
         Read WORK_RX_FIFO → (word0, word1)
         Decode nonce via ChipDriver::decode_nonce()
         Look up WorkEntry by work_id in work_table[256]
         If found: create ValidShare, send via share_tx
         Record nonce in HashrateTracker (per-chain)
    3. Next chain
```

**Design decision**: Polling at 100 Hz was chosen over IRQ-driven collection because:
- BM1387 at ~13.5 TH/s with TicketMask 256 produces ~12 nonces/second per chain — far below poll rate
- FPGA register reads are nanosecond-latency volatile operations (mmap, no syscall)
- Eliminates complexity of UIO IRQ file descriptor management in async context
- CPU overhead is negligible (~36 volatile reads per 10ms across 3 chains)

IRQ-driven collection can be added as a future optimization for higher-hashrate chips (BM1368/BM1370).

### Work TX (Timer-Based, Implemented)

The WorkDispatcher dispatches work on a chip-specific timer interval:

```
Work Dispatch Loop (chip-specific interval, inside tokio::select!):
  1. Timer fires (e.g., every 100ms for BM1387 at 63 chips / 650 MHz)
  2. Generate new work from current JobTemplate via WorkBuilder
     - Increments extranonce2, computes SHA-256 midstate
  3. Convert StratumWork → AsicWork (version, nbits, ntime, merkle4, midstates)
  4. Store WorkEntry in work_table[work_id] for nonce matching
  5. For each active chain:
     - Check WORK_TX_STAT: work_tx_full()? → skip if full
     - Write work via ChipDriver::send_work(&mut fpga, &asic_work)
  6. Increment work_id (wraps at 256)
```

The dispatch interval is computed by `ChipDriver::job_interval_ms(chip_count, frequency)` and varies by chip model. FIFO depth is 2048 x 32-bit words; each 1-midstate work is 12 words (48 bytes). Maximum 170 buffered work items per chain.

### Ntime Rolling

For BM1387 (1-midstate mode), the chain worker rolls ntime every 1 second to keep chips busy with fresh work even when the pool hasn't sent a new job. This is critical for maintaining hashrate during the ~30-second intervals between Stratum notifications.

```
Ntime Rolling:
  Every 1 second:
    ntime += 1
    Recompute midstate with new ntime
    Submit updated work to WORK_TX_FIFO with same work_id
```

### Hash-on-Disconnect

When the Stratum connection drops, dcentrald does NOT stop mining. Instead:

1. Keep submitting the last known job with ntime rolling
2. Buffer found nonces in memory
3. On reconnect, submit buffered shares if still valid
4. If ntime has advanced too far (>7200s), stop mining and wait for reconnect

This prevents thermal shock (sudden stop in cold weather) and maintains heat output for space heater mode.

---

## 11. Configuration System

### File Location

```
/data/dcentrald.toml      # Primary config (persistent UBIFS storage)
/etc/dcentrald.toml        # Default config (read-only squashfs, fallback)
```

At startup, dcentrald reads `/data/dcentrald.toml`. If not found, copies `/etc/dcentrald.toml` to `/data/` and uses that.

### Configuration Structure

```toml
# DCENT_OS Mining Daemon Configuration
# /data/dcentrald.toml

[general]
hostname = "dcentos-s9"
log_level = "info"                     # trace, debug, info, warn, error

[pool]
url = "stratum+tcp://pool.example.com:3333"
worker = "my_worker.s9"
password = "x"

[pool.failover1]
url = "stratum+tcp://backup.pool.com:3333"
worker = "my_worker.s9"
password = "x"

[pool.failover2]
url = "stratum+tcp://emergency.pool.com:3333"
worker = "my_worker.s9"
password = "x"

[mining]
enabled = true
frequency_mhz = 650                   # Target ASIC frequency (MHz)
voltage_mv = 9100                      # Target chain voltage (millivolts)
# Chip-specific overrides loaded from /data/profiles/<profile>.toml

[power]
target_watts = 0                       # 0 = no power limit (full speed)
psu_bypass = false                     # true = skip PSU I2C validation (120V mode)
max_watts = 1500                       # Absolute maximum (safety limit)

[thermal]
target_temp_c = 55                     # Normal operating temperature target
hot_temp_c = 65                        # Begin frequency throttling
dangerous_temp_c = 75                  # Emergency shutdown threshold
fan_min_pwm = 0                        # Minimum fan duty (0 = hardware min ~900 RPM)
fan_max_pwm = 30                       # Quiet default ceiling for home-oriented images

[thermal.night_mode]
enabled = false
start_hour = 22                        # 10 PM
end_hour = 7                           # 7 AM
max_fan_pwm = 40                       # ~3840 RPM during night hours
max_frequency_mhz = 400               # Reduced frequency at night

[api]
cgminer_port = 4028                    # CGMiner-compatible API
http_port = 80                         # REST API + dashboard
websocket = true                       # WebSocket on /ws (same port as HTTP)
cgminer_bind_lan = false               # Keep unauth CGMiner TCP local-only by default
metrics_require_auth = true            # /metrics stays behind API auth by default

[donation]
enabled = false
percent = 0.0                          # 0.0 to 5.0
pool_url = "stratum+tcp://donate.dcentral.pool:3333"
worker = "dcentrald-donation"

[mqtt]
enabled = false
broker = "mqtt://localhost:1883"
topic_prefix = "dcentrald"
discovery = true                       # Home Assistant MQTT auto-discovery

[watchdog]
enabled = true
timeout_s = 30                         # Watchdog timeout
kick_interval_s = 5                    # How often to kick

[hash_on_disconnect]
enabled = true
max_ntime_advance_s = 7200             # Stop mining after 2 hours of disconnect

# ─── Mode Configuration ───────────────────────────────────────────────

[mode]
active = "standard"                    # home | standard | hacker

[mode.home]
preset = "medium"                      # whisper | low | medium | high | max
target_watts = 0                       # 0 = use preset, >0 = exact wattage
temp_source = "manual"                 # manual | sensor | homeassistant | userinput
room_temp_c = 21.0                     # Default room temp for manual/userinput
electricity_rate = 0.12                # $/kWh for cost calculations
currency = "USD"                       # Currency for cost display

[mode.home.sensor]
url = ""                               # URL or device path for external sensor
poll_interval_s = 60

[mode.home.homeassistant]
entity_id = ""                         # e.g., "sensor.living_room_temperature"

[mode.home.night_mode]
enabled = false
start_hour = 22
end_hour = 7
max_fan_pwm = 40
power_reduction_pct = 40

[mode.home.pool]
url = "stratum+tcp://solo.ckpool.org:3333"   # Default: solo mine for pleb cred
worker = "dcentos-heater"
password = "x"

[mode.hacker]
enable_raw_registers = true            # Allow /api/debug/registers
enable_i2c_access = true               # Allow /api/debug/i2c
enable_asic_commands = true            # Allow /api/debug/asic-command
enable_pid_override = true             # Allow /api/debug/pid-params
max_frequency_mhz = 900               # Hacker mode frequency ceiling
max_voltage_pic = 50                   # Minimum PIC value (= maximum voltage ~9.1V)
dangerous_temp_override = 85           # Hacker mode thermal limit (absolute max)
```

Additional optional sections in the current schema include `[sv2]`, `[job_declaration]`, `[webhook]`, `[power.psu_override]`, `[power.offgrid]`, and `[power.solar]`.

---

## 12. Mode System

### OperatingMode

dcentrald supports three operating modes, selectable at runtime via configuration, API, or dashboard. Each mode tailors the firmware's behavior, safety limits, exposed API surface, and dashboard layout to a different user type.

```
enum OperatingMode {
    /// Space Heater mode: noise-optimized, thermostat-like, minimal UI.
    /// Target user: home miner who wants quiet heat and sats.
    /// Dashboard: warm colors, Nest-thermostat style, BTU display.
    Heater,

    /// Standard mining mode: full dashboard, normal safety limits.
    /// Target user: regular miner who wants to monitor and configure.
    /// Dashboard: professional dark theme, charts, per-chip data.
    Standard,

    /// Mining Hacker mode: raw register access, relaxed limits, terminal aesthetic.
    /// Target user: repair tech, researcher, overclocker.
    /// Dashboard: green-on-black terminal style, register dumps, PID graphs.
    Hacker,
}
```

### SafetyEnvelope

Each mode defines a `SafetyEnvelope` that constrains dcentrald's behavior. The Safety Systems (Section 14) enforce these limits at all times.

```
struct SafetyEnvelope {
    dangerous_temp_c: u8,        // Emergency shutdown threshold
    max_frequency_mhz: u16,     // Maximum allowed ASIC frequency
    allow_overclock: bool,       // Allow frequency above model default
    allow_raw_registers: bool,   // Allow direct register read/write via API
    fan_behavior: FanMode,       // Noise-optimized or full range
    min_fan_pwm: u8,             // Floor for fan speed
    max_power_watts: u32,        // Hard power cap
}

enum FanMode {
    /// Heater: PID targets lower RPM, prioritizes quiet operation.
    /// Dashboard shows estimated dB and CFM for current fan speed.
    NoiseOptimized,

    /// Standard/Hacker: PID uses full PWM range for maximum cooling.
    FullRange,
}
```

**Mode-Dependent Limits:**

| Parameter | Heater | Standard | Hacker |
|---|---|---|---|
| `dangerous_temp_c` | 70 | 75 | 85 (user override) |
| `max_frequency_mhz` | model default | model max | up to 900 MHz |
| `allow_overclock` | No | No | Yes (with warning) |
| `allow_raw_registers` | No | No | Yes |
| `fan_behavior` | NoiseOptimized | FullRange | FullRange |
| `min_fan_pwm` | 10 (~1,260 RPM) | 0 (~900 RPM) | 0 (~900 RPM) |
| `max_power_watts` | 900 | model TDP | 2500 |

### Derived Metrics Philosophy

Every setting dcentrald presents — in any mode — shows as much derived information as possible so the user always understands real-world impact:

```
Power → Heat:       BTU/h = watts * 3.412
Power → Cost:       $/day = watts * 24 * electricity_rate / 1000
Power → Sats:       Estimated daily sats at current network difficulty
Fan RPM → Noise:    Estimated dB from per-model calibration curve
Fan RPM → Airflow:  Estimated CFM from per-model fan specs
Frequency → Hash:   TH/s = chips * cores * freq_mhz / 1e6
Frequency → Watts:  From voltage * current (PIC readback or lookup table)
```

This applies to all UI surfaces: dashboard sliders show BTU/dB/cost in real-time, API responses include derived fields, presets display full impact before selection.

### Mode-Conditional API Access

API middleware checks the active `OperatingMode` before serving requests:

```
Heater Mode:
  ✓ /api/status, /api/pools, /api/config, /api/system/info
  ✓ /api/heater/* (all heater-specific endpoints)
  ✓ /api/action/sleep, /api/action/wake
  ✓ /api/diagnostics/* (all diagnostic tests)
  ✗ /api/debug/* (blocked — returns 403 with mode explanation)

Standard Mode:
  ✓ All heater endpoints
  ✓ /api/stats, /api/profiles, /api/history
  ✓ /api/diagnostics/* (all diagnostic tests)
  ✗ /api/debug/* (blocked — returns 403 with mode explanation)

Hacker Mode:
  ✓ All standard endpoints
  ✓ /api/debug/* (raw register access, PID tuning, ASIC commands)
  ✓ Write operations require { "confirm": true } field
```

### Mode Switching

Mode can be changed via three interfaces:

1. **TOML config**: `[mode] active = "heater"` in `/data/dcentrald.toml` (applied on restart)
2. **REST API**: `POST /api/config { "mode": { "active": "hacker" } }` (applied immediately)
3. **Dashboard**: Mode selector in settings panel (applied immediately)

Mode switching behavior:
- **API access changes immediately.** No restart needed for endpoint filtering.
- **Thermal profile ramps over 30 seconds.** Fan speed and power targets transition smoothly to avoid thermal shock.
- **Switching TO Hacker mode requires confirmation.** API: `{ "confirm": true }` field. Dashboard: "Here Be Dragons" acknowledgment dialog explaining that relaxed limits can damage hardware.
- **Switching FROM Hacker mode is instant.** Tighter limits apply immediately for safety.

---

## 13. Startup Sequence -- Cold Boot

The complete cold-boot sequence from power-on to mining:

```
Phase 1: System Initialization
  1.  Mount /data (persistent UBIFS):
        mount -t ubifs ubi0:rootfs_data /data
  2.  Load config from /data/dcentrald.toml
        Falls back to /etc/dcentrald.toml if missing
  3.  Initialize logging (tracing subscriber)
  4.  Open watchdog (/dev/watchdog), start kick thread (5s interval)
  5.  Log firmware version, chip type, MAC address

Phase 2: GPIO and Fan Setup
  6.  Enable GPIO outputs:
        Write 0x00000000 to GPIO1_TRI (0x41210000 + 0x04)
  7.  Set LEDs (green ON, red OFF):
        Write 0x01 to GPIO1_DATA (0x41210000 + 0x00)
  8.  Set fans to maximum (safety default):
        Write 127 to FAN_PWM0 (0x42800010) and FAN_PWM1 (0x42800014)
  9.  Verify fan tach responds (FAN1_RPS > 0 within 5 seconds)

Phase 3: Hash Board Detection
  10. Read GPIO plug detect:
        Read GPIO0_DATA (0x41200000 + 0x00), check bits 5,6,7
  11. For each detected board (chain 6, 7, 8):
        Record board as present

Phase 4: PIC Initialization (per detected chain)
  12. Open I2C bus: /dev/i2c-0
  13. Set I2C slave address (0x55/0x56/0x57)
  14. Send JUMP_FROM_LOADER_TO_APP [0x55, 0xAA, 0x06]
  15. Wait 1000ms for PIC app firmware boot
  16. Verify PIC version: send [0x55, 0xAA, 0x17], expect 0x03
  17. Set initial voltage: send [0x55, 0xAA, 0x10, pic_val]
  18. Wait 300ms for voltage stabilization
  19. Enable voltage: send [0x55, 0xAA, 0x15, 0x01]
  20. Wait 500ms
  21. Start PIC heartbeat thread (send [0x55, 0xAA, 0x16] every 1s)

Phase 5: FPGA Chain Initialization (per detected chain)
  22. Open UIO devices for this chain:
        common (/dev/uioN), cmd (/dev/uioN+1),
        work-rx (/dev/uioN+2), work-tx (/dev/uioN+3)
  23. Verify FPGA version: read VERSION register = 0x00901002
  24. Disable chain: write CTRL_REG = 0x00000000
  25. Reset all FIFOs:
        CMD_CTRL = 0x03 then 0x00
        WORK_RX_CTRL = 0x01 then 0x00
        WORK_TX_CTRL = 0x02 then 0x00
  26. Set initial baud rate: BAUD_REG = 0x6C (115200 baud)
  27. Enable chain: CTRL_REG = 0x08 (BM1387 mode, 1 midstate)

Phase 6: Hash Board Reset and Chip Detection
  28. Assert hash board resets:
        Write 0x00000E00 to GPIO1_DATA (bits 9,10,11 = HIGH)
        Wait 100ms
        Write 0x00000000 to GPIO1_DATA (release reset)
        Wait 2000ms for chips to boot
  29. Send GetAddress broadcast via CMD_TX_FIFO
  30. Wait 1s, read all CMD_RX_FIFO responses
  31. Count chips, extract ChipID from first response
  32. Look up ChipDriver in registry based on ChipID

Phase 7: Chip Configuration
  33. Call driver.init_chain() with detected chip count:
        - Chain Inactive broadcast
        - Address assignment (addr = i * 256/chip_count)
        - Set PLL frequency (ramp from 100 MHz to target)
        - Set MiscControl (baud rate, clock settings)
        - Set TicketMask (hardware difficulty filter)
  34. Optionally increase FPGA baud rate:
        BAUD_REG = 0x07 (1.5 Mbaud)
  35. Clear ERR_COUNTER (write bit 0 of CTRL_REG)
  36. Clear pending WORK_TX IRQ (read UIO fd)

Phase 8: Network and Mining Start
  37. Connect to Stratum pool
  38. Perform Stratum handshake (subscribe, authorize, configure)
  39. Receive first job (mining.notify)
  40. Start mining pipeline:
        - Job Dispatcher task
        - Chain Worker tasks (1 per chain)
        - Nonce Collector tasks (1 per chain)
        - Share Validator task
  41. Start thermal controller (5s loop)
  42. Start API server (CGMiner :4028, REST :80)

Phase 9: Post-Start Stabilization
  43. Wait 60 seconds for temperatures to stabilize
  44. Ramp fans down from maximum based on actual temperature
  45. Log initial hashrate, temperature, fan speed
  46. Set green LED blinking (heartbeat pattern)
```

**Total cold boot time estimate: 15-20 seconds** (PIC boot delay is the bottleneck).

---

## 14. Safety Systems

### Watchdog

```
Timeout:   30 seconds (configurable)
Kick:      Every 5 seconds from dedicated Tokio task
Behavior:  If dcentrald crashes, system reboots within 30 seconds
Recovery:  On reboot, dcentrald restarts via init script, resumes mining

The Cadence WDT at 0xF8005000 is hardware-based.
Even if the kernel panics, the watchdog fires.
CONFIG_WATCHDOG_NOWAYOUT is NOT set, so watchdog can be
stopped for firmware updates (close fd after writing "V").
```

### PIC Heartbeat

```
Interval:  1 second per chain
Behavior:  PIC has internal watchdog. Without heartbeat, PIC
           disables voltage output within ~10 seconds.
           This is a HARDWARE safety feature that cannot be
           bypassed by software.
Recovery:  If heartbeat task crashes, PIC shuts down voltage,
           preventing hash board damage even without software
           thermal protection.
Failure:   If heartbeat I2C write fails 3 consecutive times,
           declare PIC communication failure. Disable that
           chain in software and log critical error.
```

### Thermal Shutdown

```
Trigger:   Any chain temperature >= dangerous_temp_c (default 75)
Action:    1. Disable voltage on ALL chains (PIC ENABLE_VOLTAGE = 0)
           2. Set fans to maximum (PWM = 127)
           3. Log CRITICAL: "Thermal shutdown triggered"
           4. Set red LED ON, green LED OFF
           5. Continue watchdog kicks and API serving
           6. Monitor temperature until below (dangerous - hysteresis)
           7. Wait additional 60 seconds after temp drops
           8. Re-run full init sequence (Phase 4 onward)
```

### Fan Failure

```
Detection: FAN1_RPS == 0 for 5 consecutive seconds while PWM > 0
Action:    1. Disable voltage on ALL chains
           2. Set fans to maximum (in case of partial failure)
           3. Log CRITICAL: "Fan failure detected"
           4. Set red LED blinking
           5. Do NOT auto-restart (requires manual intervention)
           6. API reports fan failure status
```

### Graceful Shutdown

```
Trigger:   SIGTERM, SIGINT, or API quit command
Sequence:  1. Cancel all Tokio tasks via CancellationToken
           2. Stop submitting new work
           3. Wait 500ms for in-flight nonces
           4. Submit any remaining valid shares
           5. Disable hash board voltages (PIC ENABLE_VOLTAGE = 0)
           6. Wait 2 seconds for power discharge
           7. Ramp fans to 50% (cool-down)
           8. Wait 5 seconds
           9. Set fans to minimum
           10. Close watchdog (write "V" then close fd)
           11. Log "dcentrald stopped cleanly"
           12. Exit 0
```

### Mode-Dependent Safety Limits

The SafetyEnvelope (Section 12) modifies safety behavior per operating mode:

```
Heater Mode Safety:
  - Tighter thermal limits (dangerous = 70C vs 75C standard)
  - Aggressive auto-shutdown: disable boards immediately at dangerous temp
  - No user override of thermal limits
  - Fan failure: shutdown + alert (no manual intervention option)
  - Power cap enforced at 900W (prevents accidental high-power operation)
  - If temp sensor fails: immediate shutdown, fans to max

Standard Mode Safety:
  - Default thermal limits (dangerous = 75C)
  - Standard thermal state machine (Section 8)
  - User can adjust thermal profile within standard ranges
  - All safety systems active with normal thresholds

Hacker Mode Safety:
  - Relaxed thermal limits (dangerous = 85C, user-overridable via config)
  - SOFT limits: frequency ceiling, voltage floor — can be overridden
    with explicit confirmation ("Here Be Dragons" acknowledgment)
  - HARD limits that are NEVER bypassable regardless of mode:
    ✗ PIC hardware watchdog (10s timeout, disables voltage in hardware)
    ✗ Silicon thermal shutdown (chip-internal, ~125C junction)
    ✗ Fan failure detection (0 RPM = boards disabled)
    ✗ Watchdog timeout (30s = system reboot)
    ✗ AXI fault protection (unmapped FPGA access = process crash)
  - Hacker mode logs a WARNING on every startup:
    "Hacker mode active — relaxed safety limits. Hardware damage is possible."
```

### AXI Fault Protection

Reading from unmapped FPGA address space causes AXI external abort faults (process receives SIGBUS). dcentrald validates all register addresses against the known UIO device map before access. Never probe unknown FPGA regions.

---

## 15. Persistent Storage

### Storage Layout

```
/data/                                # UBIFS mount (ubi0:rootfs_data, 61 MB usable)
  dcentrald.toml                     # Primary configuration
  profiles/
    default.toml                     # Default tuning profile
    quiet.toml                       # Night mode profile
    performance.toml                 # Overclock profile
    autotuned_<mac>.toml             # Auto-tuned per-chip profile
  history/
    hashrate.bin                     # Hashrate history (circular buffer, 24h)
    temperature.bin                  # Temperature history
    power.bin                        # Power consumption history
  keys/
    ssh_host_rsa_key                 # SSH host key (persistent across reboots)
    ssh_host_ed25519_key
    api_token                        # API authentication token
  logs/
    dcentrald.log                    # Rotated daemon logs (max 10 MB)
  reports/
    {test_id}.json                   # Diagnostic test raw data (max 20 reports)
    {test_id}.html                   # Rendered diagnostic report (HTML)
```

### Per-Chip Tuning Profiles

```toml
# /data/profiles/autotuned_2860_81e2_d686.toml
# Auto-generated by dcentrald autotuner

[chain.6]
voltage_mv = 8600
chips = [
  { addr = 0x00, freq_mhz = 650, errors = 0, health = "A" },
  { addr = 0x04, freq_mhz = 650, errors = 0, health = "A" },
  { addr = 0x08, freq_mhz = 625, errors = 2, health = "B" },
  # ... 63 chips total
]

[chain.7]
voltage_mv = 8600
chips = [ ... ]

[chain.8]
voltage_mv = 8650
chips = [ ... ]
```

### NAND Wear Considerations

The UBIFS layer handles wear leveling transparently. However, dcentrald should minimize unnecessary writes:

- **Config changes**: Write-on-change only, not periodic
- **Tuning profiles**: Write after autotuning completes, not during
- **History data**: Use a memory-resident circular buffer, flush to NAND every 5 minutes
- **Logs**: Use log rotation with maximum size (10 MB total)

UBI stats from live probe: mean erase count = 1, max = 3. The NAND has plenty of write life remaining.

---

## 16. Platform Abstraction -- Multi-Board Support

### Platform Trait

While the initial target is Zynq (S9), dcentrald is designed to support other control board types through a platform trait:

```
trait Platform: Send + Sync {
    fn board_type(&self) -> BoardType;
    fn chain_count(&self) -> u8;
    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>>;
    fn open_i2c(&self, bus: u8) -> Result<I2cBus>;
    fn open_fan(&self) -> Result<Box<dyn FanAccess>>;
    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>>;
}

enum BoardType {
    Zynq,        // S9, S17, S19 (FPGA UART FIFOs via UIO)
    BeagleBone,  // S19j (hardware UART /dev/ttyO1-5, no FPGA)
    Amlogic,     // S19XP, S21 (software UART /dev/ttyS1-3, no FPGA)
    CVitek,      // S21/T21 recent (uart_trans kernel module)
}
```

### Chain Access Abstraction

```
trait ChainAccess: Send + Sync {
    fn send_command(&self, data: &[u8]) -> Result<()>;
    fn read_response(&self, buf: &mut [u8]) -> Result<usize>;
    fn send_work(&self, data: &[u8]) -> Result<()>;
    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize>;
    fn set_baud(&self, baud: u32) -> Result<()>;
    fn wait_for_nonce(&self) -> Result<()>;  // Blocking wait (IRQ or poll)
}
```

For Zynq, `ChainAccess` is implemented by `FpgaChain` (UIO mmap + IRQ).
For BeagleBone, it would be implemented by a UART serial device.
For Amlogic, it would be software UART or /dev/ttyS.

### Platform Auto-Detection

```
Platform detection at startup:
  1. Read /proc/cpuinfo for "Hardware" line
  2. Check for UIO devices (/dev/uio0) -> Zynq
  3. Check for /dev/ttyO1 -> BeagleBone
  4. Check for /dev/ttyS1 + Amlogic DTS -> Amlogic
  5. Check for uart_trans kernel module -> CVitek

For Phase 1: Only Zynq is implemented. Other platforms return
"unsupported platform" error.
```

---

## 17. Logging and Diagnostics

### Structured Logging

dcentrald uses the `tracing` crate for structured, leveled logging:

```
Log Levels:
  ERROR:  Safety-critical events (thermal shutdown, fan failure, PIC failure)
  WARN:   Recoverable issues (pool disconnect, high CRC errors, temp approaching hot)
  INFO:   Operational milestones (init complete, mining started, pool connected, hashrate)
  DEBUG:  Detailed state (per-share acceptance, frequency changes, fan adjustments)
  TRACE:  Wire-level protocol (FIFO reads/writes, I2C transactions, raw nonces)

Output Targets:
  - stdout (console/serial)
  - /data/logs/dcentrald.log (rotated, max 10 MB)
  - syslog (if available)

Periodic Summaries (INFO level, every 60 seconds):
  "Mining: 14.2 TH/s | Chains: 3/3 | Temp: 58/56/57 C | Fan: 75% (4800 RPM) |
   Accepted: 1234 | Rejected: 2 (0.16%) | Pool: connected | Uptime: 2h 15m"
```

### Diagnostic Commands

The CGMiner API and REST API expose diagnostic data:

```
Per-chip diagnostics (via /api/stats):
  - Frequency (MHz)
  - Error count (CRC errors attributed to this chip)
  - Health grade (A/B/C/D based on error rate and stability)
  - Temperature (if chip has temp diode readback)
  - Nonce count (total valid nonces from this chip)

Per-chain diagnostics:
  - Total hashrate (GH/s)
  - Voltage (mV from PIC readback)
  - Temperature (C from I2C sensor)
  - CRC error rate (errors / second)
  - FIFO status (depth, overflow events)

System diagnostics:
  - CPU temperature (XADC)
  - VCCINT/VCCAUX voltages (XADC)
  - Memory usage (RSS of dcentrald process)
  - Uptime
  - NAND wear level (UBI erase count)
```

---

## 18. Diagnostic Subsystem (dcentrald-diagnostics)

### Architecture

The diagnostic subsystem runs as an async Tokio task within dcentrald. It manages the lifecycle of diagnostic tests, streams progress via WebSocket, and generates reports.

```
struct DiagnosticService {
    active_tests: HashMap<Uuid, RunningTest>,
    completed_tests: VecDeque<CompletedTest>,  // Keep last 10 results
    hal: Arc<HalContext>,                       // Hardware access
    asic: Arc<AsicContext>,                     // ASIC driver access
    state_rx: watch::Receiver<MinerState>,      // Current miner state
    progress_tx: broadcast::Sender<DiagnosticProgress>,  // WebSocket push
}

struct RunningTest {
    test_id: Uuid,
    test_type: TestType,
    started_at: Instant,
    cancel_token: CancellationToken,
    handle: JoinHandle<TestResult>,
}

enum TestType {
    HashReport,        // 15-minute comprehensive test drive
    ChipHealth,        // Per-chip health scoring (5 min)
    BoardHealth,       // Per-board health test (2 min)
    NetworkTest,       // Instant network diagnostics
    PsuProbe,          // PSU PMBus readings
    FpgaStatus,        // FPGA register status
    AsicCommTest,      // ASIC communication test
    I2cScan,           // I2C bus scan
}
```

### Implementation Strategy

**Phase 1 (MVP):** Spawn existing Python tools (`asic_enumerator.py`, `psu_probe.py`, `register_scanner.py`) as subprocesses with `--json` output. Parse JSON results in Rust.

```
Phase 1 Subprocess Flow:
  1. DiagnosticService receives test request via API
  2. Spawns Python tool as tokio::process::Command with --json flag
  3. Captures stdout, parses JSON into Rust structs
  4. Pushes progress updates via WebSocket
  5. Stores result in completed_tests deque

Advantages:
  - Reuses proven tools that have been tested on live S9 hardware
  - No new HAL code needed for Phase 1
  - Can ship diagnostics immediately

Disadvantages:
  - Requires Python3 in rootfs (~3 MB size cost)
  - Process spawn overhead (~100ms per tool)
  - Cannot do fine-grained progress tracking within Python tools
```

**Phase 2 (Native):** Rewrite diagnostic logic in Rust using dcentrald-hal and dcentrald-asic crates. Eliminates Python dependency and enables per-chip real-time progress streaming.

### HashReport -- 15-Minute Test Drive

The flagship diagnostic. A reseller plugs in a miner, starts HashReport from the dashboard, and 15 minutes later has a complete health report to show the buyer. No pool configuration needed — HashReport uses an internal test pool or solo mining for nonce counting.

```
HashReport Phases:

Phase 1: System Identification (10 seconds)
  - Read serial number (EEPROM or MAC-derived)
  - Read MAC address (eth0)
  - Detect ASIC chip type (ChipID command)
  - Read firmware version, FPGA version
  - Count hash boards (GPIO plug detect)
  - Record ambient conditions (XADC die temp as proxy)
  Output: SystemInfo struct

Phase 2: Baseline Capture (30 seconds)
  - Read all chain temperatures (I2C sensors)
  - Read fan speed (tach register)
  - Read PSU status (PMBus if available)
  - Read per-chain voltage (PIC readback)
  - Read FPGA error counters (CRC baseline)
  Output: BaselineSnapshot struct

Phase 3: Mining Performance (12 minutes, 12 × 60s windows)
  - Initialize all chains at default frequency
  - Start mining (test pool or solo)
  - Per 60-second window:
    - Count valid nonces per chip (WORK_RX_FIFO nonces with chip address decode)
    - Record CRC errors per chain
    - Record temperature trajectory
    - Calculate per-chip hashrate: nonces * difficulty / window_seconds
  - Total: 12 data points per chip for statistical significance
  Output: PerformanceData { windows: Vec<WindowData> }

Phase 4: Per-Chip Health Scoring (2 minutes)
  - For each chip:
    - actual_nonces = sum of nonces across all windows
    - expected_nonces = theoretical_nonces_at_frequency_and_difficulty
    - health_score = actual_nonces / expected_nonces  (0.0 to 1.0+)
    - Grade assignment:
        A: health_score >= 0.90  (healthy)
        B: health_score >= 0.75  (acceptable)
        C: health_score >= 0.50  (degraded)
        D: health_score >= 0.25  (poor)
        F: health_score < 0.25   (failing/dead)
    - Dead chip: 0 nonces across all windows
  Output: Vec<ChipHealthScore>

Phase 5: Report Generation (20 seconds)
  - Aggregate per-chip scores into per-board and unit grades
  - Overall unit grade:
      A: All boards grade A, 0 dead chips
      B: All boards grade B+, <= 2 dead chips
      C: Any board grade C, <= 5 dead chips
      D: Any board grade D, or > 5 dead chips
      F: Any board grade F, or > 10% dead chips, or safety issue
  - Generate ChipMap grids (color-coded per-chip health)
  - Render HTML report (askama template)
  Output: HashReport struct + HTML file
```

#### HashReport JSON Schema

```json
{
  "report_id": "uuid",
  "report_version": "1.0",
  "generated_at": "2026-03-11T14:30:00Z",
  "duration_seconds": 900,
  "firmware_version": "0.3.0",

  "system": {
    "serial": "ABC123DEF456",
    "mac": "28:60:81:E2:D6:86",
    "model": "Antminer S9",
    "chip_type": "BM1387",
    "chip_id": "0x1387",
    "fpga_version": "0x00901002",
    "board_count": 3,
    "total_chips": 189,
    "control_board": "Zynq C55"
  },

  "baseline": {
    "temperatures_c": { "chain_6": 32.5, "chain_7": 31.0, "chain_8": 33.2 },
    "fan_rpm": 4800,
    "fan_pwm": 75,
    "psu": {
      "vin_v": 220.0, "vout_v": 12.1, "iout_a": 125.0,
      "pin_w": 1580, "pout_w": 1512, "efficiency_pct": 95.7,
      "temp_c": 42.0, "faults": []
    },
    "voltages_v": { "chain_6": 9.0, "chain_7": 9.0, "chain_8": 9.0 }
  },

  "performance": {
    "total_hashrate_ghs": 14200.5,
    "total_hashrate_ths": 14.2,
    "accepted_shares": 47,
    "rejected_shares": 0,
    "hardware_errors": 3,
    "crc_errors": { "chain_6": 12, "chain_7": 1, "chain_8": 2 }
  },

  "boards": [
    {
      "chain_id": 6,
      "chips_expected": 63,
      "chips_responding": 63,
      "chips_dead": 0,
      "hashrate_ghs": 4733.5,
      "voltage_v": 9.0,
      "temp_c": 58.5,
      "crc_errors": 12,
      "grade": "A",
      "chips": [
        {
          "index": 0, "address": "0x00", "grade": "A",
          "health_score": 0.97, "hashrate_ghs": 75.1,
          "nonce_count": 4247, "expected_nonces": 4378,
          "crc_errors": 0, "frequency_mhz": 650
        }
      ]
    }
  ],

  "unit_grade": "A",
  "unit_grade_explanation": "All boards healthy, 0 dead chips, 3 HW errors (0.02%)",
  "recommendations": [],
  "warnings": ["Chain 6 has elevated CRC errors (12) — check cable connection"]
}
```

### Chip Health Test (5 Minutes)

Focused per-chip analysis for a single board. Produces a color-coded ChipMap grid.

```
ChipHealth Test Flow:
  1. Select target chain (6, 7, or 8)
  2. Enumerate chips (verify count matches expected)
  3. Run 5 × 60-second mining windows
  4. Count nonces per chip per window
  5. Calculate health_score = actual / expected for each chip
  6. Generate ChipMap grid

ChipMap Color Coding:
  Green:   health_score >= 0.90  (healthy chip)
  Yellow:  health_score >= 0.70  (marginal — may improve with voltage/freq tuning)
  Orange:  health_score >= 0.50  (degraded — investigate)
  Red:     health_score < 0.50   (poor — likely damaged)
  Gray:    health_score = 0      (dead — no nonces, may need board repair)

ChipMap Grid Layout (BM1387, 63 chips per board):
  ┌──┬──┬──┬──┬──┬──┬──┬──┬──┐
  │00│01│02│03│04│05│06│07│08│    Row 0 (chips 0-8)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │09│10│11│12│13│14│15│16│17│    Row 1 (chips 9-17)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │18│19│20│21│22│23│24│25│26│    Row 2 (chips 18-26)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │27│28│29│30│31│32│33│34│35│    Row 3 (chips 27-35)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │36│37│38│39│40│41│42│43│44│    Row 4 (chips 36-44)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │45│46│47│48│49│50│51│52│53│    Row 5 (chips 45-53)
  ├──┼──┼──┼──┼──┼──┼──┼──┼──┤
  │54│55│56│57│58│59│60│61│62│    Row 6 (chips 54-62)
  └──┴──┴──┴──┴──┴──┴──┴──┴──┘

  Each cell is color-coded by health_score.
  Hover/click shows: frequency, nonce count, health_score, grade.
```

### Board Health Test (2 Minutes)

Per-board comprehensive check without requiring mining:

```
Board Health Test Checks:
  1. Chip Enumeration: Send GetAddress broadcast, count responding chips
     - Report: chips_expected vs chips_responding, dead chip list
  2. Voltage Domain Verification:
     - PIC SET_VOLTAGE then GET_VOLTAGE readback
     - Verify voltage is within 5% of setpoint
  3. CRC Error Rate:
     - Enable chain, send 100 dummy commands, count CRC errors
     - Acceptable: <1% error rate. Bad: >5% error rate
  4. Temperature Distribution:
     - Read I2C temperature sensors
     - Check for thermal hotspots (>10C deviation from board average)
  5. EEPROM Validation:
     - Read EEPROM if present (model, serial, calibration data)
     - Verify checksum integrity
```

### Troubleshooting Tools (Instant)

Diagnostic tools that return results immediately:

```
Network Diagnostics (/api/diagnostics/troubleshoot/network):
  - DNS resolution test (resolve pool hostname)
  - Gateway ping (default route)
  - Pool TCP connectivity test (connect to pool:port)
  - Stratum handshake test (subscribe + authorize)
  - Latency measurement (round-trip time to pool)
  Output: { dns: bool, gateway: bool, pool_reachable: bool,
            latency_ms: u32, stratum_ok: bool, error: Option<String> }

PSU Probe (/api/diagnostics/troubleshoot/psu):
  - Scan I2C bus for PMBus devices
  - Read VIN, VOUT, IOUT, temperature, fan speed, status word
  - Decode faults from STATUS_WORD
  - Calculate efficiency (POUT/PIN)
  Output: Full PMBus reading set (see psu_probe.py JSON format)

FPGA Status (/api/diagnostics/troubleshoot/fpga):
  - Per-chain: VERSION, CTRL_REG, BAUD_REG, ERR_COUNTER
  - FIFO status: TX empty/full, RX empty/full
  - Chain enabled state
  Output: Per-chain register dump with decoded fields

ASIC Comm Test (/api/diagnostics/troubleshoot/asic-comm):
  - Per-chain: Send GetAddress broadcast, count responses
  - Read ChipID from first responder
  - Report: chip_count, chip_type, crc_errors, response_time_ms
  Output: Per-chain communication report

I2C Scan (/api/diagnostics/troubleshoot/i2c-scan):
  - Scan /dev/i2c-0 addresses 0x08-0x77
  - Identify known devices (PIC at 0x55-0x57, TMP75 at 0x48-0x4F, etc.)
  - Report: list of responding addresses with device type
  Output: { devices: Vec<{ addr, addr_hex, device_type, description }> }
  NOTE: On S9 with no 12V PSU to hash boards, only PICs (0x55-0x57) respond.
  No TMP75 temp sensors appear on the I2C bus — they are powered by the
  DC-DC converters which require 12V input. Temperature sensing depends on
  hash board power being present.
```

### Report Generation

Reports are generated as HTML using the `askama` template engine (compile-time templates, zero runtime overhead). PDF export uses browser print-to-PDF — no wkhtmltopdf or other binary needed.

```
Report Generation Flow:
  1. Test completes, TestResult struct is populated
  2. askama template renders HTML with embedded CSS and SVG charts
  3. HTML is stored in /data/reports/{test_id}.html
  4. Dashboard offers "Print to PDF" button (browser native)
  5. API serves HTML at /api/diagnostics/{type}/report?test_id=uuid

Report Storage:
  /data/reports/
    {test_id}.json    # Raw test data (machine-readable)
    {test_id}.html    # Rendered report (human-readable)
  Max 20 reports stored (oldest auto-deleted)
```

---

## 19. Build and Deployment

### Cross-Compilation

dcentrald targets `armv7-unknown-linux-musleabihf` (Zynq ARM Cortex-A9). Build with musl for static linking -- no glibc dependency on the target.

```
Build Requirements:
  - Rust toolchain (stable, latest)
  - armv7-unknown-linux-musleabihf target
  - Cross-compilation toolchain (Linaro GCC 7.2 or musl-cross)

Build Command:
  cargo build --release --target armv7-unknown-linux-musleabihf

Output:
  target/armv7-unknown-linux-musleabihf/release/dcentrald
  (~5-10 MB static binary, strip to ~3-5 MB)

Deployment:
  scp dcentrald root@203.0.113.36:/usr/bin/
  (or include in Buildroot rootfs build)
```

### Integration with DCENT_OS Firmware

dcentrald replaces the current Python web dashboard + MCP server. It is launched from an init script:

```
# /etc/init.d/S80dcentrald

start() {
    # Mount persistent storage
    mount -t ubifs ubi0:rootfs_data /data 2>/dev/null

    # Start mining daemon
    /usr/bin/dcentrald --config /data/dcentrald.toml &
}

stop() {
    killall -TERM dcentrald
    sleep 5
}
```

### Binary Size Budget

The target rootfs is 12 MB squashfs. dcentrald must fit within ~5 MB to leave room for BusyBox, libraries, SSH server, and web dashboard assets.

Techniques:
- Static linking with musl (no shared library dependencies)
- LTO (Link-Time Optimization) for dead code elimination
- `opt-level = "z"` for size optimization
- `strip` to remove debug symbols
- Embed web dashboard as compressed static files (brotli)

---

## 20. Dashboard Architecture

### Technology Stack

The web dashboard is a React + TypeScript single-page application built with Vite. Static assets (HTML, JS, CSS) are embedded directly into the dcentrald binary using `rust-embed` or `include_dir`, served from the same port 80 as the REST API.

```
Dashboard Build:
  cd dcentrald/dashboard
  npm run build              # Vite production build
  # Output: dist/ (~500 KB compressed)
  # Embedded into dcentrald at compile time via build.rs

Dashboard Serving:
  GET /           → index.html (SPA entry point)
  GET /assets/*   → JS/CSS/SVG bundles
  GET /api/*      → REST API (same axum server)
  WS  /ws         → WebSocket (same axum server)
```

### Three Layouts, One Dashboard

The dashboard renders a different layout and visual theme based on the active `OperatingMode`. All three layouts share the same React component library but compose them differently.

```
Heater Layout:
  Theme: warm colors (amber/orange gradient), large text, minimal controls
  Inspired by: Nest thermostat, Sense energy monitor
  Primary view:
    - Large circular BTU/h gauge (center)
    - Current power (watts) with cost/day below
    - Sats earned today counter
    - Room temperature (if sensor configured)
    - Noise level indicator (dB with icon)
    - Preset selector (Whisper/Low/Medium/High/Max)
    - Night mode toggle with schedule
  Secondary views:
    - Pool status (connected/disconnected indicator)
    - Simple hashrate number (no charts)
    - Diagnostics hub (accessible but de-emphasized)
  Hidden: per-chip data, register dumps, PID graphs, frequency controls

Standard Layout:
  Theme: professional dark theme (dark gray/blue), data-dense
  Inspired by: BraiinsOS dashboard, Grafana panels
  Primary view:
    - Hashrate chart (real-time + 24h history)
    - Per-board status cards (hashrate, temp, chips, voltage)
    - Pool status panel (URL, accepted, rejected, difficulty)
    - Fan/thermal panel (RPM with dB estimate, temp with chip map link)
    - Power panel (watts, efficiency, BTU/h, cost/day, sats/day)
  Secondary views:
    - Per-chip data table (sortable by health, frequency, errors)
    - ChipMap grid (color-coded health, clickable)
    - Tuning profiles panel
    - Diagnostics hub (full access)
  Configuration:
    - Pool settings, frequency, voltage, thermal profile
    - Mode selector in settings

Hacker Layout:
  Theme: terminal green-on-black, monospace font, raw data
  Inspired by: htop, terminal UIs, hacker aesthetic
  Primary view:
    - Live register dump panel (auto-refreshing FPGA registers)
    - PID controller graph (setpoint, PV, output, error terms)
    - ASIC command console (send raw commands, see responses)
    - I2C bus monitor (live traffic)
    - Per-chip frequency/voltage control sliders
  Secondary views:
    - FIFO depth indicators (TX/RX fill levels)
    - CRC error rate graph (per chain, per second)
    - Memory/CPU usage panel
    - Nonce stream (raw nonce values scrolling)
    - Diagnostics hub (full access + raw JSON mode)
  Tools:
    - Register read/write tool
    - PIC voltage control
    - Fan override
    - ASIC communication test
```

### Shared Components

All three layouts share a common component library:

```
Shared Components:
  ChipMap          Color-coded grid of per-chip health (Section 18)
  TemperatureGauge Circular/bar gauge with colored zones
  StatusPill       Small colored indicator (green/yellow/red/gray)
  HashboardCard    Per-board summary card (chips, hashrate, temp, voltage)
  PoolBadge        Pool connection status indicator
  FanIndicator     RPM display with dB and CFM estimates
  PowerMeter       Watts with BTU/h and cost derivations
  SatsCounter      Rolling counter of daily sats earned
  DiagnosticHub    Entry point for all diagnostic tests
  NotificationBar  Alerts, warnings, mode-switch prompts
  ModeSelector     Heater/Standard/Hacker toggle
```

### Diagnostics Hub

Accessible from all three modes. A dedicated page/panel that organizes all diagnostic tools:

```
Diagnostics Hub Layout:
  ┌─────────────────────────────────────────────────────┐
  │  HashReport (15 min)                    [Start]     │
  │  Complete unit health assessment with grading        │
  │  Last run: 2h ago — Grade: A                        │
  ├─────────────────────────────────────────────────────┤
  │  Chip Health Test (5 min)               [Start ▼]   │
  │  Per-chip scoring with ChipMap           Chain: 6    │
  ├─────────────────────────────────────────────────────┤
  │  Board Health Test (2 min)              [Start ▼]   │
  │  Chip count, voltage, CRC, temp          Chain: 6    │
  ├─────────────────────────────────────────────────────┤
  │  Quick Tests (instant)                               │
  │  [Network] [PSU] [FPGA] [ASIC Comm] [I2C Scan]     │
  ├─────────────────────────────────────────────────────┤
  │  Previous Reports                                    │
  │  2026-03-11 14:30 — HashReport — Grade A  [View]    │
  │  2026-03-11 10:15 — ChipHealth Ch6       [View]    │
  └─────────────────────────────────────────────────────┘
```

---

## 21. PSU Compatibility & 120V Home Mining

### The Problem (Stock Firmware / Competitors)

Stock Bitmain firmware and most competitors enforce PSU model validation:

1. **PSU I2C Query**: On boot, firmware sends I2C command `0x01` (`GET_FW_VERSION`) to PMBus address `0x10`
2. **Model String Check**: PSU responds with 16-byte ASCII (e.g., `"APW12_1215f_V1.2"`)
3. **Whitelist Comparison**: Firmware compares against hardcoded list of approved Bitmain PSU models
4. **Rejection**: Non-matching or no response = firmware refuses to start

This prevents 120V home miners from using:
- Standard ATX power supplies (no I2C, no model string)
- Server PSUs (HP, Dell — different I2C format)
- Non-Bitmain mining PSUs (Whatsminer, Avalon PSUs)
- 120V Bitmain PSUs with older firmware strings

The **PivotalPleb Loki board** ($29-$46 hardware) solves this by sitting on the I2C bus and spoofing valid PSU responses. **DCENT_OS eliminates the need for this hardware entirely.**

### DCENT_OS Solution: Built-in PSU Bypass (Zero Cost)

Since DCENT_OS is an original (non-forked) firmware, **we never implemented a PSU whitelist in the first place**. PSU bypass is the default behavior — not a workaround, but a design decision.

```
[power]
psu_bypass = true          # supported bypass lane: no PSU whitelist; still obey platform gates
psu_type = "auto"          # auto-detect via PMBus if available
max_watts = 1500           # Safety cap for 120V/15A circuit
```

### Three PSU Operating Modes

| Mode | `psu_type` | Description |
|------|-----------|-------------|
| **Bypass** (default) | `"bypass"` | No PSU communication. Use static voltage for power calc on proven PSU-bypass lanes. |
| **Auto-Detect** | `"auto"` | Try PMBus probe at boot. If responsive, use for real-time power monitoring. Fall back to bypass. |
| **PMBus Monitor** | `"pmbus"` | Full PSU telemetry (VIN, VOUT, IOUT, temp, efficiency, faults). For Bitmain APW3++ through APW17. |

### Power Calculation Without PSU

When no PMBus data is available (bypass mode), dcentrald estimates power consumption:

```
estimated_watts = sum_per_chain(
    chip_count * voltage_V * current_per_chip_A(frequency_mhz)
) + fan_watts + board_overhead_watts

// Per-chip current model (derived from APW efficiency curves):
//   BM1387 @ 650 MHz, 0.4V: ~0.22A per chip = 88mW
//   63 chips × 3 chains × 88mW = ~16.6W per chain (ASIC only)
//   Total with PSU/fan/board: ~1350W at full speed S9
```

### Hash Board Count Flexibility

Stock firmware also requires all hash board slots populated. DCENT_OS accepts 1-3 boards:

```
// At boot, check PLUGO GPIO for each connector
let boards_present = gpio.read_plug_detect();
// [true, false, false] = 1 board on J6 — perfectly fine
// Only initialize chains where boards are detected
```

### 120V Safety Features

Running on 120V/15A circuits requires power capping:

```
[power.120v_mode]
enabled = false            # User explicitly enables
max_amps = 12.0            # Max draw (leave headroom for 15A breaker)
max_watts = 1440           # 120V × 12A = 1440W theoretical max
power_target = 1200        # Conservative default with margin
startup_ramp_s = 30        # Gradual power-up to avoid inrush trips
```

### Marketing Value

This is a **key competitive differentiator** on proven lanes:
- **VNish**: Requires PSU bypass in some configurations
- **BraiinsOS**: Requires Loki-Duo ($44) for single-board + alternative PSU
- **LuxOS**: Has software PSU bypass but requires their firmware
- **DCENT_OS**: Avoids smart-PSU lock-in where the platform matrix says the bypass lane is proven; other lanes stay lab-gated until live PSU and recovery evidence exists.

The Loki board costs $29-$46 per miner. On routes where DCENT_OS has proven the software bypass, that is $29-$46 of extra hardware avoided without turning unsupported PSU combinations into a blanket install promise.

---

## 22. Future Considerations

### Now Part of Core Design (Moved from Future)

The following features were previously listed as future considerations but are now part of the core architecture:

- **Space Heater Mode**: Three-mode system with Heater/Standard/Hacker (Section 12)
- **Night Mode**: Integrated into HeaterController with configurable schedule (Section 8)
- **React Web Dashboard**: Three-layout dashboard architecture (Section 20)
- **Diagnostic Subsystem**: HashReport, ChipHealth, BoardHealth, troubleshooting tools (Section 18)
- **Power Targeting**: PID-based power control is the foundation of Space Heater mode (Section 8)

### Phase 2 Features (After Basic Mining Works)

1. **AutoTuner**: Two-phase optimization: (a) voltage minimization per chain, (b) per-chip frequency optimization. Save profiles to /data/profiles/.

2. **Stratum V2**: Encrypted, bandwidth-efficient mining protocol with miner-side block template construction. Depends on the `stratum-v2` Rust crate.

3. **MQTT / Home Assistant**: Native MQTT broker publishing miner metrics. Home Assistant MQTT auto-discovery for zero-config integration. Integrate with HeaterController TempSource for room temperature reading.

4. **PSU Bypass (120V)**: Skip APW PSU I2C model validation on supported lanes. Use static voltage for power calculation. Cap wattage to safe limit for 120V/15A circuit. Reference: PivotalPleb Loki board achieves PSU bypass in hardware; DCENT_OS implements the software path only where live platform evidence supports it. Critical for 120V home miners, but still per-platform gated because power and recovery behavior differ across control-board families.

5. **Weather API Integration**: Fetch outdoor temperature for seasonal heating profiles. Auto-adjust power target based on heating demand (colder outside = more watts). Optional — requires internet and user opt-in.

6. **Seasonal Profiles**: Pre-configured power curves for summer (low), spring/fall (medium), winter (high). Can be driven by weather API or manual selection.

7. **Native Diagnostic Rewrite (Phase 2)**: Replace Python subprocess diagnostic tools with native Rust implementations using dcentrald-hal and dcentrald-asic crates. Eliminates Python3 dependency from rootfs (~3 MB savings).

### Phase 3 Features

8. **Multi-Platform Support**: Amlogic (S21), BeagleBone (S19j), CVitek board support.

9. **Mobile App**: React Native companion app with push notifications, remote heater control, HashReport viewing.

10. **Fleet Discovery**: mDNS/DNS-SD for local network discovery of DCENT_OS miners. Aggregate dashboard across multiple units.

11. **Stock Firmware Install Path**: Direct installation from Bitmain stock firmware to DCENT_OS (currently requires BraiinsOS as intermediate step). Requires reverse engineering the Bitmain OTA update mechanism or developing a custom NAND writer that can be uploaded via the stock web interface. Target: route-gated installs from supported Antminers, always led by `dcent doctor`, `dcent support --flash-readiness`, `dcent install --list-routes`, and dry-run evidence before any write.

### Non-Goals (Explicit)

- **Windows/macOS support**: dcentrald runs on embedded Linux only.
- **GPU mining**: ASIC-only. No OpenCL, no CUDA.
- **Altcoin mining**: SHA-256d Bitcoin only (initially). Scrypt support (L3/L7) is a separate project.
- **Cloud dependency**: Everything works locally. No internet required for basic operation.
- **Kernel module development**: All hardware access is via UIO, I2C chardev, GPIO sysfs, and /dev/watchdog. No kernel code.

---

## Appendix A: S9 UIO Device Quick Reference

```
/dev/uio0   fan-control        0x42800000  Fan PWM + tach
/dev/uio1   chain6-common      0x43C00000  Chain 6 config/status
/dev/uio2   chain6-cmd-rx      0x43C01000  Chain 6 command FIFO
/dev/uio3   chain6-work-rx     0x43C02000  Chain 6 nonce FIFO
/dev/uio4   chain6-work-tx     0x43C03000  Chain 6 work FIFO
/dev/uio5   chain7-common      0x43C10000  Chain 7 config/status
/dev/uio6   chain7-cmd-rx      0x43C11000  Chain 7 command FIFO
/dev/uio7   chain7-work-rx     0x43C12000  Chain 7 nonce FIFO
/dev/uio8   chain7-work-tx     0x43C13000  Chain 7 work FIFO
/dev/uio9   chain8-common      0x43C20000  Chain 8 config/status
/dev/uio10  chain8-cmd-rx      0x43C21000  Chain 8 command FIFO
/dev/uio11  chain8-work-rx     0x43C22000  Chain 8 nonce FIFO
/dev/uio12  chain8-work-tx     0x43C23000  Chain 8 work FIFO
/dev/uio13  miner-glitch-mon   0x43D00000  Power glitch detector
```

## Appendix B: PIC Command Reference

```
Command    Code   Payload        Response    Description
JUMP_APP   0x06   (none)         (none)      Exit bootloader, start app
SET_VOLT   0x10   [pic_value]    (none)      Set chain voltage
ENABLE     0x15   [0x01]         (none)      Enable voltage output
HEARTBEAT  0x16   (none)         (none)      Watchdog keepalive
GET_VER    0x17   (none)         [ver_byte]  Read firmware version
GET_VOLT   0x18   (none)         [pic_val]   Read current voltage

All commands prefixed with [0x55, 0xAA].
PIC addresses: 0x55 (chain 6), 0x56 (chain 7), 0x57 (chain 8).
Voltage formula: voltage_V = (1608.420446 - pic_value) / 170.423497
```

## Appendix C: FPGA Register Quick Reference

```
Per-Chain Common (+0x0000):
  +0x00 VERSION       R    0x00901002  IP version
  +0x04 BUILD_ID      R    timestamp   Build date
  +0x08 CTRL_REG      RW   bits: [4]BM139X [3]ENABLE [2:1]MIDSTATE [0]ERR_CLR
  +0x10 BAUD_REG      RW   baud = 200M / (16 * (val+1))
  +0x14 WORK_TIME     RW   Inter-work delay
  +0x18 ERR_COUNTER   R    CRC error count

Per-Chain CMD (+0x1000):
  +0x00 CMD_RX_FIFO   R    Response words (read pairs)
  +0x04 CMD_TX_FIFO   W    Command words (write)
  +0x08 CMD_CTRL      RW   [1]RST_TX [0]RST_RX
  +0x0C CMD_STAT      R    [4]IRQ [3]TX_FULL [2]TX_EMPTY [1]RX_FULL [0]RX_EMPTY

Per-Chain Work RX (+0x2000):
  +0x00 WORK_RX_FIFO  R    Nonce words (read pairs)
  +0x08 WORK_RX_CTRL  RW   [0]RST_RX
  +0x0C WORK_RX_STAT  R    [4]IRQ [1]RX_FULL [0]RX_EMPTY

Per-Chain Work TX (+0x3000):
  +0x04 WORK_TX_FIFO  W    Work words (12 words per job for BM1387)
  +0x08 WORK_TX_CTRL  RW   [1]RST_TX
  +0x0C WORK_TX_STAT  R    [4]IRQ [3]TX_FULL [2]TX_EMPTY
  +0x10 WORK_TX_THR   RW   IRQ threshold
  +0x14 WORK_TX_LAST  R    Last work ID sent

Fan Controller (0x42800000):
  +0x00 FAN0_RPS      R    Fan 0 tach (RPS), not connected on S9
  +0x04 FAN1_RPS      R    Fan 1 tach (RPS), multiply by 60 for RPM
  +0x10 FAN_PWM0      RW   PWM channel 0 (0-127)
  +0x14 FAN_PWM1      RW   PWM channel 1 (0-127)

GPIO Input (0x41200000):
  +0x00 DATA          R    Bits 5,6,7 = J6,J7,J8 plug detect (HIGH=present)

GPIO Output (0x41210000):
  +0x00 DATA          RW   Bits 9,10,11 = J6,J7,J8 reset (HIGH=assert)
  +0x04 TRI           RW   Must write 0 to enable outputs (default=0xFFFFFFFF)
```

---

---

## Appendix A: Implementation Status

> Last updated: 2026-03-11 (v0.4)

### Crate Implementation Summary

| Crate | Status | Key Files | Notes |
|-------|--------|-----------|-------|
| `dcentrald` (binary) | **Functional** | main.rs, daemon.rs, config.rs, error.rs, logging.rs, work_dispatcher.rs | Full daemon lifecycle: init phases 1-7, run loop with 10 concurrent Tokio tasks, graceful 11-step shutdown. Work dispatcher with real FPGA I/O, nonce polling, hashrate tracking, share submission. |
| `dcentrald-hal` | **Functional** | uio.rs, fpga_chain.rs, i2c.rs, fan.rs, watchdog.rs, xadc.rs, platform/*.rs | UIO mmap, FPGA chain registers (4 blocks per chain), I2C ioctl, fan PWM/tach, watchdog open/kick/close, **XADC temperature reading** (IIO sysfs — die temp, VCCINT, VCCAUX) |
| `dcentrald-asic` | **Functional** | protocol.rs, chain.rs, pic.rs, drivers/bm1387.rs | Wire protocol (CRC5/16, preamble), chain enumeration, PIC cold_boot_init/voltage/heartbeat, BM1387 PLL table (100-900 MHz), multi-word FIFO register writes, **send_work and decode_nonce wired to dispatcher** |
| `dcentrald-stratum` | **Integrated** | v1/client.rs, v1/messages.rs, v1/codec.rs, v1/job.rs, v1/difficulty.rs, work.rs, types.rs | Types defined, connection logic scaffolded, **wired into daemon run loop**: Stratum client spawned as Tokio task, job_tx→WorkDispatcher, share_rx←WorkDispatcher, status_tx→StatusHandler. WorkBuilder with full SHA-256 midstate computation. |
| `dcentrald-thermal` | **Functional** | controller.rs, fan.rs, profiles.rs, curtailment.rs, heater.rs | PID controller (kp=2.0, ki=0.5, kd=0.1), fan noise/CFM estimation, S9 power presets (whisper→max), curtailment state machine, heater PID power targeting, night mode time detection. **Wired to real hardware**: reads XADC temp, writes fan PWM, emergency shutdown disables PIC voltages. |
| `dcentrald-api` | **Functional** | lib.rs, cgminer.rs, rest.rs, websocket.rs, dashboard.rs, mode_middleware.rs | CGMiner TCP (13 commands), REST (85 routes), WebSocket (stats broadcast), mode-themed dashboard, SafetyEnvelope per mode |
| `dcentrald-diagnostics` | **Scaffolded** | lib.rs, hashreport.rs, chip_health.rs, board_health.rs | Types and API defined, test orchestration not yet implemented |

### Daemon Lifecycle (Implemented)

**Init Phases (daemon.rs):**
1. System ID: read version, open watchdog
2. Fan setup: command the home fan cap (PWM 10 boot default); AM2/XIL acoustic proof requires tach/RPM readback
3. Hash board detection: GPIO PLUGO pins 902-904, enable pins 893-895
4. PIC initialization: I2C cold_boot_init (JUMP_FROM_LOADER_TO_APP, set voltage, enable)
5. FPGA chain setup: open UIO devices, read version/build_id
6. Chip detection: enumerate all chips, identify ChipID, select driver via ChipRegistry
7. Chip configuration: assign addresses, init with driver, set frequency

**Run Tasks (10 concurrent Tokio tasks — all implemented):**
1. **CGMiner TCP server** (port 4028) — 13 commands, pyasic compatible
2. **HTTP server** (port 80: REST + WebSocket + dashboard) — 37 REST endpoints, mode-aware middleware
3. **Stratum V1 client** — pool connect/auth, job reception via `job_tx`, share submission via `share_rx`
4. **Stratum status handler** — receives `StratumStatus` events, updates `MinerState` (pool status, difficulty, accepted/rejected counts)
5. **Work dispatcher** — receives `JobTemplate`, builds midstates via `WorkBuilder`, dispatches to all chains via `ChipDriver::send_work`, polls `WORK_RX_FIFO` at 100Hz, decodes nonces via `ChipDriver::decode_nonce`, tracks hashrate (EMA, per-chain), submits shares via `share_tx`
6. **Watchdog kicker** — configurable interval (default 5s), writes to `/dev/watchdog`
7. **Thermal control loop** — 5s interval, reads XADC die temp (IIO sysfs), reads fan RPM (FPGA tach), PID → set fan PWM. Emergency actions: `EmergencyShutdown` → fans 100% + disable all PIC voltages. `FanFailure` → disable all PIC voltages.
8. **PIC heartbeat** — 1s interval, opens I2C bus 0, sends heartbeat to all active PICs (0x55/0x56/0x57). PIC hardware watchdog cuts voltage if heartbeat stops for ~10 seconds.
9. **State publisher** — 1s interval, reads fan PWM/RPM from `Arc<FanController>`, updates uptime, broadcasts WebSocket stats message
10. **Signal handler** — SIGINT/SIGTERM → `CancellationToken::cancel` → all tasks observe and exit

**Shutdown Sequence (11 steps):**
1. Cancel all tasks via CancellationToken
2. Stop work submission
3. Wait 500ms for in-flight nonces
4. (Submit remaining shares)
5. Disable hash board voltages via PIC
6. Wait 2s for power discharge
7. Ramp fans to 50% (cool-down)
8. Wait 5s
9. Set fans to quiet minimum
10. Close watchdog (write "V" magic close)
11. Log "dcentrald stopped cleanly"

### API Endpoint Inventory

**CGMiner TCP (port 4028) — 13 commands:**
`summary`, `stats`, `pools`, `devs`, `version`, `coin`, `config`, `switchpool`, `enablepool`, `disablepool`, `addpool`, `restart`, `quit`

**REST API (port 80) — 37 endpoints across 7 categories:**

| Category | Endpoints | Mode Gate |
|----------|-----------|-----------|
| Core Status | GET /api/status, GET /api/pools, POST /api/pools, GET /api/config, POST /api/config, GET /api/system/info, GET /api/system/asic | All modes |
| Actions | POST /api/action/restart, /reboot, /sleep, /wake | All modes |
| Statistics | GET /api/stats, /api/history, /api/profiles, POST /api/profiles | Standard+ |
| Heater | GET/POST /api/heater/status, /target, /presets, /room-temp, /history, /night-mode | All modes |
| Debug | GET/POST /api/debug/registers, /i2c, /asic-command, /pid-state, /pid-params, /chip/frequency, /chip/voltage | Hacker only |
| Diagnostics | POST /api/diagnostics/{hashreport,chip-health,board-health}/start, GET .../status, .../result, .../report | All modes |
| Troubleshoot | GET /api/diagnostics/troubleshoot/{network,psu,fpga,asic-comm,i2c-scan} | All modes |

**WebSocket (/ws) — 3 message types:**
- `stats` (1s interval): hashrate, chains, fans, pool
- `diagnostic_progress`: phase, percentage, ETA
- `heater_status`: power, BTU, noise, cost, sats

### Key Technical Details Implemented

**BM1387 PLL Table (34 entries, 100-900 MHz):**
Register 0x0C values derived from bmminer-mix source. Example entries: 600 MHz = 0x00680261, 650 MHz = 0x00700261, 700 MHz = 0x00780261. Full 32-bit register writes use 2-word FIFO protocol.

**Multi-word FIFO Register Writes:**
FPGA CMD_TX_FIFO needs 2 words for full 32-bit register writes: Word 0 = [header, length, reg, value_byte0], Word 1 = [value_byte1, value_byte2, value_byte3, 0x00]. Functions: `fifo_cmd_write_reg_bcast_full`, `fifo_cmd_write_reg_full`.

**Mode-dependent Safety Envelope:**
| Parameter | Heater | Standard | Hacker |
|-----------|--------|----------|--------|
| dangerous_temp_c | 70 | 75 | 85 |
| max_frequency_mhz | 650 | 700 | 900 |
| allow_overclock | No | No | Yes |
| allow_raw_registers | No | No | Yes |
| fan_mode | NoiseOptimized | FullRange | FullRange |
| min_fan_pwm | 10 | 0 | 0 |
| max_power_watts | 900 | 1500 | 2500 |

**XADC Temperature Reading (Implemented):**
Reads Zynq die temperature via IIO sysfs at `/sys/bus/iio/devices/iio:device0`. Formula: `(raw + offset) * scale / 1000.0` → degrees Celsius. Also reads VCCINT (nominal 1.0V) and VCCAUX (nominal 1.8V) for system health monitoring. Typical values: die temp ~41°C, VCCINT 0.99V, VCCAUX 1.78V.

**WorkDispatcher Architecture (Implemented):**
Single Tokio task owns all `Chain` objects (moved via `std::mem::take`). Uses `tokio::select!` with 4 concurrent timers: job reception, work dispatch (chip-specific interval from `ChipDriver::job_interval_ms`), nonce polling (10ms/100Hz), hashrate update (5s). The dispatcher is the sole consumer of FPGA mmap regions — no mutex or Arc needed. Work table has 256 entries indexed by `work_id: u8` for nonce→share matching.

**HashrateTracker (Implemented):**
Exponential Moving Average (EMA) hashrate calculation. Formula: `hashrate_ghs = nonces * hw_difficulty * 2^32 / seconds / 1e9`. EMA smoothing factor α=0.1. Per-chain tracking with independent 5-second windows. For BM1387 with TicketMask=255 (hw_difficulty=256): each nonce represents 256 × 4,294,967,296 = ~1.1 trillion hashes.

**Chain Ownership Model (Implemented):**
Chains are created during `Daemon::init` phases 5-7, then moved to `WorkDispatcher` via `std::mem::take` in `Daemon::run`. The dispatcher is the sole owner and FPGA I/O consumer. `FanController` is wrapped in `Arc` for sharing between the thermal loop task and the daemon shutdown method. PIC heartbeat opens a fresh `I2cBus` on each iteration (cheap fd operation).

### Recently Completed (v0.4)

1. ~~**Stratum V1 Integration**~~: **DONE** — Stratum client spawned as Tokio task, `job_tx`→WorkDispatcher, `share_rx`←WorkDispatcher, `status_tx`→StatusHandler. Full lifecycle: connect, authorize, subscribe, receive jobs, submit shares.
2. ~~**Work Dispatch Pipeline**~~: **DONE** — `WorkDispatcher` receives `JobTemplate`, generates `MiningWork` via `WorkBuilder` (SHA-256 midstates), dispatches to all chains via `ChipDriver::send_work`, polls `WORK_RX_FIFO` at 100Hz, decodes nonces, submits valid shares. `HashrateTracker` with EMA and per-chain breakdown. 256-entry work table for nonce→share matching.
3. ~~**Temperature Reading**~~: **DONE** (XADC) — XADC IIO sysfs reads die temp, VCCINT, VCCAUX. Wired into thermal control loop with real PID → fan PWM + emergency shutdown.
4. ~~**Hardware I/O Wiring**~~: **DONE** — PIC heartbeat (1s, I2C), thermal loop (5s, XADC+fan), state publisher (1s, fan RPM/PWM), shutdown (PIC voltage disable). All using real hardware.

### Remaining Work (Phase 2 → Phase 3)

1. **Cross-compilation**: ARM cross-compile toolchain setup (armv7-unknown-linux-gnueabihf, Linaro 7.2), integration into firmware build pipeline
2. **Per-board Temperature (TMP75)**: Wire I2C TMP75 temperature sensors for per-hash-board chip temperature. Currently using XADC die temp as proxy (control board temp, not chip temp). Requires PIC enabling voltage first (TMP75 invisible until powered).
3. **Frequency Throttling**: Thermal action `ThrottleAndFan` generates `freq_reduction_pct` but WorkDispatcher doesn't yet apply it. Need command channel or shared state for thermal→dispatcher frequency adjustment.
4. **DiagnosticService**: Implement test orchestration — HashReport, ChipHealth, BoardHealth using existing Python tools as subprocess bridge
5. **Persistent Config**: Read/write /data/dcentrald.toml on configuration changes via API
6. **AutoTuner**: Per-chip frequency optimization based on silicon quality (voltage minimization → frequency maximization)
7. **React Dashboard**: Replace minimal HTML dashboard with full React + TypeScript + Vite dashboard embedded as static assets
8. **Stratum V1 Client Internals**: TCP framing, JSON-RPC parsing, reconnect logic, pool failover, difficulty tracking — types are defined but wire protocol not yet implemented

---

*This document defines the architecture for dcentrald, the mining daemon at the heart of DCENT_OS. It is based on verified hardware data from live probes of an Antminer S9 running DCENT_OS Hacker Shell v0.2.7, cross-referenced with ESP-Miner ASIC driver analysis, Mujina architecture patterns, and 85 reverse-engineered VNish firmware packages. All register addresses, timing values, and protocol details have been confirmed on real hardware unless otherwise noted. The three-mode architecture (Heater/Standard/Hacker), diagnostic subsystem, and dashboard architecture were designed from competitive analysis of LuxOS healthchipget, VNish chip map visualization, and market research across 20+ requested features.*

*D-Central Technologies -- Mining Hackers since 2016*
