# DCENT_OS Install Notes — Antminer S9 Xilinx

**Current status (2026-06-26):** S9 is the safest public trial target because
the SD-boot path is reversible and does not write NAND. Persistent NAND install
routes must still be selected through DCENT Toolbox and its proof ladder.

## Recommended path: SD boot

1. Build or obtain the board-specific S9 image.
2. Write it to an SD card.
3. Boot the S9 control board with the SD-card jumper set.
4. Confirm the dashboard and SSH come up management-only before enabling mining.

Pulling the SD card returns the unit to its previous NAND firmware. Keep stock
recovery media nearby before any live test.

## Toolbox path

Use the toolbox to inspect the exact unit before any persistent action:

```bash
dcent doctor <MINER_IP>
dcent support --flash-readiness
dcent install --list-routes
dcent install <MINER_IP> -f <signed-package> --dry-run
```

A dry-run route is not a production success. Live writes require signed
artifacts, recovery evidence, explicit `--yes`, and post-boot verification.
