# Installing DCENT_OS

A fresh install always boots **management-only** (dashboard, SSH, and API up; hash power off) until
you explicitly enable mining, so a flash can't surprise-start a loud miner. **Always keep a
known-good recovery path before flashing.**

## 1. Build a flashable image

```bash
# Cross-compile the daemon for your miner's SoC (from the repo root):
cd dcentrald
cargo build --release --target armv7-unknown-linux-musleabihf    # Zynq  (S9 / S17 / S19 / S19j Pro)
cargo build --release --target aarch64-unknown-linux-musl        # Amlogic (S19j Pro AML / S21)

# Build the firmware image (Docker, from the repo root):
cd ..
bash scripts/build_in_docker.sh
```

## 2. Install onto the miner

There are two ways to install, depending on your hardware. **You do not need the DCENT Toolbox for
the S9 SD-boot path.**

### Option A — S9 SD-boot (no toolbox, fully reversible)

S9 hardware boots DCENT_OS straight from an SD card with **no NAND write at all** — pull the card to
revert to the stock firmware. This is the safest way to try DCENT_OS on a recoverable unit, and it
needs no companion tooling. See the per-platform notes below and [`../PLATFORMS.md`](../PLATFORMS.md).

### Option B — DCENT Toolbox (route-specific install planning)

The **DCENT Toolbox** is D-Central's separate companion CLI — a stand-alone fleet-management and
install tool published in the [D-Central GitHub org](https://github.com/DCentralTech), not part of
this repository. It auto-detects the stock firmware (BraiinsOS / VNish / stock Bitmain / LuxOS) and
shows the matching route. A route row is not a production install claim; live writes stay gated by
per-model proof, signed artifacts, recovery evidence, and explicit operator authorization:

```bash
dcent doctor <MINER_IP>
dcent support --flash-readiness --json
dcent install --list-routes
dcent install <MINER_IP> -f <signed-package> --dry-run
# Commit only from the matching per-platform runbook after gates pass.
```

> Once DCENT_OS is installed, it updates itself toolbox-free: the dashboard's **A/B sysupgrade** flow
> uploads a signed update bundle to the inactive slot and flips to it on the next boot. Failed-boot
> recovery stays platform/runbook-gated; keep a known-good serial or SD recovery path.

## 3. First boot

Open `http://<MINER_IP>/` and finish the setup wizard. Set your pool in
[`../CONFIGURATION.md`](../CONFIGURATION.md), then enable mining when you're ready.

## Per-platform notes

| Platform | Notes |
| --- | --- |
| **S9 (Zynq)** | SD-boot beta path available; see [`S9_XILINX.md`](S9_XILINX.md). Persistent NAND routes require the toolbox proof ladder. |
| **S19 Pro / S19j Pro (Zynq)** | Mining/lab evidence exists; persistent install is lab/operator-gated, not public-install-ready. See [`S19J_PRO_XILINX.md`](S19J_PRO_XILINX.md). |
| **S17 / S19 base** | Bring-up/evidence-gap or runtime-only depending on source firmware; do not advertise as production install-ready. |
| **S21 / S19k Pro / Amlogic** | aarch64 image; stock AMLCtrl in-place rootfs-window install remains blocked. VNish-AML/rootfs-window and control-board-replacement routes are lab-only and require physical recovery assumptions. |
| **S19j Pro (BeagleBone)** | AM335x IO board; SD/runtime paths, not a general NAND/sysupgrade production install. |
| **Bitaxe Max / Ultra / Supra / Gamma / Hex Ultra / Hex Supra** | ESP32-S3 OTA/USB routes require a signed DCENT_axe manifest and `DCENT_OTA_PUBLIC_KEY_HEX`; see [`ESP_DEVICES.md`](ESP_DEVICES.md). |

See [`../PLATFORMS.md`](../PLATFORMS.md) for the per-model maturity status and the universal
hash-board story, and [`../CONFIGURATION.md`](../CONFIGURATION.md) for tuning, donation, and safety
settings.

> Hardware-specific bring-up details (per-board EEPROM/dsPIC quirks, NAND layouts) are intricate and
> evolve quickly — open a **platform bring-up issue** with your captures and we'll help, and your
> findings help expand coverage.
