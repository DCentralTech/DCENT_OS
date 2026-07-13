# DCENT_OS Install Notes — Antminer S9 Xilinx

**Current status (2026-07-09):** S9 (`am1-s9`) is the safest public **trial**
target because the **SD-boot** path is reversible and does not write NAND.
That is **SD-boot only** — not “SD install to NAND.” Persistent NAND still
goes through DCENT Toolbox + a signed package + the proof ladder.

See the install-path overview in [`README.md`](README.md) for the per-method readiness table.

## Preferred path #1: SD-boot try (keep the card)

1. Build or obtain the board-specific S9 image ( packager:
   `scripts/package_private_beta_sd.sh` / see `docs/qa/S9_PRIVATE_BETA_SD_RELEASE.md`).
2. Write the `.img` with balenaEtcher (or `write_sd_card.sh`).
3. Boot with JP4 SD position **or** stock Bitmain `uEnv.txt` override path.
4. Confirm dashboard and SSH come up **management-only** before enabling mining.
5. Leave the SD card inserted for the entire trial.

Pulling the SD card returns the unit to its previous NAND firmware. Keep stock
recovery media nearby before any live test.

## Preferred path #2: Toolbox persistent NAND

Use Toolbox before any permanent action. Confirm rootfs fits the UBI volume
(oversized images fail dry-run — do not force).

```bash
dcent doctor <MINER_IP>
dcent support --flash-readiness
dcent install --list-routes --explain <MINER_IP>
dcent install <MINER_IP> -f <signed-package> --dry-run
```

Local signed beta package example:
`DCENT_OS_Antminer/output/beta-xil-20260617/DCENTOS_XIL1_S9_beta20260617.tar`

A dry-run is not a production success. Live writes require signed artifacts,
recovery evidence, explicit `--yes`, and post-boot verification. Public HTTPS
publish + witnessed capstone remain operator-gated.
