<div align="center">

# DCENT_OS

**Open-source Bitcoin-mining firmware that unifies industrial Antminers and ESP32 Bitaxe-class miners in one project — one firmware family for your whole fleet.**

Industrial Antminers · ESP32 Bitaxe-class miners · Avalon and WhatsMiner on the roadmap.
Rust · local-first · zero mandatory dev fee · GPL-3.0.

[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)
[Built by D-Central Technologies](https://d-central.tech/) · Mining Hackers since 2016

</div>

---

Built by the **Mining Hackers** at [D-Central Technologies](https://d-central.tech/) — Canada's leading
Bitcoin mining technology company since 2016, based in Laval, Québec. 2,500+ miners repaired,
400+ products shipped. This is the firmware we run ourselves, released so every operator can own, repair,
and understand their own hardware.

DCENT_OS turns an industrial or desktop Bitcoin ASIC into a quiet, efficient, locally-controlled home
mining space heater — with **no cloud account, no telemetry, no license server, and no mandatory dev
fee.** It is the firmware result of D-Central's last several months of deep, hands-on reverse engineering
across the Antminer and Bitaxe hardware families — work few others in the space have published openly.

> **Decentralize every layer.** Heat your home. Stack sats.

## One firmware family, many miners

DCENT_OS is organized as a single repository spanning every platform we support, so the shared design
language, dashboard, and mining concepts live in one place:

```
DCENT_OS/
├── DCENT_OS_Antminer/     → industrial Antminers (S9 → S21): Rust dcentrald daemon + Buildroot Linux + dashboard + docs/
├── DCENT_OS_ESP/          → ESP32-S3 Bitaxe-class miners (BM1397/1366/1368/1370): clean-room Rust firmware + built-in MCP
├── DCENT_OS_AvalonMiner/  → Avalon (Canaan) support — in development
├── DCENT_OS_WhatsMiner/   → WhatsMiner (MicroBT) support — in development
└── shared/                → schema + protocol crates shared across platforms (e.g. dcent-schema)
```

Per-platform docs (architecture, platform matrix, install guides) live under each product
directory — e.g. [`DCENT_OS_Antminer/docs/`](DCENT_OS_Antminer/docs/).

| Platform | Where | Status |
|---|---|---|
| **Antminer** S9 / S17 / S19 / S19 Pro / S19j Pro / S21 | `DCENT_OS_Antminer/` | **Supported** — multiple models mining-proven; see [`DCENT_OS_Antminer/docs/PLATFORMS.md`](DCENT_OS_Antminer/docs/PLATFORMS.md) for the honest per-model readiness matrix (mining-proven vs lab-gated vs blocked). |
| **Bitaxe-class** Max / Ultra / Supra / Gamma / Hex Ultra / Hex Supra (ESP32-S3) | `DCENT_OS_ESP/` | **Supported** — Gamma live-verified; others driver- and host-tested. Built-in MCP (AI-control) server for the miner. |
| **Avalon** (Canaan) | `DCENT_OS_AvalonMiner/` | **In development** — architecture scaffolded; no mining claim yet. |
| **WhatsMiner** (MicroBT) | `DCENT_OS_WhatsMiner/` | **In development** — reverse-engineering + bring-up underway; no mining claim yet. |

We publish an **honest readiness taxonomy** and never imply a model is production-ready before it has
mined real, accepted shares. "Supported" links to per-platform matrices that say exactly what is proven,
what is lab-gated, and what is still blocked.

## Quick start

Pick your platform and follow its guide:

- **Antminer:** [`DCENT_OS_Antminer/README.md`](DCENT_OS_Antminer/README.md) → flash a prebuilt signed
  release with [DCENT_Toolbox](https://github.com/DCentralTech/DCENT_Toolbox), or build the signed
  sysupgrade in Docker (a full flashable image needs non-redistributable SoC boot components — see
  [`DCENT_OS_Antminer/DEVELOPMENT.md`](DCENT_OS_Antminer/DEVELOPMENT.md)).
- **Bitaxe-class (ESP32):** [`DCENT_OS_ESP/README.md`](DCENT_OS_ESP/README.md) → build with the esp-rs
  toolchain and flash over USB or OTA.

## What makes DCENT_OS different

- **Open source, clean-room.** GPL-3.0. No forked proprietary code — every ASIC constant is documented
  with its source (live probe, datasheet, or our own reverse engineering).
- **Local-first.** A built-in web dashboard, REST API, CGMiner-compatible API, and an MCP server — all on
  your LAN. No cloud, no account.
- **AI-native.** One of the first miner firmwares with a built-in MCP (Model Context Protocol) server, so
  an AI agent can read status and (on an authenticated owner session) control the miner.
- **Quiet by default.** Home/space-heater profiles cut hash power before raising fan noise.
- **Honest by design.** Upload ≠ mined, scheduled ≠ flashed, connected ≠ mining — the UI and docs keep
  every claim evidence-gated.

## The D-Central open-source Bitcoin mining ecosystem

All under one roof at **[github.com/DCentralTech](https://github.com/DCentralTech)** — decentralize every
layer: mining, tools, hardware, communication.

- **[DCENT_OS](https://github.com/DCentralTech/DCENT_OS)** — open-source mining firmware for industrial
  Antminers (S9→S21) and ESP32 Bitaxe-class miners (Avalon + WhatsMiner scaffolded).
- **[DCENT_Toolbox](https://github.com/DCentralTech/DCENT_Toolbox)** — the open-source bench tool: scan,
  unlock, audit, flash, and prove — from your own machine.
- **[DCENT_axe](https://github.com/DCentralTech/DCENT_axe)** — open-hardware Bitaxe-class boards
  (Solo / Quad / Hex) with integrated LoRa mesh.
- **[DCENT_Raven](https://github.com/DCentralTech/DCENT_Raven)** — LoRa-mesh accessory for any Bitaxe.

## License & governance

GPL-3.0 (see [`LICENSE`](LICENSE)). Contributions welcome — see [`CONTRIBUTING.md`](CONTRIBUTING.md),
[`GOVERNANCE.md`](GOVERNANCE.md), [`SECURITY.md`](SECURITY.md), and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
DCENT_OS is steered by D-Central Technologies and built in the open for the whole mining community.
