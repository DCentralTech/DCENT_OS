# DCENT_OS Install Notes — Antminer S19j Pro Xilinx

**Current status (2026-06-26):** Xilinx S19j Pro has lab/mining evidence, but
the public beta gate is not a final GO. Persistent flash remains
operator-gated and should be treated as a lab route until public artifact
verification and witnessed live-install capstone evidence are complete.

## Before any install attempt

Confirm this is the Xilinx am2 carrier, not BeagleBone, Amlogic, or CVitek:

```bash
dcent doctor <MINER_IP>
dcent fingerprint <MINER_IP>
dcent support --flash-readiness
dcent install --list-routes --explain <MINER_IP>
```

The toolbox route must match the detected control board and source firmware.
Do not force an am2 route onto BeagleBone, Amlogic, or CVitek hardware.

## Lab/operator-gated path

Use only signed, board-targeted artifacts and keep restore-verified recovery
evidence in the same session:

```bash
dcent install <MINER_IP> -f <signed-package> --dry-run --json
```

Proceeding beyond dry-run requires explicit operator authorization and a
recovery plan. Historical lab-unit evidence is lab evidence, not a
blanket production install claim.
