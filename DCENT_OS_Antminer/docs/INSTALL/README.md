# Installing DCENT_OS

A fresh install always boots **management-only** (dashboard, SSH, and API up; hash power off) until
you explicitly enable mining, so a flash can't surprise-start a loud miner. **Always keep a
known-good recovery path before flashing.**

> **Public beta (not production-stable).** No model is `stable`. Install readiness is
> **path-specific** and separate from mining/driver readiness — see the per-method
> table below and the per-model notes for the honest per-path status.

## Preferred methods (priority order)

| Priority | Method | When to use | Honest readiness today |
| --- | --- | --- | --- |
| **1** | **SD card** | Reversible try + physical recovery | **S9 SD-boot try** Supported (keep card; no NAND). **AM2 SD** Public Beta Experimental — host complete+signed under `releases/am2-sd/…`; cold-boot open (not Supported; not NAND install). |
| **2** | **DCENT_Toolbox** | Detect, dry-run, persistent NAND/eMMC, recovery | **S9** persistent Supported (operator-gated capstone). **S19j Pro Zynq** guarded lab / live Blocked until clean unit. Everything else experimental, detect-only, or lab-flagged. |

## Public beta warning (read before flashing)

- Supported **beta install anchors:** Antminer **S9 (Xilinx)** and **S19j Pro (Xilinx)** only.
- Fingerprint the **control board** before any flash (S19j Pro exists as Zynq, BB, Amlogic, CV).
- **Automatic failed-boot rollback is not guaranteed** — keep serial and SD or a verified full-NAND restore path.
- On AM2, complete full-NAND backup + `restore_action_proof.verified=true` before install.
- Install only **signed** packages from the checksum ledger.
- Degraded EEPROM/dsPIC units (including `a lab unit`-class) are refused for public-beta GO evidence.
- Home safety: quiet fan profile (~PWM 30); cut hash before noise.

## 1. Build or obtain a flashable image

```bash
# The only admitted source build today is the exact S9 release capsule.
make -C DCENT_OS_Antminer release RELEASE_TARGET=s9
```

Do not invoke `build-dcentrald.sh` plus `build_in_docker.sh` as a packaging
shortcut. The inner driver now requires an authenticated release invocation and
fails closed when called directly. AM2, Amlogic, BeagleBone, CVitek, and S17
source packaging remains blocked until each lane has the same capsule lifecycle;
use only an already approved signed artifact whose target-specific evidence has
been independently verified.

Or use a **release-signed** sysupgrade tarball (local beta set under
`DCENT_OS_Antminer/output/beta-xil-20260617/` for S9 + S19j Pro XIL). Public HTTPS
publication remains operator-gated.

## 2. Install onto the miner

### Option A — S9 SD-boot try (no toolbox, fully reversible)

S9 hardware boots DCENT_OS straight from an SD card with **no NAND write at all** — pull the card to
revert to the prior firmware. Keep the card inserted while using DCENT_OS. See
[`S9_XILINX.md`](S9_XILINX.md) and [`../qa/S9_PRIVATE_BETA_SD_RELEASE.md`](../qa/S9_PRIVATE_BETA_SD_RELEASE.md).

This is **not** “SD install to NAND.” Permanent S9 NAND uses Option B.

### Option B — DCENT Toolbox (detect → plan → gated write)

The **DCENT Toolbox** auto-detects stock firmware (BraiinsOS / VNish / stock Bitmain / LuxOS) and
shows the matching route. A route row is **not** a production install claim; live writes stay gated
by per-model proof, signed artifacts, recovery evidence, and explicit operator authorization:

```bash
dcent doctor <MINER_IP>
dcent support --flash-readiness --json
dcent install --list-routes
dcent install --list-routes --explain <MINER_IP>
dcent install <MINER_IP> -f <signed-package> --dry-run
# Commit only after dry-run is unblocked and recovery evidence exists.
```

**S19j Pro Xilinx:** Toolbox is the primary path (AM2 production SD is not customer-ready). Success
on AM2 sysupgrade is often **`upload_accepted`** — power-cycle to boot the new slot; do not assume
auto-reboot. See [`S19J_PRO_XILINX.md`](S19J_PRO_XILINX.md).

> Once DCENT_OS is installed, updates can use A/B sysupgrade with a signed bundle. Failed-boot
> recovery stays platform/runbook-gated; keep serial or SD recovery media.

## 3. First boot

Open `http://<MINER_IP>/` and finish the setup wizard. Set your pool in
[`../CONFIGURATION.md`](../CONFIGURATION.md), then enable mining when you're ready.

## Per-platform notes

| Platform | Notes |
| --- | --- |
| **S9 (Zynq)** | SD-boot try Supported; see [`S9_XILINX.md`](S9_XILINX.md). Persistent NAND via Toolbox + signed package. |
| **S19j Pro (Zynq)** | Signed artifact exists; live install operator/lab-gated; **not** “any unit GO”. Multi-board: fingerprint first. See [`S19J_PRO_XILINX.md`](S19J_PRO_XILINX.md). |
| **S19 Pro (Zynq)** | Experimental runtime / guarded lab; no public-beta signed install package in XIL beta set. |
| **S17 / S19 base** | Runtime-only or evidence-gap; do not advertise as production install-ready. |
| **S21 / S19k Pro / Amlogic** | aarch64; no public SD path; rootfs-window install is lab-only (restore_verified). |
| **S19j Pro (BeagleBone)** | SD/runtime lab; not general NAND production install. |
| **S11 / S15 / S23** | Detect-only or do-not-flash. |
| **Bitaxe Max / Ultra / Supra / Gamma / Hex\*** | ESP OTA/USB require signed manifest; ESP OTA pubkey residual (B-T-1); see [`ESP_DEVICES.md`](ESP_DEVICES.md). |

See [`../PLATFORMS.md`](../PLATFORMS.md) for per-model maturity and
[`../CONFIGURATION.md`](../CONFIGURATION.md) for tuning and safety settings.

> Hardware-specific bring-up details (per-board EEPROM/dsPIC quirks, NAND layouts) are intricate and
> evolve quickly — open a **platform bring-up issue** with your captures and we'll help, and your
> findings help expand coverage.
