# DCENT_OS vs. Braiins OS+, LuxOS, and VNish

There are several good firmware options for Antminer hardware. We respect all of them — Braiins,
Luxor, and VNish each do excellent work. DCENT_OS makes a different set of choices, optimized for
**home miners** rather than warehouse operators. Here's the honest breakdown so you can choose what
fits you.

## Feature comparison

| | **DCENT_OS** | Braiins OS+ | LuxOS | VNish |
| --- | --- | --- | --- | --- |
| **License** | Open source (GPL-3.0) | Partially open | Closed | Closed |
| **Mandatory fee** | **None** (voluntary donation, 0% valid) | 2% pool fee | ~2.8% | % hashrate skim |
| **License server / lock-in** | None | — | — | — |
| **Source you can audit & fork** | ✅ all of it | ⚠️ some | ❌ | ❌ |
| **Hash-board auto-detect (ChipID)** | ✅ lab-validated for mixed rigs | ❌ | ❌ | ❌ |
| **PSU bypass / smart-PSU independence** | ✅ 3 modes on proven lanes | ⚠️ limited | ❌ | ❌ |
| **120 V household power** | ✅ | ⚠️ | ⚠️ (recent) | ⚠️ |
| **Home / space-heater UX** | ✅ first-class (BTU/h, quiet-first) | ⚠️ | ❌ | ❌ |
| **Memory-safe (Rust) daemon** | ✅ | ✅ | — | — |
| **Local-first dashboard (no cloud)** | ✅ | ⚠️ optional cloud | ⚠️ | ⚠️ |
| **Stratum V2** | ⚠️ implemented / default-off; submit/accept soak pending | ✅ (mature) | ✅ | ✅ |
| **Autotuner** | ✅ runtime curves | ✅ | ✅ | ✅ (aggressive OC) |
| **A/B sysupgrade + signed OTA** | ✅ | ✅ | ✅ | ✅ |
| **pyasic / CGMiner-API compatible** | ✅ | ✅ | ✅ | ✅ |

## Where each one shines

- **Braiins OS+** — the most mature open(-ish) firmware, with first-class native Stratum V2 and a
  long production track record at scale. If you run a large fleet on Braiins Pool, it's excellent.
- **LuxOS** — strong per-chip tuning, enterprise/compliance focus (SOC 2), tight Luxor-pool
  integration.
- **VNish** — the most aggressive overclocking and the widest multi-brand support; popular with
  performance-maximizing operators.
- **DCENT_OS** — open, fee-free, avoids smart-PSU lock-in on supported lanes, auto-detects
  supported hash boards, and is built home-first as a space heater. If you have a used miner in
  your house and you want it quiet, efficient, fully yours, and free — DCENT_OS is built for
  exactly that.

## Honest caveats

- DCENT_OS prioritizes **home efficiency and safety over raw maximum hashrate**. If your goal is the
  highest possible overclock on a well-cooled warehouse unit, a performance-tuned closed firmware
  may push further.
- Coverage of the full fleet is continuously expanding (see [`PLATFORMS.md`](PLATFORMS.md)); some
  models are still in bring-up.

## The bottom line

No competitor occupies the same corner DCENT_OS does: **open-source + zero mandatory fee +
supported hardware flexibility + space-heater-first + memory-safe.** That combination is the whole reason
DCENT_OS exists.
