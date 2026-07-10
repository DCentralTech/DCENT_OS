/* ────────────────────────────────────────────────────────────────────────────
 * DCENT_OS — Canonical plain-language glossary  ( / Agent F1, 2026-05-17)
 *
 * THE single source of truth for every hover explanation in the dashboard.
 * Centralising honest definitions here means the Tooltip primitive REINFORCES
 * the load-bearing telemetry truth-contracts everywhere it is used.
 *
 * Truth-contracts encoded here (NEVER soften these for aesthetics — Wave
 * 9D/9E/9F/9G/9H/9J/9K + W11/W13 +  W2):
 *   - connecting ≠ connected ≠ mining-capable        ()
 *   - pool-target-difficulty ≠ achieved-difficulty   ()
 *   - "lucky" = achieved ≥ 4× pool target            (LUCKY_THRESHOLD = 4)
 *   - staged ≠ written ≠ flashed ≠ booted            (Wave 9D7/9D8/W2)
 *   - scheduled ≠ booted                             (Wave 9D8)
 *   - donation, NOT devfee                           (no license server)
 *   - proxied hashrate is labelled (via bosminer)    (Wave 9D9)
 *   - autotuner never claims convergence w/o evidence (W3 finish)
 *   - cut hash power before raising fan noise         (quiet-home posture)
 *
 * Phase-3 agents: call `glossary('hashrate_local_vs_pool')` to get the text,
 * or `<InfoDot term="hashrate_local_vs_pool" />` / `[data-tooltip]` to render
 * it. Add a new key here (not an inline string) when you need a new explainer.
 * ──────────────────────────────────────────────────────────────────────────── */

/**
 * Cross-firmware provenance tag (DCENT Design Language — terminology-lexicon
 * contract). `shared` = a canonical word both DCENT_OS and DCENT_axe render;
 * `OS-only` = an industrial concept DCENT_axe never has; `axe-only` = a solo
 * concept DCENT_OS never has. OPTIONAL by design: every pre-existing entry
 * still satisfies `GlossaryEntry` with no tag, so adding the field is a pure
 * additive change with zero call-site impact. The tag documents which terms
 * the two firmwares must keep in lockstep vs. which are identity carve-outs
 * (§9/§10 of the lexicon — "convergence stops at the WORD, never the storage").
 */
export type GlossaryTag = 'shared' | 'OS-only' | 'axe-only';

export interface GlossaryEntry {
  /** Short label suitable for a heading inside a rich tooltip. */
  term: string;
  /** Plain-language explanation. Honest — no marketing spin. */
  body: string;
  /** Optional one-liner extra context / "why it matters". */
  note?: string;
  /**
   * Optional cross-firmware provenance (terminology-lexicon contract). When
   * present, `shared` terms must stay byte-aligned with DCENT_axe's inline
   * string mirror; `OS-only` / `axe-only` are tag-enforced keep-unique terms
   * that must never be cloned onto the other firmware.
   */
  tag?: GlossaryTag;
}

/* The registry. Keys are stable string IDs Phase-3 agents reference. */
export const GLOSSARY = {
  /* ── Hashrate / mining basics ─────────────────────────────────────────── */
  hashrate_local: {
    term: 'Local hashrate',
    body:
      'How fast your miner is computing SHA-256 hashes right now, measured on ' +
      'the device itself (TH/s = trillion hashes per second).',
    note:
      'This is the miner’s own measurement. The pool calculates a ' +
      'separate "effective" hashrate from the shares you actually submit — ' +
      'see "Local vs pool hashrate".',
  },
  hashrate_local_vs_pool: {
    term: 'Local vs pool hashrate',
    body:
      'Your miner reports the hashrate it measures locally. Your pool ' +
      'estimates a separate "effective" hashrate purely from the shares it ' +
      'received over time. They are calculated differently and never match ' +
      'exactly — the pool number swings a lot over short windows and ' +
      'settles toward the local number over hours.',
    note:
      'A short-term gap is normal and does NOT mean the miner is broken or ' +
      'that you are losing money. Judge health by accepted shares, not by ' +
      'matching these two numbers.',
  },
  hashrate_proxied: {
    term: 'Proxied hashrate (via bosminer)',
    body:
      'This figure is being read through a proxy to the stock/Braiins mining ' +
      'process — it is not produced by the native DCENT_OS chain driver. ' +
      'It is labelled "proxied / via bosminer" so you always know the source.',
    note:
      'Proxied telemetry is real but indirect. The distinction is kept on ' +
      'purpose — it is never relabelled as native mining.',
  },
  efficiency_jth: {
    term: 'Efficiency (J/TH)',
    body:
      'Joules of electricity spent per trillion hashes — watts ÷ ' +
      'TH/s. Lower is better. A modern S19-class board lands roughly in the ' +
      '20–35 J/TH range depending on tune; the DCENT_OS efficiency ' +
      'sweet-spot target is around 27.6 J/TH.',
    note:
      'Lower J/TH = more Bitcoin work per kWh. The autotuner’s ' +
      'Efficiency mode optimises toward this, not toward raw hashrate.',
    tag: 'shared',
  },
  wall_power: {
    term: 'Wall power',
    body:
      'Estimated total power drawn from the wall, including PSU losses. On ' +
      'PSU-bypass setups this is estimated from frequency/voltage tables, not ' +
      'measured by a smart PSU — treat it as a close estimate.',
  },
  uptime: {
    term: 'Uptime',
    body: 'How long the mining daemon has been running continuously since it last started.',
  },

  /* ── Pool / connection state ( truth-contract) ─────────────────── */
  pool_state: {
    term: 'Pool status',
    body:
      'Connecting, Connected, and Mining are three DIFFERENT states. ' +
      '"Connecting" means a socket is being opened — it is NOT connected ' +
      'and NOT yet mining. "Connected" means the Stratum session is up. Only ' +
      'once work is flowing and shares are accepted are you actually mining.',
    note:
      'These are deliberately not collapsed together. "Waiting" never silently ' +
      'becomes "Connected".',
  },
  pool_connecting: {
    term: 'Connecting',
    body:
      'A connection to the pool is being attempted. This is not "connected", ' +
      'not "authorized", and not "mining" — it is the very first step ' +
      'and can still fail.',
    tag: 'shared',
  },
  pool_connected: {
    term: 'Connected',
    body:
      'The Stratum session with the pool is established and authorized. Jobs ' +
      'can now flow. This alone is not proof that shares are being accepted.',
    tag: 'shared',
  },
  pool_offline: {
    term: 'Offline / Waiting',
    body:
      'No usable pool session right now. The miner may be retrying, or holding ' +
      'the last job (hash-on-disconnect) to avoid a thermal shock while it ' +
      'reconnects.',
  },
  sv2_fellback: {
    term: 'Stratum V2 → fell back to V1',
    body:
      'A Stratum V2 connection was attempted but the session is currently ' +
      'running on Stratum V1 instead. Mining still works; the V2 benefits ' +
      '(better privacy / job negotiation) are not active right now.',
  },

  /* ── Shares / difficulty ( truth-contract) ─────────────────────── */
  share_accepted: {
    term: 'Accepted share',
    body:
      'A valid proof-of-work the pool credited toward your payout. A steady ' +
      'stream of accepted shares is the real proof the miner is earning.',
    tag: 'shared',
  },
  share_rejected: {
    term: 'Rejected share',
    body:
      'Work the pool did not credit — usually stale (arrived after the ' +
      'block/job changed) or a duplicate. A few rejects are normal; a high ' +
      'reject rate (>5%) is worth investigating (latency, overclock).',
    tag: 'shared',
  },
  share_stale: {
    term: 'Stale share',
    body:
      'A share that was valid when computed but arrived after the job moved ' +
      'on. Counted separately from rejects because the cause (network/timing) ' +
      'is different.',
    tag: 'shared',
  },
  pool_target_difficulty: {
    term: 'Pool target difficulty',
    body:
      'The minimum difficulty the pool requires for a share to count. The ' +
      'pool sets and raises this (vardiff) based on your speed. It is pool ' +
      'credit / minimum-work evidence — NOT how hard your luckiest share ' +
      'actually hit.',
    note:
      'This is a different number from "Achieved difficulty" and the two ' +
      'columns are never merged.',
    tag: 'shared',
  },
  achieved_difficulty: {
    term: 'Achieved difficulty',
    body:
      'The actual difficulty your submitted share hashed to. This can be far ' +
      'higher than the pool target on a lucky share — it is the only ' +
      'real "how hard did this share hit" evidence.',
    note:
      'If not reported by the pool/protocol it stays shown as "not reported" ' +
      '— it is never faked from the pool target.',
    tag: 'shared',
  },
  lucky_share: {
    term: 'Lucky share',
    body:
      'A share whose achieved difficulty was at least 4× the pool target ' +
      'difficulty — a statistically lucky hit. Fun to see; it does not ' +
      'change your expected earnings.',
    note: 'The 4× threshold is a fixed, load-bearing constant.',
    tag: 'shared',
  },
  earning_proof: {
    term: 'Are you actually earning?',
    body:
      'Yes — if accepted shares are increasing and the last accepted ' +
      'share was recent. Local hashrate dipping or the pool’s estimate ' +
      'lagging does NOT mean you stopped earning. Accepted shares over time ' +
      'are the proof.',
    note:
      'Solo/lottery pools pay only on a found block; pooled mining pays ' +
      'continuously from accepted shares.',
    tag: 'shared',
  },

  /* ── Autotuner (W3 finish truth-contract) ─────────────────────────────── */
  autotuner_phase: {
    term: 'Tuner phase',
    body:
      'The raw state the tuner is in (e.g. Searching, Stable, Idle). ' +
      '"Searching" is not "Stable" and is not "Idle" — the real state ' +
      'is shown, never prettified into a falsely-confident word.',
  },
  autotuner_convergence: {
    term: 'Convergence',
    body:
      'How close the tuner is to a steady operating point. It is only ' +
      'reported once there is evidence for it — the tuner never claims ' +
      'it has converged without transition history to back it up.',
  },
  autotuner_transitions: {
    term: 'Transitions',
    body:
      'How many frequency/voltage steps the tuner has made. If none have ' +
      'happened it says "No transitions yet" — not "Stable for 0s".',
  },
  autotuner_expectation: {
    term: 'Tuning takes time — this is expected',
    body:
      'Autotuning legitimately takes hours and the hashrate is unstable while ' +
      'it explores. Early numbers are not final and a tuning reboot can ' +
      'happen. This is normal across all firmware — do not panic at a ' +
      'temporary half-rate.',
  },
  autotuner_last_delta: {
    term: 'Last Δ TH/s',
    body:
      'The hashrate change from the tuner’s most recent step. Small ' +
      'oscillation near the target is normal as it settles.',
  },
  tuner_mode_efficiency: {
    term: 'Efficiency mode',
    body:
      'Optimises toward the lowest J/TH (best Bitcoin work per kWh), not the ' +
      'highest raw hashrate. This is the default posture for a home/space-' +
      'heater unit.',
  },
  voltage_soft_warn: {
    term: 'Voltage soft-warning',
    body:
      'You are within ~80% of the chip’s PVT voltage cap. Allowed, but ' +
      'higher voltage means more heat and stress — the hard cap (a ' +
      'safety limit) cannot be exceeded.',
  },

  /* ── Thermal / fans (quiet-home posture) ──────────────────────────────── */
  fan_pwm: {
    term: 'Fan PWM %',
    body:
      'Fan duty-cycle request. On AM2/XIL, PWM is not acoustic proof; ' +
      'use live RPM/tach feedback and operator hearing before calling it quiet.',
  },
  quiet_boot: {
    term: 'Quiet-boot expectation',
    body:
      'A brief loud fan burst at power-on is a hardware behaviour of the ' +
      'control board before software takes over. Once DCENT_OS is running ' +
      'it requests the home fan cap and reports RPM for proof.',
  },
  cut_hash_before_noise: {
    term: 'Cut hash before noise',
    body:
      'The home-safety posture: when the unit runs warm, DCENT_OS reduces ' +
      'hash power FIRST. Fan noise is raised only for genuine thermal need, ' +
      'and acoustic claims require RPM-backed evidence.',
  },
  temp_die_vs_board: {
    term: 'Die vs board temperature',
    body:
      'Board sensors measure the PCB; die temperature is read from the chip ' +
      'itself. If board sensors return nothing, the chip die temperature is ' +
      'used as the safety fallback — thermal protection never relies on ' +
      'missing data alone.',
  },
  thermal_throttle: {
    term: 'Protecting: throttled',
    body:
      'The miner is intentionally reducing hash power to stay within safe ' +
      'temperatures. This indicator stays visible while protection is active ' +
      '— it is not hidden when throttling engages.',
  },

  /* ── Firmware / flash (Wave 9D7/9D8/W2 truth-contract) ────────────────── */
  firmware_staged: {
    term: 'Firmware staged',
    body:
      'The new firmware image has been uploaded and preflight-checked. It has ' +
      'NOT been written to flash yet and the miner has NOT rebooted into it. ' +
      '"Staged" is not "written", "flashed", or "booted".',
  },
  nand_backup_not_written: {
    term: 'NAND backup not written yet',
    body:
      'A stock-firmware backup has been prepared but has NOT been written to ' +
      'NAND. Do not treat "staged" as "backed up". The write is a separate, ' +
      'explicitly-confirmed step.',
  },
  scheduled_not_booted: {
    term: 'Scheduled ≠ booted',
    body:
      'A flash/update has been scheduled. Until you actually observe the ' +
      'miner reboot and come back on the new version, it is not "complete", ' +
      'not "booted", and rollback is not yet committed.',
  },
  restore_gates: {
    term: 'X / 5 gates met',
    body:
      'Restore-to-stock requires several independent safety gates (model ' +
      'match, wired power, backup prepared, etc.). All must be met before the ' +
      'destructive flash is allowed — the count is honest, not ' +
      'cosmetic.',
  },
  slot_removed_warning: {
    term: 'Which slot is removed',
    body:
      'Restoring stock firmware overwrites a NAND slot. The warning names ' +
      'exactly what will be removed up front — this is irreversible ' +
      'without re-flashing DCENT_OS.',
  },
  brick_anxiety: {
    term: 'Why the don’t-interrupt warning',
    body:
      'The top causes of a bricked control board are a power interruption ' +
      'mid-flash and flashing the wrong model. The preflight checklist and ' +
      '"do not power-cycle" warning exist specifically to prevent those.',
  },

  /* ── Donation (NOT devfee) ────────────────────────────────────────────── */
  donation: {
    term: 'Donation (not a dev fee)',
    body:
      'DCENT_OS has no license server and no mandatory fee. The optional ' +
      'donation is transparent, adjustable (0–5%), and fully ' +
      'disableable. It funds DCENT_OS + D-Central’s open-source work. ' +
      'It is always called a donation, never a "dev fee".',
    note:
      'For context, competing firmware charges a mandatory ~1.8–3% dev ' +
      'fee with a license server.',
  },
  donating_indicator: {
    term: 'DONATING',
    body:
      'Shown while the optional donation routing is active for the current ' +
      'time slice. Mining for you the rest of the time. Toggle it off any ' +
      'time in settings.',
  },
  dcent_pool: {
    term: 'DCENT_Pool (Solo/Guild pool)',
    body:
      'D-Central’s own mining pool and the default donation destination. It ' +
      'is a trustless, MMORPG-style take on solo mining: hunt for a block on ' +
      'your own, or join a guild to share the block reward with other miners ' +
      '— fully non-custodial, with no account custody of your coins.',
  },

  /* ── Competitive readiness / manifest ─────────────────────────────────── */
  competitive_proven: {
    term: 'PROVEN',
    body:
      'This capability has live accepted-share or equivalent hard evidence on ' +
      'real hardware — not just code that compiles.',
  },
  competitive_blocked: {
    term: 'BLOCKED',
    body:
      'A capability that is implemented but cannot be claimed live yet ' +
      'because a hardware/operator gate is open. The dashboard never shows a ' +
      'blocked capability as if it were live.',
  },
  fee_route: {
    term: 'FEE ROUTE',
    body:
      'The transparent, bounded route donation work takes (primary, then a ' +
      'visible backup). It never extends the configured donation percentage.',
  },

  /* ── Bitcoin block fields (pleb-literacy) ─────────────────────────────── */
  block_height: {
    term: 'Block height',
    body:
      'The number of blocks in the Bitcoin chain so far. It ticks up roughly ' +
      'every ~10 minutes when a new block is found network-wide.',
  },
  block_subsidy: {
    term: 'Block subsidy',
    body:
      'The new bitcoin minted in a block (currently 3.125 BTC, halving every ' +
      '210,000 blocks). Part of the miner reward.',
  },
  block_fees: {
    term: 'Block fees',
    body:
      'Total transaction fees paid by transactions in the block — the ' +
      'other part of the miner reward, on top of the subsidy.',
  },
  block_reward: {
    term: 'Block reward',
    body:
      'Subsidy + fees — what the miner who finds this block receives. ' +
      'In a pool you earn a proportional share of this over many blocks; solo ' +
      'you only earn it if you find the block.',
  },
  block_tx_count: {
    term: 'Transaction count',
    body: 'How many transactions are confirmed in this block.',
  },

  /* ── Honest-mode / proxy trust ────────────────────────────────────────── */
  honest_mode: {
    term: 'Honest-mode status',
    body:
      'Shows whether mining is running natively, through a proxy, or hardware-' +
      'blocked. "proxy_alive" = working via the stock process; ' +
      '"proxy_degraded" = working but impaired; "hardware_blocked" = a ' +
      'hardware gate is preventing native mining. Surfaced so you always know ' +
      'what you can trust.',
  },

  /* ── Advanced register/bus literacy (P3 applies the dense ones) ───────── */
  bip320_version_rolling: {
    term: 'BIP320 version-rolling',
    body:
      'ASICBoost: the miner rolls allowed bits of the block-version field ' +
      '(mask 0x1FFFE000) to search more space per work item. Standard, pool-' +
      'negotiated, and ~3.4% more efficient.',
  },
  crc_type: {
    term: 'CRC type',
    body:
      'The checksum scheme on the chip serial link. The canonical chip-UART ' +
      'frame uses CRC16-CCITT-FALSE — a wrong CRC type silently drops ' +
      'all nonces.',
  },
  i2c_address: {
    term: 'I²C address',
    body:
      'The bus address of a device (PSU/PIC/EEPROM/sensor). EEPROM addresses ' +
      '0x50–0x57 are write-protected at the HAL to prevent the ' +
      'corruption class that bricked a past unit.',
  },
  fpga_register: {
    term: 'FPGA register',
    body:
      'A memory-mapped hardware register. Reading is safe; writing the wrong ' +
      'value can disrupt mining or hardware. Advanced mode only.',
  },
  pid_gain: {
    term: 'PID gain',
    body:
      'Proportional/Integral/Derivative tuning of the thermal control loop. ' +
      'Aggressive gains chase the target faster but can oscillate fan/temp.',
  },

  /* ════════════════════════════════════════════════════════════════════════
   *  additions (Agent F6, 2026-05-17 — APPEND-ONLY).
   * Formalize the niche inline literals the recon audit (§6) catalogued.
   * Every body is byte-faithful in MEANING to the truthful inline string it
   * replaces — chrome only, never softened. Truth-contracts reinforced:
   * telemetry-stale reads as an honest warning (never "all fine"); the
   * autotuner-receipts entry never claims a result, only that evidence files
   * exist; donation≠devfee and quiet-home posture preserved.  keys
   * above are NOT renamed or altered — callers depend on them.
   * ════════════════════════════════════════════════════════════════════════ */

  /* ── Advanced: PIC DAC code (replaces advanced/VoltageControl.tsx:306) ─── */
  pic_dac_value: {
    term: 'PIC DAC code',
    body:
      'The 8-bit code written to the hash-board voltage DAC. ' +
      'pic_val = round(1608.420446 − 170.423497 × voltage_V) — a lower ' +
      'code means a higher chip-rail voltage.',
    note:
      'Out-of-range codes can permanently damage the hash board. Advanced ' +
      'mode only.',
  },

  /* ── Charts: telemetry freshness (Wave 9D9 truth-contract) ────────────── */
  telemetry_stale: {
    term: 'Telemetry stale',
    body:
      'No fresh samples arrived recently, so the chart is holding its last ' +
      'points. They are real past readings — not faked or extrapolated ' +
      'forward.',
    note:
      'Judge mining health by accepted shares, not by a momentarily stale ' +
      'chart.',
    tag: 'shared',
  },
  telemetry_live: {
    term: 'Live updates',
    body:
      'Fresh telemetry is streaming from the miner over the WebSocket. If ' +
      'it stops, the dashboard shows the last known values and says so — ' +
      'it never invents data.',
    tag: 'shared',
  },

  /* ── Autotuner: evidence-only panel (W3 finish truth-contract) ────────── */
  autotuner_receipts: {
    term: 'Receipts only',
    body:
      'Every value here is read from a real file on disk or a live runtime ' +
      'probe. Nothing is inferred, predicted, or fabricated — it is ' +
      'evidence the autotuner state and rollback backups exist, not a ' +
      'claim about results.',
  },

  /* ── Setup wizard: pool fields (replaces wizard/PoolStep.tsx inline) ───── */
  wizard_pool_url: {
    term: 'Pool URL',
    body:
      'The server your miner connects to. Format ' +
      'stratum+tcp://host:port (or stratum+ssl:// for an encrypted pool). ' +
      'Your pool’s site lists this; it was auto-filled when you picked a ' +
      'pool. The port is required.',
  },
  wizard_worker_name: {
    term: 'Worker / username',
    body:
      'The username the pool credits. For most pools this is your Bitcoin ' +
      'address (or a pool account) followed by a dot and a device name, ' +
      'e.g. bc1q…livingroom. Earnings go here — double-check it.',
  },
  wizard_worker_label: {
    term: 'Worker label',
    body:
      'A short name so you can tell this miner apart in your pool stats — ' +
      'like livingroom or garage. It becomes the part after the dot in ' +
      'address.workername. The pool password can usually be anything ' +
      '(x is fine).',
  },

  /* ── Pleb micro-explainers (P1’s  request — Basic/Heater mode) ────
     These canonicalize the honest inline data-tooltip strings already in
     basic/HeaterStatus, HeatingValueSummary, HistoryView, NightMode,
     SettingsView, Thermostat. Plain-language, no spin, no overstatement. */
  btu_per_hour: {
    term: 'BTU/h',
    body:
      'How much heat this miner puts into the room every hour. Every watt ' +
      'it draws becomes useful heat — about 3,412 BTU/h per kW, roughly a ' +
      '1 kW space heater.',
    tag: 'shared',
  },
  daily_cost: {
    term: 'Daily cost',
    body:
      'Estimated electricity for 24 h at the current power draw and your ' +
      'set rate. You would pay most of this for any space heater anyway.',
    note:
      'It is an estimate from the current draw — actual cost varies with ' +
      'tariff and runtime.',
  },
  net_value_offset: {
    term: 'Net value',
    body:
      'Electricity cost minus the Bitcoin value earned, per day. If you ' +
      'are using the heat you would be paying for anyway, the real cost is ' +
      'even lower.',
    note:
      'BTC value uses the manual price you set; it does not change actual ' +
      'mining.',
  },
  sats_estimate: {
    term: 'Sats estimate',
    body:
      'An estimate of the sats earned over this period from the measured ' +
      'hashrate. It is a projection, not a settled payout.',
    note:
      'Solo/lottery pools pay only when a block is actually found; pooled ' +
      'mining pays continuously from accepted shares.',
  },
  accept_rate_thresholds: {
    term: 'Accept rate',
    body:
      'Share of submitted shares the pool credited. Above 99% is great, ' +
      '95–99% is normal, below 95% is worth checking (latency or ' +
      'overclock). Earning is judged by accepted shares.',
    tag: 'shared',
  },
  power_budget: {
    term: 'Power budget',
    body:
      'A circuit/cost planning reference recorded in the dashboard — it is ' +
      'NOT enforced on the miner. To actually cap wall draw, set a power or ' +
      'efficiency target in Tuning.',
  },
  btc_price_display: {
    term: 'BTC price',
    body:
      'A manual USD price used only to convert the sats estimate for ' +
      'display. It does NOT affect mining, payouts, or the miner in any ' +
      'way.',
  },
  night_mode_behaviour: {
    term: 'Night Mode',
    body:
      'Runs the miner at the reduced power level you set while you sleep. ' +
      'It cuts hash power before asking for more fan; acoustic quiet still ' +
      'needs live tach/RPM proof.',
  },

  /* ════════════════════════════════════════════════════════════════════════
   *  additions (Agent 1A, 2026-05-22 — APPEND-ONLY).
   * PSU Override wizard step (Loki / bare-APW3 / stock-APW12). Truth-contract
   * reinforced via the SE+DevOps Option B vocabulary: declared-not-autodetect,
   * byte-identical Rust config for loki and bare-apw3, fleet-inventory tag.
   * Stock-APW12 stays disabled / "coming-soon" until live-validated.
   * ════════════════════════════════════════════════════════════════════════ */

  psu_override: {
    term: 'PSU Override',
    body:
      'Tells DCENT_OS that you have a non-smart PSU (an APW3 modded to a ' +
      'fixed voltage rail), bypassing the smart-APW12 SMBus voltage-program ' +
      'path. Mining proceeds via PWR_CONTROL GPIO assertion instead. ' +
      'Required for any S19j Pro XIL on a modded APW3.',
    note:
      'If you have a stock APW12 PSU, pick the Stock APW12 option — that ' +
      'disables psu_override entirely so the daemon runs the canonical ' +
      'smart-APW12 handshake.',
  },
  psu_override_loki: {
    term: 'Loki spoof board',
    body:
      'A small daughter-board on i2c-0 @ 0x10 that electrically spoofs the ' +
      'APW12 SMBus protocol on a modded APW3. Lets DCENT_OS or BraiinsOS ' +
      'complete their smart-APW12 handshake even though the APW3 has no ' +
      'SMBus device. The rail stays at 12.8 V regardless — Loki is a ' +
      'protocol courtesy, not a regulator.',
  },
  psu_override_bare_apw3: {
    term: 'Bare APW3 (no Loki)',
    body:
      'Modded APW3 without the Loki daughter-board. Recommended for new ' +
      'fleet builds. DCENT_OS detects the silent i2c-0 @ 0x10 bus, falls ' +
      'through the smart-APW12 lenient probe in ~200 ms, and proceeds in ' +
      'PWR_CONTROL-only mode. Rail stays at 12.8 V — identical to the ' +
      'Loki-attached case at the chip side.',
  },
  psu_override_stock_apw12: {
    term: 'Stock APW12 PSU',
    body:
      'The original Bitmain smart PSU. Speaks SMBus on i2c-0 @ 0x10 and ' +
      'accepts SetVoltage / Watchdog commands. With this hardware variant, ' +
      'DCENT_OS uses the canonical smart-APW12 handshake; psu_override is ' +
      'disabled entirely. No Loki spoof or PWR_CONTROL bypass.',
    note: 'This is the default for unmodified Antminer S19j Pro units.',
  },
  // ────────────────────────────────────────────────────────────────────
  //  HIGH-1 / HIGH-2 (2026-05-24): .25-class XIL bosminer-handoff
  // recipe surfaces. Encode the operator caveats here so EVERY tooltip
  // that links them stays truthful (no spin, no "always works" claims).
  // ────────────────────────────────────────────────────────────────────
  wave54_handoff_caveat: {
    term: 'Handoff mining (bosminer pre-engaged)',
    body:
      'First DCENT_OS mining on the XIL S19j Pro (2026-05-24) — 12 ' +
      'shares accepted by public-pool.io in 165 s. The path requires a 5-step ' +
      'bosminer-handoff procedure: AC-cycle the unit, boot the BraiinsOS slot, ' +
      'wait ~75 s for bosminer to pre-engage the PSU/dsPIC/Loki spoof, then ' +
      'kill bosminer and launch the DCENT_OS daemon under the BraiinsOS rootfs ' +
      'with the proven 13-env-var recipe.',
    note:
      'The mining session survives daemon uptime but NOT a reboot. ' +
      'Re-running the recipe requires another AC power cycle. ' +
      'Standalone DCENT_OS bring-up (no bosminer dependency) is Phase 2 work. ' +
      'See the documentation.',
  },
  chip_enum_handoff_caveat: {
    term: 'Chain presence (chips responding / expected)',
    body:
      'How many ASIC chips on each chain are currently answering chip-UART ' +
      'enumeration vs how many should be physically present. This unit has ' +
      '2 hashboards × 63 chips = 126 expected; the handoff recipe sometimes ' +
      'shows partial enumeration (e.g. 34/126) — that is honest telemetry, not ' +
      'a UI bug.',
    note:
      'A green pill means ≥90% of chips are responding (healthy chain). ' +
      'Yellow 50–89% means a known partial-chain condition (the handoff recipe ' +
      'still produces accepted shares from this state on this unit). Red <50% means the ' +
      'chip rail almost certainly did NOT engage — most likely the handoff ' +
      'recipe is broken (forbidden env var set, or BraiinsOS slot was not ' +
      'booted long enough before the handoff).',
  },
  recipe_state_intact: {
    term: 'Handoff recipe state',
    body:
      'Whether the live process environment matches the 13-env-var proven ' +
      'handoff mining recipe. Green = all 13 required vars are exported AND ' +
      'zero of the 4 forbidden vars are set. Yellow = some required vars are ' +
      'missing (the launcher script is the canonical source — env was ' +
      'partially overridden). Red = at least one forbidden var is set; the ' +
      'daemon WILL refuse to start or mine on this hardware.',
    note:
      'The 4 forbidden env vars (PIC_RESET_AND_START_APP, ' +
      'PIC_RESET_STRACE_DERIVED, PSU_LOKI_REGISTER_POINTER, ' +
      'PSU_CALIBRATION_PROBE_WAKE) were each independently LIVE-FALSIFIED — ' +
      'setting any one re-breaks the proven path. See the documentation.',
  },
  chain_rail_mv_xil25: {
    term: 'Chain rail (mV actual vs target)',
    body:
      'The voltage the dsPIC is currently regulating to on the chip rail vs ' +
      'the target the daemon commanded. On this unit the target is 13.7 V (13700 ' +
      'mV) — the dsPIC fw=0x82 ACK\'d SetVoltage(13700 BE-mV) live on ' +
      '2026-05-24. Green within ±200 mV of target.',
    note:
      'A reading well below target (<50%) means the dsPIC is most likely in ' +
      'fw=0x82 bootloader/echo mode and never accepted the SetVoltage command. ' +
      'That almost always means the handoff recipe is broken — most often a ' +
      'forbidden env var is set, or the BraiinsOS slot bring-up was skipped.',
  },

  /* ════════════════════════════════════════════════════════════════════════
   * DCENT Design Language — Terminology Lexicon emission (2026-06-14, terms
   * lane, APPEND-ONLY). Source of truth:
   * docs/design-system/DCENT_DESIGN_LANGUAGE/terminology-lexicon.md.
   *
   * These keys make `glossary.ts` the tagged STRUCTURAL MODEL (TERM-7): each
   * entry carries the cross-firmware `tag` so a future agent can never silently
   * collapse a `[shared]` word, clone an `[OS-only]` industrial term onto axe,
   * or absorb an `[axe-only]` solo term into OS. Bodies mirror the lexicon's
   * exact truth phrasing — connecting ≠ connected ≠ mining; scheduled ≠ booted;
   * achieved "not reported" is never faked (§0 truth-contract supremacy).
   *
   * STRING-SOURCE ONLY: these are the canonical strings wave-2 components import
   * (StatusPill truth-ladder, OTA/pool ladders, unit/no-data words). No .tsx
   * component is edited here. No i18n locale key is added (the lexicon lives in
   * this TS data layer), so key-parity stays green by construction.
   * ════════════════════════════════════════════════════════════════════════ */

  /* ── Autotuner mode lexicon — the 4 canonical [shared] modes (TERM-1 §1.1) ── */
  tuner_mode_max_hashrate: {
    term: 'Max Hashrate',
    body:
      'Push frequency up until power or thermals cap it. Maximum hash power.',
    tag: 'shared',
  },
  tuner_mode_best_efficiency: {
    term: 'Best Efficiency',
    body:
      'Find the lowest J/TH sweet spot, typically 60–70% of the max frequency.',
    tag: 'shared',
  },
  tuner_mode_target_watts: {
    term: 'Target Watts',
    body:
      'Hit a power budget and squeeze the best hashrate under that ceiling.',
    tag: 'shared',
  },
  tuner_mode_target_temp: {
    term: 'Target Temp',
    body:
      'Keep the chip at a chosen temperature — the autotuner raises frequency ' +
      'until it is just warm enough.',
    tag: 'shared',
  },

  /* ── OS-only home presets (TERM-1 §1.2) — MUST NOT be cloned onto axe. ──── */
  tuner_preset_quiet_home: {
    term: 'Quiet Home',
    body:
      'OS-only home posture: a quiet, conservative profile that prioritises ' +
      'low fan noise and a calm rail over raw hashrate. It is part of OS\'s ' +
      'richer home/quiet preset set and has no DCENT_axe equivalent.',
    tag: 'OS-only',
  },
  tuner_preset_balanced_home: {
    term: 'Balanced Home',
    body:
      'OS-only home posture: a middle-ground profile balancing efficiency, ' +
      'noise, and hashrate for everyday home/space-heater use. OS-only — axe ' +
      'expresses the equivalent intent through its own wattage-descent framing.',
    tag: 'OS-only',
  },
  tuner_preset_advanced_manual: {
    term: 'Advanced Manual',
    body:
      'OS-only Hacker-tier preset: the operator sets frequency/voltage by hand ' +
      'instead of letting a policy drive them. Part of OS\'s industrial ' +
      'per-chip control surface; not present on axe.',
    tag: 'OS-only',
  },

  /* ── Mining-state truth ladder (TERM-2 §2.1) — the keystone reconciliation.
     Rung-2 canonical id is `ready` (NOT `enabled`); rung-3 is `standby`. ──── */
  state_telemetry_pending: {
    term: 'Telemetry pending',
    body:
      'Rung 0 of the mining-state ladder: no telemetry has arrived yet (before ' +
      'the first /api/system/info). It is not "offline" and not "mining" — the ' +
      'dashboard is simply waiting for the first real sample.',
    tag: 'shared',
  },
  state_mining: {
    term: 'Mining',
    body:
      'Rung 1: mining is enabled AND a positive hashrate is visible. This is ' +
      'the only state that claims work is actually flowing — it is never shown ' +
      'on the strength of "connected" or "enabled" alone.',
    tag: 'shared',
  },
  state_ready: {
    term: 'Ready',
    body:
      'Rung 2: mining is permitted but hashrate is still zero or unknown — ' +
      'permitted, work not yet flowing. A neutral, honest word that does NOT ' +
      'imply active mining; it is distinct from "Standby" (stopped) and from ' +
      '"Mining" (positive hashrate).',
    note:
      'Canonical id is `ready`. Replaces the overloaded "Enabled" wording for ' +
      'the permitted-but-zero-hashrate rung so it can never be read as mining.',
    tag: 'shared',
  },
  state_standby: {
    term: 'Standby',
    body:
      'Rung 3: mining is disabled or not running. The canonical health word ' +
      'for "not mining". User-paused and never-started are sub-states of ' +
      'Standby that may keep their own pill text where that distinction is ' +
      'load-bearing.',
    note: 'Canonical id is `standby`.',
    tag: 'shared',
  },
  state_stopped: {
    term: 'Stopped',
    body:
      'Mining has been explicitly stopped. A sub-state of Standby surfaced ' +
      'where the operator needs to see "stopped" distinctly from a unit that ' +
      'simply never started.',
    tag: 'shared',
  },

  /* ── OTA / firmware proof-ladder (TERM-2 §2.2) — never collapsed.
     uploaded → signature-verified → preflight-passed → scheduled → booted. ── */
  ota_uploaded: {
    term: 'Uploaded',
    body:
      'Rung 1 of the firmware-update ladder: the image was received. It has ' +
      'NOT been verified, scheduled, flashed, or booted. "Uploaded" is the ' +
      'weakest claim — never treat it as final write proof.',
    tag: 'shared',
  },
  ota_signature_verified: {
    term: 'Signature verified',
    body:
      'Rung 2: the image\'s cryptographic signature was checked and trusted. ' +
      'This proves authenticity only — it is not preflight, not scheduled, and ' +
      'not booted.',
    tag: 'shared',
  },
  ota_preflight_passed: {
    term: 'Preflight passed',
    body:
      'Rung 3: target/model/slot-fit preflight checks passed. The image is ' +
      'cleared to be scheduled — but it has NOT been written or booted yet.',
    tag: 'shared',
  },
  ota_scheduled: {
    term: 'Scheduled',
    body:
      'Rung 4: the flash is scheduled. Until you actually observe the miner ' +
      'reboot onto the new version it is NOT booted, NOT complete, and rollback ' +
      'is not yet committed. Scheduled ≠ booted.',
    tag: 'shared',
  },
  ota_booted: {
    term: 'Booted',
    body:
      'Rung 5: the miner was observed rebooting onto the new version. This is ' +
      'the only rung that proves the update actually took effect — every rung ' +
      'below it is a separate, weaker claim.',
    tag: 'shared',
  },

  /* ── Pool / share proof-ladder (TERM-2 §2.3) — never collapsed.
     connecting → connected → authorized → job-fresh → share-accepted.
     (pool_connecting / pool_connected / share_accepted already exist above.) ─ */
  pool_authorized: {
    term: 'Authorized',
    body:
      'Rung 3 of the pool ladder: mining.authorize succeeded, so the pool ' +
      'accepts this worker. It is past "connected" but still not proof that a ' +
      'job is in hand or that any share was accepted.',
    tag: 'shared',
  },
  pool_job_fresh: {
    term: 'Job fresh',
    body:
      'Rung 4: a current mining job is in hand. Work can be hashed against it — ' +
      'but a fresh job is not yet an accepted share; the pool still has to ' +
      'credit the work.',
    tag: 'shared',
  },

  /* ── Units (TERM-3) — canonical decimals / constants live in format.ts /
     thermal.ts / modelProfiles.ts; these are the operator-facing labels. ── */
  unit_hashrate: {
    term: 'Hashrate',
    body:
      'How fast the miner computes SHA-256 hashes. Internal unit is GH/s; the ' +
      'display ladder is MH/s (.0) · GH/s (.1, canonical) · TH/s (.2) · ' +
      'PH/s (.2). Higher is more work attempted per second.',
    tag: 'shared',
  },
  unit_power: {
    term: 'Power',
    body:
      'Electrical power draw. Shown as watts below 1000 W and as kilowatts ' +
      '(kW, two decimals) at or above 1000 W.',
    tag: 'shared',
  },
  unit_voltage: {
    term: 'Voltage',
    body:
      'The chip-rail or domain voltage, shown to three decimals (e.g. ' +
      '13.700 V). Higher voltage means more heat and stress; a hard safety cap ' +
      'cannot be exceeded.',
    tag: 'shared',
  },
  unit_frequency: {
    term: 'Frequency',
    body:
      'The ASIC clock frequency in megahertz (MHz). Raising it increases ' +
      'hashrate and power until thermals or the voltage envelope cap it.',
    tag: 'shared',
  },

  /* ── Best-diff vocabulary (TERM-4 §4.2) — axe\'s cleaner naming, OS adopts. ─ */
  best_diff_session: {
    term: 'Best Diff (session)',
    body:
      'The highest achieved difficulty seen this session — the luckiest single ' +
      'share since the miner last started. Resets when the session does.',
    tag: 'shared',
  },
  best_diff_all_time: {
    term: 'Best Ever (all-time)',
    body:
      'The highest achieved difficulty ever recorded on this miner, persisted ' +
      'across restarts. Your all-time luckiest share.',
    tag: 'shared',
  },

  /* ── No-data / empty / stale lexicon (TERM-6 §6.1-6.2).
     (telemetry_stale / telemetry_live already exist above.) ──────────────── */
  telemetry_absent: {
    term: 'No telemetry',
    body:
      'Nothing is arriving from the miner at all — distinct from "stale" ' +
      '(held last real readings) and from "pending" (no first sample yet). ' +
      'Shown as Offline; it is never dressed up as held data.',
    tag: 'shared',
  },
  empty_value: {
    term: 'No value yet',
    body:
      'A value that is absent, unknown, or has not arrived yet is shown as an ' +
      'em-dash (—), never as 0 and never as a fabricated number. A real ' +
      'measured zero is shown as 0 — the em-dash means "no data", not "zero".',
    tag: 'shared',
  },
  telemetry_unpowered: {
    term: 'No power to hash board',
    body:
      'The hash board is unpowered: hashrate, frequency, and voltage all read ' +
      '0. Power presence is proven by hashrate/frequency/voltage — never by ' +
      'temperature alone — so a cold, voltage-less board is shown honestly as ' +
      'unpowered rather than as a board that is merely idle.',
    tag: 'shared',
  },
  telemetry_per_chip_unavailable: {
    term: 'Per-chip telemetry unavailable',
    body:
      'The daemon did not return per-chip data from /api/chips for this chain. ' +
      'DCENT_OS only shows real per-chip values — it will not estimate or ' +
      'fabricate a heatmap. Per-chip detail appears when the daemon publishes ' +
      'live or saved chip-health for this chain.',
    tag: 'shared',
  },

  /* ── Status pills — additional canonical states (TERM-2/§7).
     telemetry_pending / mining / ready / standby / stopped / connecting /
     connected reuse the keys above; these three complete the pill set. ───── */
  state_online: {
    term: 'Online',
    body:
      'The miner is reachable and reporting fresh telemetry. A management/' +
      'reachability state — it does not by itself claim the unit is mining.',
    tag: 'shared',
  },
  state_warning: {
    term: 'Warning',
    body:
      'A non-fatal condition needs attention (e.g. stale telemetry, a hot ' +
      'chain easing off, or a degraded board). Mining may continue, but the ' +
      'condition is surfaced honestly rather than hidden.',
    tag: 'shared',
  },
  state_error: {
    term: 'Error',
    body:
      'A fault that blocks normal operation (e.g. telemetry offline, a fan ' +
      'tachometer reading zero while cooling is active, or a critical ' +
      'temperature). Always surfaced — never silently cleared.',
    tag: 'shared',
  },

  /* ── Shared-word / different-scope register (TERM §9) — OS scope stated in
     the body so a future agent can never conflate it with axe\'s scope. ──── */
  swarm_os_fleet: {
    term: 'Swarm (fleet)',
    body:
      'On DCENT_OS, "Swarm" means fleet control — discovery and coordinated ' +
      'management of many miners (FleetView). This is the OS-scoped meaning. ' +
      'On DCENT_axe the same word means single-device local consensus + Queen ' +
      'election; the two are deliberately NOT the same feature.',
    tag: 'OS-only',
  },
  advanced_os_tier: {
    term: 'Advanced (Hacker tier)',
    body:
      'On DCENT_OS, "Advanced" is the Hacker tool-tier — raw FPGA/register/I²C ' +
      'access and per-chip control. This is the OS-scoped meaning. On ' +
      'DCENT_axe "Advanced" is just an inline disclosure; the two are not the ' +
      'same surface and must not be merged.',
    tag: 'OS-only',
  },
  /* ════════════════════════════════════════════════════════════════════════
   * FWT-1 / Telemetry-honesty provenance (2026-06-19, APPEND-ONLY).
   * The daemon publishes per-field `*_source` provenance on PoolState so the
   * UI can distinguish a REAL measurement from an honest-default placeholder.
   * "honest_default" = a fresh-boot/never-observed baseline (latency 0,
   * acceptance 100%) — it must be SURFACED as an estimate, never shown as if
   * measured. These keys back the subtle "estimate / not yet measured"
   * affordance next to the value. Truth-contract: never hide the value, just
   * mark its provenance (mirrors the chain-temp `soc_die_fallback` affordance).
   * ════════════════════════════════════════════════════════════════════════ */
  honest_default_estimate: {
    term: 'Estimate — not yet measured',
    body:
      'This value is a fresh-boot placeholder, not a real measurement yet. The ' +
      'firmware reports it honestly as an "honest default" (e.g. ping 0 ms or ' +
      '100% acceptance before any share has been ACKed) so the dashboard does ' +
      'NOT pretend it was measured. It becomes a real reading once the pool ' +
      'session has produced live telemetry.',
    note:
      'The value is still shown — it is just marked as an estimate so you know ' +
      'not to trust it as a measured number yet.',
    tag: 'shared',
  },
  pool_latency_ms: {
    term: 'Pool ping (latency)',
    body:
      'The round-trip time from submitting a share to the pool\'s response, in ' +
      'milliseconds. Lower is better; high latency can raise the stale-share ' +
      'rate. It is only a real measurement after at least one share round-trip.',
    note:
      'Before the first measured round-trip it reads 0 ms as an honest default ' +
      '— marked as an estimate until a real latency sample exists.',
    tag: 'shared',
  },
  pool_rolling_acceptance: {
    term: 'Rolling acceptance (30 min)',
    body:
      'The share of submitted shares the pool credited over the last 30 minutes. ' +
      'A high, stable number is the real proof the miner is earning.',
    note:
      'Before any share has been ACKed it reads 100% as an honest default — ' +
      'marked as an estimate until real accepted/rejected counts exist.',
    tag: 'shared',
  },

  /* ── New pool-status truth-ladder rungs ( connection states) ──────────
     The daemon now emits real connection states on PoolState.status:
     connecting / authorized / mining / rejecting / disconnected / auth_failed.
     "rejecting" and "auth_failed" get a warning/error tint so the pool card
     never shows a raw token. (pool_connecting / pool_authorized / state_mining
     already exist above and cover the healthy rungs.) ──────────────────────── */
  pool_rejecting: {
    term: 'Rejecting shares',
    body:
      'The pool session is up but it is currently rejecting submitted shares — ' +
      'usually stale (latency / a fast job change) or an overclock that is ' +
      'producing bad work. A few rejects are normal; sustained rejection is ' +
      'worth investigating.',
    note:
      'This is surfaced honestly with a warning tint rather than being hidden ' +
      'or shown as plain "Connected".',
    tag: 'shared',
  },
  pool_auth_failed: {
    term: 'Authorization failed',
    body:
      'The pool refused this worker\'s mining.authorize — almost always a wrong ' +
      'worker name / username or password. No shares can be credited until the ' +
      'credentials are fixed. Check the worker field on the Pools page.',
    note:
      'Shown with an error tint so it is never mistaken for a healthy ' +
      '"Connected" state.',
    tag: 'shared',
  },
  pool_disconnected: {
    term: 'Disconnected',
    body:
      'There is no pool session right now. The miner may be retrying, or holding ' +
      'the last job (hash-on-disconnect) to avoid a thermal shock while it ' +
      'reconnects. No shares are flowing until it reconnects.',
    tag: 'shared',
  },

  /* ── Appearance (UINAV-7) — OS-only. DCENT_axe is a single dark terminal by
     identity and has no light/dark concept, so this never crosses to axe. ─── */
  appearance_theme: {
    term: 'Appearance (light / dark)',
    body:
      'Light or dark surface palette. Independent of the Basic / Standard / ' +
      'Advanced operating-mode skins — it only changes surface lightness, not ' +
      'which mode you’re in.',
    note:
      'Saved locally per browser; defaults to dark. Light recolors the ' +
      'Standard skin and shared chrome; the Advanced (terminal) and Basic ' +
      '(heater) skins keep their signature palettes by design.',
    tag: 'OS-only',
  },

  /* ════════════════════════════════════════════════════════════════════════
   * FE-1 / FE-2 truthfulness copy (2026-06-20, APPEND-ONLY).
   * Truth-contract: a projection is never presented as realized earnings, and a
   * manual/fallback BTC price is never presented as an authoritative quote.
   *   - earnings_projection_series: the "Earnings over time" chart is computed
   *     from current hashrate x the live sats/day estimate applied back over
   *     time — a PROJECTION, NOT credited on-chain payouts.
   *   - usd_estimate_fallback: the USD value converts the sats estimate with a
   *     manual BTC price; local-first means no live price, so an unset price
   *     falls back to a built-in default and must read as an estimate.
   * ════════════════════════════════════════════════════════════════════════ */
  earnings_projection_series: {
    term: 'Projected earnings over time',
    body:
      'This chart is a PROJECTION built from your current hashrate and the live ' +
      'sats/day estimate applied back over time — it is NOT a record of realized ' +
      'on-chain payouts. Each point shows what the current rate would have ' +
      'produced, not what the pool actually credited.',
    note:
      'Real earnings are proven by accepted shares over time, not by this ' +
      'projected curve. Solo/lottery pools pay only when a block is found.',
    tag: 'shared',
  },
  usd_estimate_fallback: {
    term: 'USD value is an estimate',
    body:
      'The USD figure is an estimate that converts the sats projection using a ' +
      'manual BTC price. DCENT_OS is local-first and does not fetch a live ' +
      'price, so when no price has been set it falls back to a built-in ' +
      'default. Set your BTC price in Settings for an accurate conversion.',
    note:
      'The BTC price is display-only — it does NOT affect mining, payouts, or ' +
      'the miner in any way.',
    tag: 'shared',
  },
} as const satisfies Record<string, GlossaryEntry>;

export type GlossaryKey = keyof typeof GLOSSARY;

/**
 * Resolve a glossary entry. Phase-3 agents: prefer the typed key form
 * `glossary('efficiency_jth')`. Returns `undefined` for an unknown key so a
 * caller can fall back to a literal string without throwing.
 */
export function glossary(key: GlossaryKey): GlossaryEntry;
export function glossary(key: string): GlossaryEntry | undefined;
export function glossary(key: string): GlossaryEntry | undefined {
  return (GLOSSARY as Record<string, GlossaryEntry>)[key];
}

/**
 * Flatten an entry to a single tooltip string (term + body + note). Used by
 * the lightweight `[data-tooltip]` CSS path where only a string fits.
 */
export function glossaryText(key: GlossaryKey | string): string {
  const e = (GLOSSARY as Record<string, GlossaryEntry>)[key as string];
  if (!e) return '';
  return e.note ? `${e.body} — ${e.note}` : e.body;
}
