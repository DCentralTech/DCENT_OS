# DCENT_OS Install Notes — Antminer S19j Pro Xilinx

**Current status (2026-07-09):**

| Layer | Status |
|---|---|
| Mining / driver (lab) | Evidence exists (internal); code tier remains Experimental |
| **Signed sysupgrade artifact** | **Present** (`DCENTOS_XIL3_S19jPro_beta20260617.tar`) |
| **Toolbox live install** | **Blocked by Missing Artifact (live)** until a **clean** unit + restore_action + capstone (`a lab unit` degraded HW is not GO) |
| **AM2 SD production** | **Public Beta Experimental** host complete+signed under `releases/am2-sd/…` (CE-410 complete); **cold-boot unproven** → not Supported; not a NAND installer |

**Preferred method:** DCENT_Toolbox (not SD). SD is recovery/lab only until the
CE-410 checklist is closed.

See the install-path overview in [`README.md`](README.md) for the per-method readiness table.

## Before any install attempt

Confirm this is the **Xilinx AM2** carrier, not BeagleBone, Amlogic, or CVitek:

```bash
dcent doctor <MINER_IP>
dcent fingerprint <MINER_IP>
dcent support --flash-readiness
dcent install --list-routes --explain <MINER_IP>
```

Do not force an am2 route onto BB / Amlogic / CVitek. Stock Xilinx units are
signposted to convert to BraiinsOS first — there is no silent one-step stock flash.

## Lab / operator-gated Toolbox path

1. Full-NAND backup with `readback_verified=true` (mtd4 + mtd7 + mtd8). 
2. `dcent backup am2-restore-action` until `restore_action_proof.verified=true`. 
3. Signed package only — verify Ed25519 against the release pin. 
4. Dry-run plan; then write only with required lab flags + `--yes`.

```bash
dcent install <MINER_IP> -f <signed-package> --dry-run --json
```

**Honesty:** AM2 `TARGET_SYSUPGRADE` success is **`upload_accepted`** (inactive
slot written). The tool does **not** reboot the unit for you — power-cycle, then
re-detect. Automatic failed-boot rollback is **not** guaranteed; hold serial and
recovery media.

Historical lab success on named units is lab evidence, not a blanket
“any S19j Pro” install claim.
