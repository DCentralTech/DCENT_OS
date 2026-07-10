# Supported Platforms

This page covers the industrial Antminer member in `DCENT_OS_Antminer/`. The
ESP32-S3 / Bitaxe-class member lives in `DCENT_OS_ESP/`. On Antminers,
DCENT_OS targets the Zynq- and Amlogic-era Bitmain fleet. Because `dcentrald`
auto-detects the ASIC via its ChipID and loads the matching driver, **one
firmware architecture covers many models**. Mixed control-board/hash-board
operation is a validated/lab compatibility capability, not a blanket
production-install promise for every board mix.

This page is the honest, per-model picture. **"Mining proven"** means DCENT_OS has produced accepted
pool shares on that hardware on the bench; cold-boot or nonce evidence alone does not promote a platform
to mining-proven. **"Bring-up"** means the driver paths exist and per-model validation is expanding. The
**public install readiness** column is separate from mining evidence and should match DCENT_Toolbox
readiness output; help from the community is welcome (see the platform-bring-up issue template).

## Control-board families

| Family | SoC | Examples | Notes |
| --- | --- | --- | --- |
| **Zynq (am1/am2)** | Xilinx Zynq-7000 (ARMv7) | S9, S17, T17, S19, S19 Pro, S19j Pro (Zynq) | FPGA chain via UIO + `/dev/mem`, no kernel modules |
| **CVITEK (cv183x)** | CVITEK CV183x (ARM) | T19, S19j Pro CV1835 | Serial/PIC1704 family support is in development; public install route remains evidence-gated |
| **Amlogic (am3)** | Amlogic A113D (aarch64) | S19j Pro (AML), S19 XP, S19k Pro, S21 | Serial UART chains, sysfs PWM/GPIO, no FPGA |
| **BeagleBone (am3-bb)** | TI AM335x (ARMv7) | S19j Pro on `S19J_IO_BOARD_V2` | Serial UART chains via the IO board |

## Per-model status

| Miner | ASIC | Board | Mining / driver evidence | Public install readiness |
| --- | --- | --- | --- | --- |
| **Antminer S9** | BM1387 | Zynq | Mining proven: sustained cold-boot mining, 3/3 chains, accepted shares | Lab-gated: Toolbox route exists, public artifact + witnessed live-install capstone still required |
| **Antminer S19 Pro** | BM1398 | Zynq | Experimental bring-up: cold boot, 3 chains x 114 chips, 146K nonces, 0 HW errors; accepted-share gate open | Lab-gated: dedicated signed package/runbook/live capstone pending |
| **Antminer S19j Pro** | BM1362 | Zynq | Mining proven: accepted pool shares, standalone cold-boot | Lab-gated: Xilinx public beta remains pending public artifact + witnessed clean-unit capstone |
| **Antminer S19j Pro** | BM1362 | BeagleBone | Mining proven: accepted pool shares, all chains enumerated | Lab-gated: SD/runtime path only, not a general NAND/sysupgrade production install |
| **Antminer S21** | BM1368 | Amlogic | Mining proven: first Amlogic hash, ~66 TH/s sustained, 30K nonces | Lab-gated: runtime evidence exists; stock AMLCtrl in-place install remains blocked |
| **Antminer S17 / S17 Pro** | BM1397 | Zynq | Bring-up: drivers in place; validation expanding | Evidence gap: no public install route |
| **Antminer T17** | BM1397 | Zynq | In development: explicit model profile and X17 runtime identity exist; validation expanding | Evidence gap: no public install route |
| **Antminer S19** | BM1398 | Zynq | Bring-up: shares the S19 Pro driver path | Evidence gap: does not inherit S19 Pro install readiness |
| **Antminer T19** | BM1398 | CVITEK | In development: explicit model profile exists; chip geometry and tuning defaults remain hardware-gated | Evidence gap: no public install route |
| **Antminer S19 XP** | BM1366 | Amlogic / CVITEK variants | Bring-up: driver present; validation expanding | Evidence gap: no public install route |
| **Antminer S19j Pro (Amlogic)** | BM1362 | Amlogic | Bring-up: code paths in place; validation expanding | Evidence gap / lab-only: AMLCtrl route boundaries still apply |
| **Antminer S19k Pro** | BM1366 | Amlogic | Bring-up: Amlogic port in progress | Evidence gap / lab-only: first-flash route proof pending |
| **AvalonMiner (Canaan)** | — | K230 RISC-V | In development | Not public-install-ready |
| **WhatsMiner (M-series)** | — | H616 | In development | Not public-install-ready |

## Universal hash-board compatibility

The Zynq-era miners share an 18-pin hash-board connector and UART protocol. DCENT_OS detects the
chip via the ChipID command (`0x1387` = S9, `0x1397/0x1398` = S17/S19, `0x1362` = S19j Pro, etc.)
and loads the right driver. In validated/lab configurations that means an inexpensive S9 control
board can drive supported later-generation hash boards. Treat mixed-generation rigs as
compatibility/lab work until their exact control-board, hash-board, power, and recovery path has a
documented install route.

## Power supplies

DCENT_OS does **not** require a "smart PSU" or a Loki board. Three PSU modes are supported:

- **Bypass** — estimate power from frequency/voltage tables; run any dumb PSU.
- **Auto-Detect** — probe a PMBus-capable PSU for live telemetry.
- **PMBus Monitor** — full telemetry from a smart PSU when present.

APW3 / APW7 / APW12 and generic bench supplies are supported through these modes on proven lanes.
120 V household power is supported via PSU bypass on the appropriate hardware. Amlogic and
BeagleBone PSU-bypass behavior remains live-soak gated; do not infer those lanes from S9/AM2 proof.

## Safety before you flash

- Keep a **known-good recovery path** before flashing experimental firmware.
- On a fresh install DCENT_OS boots **management-only** (dashboard/SSH/API up, hash power off) until
  you explicitly enable mining — a fresh flash will not surprise-start a loud miner.
- For bring-up models, treat results as experimental and report what you find.

See [`INSTALL/`](INSTALL/) for per-platform install procedures and
[`CONFIGURATION.md`](CONFIGURATION.md) for tuning.
