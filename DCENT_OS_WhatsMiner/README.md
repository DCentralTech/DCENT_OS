# DCENT_OS — WhatsMiner (MicroBT) support

**Status: in development. No mining claim yet.** This directory is a scaffold that marks WhatsMiner as a
first-class target on the DCENT_OS roadmap. There is no production WhatsMiner firmware here today.

## Why it's here

DCENT_OS is built as one firmware family for the whole fleet. The Antminer (`../antminer/`) and ESP32
Bitaxe-class (`../esp/`) platforms are supported and mining-proven; WhatsMiner and Avalon are the next two
families we are bringing up, and we commit to that structure publicly rather than hiding it.

## Planned scope

- **WhatsMiner M-series** (e.g. M60S-class, Allwinner H616 / aarch64 control boards) — daemon bring-up
  over MicroBT's control-board interfaces.
- Reuse of the shared DCENT_OS daemon core (HAL abstraction, Stratum, autotuner, thermal) behind a
  WhatsMiner platform implementation.

## Honest status

- Reverse engineering and bring-up are **underway**; this is not installable firmware.
- **No accepted-share proof exists yet** for DCENT_OS on WhatsMiner hardware — when it does, it will be
  documented with the same evidence ladder the Antminer and ESP platforms use.
- Do not treat this directory as installable firmware.

## Get involved

WhatsMiner hardware, control-board captures, and contributors are welcome — see the repo root
[`CONTRIBUTING.md`](../../CONTRIBUTING.md). Built by the Mining Hackers at
[D-Central Technologies](https://d-central.tech/).
