# DCENT_OS Install Notes — Antminer S19j Pro Xilinx

**Current status (2026-07-23):**

| Layer | Status |
|---|---|
| Mining / driver (lab) | Evidence exists (internal); code tier remains Experimental |
| **Signed sysupgrade artifact** | **Present** (`DCENTOS_XIL3_S19jPro_beta20260617.tar`) |
| **Toolbox self-update** | Guarded DCENT_OS-to-DCENT_OS `TARGET_SYSUPGRADE`; exact restore evidence + recovery acknowledgement required |
| **Vendor-source first install** | **Evidence gap**; no authenticated first-install capsule or executable write method |
| **AM2 SD production** | **Public Beta Experimental** host complete+signed under `releases/am2-sd/…` (CE-410 complete); **cold-boot unproven** → not Supported; not a NAND installer |

**Preferred self-update method:** DCENT_Toolbox (not SD). There is no supported
vendor-source first-install method. SD is recovery/lab only until exact-target
cold-boot proof exists.

See the install-path overview in [`README.md`](README.md) for the per-method readiness table.

## Before any install attempt

Confirm this is the **Xilinx AM2** carrier, not BeagleBone, Amlogic, or CVitek:

```bash
dcent doctor <MINER_IP>
dcent fingerprint <MINER_IP>
dcent support --flash-readiness
dcent install --list-routes --explain <MINER_IP>
```

Do not force an am2 route onto BB / Amlogic / CVitek. Stock, BraiinsOS, LuxOS,
and VNish AM2 sources remain plan-visible evidence gaps; converting source
firmware does not create a DCENT_OS first-install capsule.

## Guarded DCENT_OS-source self-update

1. Full-NAND backup with `readback_verified=true` (mtd4 + mtd7 + mtd8). 
2. `dcent backup am2-restore-action` until `restore_action_proof.verified=true`. 
3. Signed package only — verify Ed25519 against the release pin. 
4. Confirm the detected source firmware is already DCENT_OS.
5. Dry-run the complete gated command; add `--yes` only after reviewing it.

```bash
dcent install <MINER_IP> \
  -f <signed-package> \
  --artifact-dir <restore_verified_dir> \
  --accept-am2-persistent-lab \
  --i-have-recovery \
  --dry-run \
  --json
```

**Honesty:** AM2 `TARGET_SYSUPGRADE` success is **`upload_accepted`** (inactive
slot written). The tool does **not** reboot the unit for you — power-cycle, then
re-detect. Automatic failed-boot rollback is **not** guaranteed; hold serial and
recovery media.

The same command against a vendor-source AM2 target must remain an evidence-gap
plan with no install method and no persistent write. Historical lab success on
named units is not a blanket “any S19j Pro” first-install claim.
