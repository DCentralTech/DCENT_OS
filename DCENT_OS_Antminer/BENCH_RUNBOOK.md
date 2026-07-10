# Dashboard Public Beta Bench Runbook

The current operator-run dashboard public-beta bench runbook lives at:



The final offline evidence-packet validator is:

```powershell
python scripts\dashboard_bench_evidence_check.py <bench-evidence-bundle.json> --json --output <evidence-dir>\final-bench-evidence-validation.json
```

Initialize a fail-closed evidence directory before the bench run with:

```powershell
python scripts\dashboard_bench_evidence_check.py --init-dir <evidence-dir> --target <miner-ip>
```

The dashboard-only deploy, delivery, WebSocket, recovery, and final evidence
validator scripts accept `--output <path>` so their JSON reports can be written
directly into that evidence directory.

That validator reads saved JSON files and manual observations only. It does not
contact a miner. The bundle names saved probe report JSON files and required
local evidence attachments; the validator fails if any required file is missing
or empty, if the probe reports disagree about the bench target host, and emits a
SHA-256 plus byte-count ledger for accepted reports and attachments. Incomplete
probe packets produce structured failing checks instead of stopping at the first
missing report, which makes the remaining evidence gaps explicit. The validator
also requires first-share UI and LED timestamps to match the recorded
accepted-share event within 30 seconds, and requires achieved difficulty to be
either a positive sourced value or explicitly `not_reported`. Browser and
DevTools screenshot attachments must be valid PNG, JPEG, or WebP image files
with parseable dimensions of at least 320x180. The dashboard-only deploy JSON
must target the same bench host and match the
delivery report SHA-256 plus byte count. The WebSocket report must prove the
direct production endpoint `ws://<miner-ip>:8080/ws`.
