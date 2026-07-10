# VNish/CVitek Takeover Guardrails

`flash_vnish.sh` is a fail-closed preflight and stub. It must not upload,
erase, write, or reboot a miner.

## Known Constraints

- VNish/AnthillOS v1.2.7 exposes a common API under `/api/v1`.
- The firmware update endpoint is `POST /api/v1/firmware/update`.
- The request body is `multipart/form-data` with `file` and optional
  `keep_settings`.
- CVitek VNish packages are overlay-only in the current corpus.
- CVitek VNish targets commonly ship with SSH disabled.

## Safety Rules

- Do not infer SSH availability from a VNish banner or package name.
- Do not enable SSH on CVitek as an implicit takeover fallback.
- Do not use `scp`, `ubiupdatevol`, `flash_erase`, or `nandwrite` from this
  path.
- Do not write `mtd7` or `mtd8` unless the active slot is proven and the target
  command writes only the inactive slot.
- Do not preserve VNish settings when crossing into DCENT_OS unless a migration
  step has explicitly mapped and sanitized the config.

## HTTP Path Requirements Before Implementation

1. Authenticate through `/api/v1/unlock` or an API-key flow without storing
   vendor keys.
2. Validate the DCENT_OS package signature and board target before upload.
3. Prove the VNish firmware update handler writes an inactive or recoverable
   slot for the exact board family.
4. Pass rollback health checks equivalent to the DCENT_OS `upgrade_stage` guard.
5. Treat CVitek overlay-only packages as HTTP/SD migration targets, not SSH
   targets.

Until those are proven, the correct behavior is to report diagnostics and abort.
