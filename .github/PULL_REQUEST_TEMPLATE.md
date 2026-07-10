<!--
Thanks for contributing to DCENT_OS! Please read CONTRIBUTING.md and GOVERNANCE.md first.
D-Central sets the project's direction and decides what merges upstream — opening an issue
before a non-trivial PR saves everyone time.
-->

## What this changes
<!-- A clear summary. Link the issue it addresses: "Closes #123". -->

## Why
<!-- The problem it solves and which project value it advances (safety / honesty / openness /
     home-miner UX / decentralization). -->

## Hardware impact
- [ ] This PR does **not** touch voltage, thermal, fan, PIC/dsPIC, PSU, or chain/UART code.
- [ ] This PR **does** touch hardware-critical code (it will get an extra safety review pass).

## Evidence
<!-- For behavior changes, attach reproducible results. "It compiles" is not proof a miner mines. -->
- Tests: <!-- cargo test / vitest output -->
- Real-hardware log (if applicable):

## Checklist
- [ ] `cargo fmt` + `cargo clippy` clean (Rust) / `npm run build` + `vitest` clean (dashboard)
- [ ] No load-bearing safety guard weakened or removed
- [ ] User-facing surfaces stay honest (no false "connected/mining/flashed" claims)
- [ ] New source files carry the SPDX + D-Central copyright header
- [ ] Commits are `Signed-off-by:` (DCO — see CONTRIBUTING.md)
