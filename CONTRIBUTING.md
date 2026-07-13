# Contributing to DCENT_OS

Thanks for your interest in DCENT_OS! This firmware controls real voltage, heat, and hash power on
real hardware, so we hold contributions to a high bar — especially anything that touches the
hardware. This guide explains how to contribute effectively.

> **First, read [`GOVERNANCE.md`](GOVERNANCE.md).** DCENT_OS's direction is set solely by
> D-Central Technologies; we decide what merges upstream and reserve the right to accept or decline
> any contribution. The GPL guarantees your freedom to fork — if our direction isn't yours, that's
> a welcome and supported path.

## Ways to help

- **Bug reports** — open a GitHub Issue using the bug template. Real-hardware logs are gold.
- **Platform bring-up** — using DCENT_OS on a miner we mark as "bring-up"? Your captures, logs, and
  findings are extremely valuable. Use the platform-bring-up issue template.
- **Documentation** — clarity fixes, install-guide improvements, and FAQ additions are always welcome.
- **Code** — bug fixes and features that advance the project's [values](GOVERNANCE.md#what-we-optimize-for-the-projects-values).

## Before you open a pull request

1. **Open an issue first** for anything non-trivial, so we can confirm it's a fit before you invest
   time. We'd hate for you to build something we then have to decline.
2. **Keep changes focused.** One logical change per PR. Small, reviewable diffs merge faster.
3. **Don't regress safety.** DCENT_OS has load-bearing safety guards (fan-PWM caps for home/quiet
   operation, EEPROM write-protection, voltage clamps, cut-hash-before-noise teardown, feature-gated
   recovery tooling). Do not weaken or remove these. If your change touches voltage, thermal, fan,
   PIC/dsPIC, or PSU code, say so explicitly in the PR — it gets an extra review pass.
4. **Bring evidence for hardware changes.** A change that affects mining behavior should include a
   reproducible log from real hardware where you can (chip enumeration, accepted shares, thermal
   readings). "It compiles" is not proof a miner still mines.
5. **Be honest in user-facing surfaces.** The dashboard and APIs must never claim a state that
   isn't true (e.g. "connected" ≠ "mining", "scheduled" ≠ "flashed", pool-target ≠ achieved
   difficulty). Truthfulness is a hard rule.
6. **Prefer decade-scale architecture.** Read [`docs/architecture/`](docs/architecture/README.md)
   before adding a platform path. **Do not** add a new full `*_mining.rs` engine (ADR-0009). Prefer
   composition facets (ASIC / board / power / cooling / storage / network). Prefer config/markers
   over a new product `DCENT_*` env var (ADR-0012). Prefer transport-neutral ASIC helpers over new
   `FpgaChain`-only APIs (ADR-0010).

## Development setup

> See [`DCENT_OS_Antminer/DEVELOPMENT.md`](DCENT_OS_Antminer/DEVELOPMENT.md) for a deeper architecture orientation (the crate map, how
> the daemon reaches the FPGA, and the safety architecture).


```bash
# Rust mining daemon (cross-compile for the miner's SoC)
cd DCENT_OS_Antminer/dcentrald
cargo build --release --target armv7-unknown-linux-musleabihf   # Zynq (S9/S17/S19)
# or:  ...--target aarch64-unknown-linux-musl                    # Amlogic (S19j Pro/S21)
cargo fmt --all && cargo clippy --all-targets
cargo test                                                       # host-side unit/integration tests

# Dashboard
cd DCENT_OS_Antminer/dashboard
npm install
npm run build       # tsc + vite + size/i18n guards
npx vitest run      # unit tests
```

Coding standards live in the per-area rules and are summarized here:

- **Rust:** memory-safe, async (`tokio`), `tracing` for logs (never `println!`), no `unsafe` without
  a comment explaining why. Hardware constants must cite their source (live probe / datasheet / RE).
- **Comments:** write for an outside contributor. Explain *why*, not just *what*. Avoid private
  jargon — if you must reference a hardware quirk, explain it inline.
- **Dashboard:** React + TypeScript, local-first (no CDN/Google-Fonts/outbound calls), mobile-aware,
  and within the compiled-size budget.
- **Attribution:** new source files carry the standard header
  (`SPDX-License-Identifier: GPL-3.0-or-later` + the D-Central copyright line).

## Sign-off (DCO)

By submitting a pull request, you certify the
[Developer Certificate of Origin](https://developercertificate.org/) — i.e. you wrote the code (or
have the right to submit it) and agree to license it under GPL-3.0. Add a `Signed-off-by:` line to
your commits (`git commit -s`). You retain copyright to your contribution; it is licensed to the
project under GPL-3.0.

## Review

Maintainers review for correctness, safety, honesty, and fit with the project's direction.
Hardware-touching changes go through D-Central's expert review process. We may ask for changes, or
we may decline a PR that doesn't fit DCENT_OS's direction — see [`GOVERNANCE.md`](GOVERNANCE.md).

## Security

Found an auth bypass, credential leak, OTA-signing flaw, or unsafe recovery path? **Do not open a
public issue.** Follow [`SECURITY.md`](SECURITY.md) and email **security@d-central.tech**.

---

*Thanks for helping turn industrial miners into home space heaters the open way.*
— The Mining Hackers at [D-Central Technologies](https://d-central.tech/)
