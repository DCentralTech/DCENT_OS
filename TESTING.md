# Testing DCENT_OS — from zero hardware to a live miner

You don't need an Antminer to start kicking the tires. This guide is ordered from
**no hardware at all** to **reversible live trials** to **persistent installs**.
Every step tells you what it proves and how to undo it.

> **Golden rule:** always keep a known-good recovery path (SD card, stock/BraiinsOS
> slot, or a full NAND backup) before writing anything persistent to a miner.

---

## Tier 0 — No hardware at all

### Build and test the mining daemon

The `dcentrald` daemon workspace is host-testable on any Linux (or Windows WSL):

```bash
cd DCENT_OS_Antminer/dcentrald
rustup toolchain install 1.90.0
cargo +1.90.0 check --workspace --locked   # type-check everything
cargo +1.90.0 test  --workspace --locked   # run the test suite
```

This is the same suite our CI runs. It exercises the Stratum V1/V2 clients (against
in-process mock pools), share validation, BIP 320 version-rolling reconstruction,
ASIC frame codecs, autotuner logic, and the API contracts — all without hardware.

### Drive the dashboard with mock telemetry

The dashboard runs standalone with ~70 mocked API endpoints and realistic data:

```bash
cd DCENT_OS_Antminer/dashboard
npm ci
npm run build
python3 scripts/preview-with-mocks.py --port 4173
# open http://127.0.0.1:4173 — try all three modes: Space Heater / Mining / Hacker
```

You'll see the full product UI — the autotuner panel, the shares table with
pool-target vs achieved difficulty, the Companion chat, the Hacker-mode tools —
rendered from mock data, no miner required.

### Cross-compile the real firmware binary

```bash
cd DCENT_OS_Antminer/dcentrald
cargo build --release --target armv7-unknown-linux-musleabihf    # Zynq (S9/S17/S19)
```

A successful build proves the published tree is complete and standalone.

---

## Tier 1 — Bitaxe-class hardware (lowest-cost live test)

A Bitaxe is the cheapest way to run DCENT_OS on real silicon. The ESP32 firmware
lives in [`DCENT_OS_ESP/`](DCENT_OS_ESP/README.md):

1. Install the esp-rs toolchain (see `DCENT_OS_ESP/README.md` → Building).
2. Build and flash over USB with `espflash`.
3. Join the device AP, run the first-run wizard (pool, worker, safety acknowledgement).
4. Watch shares arrive on your pool's worker page.

**Undo:** reflash stock ESP-Miner with `esptool`/Bitaxe web flasher at any time —
the flash is fully rewritable over USB; nothing is permanent.

---

## Tier 2 — Antminer, reversible `/tmp` trial (no persistent write)

On a supported Antminer already running BraiinsOS (or another SSH-accessible
firmware), you can trial the DCENT_OS daemon **without flashing anything**:

1. Cross-compile `dcentrald` (Tier 0) or take a signed release binary.
2. Copy it to the miner's `/tmp` (a RAM filesystem) over SSH/SFTP.
3. Stop the incumbent miner process and launch `dcentrald` with a config pointing
   at your pool. Watch the log for chip enumeration and accepted shares.
4. **Undo:** reboot. `/tmp` is RAM — the incumbent firmware boots untouched.

Per-platform launch details, known-good configs, and safety notes are in
[`DCENT_OS_Antminer/docs/PLATFORMS.md`](DCENT_OS_Antminer/docs/PLATFORMS.md) and
the install guides under [`DCENT_OS_Antminer/docs/INSTALL/`](DCENT_OS_Antminer/docs/INSTALL/).
Respect the per-model readiness matrix: rows marked *bring-up* are for
experimenters who read logs, not for unattended operation.

---

## Tier 3 — Persistent install (when your model's route is ready)

Persistent installs go through [DCENT_Toolbox](https://github.com/DCentralTech/DCENT_Toolbox),
which enforces the safety gates (backup-first, degraded-hardware refusal, dry-run
by default, signed-package verification):

```bash
pip install dcent-toolbox
dcent install --dry-run <MINER_IP> -f <signed-package>.tar   # preflight only
```

The dry run tells you exactly what would be written and which gates pass or fail
for your unit — run it freely; it writes nothing. Fresh DCENT_OS installs boot
**management-only** (dashboard/SSH/API up, hash power off) until you explicitly
enable mining, so a flash never surprise-starts a loud miner.

### Verify what you flash

Every signed release ships a SHA256 manifest and an Ed25519 signature. Verify
before flashing; the Toolbox does this automatically and fails closed on mismatch.

---

## Reporting results

Whether it worked or not, we want to hear it:

- **It mined** — open a GitHub issue with your model, board family, and pool
  screenshot (redact your wallet), or use the platform-bring-up issue template.
- **It didn't** — same template; attach the daemon log. Bring-up rows get promoted
  by exactly this kind of community evidence.
- **Security issue** — see [SECURITY.md](SECURITY.md); please don't open a public issue.
