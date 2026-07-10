# Configuration

DCENT_OS is configured through a single TOML file. On a running miner it lives at
**`/data/dcentrald.toml`** (a read-only baked default ships in the image; the `/data` copy
overrides it and survives reboots and A/B upgrades). Edit it via the dashboard, the REST API, or
directly over SSH, then restart the daemon (`/etc/init.d/S82dcentrald restart`).

## Key sections

```toml
[mining]
enabled        = false   # fresh installs boot management-only until you enable mining
frequency_mhz  = 525     # per-platform; the autotuner adjusts within safe bounds

[[pool]]                 # your pool (you keep 100% of this hashtime)
url      = "stratum+tcp://your.pool:3333"
worker   = "your-btc-address-or-account"
password = "x"

[donation]               # optional, transparent, fully disableable
enabled       = true
percent       = 2.0      # 0.0–5.0; set 0 or enabled=false to turn it off
pool_url      = "stratum+tcp://pool.d-central.tech:3333"   # DCENT_Pool (Solo/Guild)
worker        = "DungeonMaster"
fallback_pool_url = "stratum+tcp://stratum.braiins.com:3333"  # used only if the primary is down
fallback_worker   = "DungeonMaster"
cycle_duration_s  = 3600 # at 2%, that's ~72s of donation per hour

[thermal]
fan_max_pwm = 30         # home/quiet cap; the daemon cuts hash before exceeding this

[power.psu_override]      # supported PSU-bypass lane; no smart-PSU/Loki when the platform matrix allows it
enabled  = false
model    = "APW3"
voltage_v = 12.8
```

## Donation

The donation is a voluntary, transparent slice of hashtime — never a hidden dev fee. It routes to
**DCENT_Pool**, D-Central's [Solo/Guild pool](FAQ.md#what-is-dcent_pool), and shows a live
"DONATING" indicator on the dashboard whenever it's active. Set `percent = 0` or `enabled = false`
to disable it entirely. If the primary donation endpoint is temporarily unreachable, a visible
backup route keeps the (still-bounded) donation slice flowing — it never extends your configured
percentage and is never part of your own pool's failover.

## Safety-relevant settings

- **`[thermal].fan_max_pwm`** caps fan noise for home/quiet operation. The daemon cuts hash power
  before exceeding this cap; raising it is an explicit choice.
- **`[mining].enabled = false`** keeps a fresh unit management-only (no hash power) until you opt in.
- Voltage and frequency are bounded by per-chip silicon profiles with hard clamps you can't exceed
  from config.

For the full runtime model, see [`DCENTRALD_ARCHITECTURE.md`](DCENTRALD_ARCHITECTURE.md).
