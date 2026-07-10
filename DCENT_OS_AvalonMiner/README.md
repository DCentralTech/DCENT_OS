# DCENT_OS — Avalon (Canaan) support

**Status: in development. No mining claim yet.** This directory is a scaffold that marks Avalon as a
first-class target on the DCENT_OS roadmap. There is no production Avalon firmware here today.

## Why it's here

DCENT_OS is built as one firmware family for the whole fleet. The Antminer (`../antminer/`) and ESP32
Bitaxe-class (`../esp/`) platforms are supported and mining-proven; Avalon and WhatsMiner are the next
two families we are bringing up, and we are committing to that structure publicly rather than hiding it.

## Planned scope

- **Avalon industrial** (e.g. Avalon Q / Nano-class controllers on Canaan's newer SoCs) — daemon bring-up
  over Canaan's `mm_pkg` / AUP control protocol.
- A shared Avalon protocol core (the `mm_pkg` codec + ascset operations) reused across industrial and
  home Avalon variants.

## Honest status

- Architecture and protocol work are **scaffolded / under active reverse engineering.**
- **No accepted-share proof exists yet** for DCENT_OS on Avalon hardware — when it does, it will be
  documented with the same evidence ladder the Antminer and ESP platforms use (upload ≠ mined,
  connected ≠ mining, partial ≠ proven).
- Do not treat this directory as installable firmware.

## Get involved

Avalon hardware, protocol captures, and contributors are welcome — see the repo root
[`CONTRIBUTING.md`](../../CONTRIBUTING.md). Built by the Mining Hackers at
[D-Central Technologies](https://d-central.tech/).
