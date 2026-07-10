# Media

Dashboard screenshots live here. The dashboard has three modes — **Space Heater**, **Mining**,
and **Hacker** — captured against the D-Central Orange (`#FAA500`) design system.

| File | Shows |
| --- | --- |
| `dashboard-heater.png` | Space Heater mode — room-temp thermostat, BTU/h output, "earning sats while heating" ledger, quiet-first presets (Boost / Away / Quiet) |
| `dashboard-standard.png` | Mining mode — live hashrate, silicon telemetry, current-block hero, shares and pool state |
| `companion-chat.png` | Companion chat — ask "my miner is noisy" and it offers to enable quiet mode (local-LLM, takes real actions with explicit Run-it consent) |
| `autotuner-tune-by-priority.png` | Autotuner "Tune by Priority" — power / hashrate / fan / heat / efficiency modes + honest before→after predicted estimate |

> All captures come from the dashboard running against the mock preview server
> (`dashboard/scripts/preview-with-mocks.py`) — sample data only; no real wallet, worker, or
> operator details are shown. A Hacker-mode capture will be added once its
> preview-render issue is resolved.
