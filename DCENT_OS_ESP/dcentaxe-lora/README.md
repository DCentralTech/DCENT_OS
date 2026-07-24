# dcentaxe-lora

SX1262 LoRa driver + the lightweight **DCENT mesh** stack for the DCENT_axe board
line. Every DCENT_axe board (BM1397 single → 6× Hex) carries an onboard SX1262 on
its **own dedicated SPI bus** — never the BAP/J4 header — so a UART-mode BAP
accessory can never kill the radio.

Source dossier:
- §4.4 (LoRa stack), §6 (guardrails)
- §1 (subcircuit, GPIO map, use-cases)
- (SX1262 electricals)

## Status: INTEGRATED behind default-OFF `lora` (live RF unproven)

This crate is the DCENT mesh / SX1262 driver. It is a workspace member and is
**wired into the `dcentaxe` binary** when the orthogonal Cargo feature `lora` is
selected (`dep:dcentaxe-lora` + HAL `pins-lora`):

- `dcentaxe` opens SPI3 + control GPIOs, spawns `lora_task`, registers MCP tools,
  and injects an honesty-gated dashboard mesh panel (`#[cfg(feature = "lora")]`).
- **Default features and public board features do NOT enable `lora`** — stock
  images stay radio-dark so a non-functional "LoRa enabled" UI cannot ship.
- Combine explicitly, e.g. `--features dcent-axe-bm1397,lora` (check OTA slot /
  `updateFitsSlot` before shipping that image).
- Host tests pass; pin map is **netlist-locked 9/9** on DCENT_axe BM1397.
- **Live SX1262 TX/RX and multi-node relay are unproven** — no product mesh claim
  until first-article RF bring-up.

## What's here

| File | Role |
|------|------|
| `src/lib.rs` | Module wiring + the host-testable `SpiBus` / `GpioPin` traits + `LoraError`. Mock SPI/GPIO doubles for tests. |
| `src/sx1262.rs` | Register-level SX1262 driver skeleton: command opcodes, register/IRQ constants, BUSY-poll handshake, DIO1 IRQ decode, region select, RF-frequency-word maths, cold-boot `begin`. |
| `src/mesh.rs` | The DCENT mesh protocol (frame types `Telemetry` / `BlockFound` / `Identify` / owner-authed `Command` / `Ack`) on the wire `$DCM,<TOK>,<src-hex8>,<seq-hex2>,<ttl-hex>,<fields…>*XX\r\n` — the BAP `$…,<TOK>,…*XX` grammar plus a per-source `seq` (dedup + anti-replay key) and a `ttl` hop budget for store-and-forward relay. `meshtastic-interop` Phase-2 stub gated behind a feature. |
| `src/auth.rs` | Owner-command authentication: HMAC-SHA256 (RustCrypto `hmac`/`sha2`, `subtle`-backed constant-time verify) over `dcm-cmd:<src>:<seq>:<verb>:<param>:<value>`, plus a bounded `ReplayGuard` and the combined `MeshAuthenticator` (MAC-then-freshness). Replaces the scaffold's placeholder string compare. |
| `src/mcp.rs` | MCP tool **definitions** `lora_status` (read), `lora_send_beacon` (owner-control), `get_mesh_peers` (read) + their serde I/O structs — ready to register, **not** registered. |
| `src/esp_hal.rs` | Real ESP32-S3 SPI3/HSPI + GPIO transport. Feature-gated `esp-idf` (off by default, like dcentaxe-bap's `uart.rs`). Integration seam — verify against esp-idf-hal 0.46 at wire-up. |

## Host-test story

The driver logic is written against the abstract `SpiBus` + `GpioPin` traits, so
every byte-level command is exercised on the dev machine with a mock bus — no
ESP32 required. The real ESP-IDF transport (`esp_hal.rs`) is gated behind the
`esp-idf` feature so host `cargo test` stays pure-Rust (mirrors `dcentaxe-bap`).

```
cargo test -p dcentaxe-lora --target x86_64-pc-windows-msvc
```

`--target <host-triple>` overrides the workspace's xtensa default; `+stable`
(or any non-`esp` toolchain) bypasses the workspace `build-std`. **24 host tests
pass**, covering: BUSY-wait (success + timeout), the `SetRfFrequency` word for
915 MHz (`0x39300000`) and 868 MHz (`0x36400000`), IRQ-status decode,
`GetIrqStatus` read framing, mesh frame round-trips (telemetry / block-found /
identify / command with reserved-char escaping), the owner-auth gate, and the MCP
access-class contract. `cargo clippy -- -D warnings` is clean.

> Note: the in-tree `cargo test -p dcentaxe-lora` requires the dcentos-esp
> workspace to load. After the 2026-06-26 re-home, the `dcentaxe` binary's
> `dcent-schema` path dep is `../../dcent-schema` from the new
> `DCENT_OS_ESP/` workspace root.

## GPIO map — ✅ LOCKED (BM1397 netlist 9/9, 2026-07-11)

Authoritative constants live in `dcentaxe-hal::lora_pins` (table-tested). MOSI is
**not** on GPIO14 (fan tach). TXEN/RXEN are host-driven on the E22 module.

| Signal | GPIO | Notes |
|--------|------|-------|
| LORA_SCLK | 5 | dedicated SPI3/HSPI, non-strap |
| LORA_MOSI | 6 | |
| LORA_MISO | 7 | |
| LORA_NSS | 15 | active-low CS |
| LORA_BUSY | 16 | readable GPIO, polled before every command |
| LORA_DIO1 | 21 | IRQ-capable (TxDone/RxDone) |
| LORA_NRESET | 8 | active-low reset |
| LORA_TXEN | 2 | E22 RF-switch TX enable (host-driven) |
| LORA_RXEN | 9 | E22 RF-switch RX enable (host-driven) |

SX1262 electricals (doc 08): SPI ≤ 16 MHz (ESP32-S3 ≤ 40 MHz clears it >2×); TCXO
enabled via DIO3 (`SetDIO3AsTcxoCtrl`, mandatory or RF is dead; 1.8 V on the E22);
DIO1 = IRQ; BUSY polled; region 868 (EU) / 915 (NA) selectable on one populated
board.

## DCENT_Raven harmonization

`dcent-raven` is the sibling LoRa-mesh BAP **accessory** (its own MCU as BAP host).
This crate is aligned with it so the two projects share register-level + protocol
vocabulary:

- **Same silicon & control set** — SX1262 (Ebyte E22-900M22S class, on-module
  32 MHz TCXO), SPI + BUSY + DIO1(IRQ) + NRESET, TCXO via DIO3, region 915 default
  / 868 EU. The driver opcodes/registers/IRQ bits match the Raven `DESIGN_FREEZE`.
- **Same mesh telemetry vocabulary** — `mesh::Telemetry` / `BlockFound` /
  `Identify` field names mirror Raven's `MinerState` (hashrate, chip_temp, power,
  shares acc/rej, best_diff, block_height, device_model, asic_model). The
  block-found beacon maps directly to Raven's `found_block` rising edge →
  high-priority broadcast.
- **Same control safety posture** — air-gap control is owner-gated
  (`MeshCommand::authorize`), mirroring Raven's fail-closed monitor-only default.
- **One grammar** — the mesh frame reuses the BAP `$…,<TOK>,…*XX\r\n` NMEA shape +
  XOR checksum (`dcentaxe_bap::protocol::nmea_xor`), so dashboard / MCP / BAP / LoRa
  share one vocabulary.

**Integration differs (do not copy verbatim):** Raven drives the radio from its
**own MCU** as a BAP host and froze its pin-mux on the FSPI IOMUX quartet
(SCK12/MOSI11/MISO13/NSS10, BUSY14/DIO1-9/NRST8). DCENT_axe wires the SX1262
**directly to the main-board ESP32-S3** on a dedicated SPI3/HSPI bus, so its pin
map is different (above) and constrained by the stock Bitaxe pin usage. Raven
also runs **Meshtastic**; DCENT_axe ships the **custom DCENT mesh** for v1 with a
feature-gated Meshtastic-interop stub for Phase 2.

## Integration status

### Done (binary path under `feature = "lora"`)

1. ~~**HAL SPI3/HSPI bus + pin map**~~ — ✅ `dcentaxe-hal::lora_pins` (netlist-locked 9/9).
2. ~~**Own FreeRTOS task**~~ — ✅ `dcentaxe::lora_task` (fail-soft if bus init fails).
3. ~~**MCP registration**~~ — ✅ tools registered under `cfg(feature = "lora")`.
4. ~~**Dashboard surface**~~ — ✅ honesty-gated panel (no "live" claim until `present`).
5. ~~**Orthogonal `lora` Cargo feature**~~ — ✅ default-OFF; not pulled by public board features.
6. ~~**Constant-time owner-auth MAC**~~ — ✅ `src/auth.rs` HMAC-SHA256 + `ReplayGuard`.

### Still open (first-article / product)

1. **Live RF bring-up** — SPI WHOAMI, antenna TX/RX, multi-node `$DCM` relay on silicon.
2. **Enable on a board feature** (optional product decision) — e.g. fold `lora` into
   `dcent-axe-bm1397` only after RF proof + OTA size check.
3. **Region duty-cycle/dwell clamp** — EU 1% / NA dwell policy hardening.
4. **Mesh peer table + live telemetry depth** — back MCP/dashboard with long-run peer state.
5. **esp-idf-hal SPI transport NEEDS-VERIFY** on target (integration seam).

## Guardrails honored

- Standalone crate + **default-OFF** orthogonal `lora` feature on the binary.
- Stock board images do not register LoRa MCP tools or mesh UI surfaces.
- Zero new *lock* entries — `log` / `serde` / `serde_json` (dev) and the owner-auth
  `hmac` / `sha2` crates were all already in the workspace lock (via
  dcentaxe-stratum / -v2), so the `--locked` reproducibility discipline is
  preserved; `esp-idf-hal` is the existing optional transport dep.
- GPL-3.0; register magic values preserved exactly per the Semtech datasheet.
