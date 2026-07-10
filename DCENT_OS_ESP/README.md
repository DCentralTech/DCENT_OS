# DCENT_OS for ESP devices - DCENT_axe Mining Firmware

> AI-native open-source firmware for the BitAxe-class ESP32-S3 family, built by
> [D-Central Technologies](https://d-central.tech/).
> An original Rust firmware for BitAxe-class hardware (informed by the
> open-source ESP-Miner project, not a fork) with a built-in
> **MCP (Model Context Protocol) server** — supported BitAxe-class
> devices expose a local AI-control surface while preserving the current
> proof boundary for live mining validation.

**DCENT_axe** is the ESP32 product identity inside the
[DCENT_OS](https://github.com/DCentralTech/DCENT_OS) family. It targets the same
home-miner audience, with the same philosophy — *quiet by default,
transparent, locally controlled, no cloud account, no mandatory devfee* —
but on ESP32-S3 BitAxe-class boards instead of industrial Antminers. The
D-Central DCENT_axe open-hardware boards live in `projects/dcent-axe/` and
run this firmware.

It is an **original rewrite in Rust**, informed by the public
[ESP-Miner](https://github.com/bitaxeorg/ESP-Miner) project as a protocol
reference, but not a fork: every line of `dcentaxe` is D-Central's own,
GPL-3.0 from day one.

[![Fund the Sovereign Stack](https://img.shields.io/badge/Fund%20the%20Sovereign%20Stack-%E2%9A%A1%20Bitcoin%20%7C%20Card-F7931A)](https://d-central.tech/fund/go?source=dcent_axe&placement=release_notes)

> D-Central gives this away under GPL-3.0. If it helps you, [keep it alive](https://d-central.tech/fund/) — in Bitcoin or by card. Not a licence; commercial use is always free.

---

Built by the **Mining Hackers** at [D-Central Technologies](https://d-central.tech/) — Canada's leading
Bitcoin mining technology company since 2016, based in Laval, Québec. 2,500+ miners repaired,
400+ products shipped. This is the bench we use ourselves, released so every operator can own, repair,
and understand their own hardware.

---

## Why this exists

The BitAxe is the most exciting open-hardware Bitcoin product of the last
decade. The default firmware (ESP-Miner) is great, and the ecosystem around
it is healthy. So why build another firmware?

Three reasons:

1. **Memory-safe Rust on ESP-IDF v5.3+.** The BitAxe runs on an ESP32-S3
   with ~300 KB of internal heap. Rust's ownership model plus a ground-up
   architecture make it possible to fix entire bug *classes* (use-after-free
   on shared work objects, partial-frame UART corruption, allocator
   fragmentation under sustained mining) instead of one report at a time.
2. **AI-native control, by design.** DCENT_axe ships an
   [MCP](https://modelcontextprotocol.io/) server over JSON-RPC 2.0 with
   12+ tools and 4 live resources, so any MCP-aware AI client (Claude,
   Cursor, custom agents, your own scripts) can read miner state and drive
   it directly. Set frequency, swap pools, run an autotune, query the
   block-tile, fetch history — over a documented protocol, not a scraped
   HTML form.
3. **Home-miner UX, not datacenter UX.** Quiet boot, BTU/h headlines, a
   Tamagotchi-style OLED carousel, and a cyberpunk terminal dashboard.
   Every BitAxe ships as both a miner *and* a useful piece of desk
   furniture.

DCENT_axe stays neutral toward existing pools and existing AxeOS workflows.
It is not pool-locked, not vendor-locked, and not D-Central-locked, and it
ships with **zero dev fee** — every share is yours.

---

## How it works

```
+---------------+    +-----------------------------------+    +---------------+
|  Bitcoin pool | <- |  DCENT_axe (Rust on ESP-IDF v5.3) | -> |  BM1366 /     |
|  (V1; opt V2) |    |  - dispatcher (work + slot scan)  |    |  BM1368 /     |
+---------------+    |  - Stratum V1 / optional V2       |    |  BM1370 /     |
                     |  - autotuner (Hz / W / J/TH / T)  |    |  BM1397       |
+---------------+    |  - power: TPS546 + DS4432U        |    |  ASICs over   |
|  AI client    | -> |  - thermal: EMC2101 / EMC2103     |    |  UART daisy   |
|  (Claude etc.)|    |  - MCP JSON-RPC 2.0 server        |    |  chain        |
|  via MCP      |    |  - HA-compatible REST API         |    +---------------+
+---------------+    +------------------+----------------+
                                        |
                                        | local LAN (HTTP + MCP)
                                        v
                  +------------------------------------------+
                  |  Web dashboard (cyberpunk terminal v8)   |
                  |  + Tamagotchi OLED carousel (8 pages)    |
                  +------------------------------------------+
```

The firmware is a multi-crate Rust workspace targeting `xtensa-esp32s3-espidf`:

- `dcentaxe/` — main binary + embedded web dashboard.
- `dcentaxe-asic/` — BM1366 / BM1368 / BM1370 / BM1397 drivers, CRC, PLL,
  and the `AsicDriver` trait.
- `dcentaxe-hal/` — board detection, UART, I²C, TPS546 power, fan PWM, GPIO,
  EMC2101/EMC2103/TPS546 temperature.
- `dcentaxe-mining/` — work dispatcher, rolling hashrate, share tracking.
- `dcentaxe-stratum/` — Stratum V1 client (shared with DCENT_OS).
- `dcentaxe-stratum-v2/` — optional Stratum V2 implementation; non-V2 builds
  fail closed when a V2 pool is configured.
- `dcentaxe-bap/` — BAP UART accessory protocol crate for Bitaxe Touch-style
  displays and the [DCENT_ExpansionPack](https://github.com/DCentralTech/DCENT_ExpansionPack)
  BAP header. Live BAP server/control is scaffolded and not advertised as
  spawned in the shipping binary yet.
- `dcentaxe-design-bundle/` — design system tokens shared with DCENT_OS.

---

## Key features

- **Build targets and drivers for current BitAxe variants** — Max
  (BM1397), Ultra (BM1366), Supra (BM1368), Gamma / legacy BM1370 dual-chip lab context, Hex
  Ultra (6× BM1366), Hex Supra (6× BM1368). See the hardware table for
  live bring-up versus host-tested status.
- **Rust on ESP-IDF v5.3+.** Memory-safe core, alloc-free panic + OOM
  hooks, NVS breadcrumb on crash for post-mortem.
- **PSRAM enabled (where the module provides it).** Adds ~8 MB of heap on
  supported S3 modules; designed for low heap drift — bench soaks on lab
  hardware showed internal-heap drift under ~15 KB (per-board public soak
  evidence pending), and a captured OOM is preserved as an NVS breadcrumb for
  post-mortem.
- **Robust UART recovery.** Fallback slot scan + frame-recovery clear on
  partial reads. Took Hex board hashrate from 476 GH/s → 3.7 TH/s (rated;
  full live soak pending) by fixing the BM1368 job-id mismatch class.
- **Per-chip stats.** Real per-ASIC hashrate / shares / errors on Hex
  boards — not just an aggregate.
- **Cyberpunk terminal dashboard (v8).** Modular component front-end
  on top of a pre-rendered shell. Mining-core sphere, ASIC silicon SVG,
  flow ribbon, rich block modal with solo-verification chip drill-down.
- **Tamagotchi OLED carousel.** 8-page rotating display with pixel art,
  sparklines, and live mining stats.
- **AI-native MCP server.** JSON-RPC 2.0 over HTTP at `/mcp`, 12+ tools,
  4 live resources. Drive your miner from any MCP-compatible AI client.
- **Space Heater mode.** Room-temperature targeting, BTU/h headline,
  thermostat-style control surface.
- **Advanced autotuner.** User-selectable target — max hashrate, target
  watts, target J/TH, or target temperature.
- **Home Assistant via MQTT auto-discovery (opt-in, default-OFF).** Point the
  firmware at your MQTT broker and the miner appears in Home Assistant
  automatically — sensor entities for hashrate, ASIC temperature, input power,
  fan RPM, accepted/rejected shares and uptime, plus a mining-active
  binary_sensor (the "Bitcoin space heater" surface). Outbound + publish-only;
  it never touches mining or the safety paths and adds no HTTP handler.
  *Status: implemented + host-unit-tested (the discovery/state payload builder)
  and built for the device; live broker delivery is not yet field-proven.*
  HA can also still poll the AxeOS-style REST API directly.
- **Stratum V1, with optional Stratum V2 builds.** Non-V2 builds fail closed if
  a V2 pool is configured.
- **Signed OTA release path.** Public release packages are Ed25519-signed;
  ad hoc local packaging emits signatures only when the signing environment is
  configured. Manifest-checked update slot fit is always part of the package
  gate.
- **Local-first.** All dashboard assets ship with the firmware. No CDN
  fonts, no telemetry phone-home, no remote-management backdoor.
- **Zero dev fee.** No mandatory fee and no pool lock-in — 100% of every
  share goes to your own pool account. Fully open source (GPL-3.0).

---

## Hardware support

| ASIC    | BitAxe boards                | Hashrate (typical)  | Status |
| ------- | ---------------------------- | ------------------- | ------ |
| BM1397  | Max; DCENT_axe BM1397        | ~400 GH/s           | Driver proven / host-tested; DCENT_axe first-article bring-up pending |
| BM1366  | Ultra, Hex Ultra (6× BM1366) | ~500 GH/s / ~3 TH/s | Ultra: driver proven / host-tested. **Hex Ultra: EXPERIMENTAL** — 6×BM1366 (the largest Bitaxe topology) |
| BM1368  | Supra, Hex Supra (6× BM1368) | ~600 GH/s / ~3.6 TH/s | Driver proven / host-tested; Hex Supra dispatcher/job-id fix live-proven, ~3.7 TH/s rated (live soak pending) |
| BM1370  | Gamma (1× BM1370), legacy dual-chip lab context (2× BM1370) | ~1.2 TH/s / ~2.7 TH/s | Driver proven / host-tested; Gamma live bring-up confirmed (sustained soak pending); legacy BM1370 lab evidence remains internal context |

The public Toolbox-installable Bitaxe-class variants are Max, Ultra, Supra,
Gamma, Hex Ultra, and Hex Supra. They run from the same Rust workspace with a
build feature per board (`--features bitaxe-gamma` etc.) for the right fan
controller and ASIC count.

LoRa mesh support is scaffolded, default-OFF, and not wired into the
`dcentaxe` binary yet. This ESP tier is not presented as having passed the
industrial two-Xilinx public-beta gate used by the Antminer side of
DCENT_OS.

---

## AI-native control via MCP

DCENT_axe exposes an [MCP](https://modelcontextprotocol.io/) JSON-RPC 2.0
server at `http://<bitaxe-ip>/mcp`. The same protocol Claude and Cursor
already speak.

**Tools (callable):**

```
get_status        get_asic_info     set_frequency     set_core_voltage
set_fan_speed     set_pool          get_network       get_history
restart_mining    identify_device   get_swarm         run_autotune
```

(`ota_check` is planned, not yet callable; mutating tools require
`authorize_mcp_control`.)

**Resources (subscribable):**

```
bitaxe://status     live status snapshot
bitaxe://history    rolling per-minute history
bitaxe://config     current configuration
bitaxe://swarm      local swarm metadata + reported peers
```

That means you can hand a Claude chat session your BitAxe's IP and ask:

> *"What's the J/TH on this BitAxe right now? If it's above 22, drop the
> frequency 5%, then run an autotune for max efficiency, and let me know
> when it converges."*

…and the model will actually do it, because the protocol underneath is
a real, documented, type-checked surface — not screen-scraping.

For multi-miner fleets, the same MCP surface works against every
DCENT_axe on the LAN, and the upcoming swarm coordinator (v1.1) will
expose the fleet itself as a single MCP server.

---

## Status

DCENT_axe is at **v0.3.0 with 16 stability + UX phases shipped**. The
driver and dispatcher stack is proven by host tests and focused live runs:
Gamma had live bring-up confirmed (sustained soak pending); legacy BM1370 dual-chip lab evidence remains internal context; Hex Supra dispatcher/job-id recovery is live-proven in a focused run;
Max/Ultra/Supra drivers are proven by host tests; Bitaxe Hex Ultra
(6× BM1366) is EXPERIMENTAL — the largest Bitaxe topology. Internal-heap
management is hardened (Phase A–T): designed for low heap drift — bench soaks
on lab hardware showed freeHeap drift under ~15 KB (per-board public soak
evidence pending), and a captured OOM is preserved as an NVS breadcrumb for
post-mortem.

Selected milestones from the engineering log:

- **Hex Supra dispatcher path fixed** — ~3.7 TH/s rated (live soak pending); legacy BM1370 dual-chip lab
  first-boot/live verification reached ~2.7 TH/s.
- **Supported board families are driver-proven/host-tested.** Gamma has
  live bring-up confirmed (sustained soak pending), legacy BM1370 lab evidence remains internal context,
  and Hex Supra has live-proven dispatcher/job-id recovery (~3.7 TH/s rated, live soak pending).
  **Bitaxe Hex Ultra (6× BM1366) is EXPERIMENTAL — the largest Bitaxe
  topology (6 chips on a single daisy-chain).**
  Production install proof still requires
  per-board factory flash, signed OTA, reboot/version, accepted-share, and soak
  evidence.
- **BM1368/BM1370 job-id mismatch class fixed** — Hex Supra
  476 GH/s → 3.7 TH/s after fallback slot scan + UART frame recovery.
- **BM1370 job-id extraction mask matched to ESP-Miner** — legacy BM1370 dual-chip lab context
  163 GH/s → 2.7 TH/s after fixing `(id & 0xf0) >> 1` and
  `DispatcherConfig::for_bm1370` with `job_id_step=8`.
- **TPS546 CML + phantom-overvoltage recovery** — clean recovery from
  PMBus fault states without bricking the buck regulator.
- **Coredump streaming** — chunked download of post-crash coredumps
  through the dashboard.
- **Alloc-free panic + OOM hook → NVS breadcrumb** — raw FFI
  `nvs_set_blob`, zero `format!` / `String` allocations in the panic
  path. Live-proven.
- **PSRAM enabled (+8 MB heap)** — single biggest stability win.
- **Periodic restart + heap watchdog** — defensive bounds for soak runs.
- **Streaming chips JSON + HTTP buffer pool** — eliminated >5 KB
  per-handler allocations on the hot HTTP path.
- **`/api/system/info` Serialize-derive** — 158-field DTO, JSON byte-
  identical, hammer-tested with 50 parallel pollers.
- **Modular dashboard wired end-to-end** — Phase 2.A-3.2 components,
  canonical lockup logo, Logs page, coinbase decoder feeding the
  block-tile solo-verification path.
- **Block-tile rich modal** — AGE / TXS / REWARD / DIFF / hash preview,
  LIVE pill, "Hashrate · 10m Average" caption.

Per-board live soak for the six public Toolbox-installable targets is still
pending and is operator/hardware-gated; it is not yet shipped as evidence in
this repository.

---

## Build and flash

DCENT_axe is built with the **esp-rs** Rust toolchain on
ESP-IDF v5.3+ targeting `xtensa-esp32s3-espidf`.

### Build (Windows path-length workaround)

ESP-IDF requires short build paths. Use `CARGO_TARGET_DIR`:

```bash
cd DCENT_OS_ESP
CARGO_TARGET_DIR=C:/bt cargo build --release -p dcentaxe
```

The built artifact is an **ELF** at
`C:/bt/xtensa-esp32s3-espidf/release/dcentaxe`. It is not a raw `.bin`
— flashing tools that don't understand ELF will brick the app slot.

For board-specific builds, use the matching feature:

```bash
# Bitaxe Gamma (BM1370)
cargo build --release -p dcentaxe \
  --no-default-features --features bitaxe-gamma
```

| Board | ASIC | Build feature | OTA payload | Factory payload | Status |
| --- | --- | --- | --- | --- | --- |
| Bitaxe Max | BM1397 | `bitaxe-max` | `dcentaxe-bitaxe-max-<version>-update.bin` | `dcentaxe-bitaxe-max-<version>-factory.bin` | Driver proven / host-tested |
| Bitaxe Ultra | BM1366 | `bitaxe-ultra` | `dcentaxe-bitaxe-ultra-<version>-update.bin` | `dcentaxe-bitaxe-ultra-<version>-factory.bin` | Driver proven / host-tested |
| Bitaxe Supra | BM1368 | `bitaxe-supra` | `dcentaxe-bitaxe-supra-<version>-update.bin` | `dcentaxe-bitaxe-supra-<version>-factory.bin` | Driver proven / host-tested |
| Bitaxe Gamma | BM1370 | `bitaxe-gamma` | `dcentaxe-bitaxe-gamma-<version>-update.bin` | `dcentaxe-bitaxe-gamma-<version>-factory.bin` | Driver proven / host-tested; Gamma live bring-up confirmed (sustained soak pending); legacy BM1370 lab evidence is internal context |
| Bitaxe Hex Ultra | 6× BM1366 | `bitaxe-hex-ultra` | `dcentaxe-bitaxe-hex-ultra-<version>-update.bin` | `dcentaxe-bitaxe-hex-ultra-<version>-factory.bin` | **EXPERIMENTAL** — 6×BM1366 (the largest Bitaxe topology) |
| Bitaxe Hex Supra | 6× BM1368 | `bitaxe-hex-supra` | `dcentaxe-bitaxe-hex-supra-<version>-update.bin` | `dcentaxe-bitaxe-hex-supra-<version>-factory.bin` | Hex dispatcher path host-tested, including BM1368 job-id fix; ~3.7 TH/s rated (live soak pending) |

Gamma Duo, legacy BM1370 dual-chip lab targets, Touch-class, Nerd, and DCENT_axe first-article targets remain
internal/lab build targets. They can still be built deliberately with a manual
feature/package invocation, or included in the matrix with
`INCLUDE_INTERNAL_TARGETS=1` / `-IncludeInternalTargets`, but the current public
Toolbox routes intentionally accept only the six rows above.

### Flash

Three supported paths:

```bash
# espflash (preferred — handles ELF + partition table automatically)
espflash flash --port COM3 \
  --partition-table partitions.csv \
  C:/bt/xtensa-esp32s3-espidf/release/dcentaxe

# DCENT Toolbox
dcent flash --serial COM3 -f firmware.bin
dcent build-flash COM3

# OTA over Wi-Fi
# Open http://<bitaxe-ip>/ → Advanced → Firmware Update
```

`scripts/package-firmware.sh` (and the PowerShell equivalent) is the
canonical packaging gate — it reads `partitions.csv`, verifies the
update image fits the OTA app slot, and writes a manifest with
`updateFitsSlot`, `slotSize`, and SHA-256 fields. The dashboard refuses
uploads when the manifest is dishonest.

`scripts/build-matrix.sh` and `scripts/build-matrix.ps1` default to the same six
public Toolbox-installable targets. Internal/lab targets are opt-in so a public
release run does not emit packages that Toolbox correctly refuses.

---

## Repository layout

```
dcentos-esp/
├── dcentaxe/             Main binary + embedded web dashboard
├── dcentaxe-asic/        BM1366 / BM1368 / BM1370 / BM1397 drivers
├── dcentaxe-hal/         Board detection, UART, I²C, power, fan, GPIO, temp
├── dcentaxe-mining/      Work dispatcher + hashrate / share tracking
├── dcentaxe-stratum/     Stratum V1 client (shared with DCENT_OS)
├── dcentaxe-stratum-v2/  Stratum V2 client
├── dcentaxe-bap/         BAP UART accessory protocol (Bitaxe Touch + DCENT_XPack)
├── dcentaxe-design-bundle/  Design system tokens shared with DCENT_OS
├── partitions.csv        Flash layout (3 MB app, 2 MB LittleFS)
├── sdkconfig.defaults    ESP-IDF config (PSRAM on, 32 KB main stack, etc.)
├── docs/                 Architecture, reviews, ship-readiness reports
├── scripts/              Package, flash, soak, and bring-up helpers
└── releases/             Retained release artifacts; public bundles/manifests are generated by the release matrix
```

---

## About D-Central

[D-Central Technologies](https://d-central.tech/) is Canada's leading
Bitcoin mining technology company. Founded in 2016 in Laval, Québec.
Self-described *Mining Hackers*. 2,500+ miners repaired, 400+ products,
and a stubborn belief that **every Bitcoin miner deserves to be open,
auditable, and hackable**.

## The D-Central open-source Bitcoin mining ecosystem

All under one roof at **[github.com/DCentralTech](https://github.com/DCentralTech)** — decentralize
every layer: mining, tools, hardware, communication.

- **[DCENT_OS](https://github.com/DCentralTech/DCENT_OS)** — open-source mining firmware for industrial
  Antminers (S9→S21) and ESP32 Bitaxe-class miners (Avalon + WhatsMiner scaffolded).
- **[DCENT_Toolbox](https://github.com/DCentralTech/DCENT_Toolbox)** — the open-source bench tool: scan,
  unlock, audit, flash, and prove — from your own machine.
- **[DCENT_axe](https://github.com/DCentralTech/DCENT_axe)** — open-hardware Bitaxe-class boards
  (Solo / Quad / Hex) with integrated LoRa mesh.
- **[DCENT_Raven](https://github.com/DCentralTech/DCENT_Raven)** — LoRa-mesh accessory for any Bitaxe.

---

## Acknowledgments

DCENT_axe is an original implementation, and it stands on the shoulders of
public work that made the BitAxe ecosystem what it is:

- **[BitAxe](https://bitaxe.org/)** and **[ESP-Miner](https://github.com/bitaxeorg/ESP-Miner)**
  by [@skot](https://github.com/skot) and the BitAxe community — the
  open hardware and the protocol reference that this firmware is built
  to be compatible with.
- **[Mujina](https://github.com/skygate/mujina)** — Rust-on-Zynq mining
  firmware reference work.
- **[BraiinsOS / Bosminer](https://github.com/braiins/braiins)** — public
  reverse-engineering effort that informed the broader DCENT stack.

---

## License

DCENT_axe is licensed under [**GPL-3.0**](LICENSE). Forks, audits, and
community contributions are explicitly welcome under that license.

---

## Contributing

Bug reports and beta-tester feedback are welcome via GitHub Issues.
Pull requests should target the active development branch and include:

- The board variant you tested on (Max / Ultra / Supra / Gamma / legacy BM1370 lab target /
  Hex Ultra / Hex Supra).
- A serial log from a flashed board if your change touches ASIC drivers,
  Stratum, the dispatcher, or the OTA path.
- A soak result (`soak*.json`) for changes that affect long-running
  stability — heap, dispatcher, HTTP buffer pool, or panic / OOM paths.

If you find a security issue (auth bypass, OTA-signing flaw, MCP-tool
injection, dashboard XSS), please email **security@d-central.tech**
instead of opening a public issue.
