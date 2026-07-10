# DCENT_OS for ESP Devices

DCENT_OS for ESP is the ESP32-S3 / Bitaxe-class member of the DCENT_OS family. The product identity remains **DCENT_axe** and the binary/crate naming remains `dcentaxe`; the firmware is part of the same DCENT_OS family as the Antminer `dcentrald` stack.

In the monorepo source tree, the ESP workspace lives at `DCENT_OS_ESP/`. This public DCENT_OS export includes this overview so the Antminer and ESP support story is self-contained even when the ESP source is published or packaged separately.

## Public Targets

| Target | ASIC | Package boardTarget | Current status |
| --- | --- | --- | --- |
| Bitaxe Max | BM1397 | `bitaxe-max` | Host-tested driver path; D-Central BM1397 first-article hardware bring-up pending |
| Bitaxe Ultra | BM1366 | `bitaxe-ultra` | Host-tested driver path |
| Bitaxe Supra | BM1368 | `bitaxe-supra` | Host-tested driver path |
| Bitaxe Gamma | BM1370 | `bitaxe-gamma` | Host-tested driver path; Gamma live-verified |
| Bitaxe Hex Ultra | 6x BM1366 | `bitaxe-hex-ultra` | Hex dispatcher host-tested; live soak pending |
| Bitaxe Hex Supra | 6x BM1368 | `bitaxe-hex-supra` | Host-tested hex dispatcher path, including BM1368 job-id recovery and internal 3.7 TH/s lab evidence |

Legacy BM1370 lab evidence exists for internal dual-chip work, but it is not a public install target and must not be treated as Gamma.

LoRa mesh is planned/default-OFF and is not included in the shipping binary route.

## Install Boundary

DCENT_OS for ESP uses two package rails:

- OTA update: `dcent install <ip> -f <board>-update.bin --dry-run` or `dcent ota update <ip> -f <board>-update.bin --dry-run`
- USB factory flash: `dcent flash --serial <port> -f <board>-factory.bin --dry-run`

Both rails require a companion signed DCENT_axe manifest and `DCENT_OTA_PUBLIC_KEY_HEX`. The manifest must match `boardTarget`, `deviceModel`, payload SHA-256, and OTA slot fit before upload or USB write.

Upload/image-write acceptance is not boot, rollback, thermal, or mining proof. Use the per-platform install guide before any live write: [`INSTALL/ESP_DEVICES.md`](INSTALL/ESP_DEVICES.md).
