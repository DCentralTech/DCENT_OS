# Third-Party Notices — DCENT_OS

DCENT_OS is Copyright (C) 2016-2026 D-Central Technologies. DCENT_OS (`dcentrald`, the Buildroot tree,
the dashboard, and the ESP firmware) is original D-Central code released under **GPL-3.0** (see
[`LICENSE`](LICENSE) — the verbatim license text). It is an original implementation — **not** a fork of
any existing firmware — written from live hardware probing, datasheets, our own reverse engineering, and
study of public open-source references (ESP-Miner, cgminer, BraiinsOS-published components). Where code
or protocol knowledge derives from a specific upstream project, that lineage is attributed below.

To boot and build, however, DCENT_OS reuses, references, or depends on a number of third-party
components. This file is the authoritative attribution record for them. Where a component carries its
own license, that license governs that component (not the repository-wide GPL-3.0 grant).

---

## 1. Boot-critical components reused from BraiinsOS (Zynq / Antminer S9–S19)

On Zynq-based control boards, a fully flashable DCENT_OS image **reuses the proven boot chain from
[BraiinsOS](https://github.com/braiins/braiins-os)** — the **First-Stage Boot Loader (FSBL)**,
**U-Boot**, the **FPGA bitstream**, and the **Linux kernel**. DCENT_OS pairs that boot chain with its
own Buildroot root filesystem and the `dcentrald` daemon.

**Important:** these boot components are **NOT vendored in this repository.** They are extracted at
build time from a licensed BraiinsOS image on the builder's machine and are **absent from this source
tree** (see [`DEVELOPMENT.md`](DEVELOPMENT.md) → "What builds from this repo, and what needs vendor
artifacts"). The corresponding source is available from the upstream projects below, satisfying the
GPL source-availability obligation for those components:

| Component | Upstream | License |
|---|---|---|
| Linux kernel (Zynq, `4.4.x-xilinx`) | [braiins/braiins-os](https://github.com/braiins/braiins-os) · [torvalds/linux](https://github.com/torvalds/linux) | GPL-2.0-only |
| U-Boot | [braiins/braiins-os](https://github.com/braiins/braiins-os) · [u-boot/u-boot](https://github.com/u-boot/u-boot) | GPL-2.0-or-later |
| FPGA bitstream / HDL (Zynq mining IP) | [braiins/zynq-io](https://github.com/braiins/zynq-io) | Apache-2.0 (HDL); see upstream |
| Zynq First-Stage Boot Loader (FSBL) | Xilinx, redistributed via BraiinsOS | BSD-3-Clause / Xilinx (per upstream headers) |

D-Central does not relicense these components; each remains under its upstream license. Amlogic and
AM335x targets do not use the BraiinsOS Zynq boot chain (different SoC boot model).

## 2. Patched packages (Buildroot)

| Package | Upstream | License | What we redistribute |
|---|---|---|---|
| **Dropbear SSH** | [mkj/dropbear](https://github.com/mkj/dropbear) (Matt Johnston) | MIT (with PuTTY / LibTomCrypt portions under their own permissive terms) | A single build-time patch, [`br2_external_dcentos/board/zynq/patches/dropbear/0001-disable-getrandom-to-avoid-boot-blocking.patch`](br2_external_dcentos/board/zynq/patches/dropbear/0001-disable-getrandom-to-avoid-boot-blocking.patch), that falls `getrandom` back to `/dev/urandom` for early Zynq boot. Dropbear itself is fetched from upstream by Buildroot — only the patch lives here. |

The rest of the Buildroot userland (BusyBox, libubootenv, etc.) is fetched and built from upstream
under each package's own license; nothing else is patched or vendored.

## 3. Reverse-engineering / protocol references (no code reused)

DCENT_OS is informed by public reverse-engineering and reference implementations. These are studied for
**protocol and register-map reference only — no source code is copied or forked** from them:

| Project | Upstream | License | Use |
|---|---|---|---|
| ESP-Miner | [bitaxeorg/ESP-Miner](https://github.com/bitaxeorg/ESP-Miner) | GPL-3.0 | BM1366/1368/1370/1397 ASIC-driver and API-contract reference |
| Mujina | [256foundation/mujina](https://github.com/256foundation/mujina) | Apache-2.0 | Rust mining-firmware architecture reference |
| BM1397 register maps | [skot/BM1397](https://github.com/skot/BM1397) | (see upstream) | Reverse-engineered register/protocol reference |
| pyasic | [UpstreamData/pyasic](https://github.com/UpstreamData/pyasic) | Apache-2.0 | API-compatibility contract reference |
| BraiinsOS / BCB100 | [braiins/braiins-os](https://github.com/braiins/braiins-os) · [braiins/BCB100](https://github.com/braiins/BCB100) | GPL | Boot-chain and S9 control-board reference |

## 4. Rust crate dependencies (`dcentrald`)

The `dcentrald` daemon links the crates below (and their transitive dependencies). Each is used under
its own license — predominantly permissive MIT / Apache-2.0. The authoritative, version-pinned list is
the crate graph (`cargo tree` / `Cargo.lock`); the principal direct dependencies are:

| Crate | Typical license |
|---|---|
| `tokio`, `tokio-util`, `tokio-tungstenite` | MIT |
| `serde`, `serde_json` | MIT OR Apache-2.0 |
| `toml` | MIT OR Apache-2.0 |
| `tracing`, `tracing-subscriber` | MIT |
| `thiserror`, `anyhow` | MIT OR Apache-2.0 |
| `nix` | MIT |
| `sha2`, `hmac` | MIT OR Apache-2.0 |
| `axum` | MIT |
| `askama` | MIT OR Apache-2.0 |
| `uuid` | MIT OR Apache-2.0 |
| `bytes` | MIT |
| `rumqttc` | Apache-2.0 |

## 5. Dashboard (npm) dependencies

The React/TypeScript dashboard depends on:

| Package | License |
|---|---|
| `react`, `react-dom` | MIT |
| `zustand` | MIT |
| `typescript` | Apache-2.0 |
| `vite`, `@vitejs/plugin-react`, `vite-plugin-singlefile` | MIT |
| dev/test tooling (`vitest`, `cypress`, `@testing-library/*`, `@axe-core/cli`, `jsdom`, …) | MIT (per package) |

The full, version-pinned list is `dashboard/package.json` + `dashboard/package-lock.json`. The shipped
dashboard is built locally — no fonts, CDNs, or remote assets are bundled.

---

## 6. License summary

- **DCENT_OS original code:** GPL-3.0 (`LICENSE`).
- **Reused BraiinsOS boot components:** their upstream GPL-2.0 / Apache-2.0 / BSD licenses (extracted at
  build time, not vendored here — source available upstream).
- **Bundled patch:** Dropbear under its MIT-style license.
- **Crate / npm dependencies:** their own MIT / Apache-2.0 / permissive licenses.

Canonical license texts: GPL-3.0 → https://www.gnu.org/licenses/gpl-3.0.txt · GPL-2.0 →
https://www.gnu.org/licenses/old-licenses/gpl-2.0.txt · Apache-2.0 →
https://www.apache.org/licenses/LICENSE-2.0.txt · MIT → https://opensource.org/license/mit/

Questions about attribution or licensing: **legal@d-central.tech**.
