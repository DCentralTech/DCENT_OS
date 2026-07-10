# ESP-Miner upstream fixture

This directory pins the upstream Bitaxe ESP-Miner board-version table used by
`dcentaxe-hal` parity tests.

- Source repository: https://github.com/bitaxeorg/ESP-Miner
- Source file: `main/device_config.h`
- Upstream commit: `b4c3dcbb9ed36c2a0eb9ae7d57a4132e8c52c14b`
- Upstream commit date: 2026-07-03
- Retrieved: 2026-07-04
- License: GPL-3.0-or-later, matching this firmware tree; see the upstream
  repository `LICENSE`.

Update this fixture only by copying the upstream file from a named ESP-Miner
commit and recording the new commit metadata here and in
`fixture_manifest.json`. CI must validate this local fixture without live
network fetches; operators do the upstream comparison before public ESP release.
