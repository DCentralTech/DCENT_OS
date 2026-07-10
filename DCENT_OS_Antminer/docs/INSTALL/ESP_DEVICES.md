# Installing DCENT_OS for ESP Devices

This guide applies to the public DCENT_OS-for-ESP targets: Bitaxe Max, Ultra, Supra, Gamma, Hex Ultra, and Hex Supra.

DCENT_OS for ESP packages are route-gated. Do not flash a bare `.bin`. A valid package set includes:

- `<board>-update.bin` for OTA update on a running AxeOS/DCENT_axe device
- `<board>-factory.bin` for first USB serial flash
- `<board>-manifest.json` as the signed companion manifest
- `DCENT_OTA_PUBLIC_KEY_HEX` set to the release public key used to verify the manifest signatures

## OTA Update

Use OTA only on a running ESP-Miner/AxeOS/DCENT_axe device that answers `/api/system/info`.

```bash
dcent install <BITAXE_IP> -f dcentaxe-bitaxe-gamma-<version>-update.bin --dry-run
dcent ota update <BITAXE_IP> -f dcentaxe-bitaxe-gamma-<version>-update.bin --dry-run
```

The Toolbox detects Bitaxe-class devices and routes `dcent install` to the OTA flow. The route refuses unsupported internal targets and refuses packages whose signed manifest does not match the board target and device model.

Commit a live OTA only after the dry-run passes and the exact release notes call out the target as flash-ready.

```bash
dcent ota update <BITAXE_IP> -f dcentaxe-bitaxe-gamma-<version>-update.bin --yes --wait
```

## USB Factory Flash

Use USB factory flash for first install or recovery on a directly attached ESP32-S3 board.

```bash
dcent flash --serial <PORT> -f dcentaxe-bitaxe-gamma-<version>-factory.bin --dry-run
```

Commit only with the correct signed factory image for the board in front of you.

```bash
dcent flash --serial <PORT> -f dcentaxe-bitaxe-gamma-<version>-factory.bin --yes
```

## Current Gates

| Target | Status |
| --- | --- |
| Max / Ultra / Supra / Gamma | Driver proven and host-tested; Gamma is live-verified |
| Hex Ultra | Dispatcher host-tested; live soak pending |
| Hex Supra | Dispatcher/job-id recovery proven with 3.7 TH/s documented throughput |
| LoRa mesh | Scaffolded and default-OFF; not wired into the binary |

Upload or image-write success is not the same as boot, rollback, thermal, or mining proof. Keep recovery access available and do not promote a target beyond the release notes and `dcent support --flash-readiness` output.
