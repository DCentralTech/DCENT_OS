# Custom Antminer Firmware — Origins & Design Rationale

This is a short historical note on where DCENT_OS came from and why it is built the way it is.
Early on, D-Central ran a feasibility study to decide whether to build its own open-source firmware
for Bitmain Antminer hardware. The study concluded it was achievable, and that study motivated
DCENT_OS — which now ships and is **mining-proven on multiple platforms**: S9, S19j Pro on
both Zynq and BeagleBone, and S21. For the current per-model status, see
[`PLATFORMS.md`](PLATFORMS.md).

S19 Pro remains an Experimental feature with cold-boot and nonce evidence while accepted-share and
persistent-install promotion gates stay open.

The design choices below are recorded here as durable rationale, not as a plan.

## Why D-Central built its own firmware

1. **No mandatory dev fee or license server** — the major aftermarket firmwares use paid license or
   fee routes; DCENT_OS uses a transparent, configurable, fully-disableable donation instead.
2. **Space-heater optimization** — profiles designed for home use, not the datacenter.
3. **110 V / 120 V home power** — underclocking profiles for standard household circuits.
4. **Noise reduction** — firmware-level fan-curve control for quiet operation.
5. **Open source** — community trust, auditability, and customizability.
6. **Integration** — native support for D-Central's tools, dashboards, and companion hardware.
7. **Independence** — not reliant on third-party firmware development or licensing.

## Architecture — a clean rewrite on Buildroot

DCENT_OS is **100% D-Central original code**, not a fork. The 256 Foundation's Mujina, ESP-Miner,
and Skot's reverse-engineering work were studied as references for patterns and chip protocols, but
no code was forked.

```
┌────────────────────────────────────────┐
│ DCENT_OS Web Dashboard (React)          │
├────────────────────────────────────────┤
│ DCENT_OS Management API (REST + WS)     │
├────────────────────────────────────────┤
│ dcentrald — Mining Daemon (Rust)        │  100% D-Central original code
│ + Space-heater profiles                 │
│ + Auto-tuning engine                    │
│ + Home Assistant integration            │
├────────────────────────────────────────┤
│ ASIC Driver Layer (Rust)                │  Original, ESP-Miner as reference
├────────────────────────────────────────┤
│ DCENT_OS / Buildroot Linux              │  Custom Buildroot config
├────────────────────────────────────────┤
│ Zynq kernel + FPGA bitstream            │  Preserved boot chain
└────────────────────────────────────────┘
```

### Why each choice

**Clean rewrite (not a fork):** full IP ownership, no upstream dependency risk, D-Central branding
and direction throughout, and the freedom to make different architectural choices. It was more work
up front, but D-Central's research phase had already documented enough to write clean original code
confidently.

**Rust (not C):** memory safety without a garbage collector — which matters a great deal when the
firmware controls voltage regulators and ASICs worth thousands of dollars in a 24/7 home appliance.
Async/await fits concurrent hash-board management, and the type system catches ASIC-protocol errors
at compile time. BraiinsOS and LuxOS both proved Rust works for mining firmware.

**React (not Svelte/Vue):** the largest frontend ecosystem and component libraries for the
hashrate graphs and temperature gauges the dashboard needs. The bundle-size difference is irrelevant
inside the firmware image.

**Buildroot (not OpenWrt/Yocto):** simpler and lighter than OpenWrt's package management (which
DCENT_OS, as a single-purpose appliance, does not need) and far less complex than Yocto. Smaller
images, faster builds, simpler configuration.

**S9 first:** D-Central had an abundance of cheap, expendable S9s on the same Zynq architecture as
the S19. The I2C buses, FPGA registers, NAND layout, boot process, and PIC voltage control all
transferred to the later platforms — the S9 was the place to learn the Zynq platform safely.

## Key technical challenges (and how they were addressed)

- **ASIC chip communication** — register maps and per-generation job formats were only partially
  documented publicly. ESP-Miner's working C drivers were the reference; DCENT_OS re-derived clean
  Rust drivers from them and from live-probe data.
- **Per-chip auto-tuning** — no open-source reference existed (BraiinsOS and VNish keep this
  proprietary). DCENT_OS started conservative and iterated, with hard voltage and thermal clamps to
  protect the hardware.
- **Control-board diversity** — Zynq, BeagleBone, and Amlogic each need their own build. DCENT_OS's
  ChipID auto-detection and platform abstraction grew to cover them.
- **Bitmain security locks** — the Amlogic boot chain is encrypted, so DCENT_OS deliberately does
  not operate inside it (see [`security/AMLCTRL_BOUNDARY.md`](security/AMLCTRL_BOUNDARY.md)); the
  Zynq boards have an unencrypted boot chain.

## Space-heater features that set DCENT_OS apart

- **Quiet profiles** — firmware-level fan curves for quiet home operation.
- **Heat optimization** — maximize useful thermal output at a given power.
- **110 V mode** — reduced-power configurations for standard household power.
- **BTU/h readout** — built into every dashboard mode.
- **Schedule modes** — heat during cold hours, ease off during warm ones.
- **Home Assistant** — native integration for smart-home control.
- **Temperature targeting** — set a room temperature and let the firmware drive hashrate to hold it.

## Open-source references studied (never forked)

- [mujina-xilinx-platform](https://github.com/256foundation/mujina-xilinx-platform) — Zynq boot
  bypass + Buildroot
- [ESP-Miner](https://github.com/bitaxeorg/ESP-Miner) — BM1366/68/70/97 C drivers
- [Mujina](https://github.com/256foundation/mujina) — Rust mining firmware framework
- [skot/BM1397](https://github.com/skot/BM1397) — register maps and protocol documentation
- [braiins/BCB100](https://github.com/braiins/BCB100) — open-source control board HW+FW
- [skot/amlogic-cb-tools](https://github.com/skot/amlogic-cb-tools) — Amlogic board tools
- [256foundation/asic-rs](https://github.com/256foundation/asic-rs) — multi-vendor ASIC management
