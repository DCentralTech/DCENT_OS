# SV2 broad-pool test fixtures

Companion data for `tests/sv2_multi_pool.rs`.

## Why the fixtures live in code, not on disk

SV2 is a binary wire protocol layered under Noise_NX with ChaChaPoly1305
AEAD. The full session bytes — `client_e`, `server_e`, encrypted static
key, encrypted SIGNATURE_NOISE_MESSAGE, then encrypted SetupConnection,
SetupConnectionSuccess, OpenStandardMiningChannel*, NewMiningJob /
NewExtendedMiningJob, etc. — depend deterministically on:

  1. The client's ephemeral keypair (RNG seed at handshake start).
  2. The server's ephemeral keypair.
  3. The server's static keypair.
  4. Every `mix_hash(...)` call in lockstep on both sides.

A frozen byte-for-byte capture of an OCEAN or DEMAND/SRI session would
encode all four sources of entropy, so any refactor of the client RNG
seed or the noise transcript would invalidate the capture without
catching a real bug.

We chose **fixtures-from-spec** instead: the mock pool in
`src/v2/test_server.rs` runs the real Noise_NX server protocol with
**deterministic seeds**, then dispatches an SV2 frame sequence that
mirrors each pool style's published behavior. The seeds are pinned in
[`MockPoolStyle::server_e_seed`] /
[`MockPoolStyle::server_s_seed`] in `test_server.rs`:

| Pool style    | Server ephemeral seed | Server static seed |
|---------------|-----------------------|--------------------|
| OCEAN-style   | `[0xCAu8; 64]`        | `[0x71u8; 64]`     |
| DEMAND/SRI    | `[0xD3u8; 64]`        | `[0x5Au8; 64]`     |

Bumping a seed is a real change to the test's wire bytes — bump it in
the same commit that documents the reason.

## Pool-style behavior we model

### OCEAN-style (`MockPoolStyle::Ocean`)

- `OpenStandardMiningChannel` → `OpenStandardMiningChannelSuccess`
  with `extranonce_prefix` length 0 and a permissive `[0xFF; 32]`
  initial target.
- `SetNewPrevHash` arrives **before** `NewMiningJob` so the
  `future_job=false` job dispatches immediately.
- No version rolling. `JobTemplate.version` ends up at the literal
  block version `0x2000_0000` the mock pool sent.
- Standard channel → no merkle path / coinbase split — pool sends
  pre-computed merkle root directly.

### DEMAND/SRI-style (`MockPoolStyle::DemandSri`)

- `OpenExtendedMiningChannel` → `OpenExtendedMiningChannelSuccess`
  with `extranonce_size=4`, `extranonce_prefix=[0xDE, 0xAD]`,
  `group_channel_id=0`.
- `SetNewPrevHash` arrives before `NewExtendedMiningJob`, same
  ordering reason as OCEAN.
- `version_rolling_allowed=true` in the extended job.
- Extended channel → 2-deep `merkle_path`, 4-byte
  `coinbase_tx_prefix`/`coinbase_tx_suffix`. The adapter
  reconstructs the merkle root client-side using these.

## How to capture a real pool transcript instead

If you ever need to add a frozen real-pool capture (e.g. to chase a
suspected wire-format drift on a specific pool), capture it under
`tests/sv2_pool_fixtures/` as a `.bin` and a `.note.md`:

```
ocean-2026-mm-dd-handshake.bin       # raw bytes from `nc -q1 host port`
ocean-2026-mm-dd-handshake.note.md   # client seed, mining context, pool name
```

Then load it in a new `#[ignore]` test that decrypts the bytes against
the recorded client seed. Keep the deterministic-seed mock pool tests
above as the always-on CI gate; the captured-bytes tests are optional
forensics.

## Running locally

```bash
# From DCENT_OS_Antminer/dcentrald/
cargo test -p dcentrald-stratum --features mock-pool --test sv2_multi_pool
```

CI runs the same command on every PR via the `sv2-mock-pool-tests`
cell of `.github/workflows/cross-compile-matrix.yml`.
