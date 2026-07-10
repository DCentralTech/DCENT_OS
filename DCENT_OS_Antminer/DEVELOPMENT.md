# Developing DCENT_OS

This is the orientation guide for developers working on DCENT_OS. Read
[`CONTRIBUTING.md`](CONTRIBUTING.md) first for the contribution workflow and the project's safety
rules; this document goes deeper on architecture and the build.

## The big picture

This directory is the industrial Antminer member of the DCENT_OS family. It
replaces the firmware on supported Bitmain Antminer control boards. The
ESP32-S3 / Bitaxe-class member lives in `../dcentos-esp/`. The Antminer member
has three parts:

1. **`dcentrald`** — the Rust mining daemon. It talks to the hash boards and the power/thermal
   hardware, runs the Stratum client, and serves the API. This is the heart of the project.
2. **Buildroot Linux** (`br2_external_dcentos/`) — a minimal embedded Linux image that boots the
   miner and runs `dcentrald`.
3. **The dashboard** (`dashboard/`) — a local React/TypeScript web UI served off the miner.

A defining design choice: `dcentrald` reaches the FPGA through **UIO + `/dev/mem`**, with **no
proprietary kernel modules.** That decoupling from the kernel ABI is what lets one firmware
auto-detect the ASIC (via its ChipID) and drive supported/lab-validated hash boards in the
Zynq-era fleet.

## The `dcentrald` workspace

`dcentrald/` is a multi-crate Cargo workspace. The main crates:

| Crate | Responsibility |
| --- | --- |
| `dcentrald` | Daemon entry point, mining orchestration, config, runtime |
| `dcentrald-hal` | Hardware abstraction: UIO, I2C, GPIO, FPGA, PSU, serial, platform detection |
| `dcentrald-asic` | ASIC chip drivers (BM1387 / BM1397 / BM1398 / BM1362 / BM1368) + work/nonce codecs |
| `dcentrald-stratum` | Stratum V1 + V2 client, share validation, BIP320 version rolling, pool failover |
| `dcentrald-thermal` | PID fan control, thermal telemetry, emergency throttling |
| `dcentrald-autotuner` | Runtime frequency/voltage tuning with hard safety clamps |
| `dcentrald-api` / `-api-types` | REST + WebSocket API (axum) and its serializable types |
| `dcentrald-silicon-profiles` | Per-chip voltage/frequency envelopes and PVT limits |
| `dcentrald-common` | Shared types and helpers (e.g. wallet/worker masking) |

Some crates depend on Linux-only HAL code, so they build for the target rather than the host — see
testing notes below.

## Building

```bash
# Cross-compile the daemon for the miner's SoC:
cd dcentrald
cargo build --release --target armv7-unknown-linux-musleabihf   # Zynq (S9/S17/S19)
cargo build --release --target aarch64-unknown-linux-musl       # Amlogic (S19j Pro/S21)

# Build a flashable image (uses Docker, from the repo root):
bash ../scripts/build_in_docker.sh

# Dashboard:
cd ../dashboard && npm install && npm run build
```

The cross-compile uses `rust-lld` (no external GCC cross-compiler needed for the Rust side); the
Docker image build handles the C toolchain for crates that need it.

### What builds from this repo, and what needs vendor artifacts

- **The `dcentrald` daemon builds cleanly from a fresh clone** with the `cargo build` commands above —
  that is the supported from-source build, and it is all most contributors need. (A bare `cargo build`
  from `dcentrald/` builds the daemon stack via the workspace's `default-members`; it does **not** build
  the optional `pic-recovery` tool — see below.)
- **The `pic-recovery` tool** (low-level dsPIC/FPGA recovery for a bricked board) embeds a stock Bitmain
  FPGA bitstream that, like the image-build artifacts, is not redistributable. It is excluded from the
  default workspace build; build it explicitly with `cargo build -p pic-recovery` after placing the
  bitstream at `dcentrald/pic-recovery/firmware/stock_fpga_s9.bin` (extracted from your own unit).
- **A full flashable firmware image** is different. DCENT_OS reuses some boot-critical, non-redistributable
  components from each miner's stock/BraiinsOS firmware (the SoC kernel, FPGA bitstream, FSBL/U-Boot). We
  cannot ship those binaries in an open-source repo, so `build_in_docker.sh` expects them to be supplied
  locally (it references per-platform kernel/FPGA artifacts that are not part of this repository). If you
  just want to run DCENT_OS, **use the prebuilt, signed release images** rather than building one yourself.
  Producing your own image means extracting those artifacts from the firmware already on your unit.

## Testing

```bash
# Host-side Rust unit/integration tests (run from dcentrald/):
cargo test                 # pure + host-testable crates
cargo fmt --all --check
cargo clippy --all-targets

# Dashboard:
cd dashboard && npm run build && npx vitest run
```

Crates with Linux-specific HAL code are validated on the target / in CI rather than on a Windows
host. When you change a shared struct, grep for ALL its fixtures — including feature-gated ones
(`--features mock-pool`) and other crates' integration tests — so you don't miss a test that the
default `cargo check --tests` skips.

## Coding standards

- **Rust:** memory-safe, async (`tokio`). Log with `tracing` (never `println!`). No `unsafe` without
  a comment explaining why. Every hardware constant cites its source (live probe / datasheet / RE).
- **Hardware constants** must match verified probe data, not open-source docs (which contain errors).
- **Comments** are written for an outside contributor: explain *why*, avoid private jargon.
- **Attribution:** new source files carry the header
  `// SPDX-License-Identifier: GPL-3.0-or-later` + `// Copyright (c) D-Central Technologies — https://d-central.tech`.
- **Honesty:** user-facing surfaces never claim an untrue state (connected ≠ mining, scheduled ≠
  flashed, pool-target ≠ achieved difficulty).

## Safety architecture (do not regress)

DCENT_OS runs on hardware that controls real voltage and heat. Several guards are load-bearing —
they exist to prevent a class of physical damage and must not be weakened:

- **Fan-PWM home cap** — home/quiet profiles cap commanded fan PWM; the daemon cuts hash power
  before raising fan noise. Fan blast is reserved for measured thermal need.
- **EEPROM write-protection** — the HAL refuses writes to hash-board EEPROM I²C addresses
  (`0x50–0x57` on AM2). Reads still work; writes are denied to prevent board-identity corruption.
- **Thermal fail-safe** — when board temp sensors are unavailable, the controller falls back to the
  SoC die temperature rather than mis-triggering an emergency.
- **Destructive recovery is feature-gated** — PIC reset/erase/reflash ops live behind a Cargo
  feature the shipping daemon does not enable, so they can't be invoked by accident.
- **Voltage envelopes** are bounded by per-chip silicon profiles with hard clamps you can't exceed
  from config.

If a change touches voltage, thermal, fan, PIC/dsPIC, PSU, or chain/UART code, call it out in your
PR — it gets an extra review pass.

## Where to go deeper

- [`docs/DCENTRALD_ARCHITECTURE.md`](docs/DCENTRALD_ARCHITECTURE.md) — the full runtime/system design.
- [`docs/PLATFORMS.md`](docs/PLATFORMS.md) — per-model status and the universal-hash-board model.
- [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) — the `dcentrald.toml` reference.

---

*Built by the Mining Hackers at [D-Central Technologies](https://d-central.tech/).*
