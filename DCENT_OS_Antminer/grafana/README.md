# DCENT_OS Grafana Dashboards (G6)

Ready-to-import Grafana dashboards that consume the **native dcentrald
Prometheus exporter** (`GET /metrics`, G5). No exporter sidecar, no
PushGateway — dcentrald serves canonical Prometheus text exposition
(`text/plain; version=0.0.4`) directly, the same way BraiinsOS ships its
exporter.

This is **artifact only** — JSON + scrape config. Nothing here runs on a
miner or touches the mining/voltage/NAND path.

## Files

| File | What |
|---|---|
| `dcentos-overview.json` | Fleet/single-miner overview: hashrate, power, efficiency, HW-error rate, share rate, temps, fans, BTU/h, uptime. `$instance` is multi-select for fleet view. |
| `dcentos-per-chip-thermal.json` | Per-board (per-chain) deep dive: per-chain temp (graph + bar gauge), responding-chip-count health proxy, per-chain hashrate / HW-error rate / frequency / chip-rail voltage. `$instance` single-select. |
| `provisioning/datasource.yml` | Example Grafana provisioned Prometheus data source. |
| `provisioning/dashboards.yml` | Example Grafana dashboard provider pointing at this directory. |

## 1. Scrape config (Prometheus)

dcentrald serves `/metrics` on the dashboard/REST port (default `8080`).
Add a job to `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: dcentos
    metrics_path: /metrics
    scrape_interval: 15s
    static_configs:
      - targets:
          - 192.0.2.10:8080   # miner-a
          - 192.0.2.11:8080   # miner-b
          - 192.0.2.12:8080   # miner-c
        labels:
          fleet: example
```

Or, for a larger fleet, use file-based service discovery:

```yaml
scrape_configs:
  - job_name: dcentos
    metrics_path: /metrics
    scrape_interval: 15s
    file_sd_configs:
      - files: ['/etc/prometheus/dcentos_targets.json']
```

```json
[
  { "targets": ["192.0.2.10:8080", "192.0.2.11:8080"], "labels": { "fleet": "example" } }
]
```

> **Auth note.** If `metrics_require_auth = true` in dcentrald config
> (the shipped default), `/metrics` requires the API session/bearer like
> the rest of `/api/`. For a closed home LAN, set
> `metrics_require_auth = false` (via `/api/config` / `dcentrald.toml`)
> so Prometheus can scrape unauthenticated, or run Prometheus behind the
> same trust boundary. dcentrald never logs scrape credentials.

## 2. Import the dashboards

### Manual (UI)

Grafana → Dashboards → New → Import → Upload JSON file → pick the
Prometheus data source when prompted (`DS_PROMETHEUS`).

### Provisioned (recommended for a kiosk / always-on Grafana)

Copy this directory onto the Grafana host and point Grafana's
provisioning at it:

```
/etc/grafana/provisioning/datasources/dcentos.yml   <- provisioning/datasource.yml
/etc/grafana/provisioning/dashboards/dcentos.yml    <- provisioning/dashboards.yml
/var/lib/grafana/dashboards/dcentos/                <- *.json
```

Restart Grafana; both dashboards appear under the **DCENT_OS** folder and
auto-update when the JSON changes.

## 3. Metric reference

All series are emitted by `dcentrald_api_types::prometheus_metrics`
(unit-tested, host-safe). Metric names are stable — do not rename without
updating both dashboards.

| Metric | Type | Labels | Meaning |
|---|---|---|---|
| `dcentrald_info` | gauge (=1) | `version`,`model`,`mode`,`firmware` | Build/identity |
| `dcentrald_hashrate_ghs` | gauge | — | Instantaneous GH/s |
| `dcentrald_hashrate_5s_ghs` | gauge | — | 5s rolling GH/s |
| `dcentrald_hashrate_15m_ghs` | gauge | — | 15m avg (only if available) |
| `dcentrald_hashrate_24h_ghs` | gauge | — | 24h avg (only if available) |
| `dcentrald_power_watts` | gauge | — | Board watts |
| `dcentrald_wall_watts` | gauge | — | Wall watts |
| `dcentrald_efficiency_jth` | gauge | — | J/TH |
| `dcentrald_btu_h` | gauge | — | Heat output BTU/h |
| `dcentrald_temp_c` | gauge | `chain` | Per-board temp °C |
| `dcentrald_chain_hashrate_ghs` | gauge | `chain` | Per-board GH/s |
| `dcentrald_chain_chips` | gauge | `chain` | Responding chips (chip-health proxy) |
| `dcentrald_chain_frequency_mhz` | gauge | `chain` | Per-board MHz |
| `dcentrald_chain_voltage_mv` | gauge | `chain` | Per-board chip-rail mV |
| `dcentrald_chain_errors_total` | counter | `chain` | Cumulative CRC/HW errors |
| `dcentrald_hw_errors_total` | counter | — | Sum of all chain errors |
| `dcentrald_hw_error_rate` | gauge | — | errors / (accepted+rejected+errors) |
| `dcentrald_fan_rpm` | gauge | `fan` | Per-fan RPM |
| `dcentrald_fan_pwm` | gauge | `fan` | Per-fan PWM % (home cap 30) |
| `dcentrald_shares_accepted_total` | counter | — | Accepted shares |
| `dcentrald_shares_rejected_total` | counter | — | Rejected shares |
| `dcentrald_pool_connected` | gauge | — | 1=connected |
| `dcentrald_pool_connecting` | gauge | — | 1=connecting (≠ connected) |
| `dcentrald_pool_difficulty` | gauge | — | Current pool target difficulty |
| `dcentrald_uptime_seconds` | counter | — | Daemon uptime |

> **Truth contract.** `dcentrald_pool_connecting` is a *distinct* series
> from `dcentrald_pool_connected` (connecting ≠ connected, per the Wave
> 9E pool-state contract). `dcentrald_pool_difficulty` is the pool
> *target* difficulty, not achieved/lucky-share difficulty. 15m/24h
> hashrate series are emitted **only when the daemon actually has that
> rolling window** — they are never fabricated, so a panel may legitimately
> show "No data" for them on a fresh boot.

## 4. Relationship to the CSV ring

dcentrald also writes a LuxOS-style 3-tier CSV history to
`/data/metrics/{5s,1m,5m}.csv` (`metrics_export` task). That is a
**separate, additive** surface (local file history / offline analysis) —
the Prometheus exporter here is for live scrape/alerting. Neither
replaces the other; both read the same daemon watch-channel state.
