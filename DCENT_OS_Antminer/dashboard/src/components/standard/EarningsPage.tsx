import React, { useState, useMemo, useCallback, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { StatePanel } from '../common/StatePanel';
import { TaskHandoffBanner } from '../common/TaskHandoffBanner';
import { formatHashrateShort, formatWatts, formatSats, formatBtu } from '../../utils/format';
import { api, type PerfEfficiencyResponse } from '../../api/client';
import {
  estimateDailyProfit,
  estimateDailyCost,
  wattsToBtu,
  btuComparison,
  estimateHeatingOffset,
  daysToHalving,
  nextHalving,
  blockRewardAt,
  fourYearRevenueWithHalving,
} from '../../utils/thermal';
import { getLivePowerEfficiencyJth, getLiveWallWatts } from '../../utils/power';
import { useNetworkContext } from '../../hooks/useNetworkContext';
import { useValueFlash } from '../../hooks/useValueFlash';
import { EarningsChart, type EarningsPeriod, type EarningsPoint } from './EarningsChart';
import { PageSkeleton } from '../common/skeletons';
import { glossaryText } from '../../utils/glossary';

type Period = 'daily' | 'weekly' | 'monthly';

const PERIOD_MULTIPLIERS: Record<Period, number> = {
  daily: 1,
  weekly: 7,
  monthly: 30,
};

function MetricCard({
  label,
  value,
  note,
  valueClassName = '',
  cardClassName = '',
}: {
  label: string;
  value: React.ReactNode;
  note?: React.ReactNode;
  valueClassName?: string;
  cardClassName?: string;
}) {
  // Kit `.earn-kpi-tile` grammar (EarningsShares.jsx KpiTile,
  // styles.css:2476-2495): label / value / unit. Dual-classed with the
  // production `metric-card` hooks so every existing caller, tone class
  // and data binding is preserved while the calm kit tile treatment
  // applies. `accent`/`good`/`bad` value tones map onto the kit tile.
  const tileTone = /\bgreen\b/.test(valueClassName)
    ? 'good'
    : /\bred\b/.test(valueClassName)
      ? 'bad'
      : /\baccent\b/.test(valueClassName)
        ? 'accent'
        : '';
  return (
    <div className={`earn-kpi-tile ${tileTone} metric-card centered ${cardClassName}`.trim()}>
      <div className="earn-kpi-label metric-card-title">{label}</div>
      <div className={`earn-kpi-value metric-card-value ${valueClassName}`.trim()}>{value}</div>
      {note != null && <div className="earn-kpi-unit metric-card-note">{note}</div>}
    </div>
  );
}

export function EarningsPage() {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const hashrateHistory = useMinerStore(s => s.hashrateHistory);
  //  fix (STD-A-01): difficulty was read from heaterStatus, which is only
  // populated in heater mode — so in Standard mode it was always null and the
  // whole profitability page rendered $0 / 0 sats / net-loss while mining. Source
  // it from useNetworkContext (self-polling /api/network/block, the same endpoint
  // CurrentBlockCard uses) so it works in every mode; null → honest 0 / paused.
  const { networkDifficulty } = useNetworkContext();

  const [period, setPeriod] = useState<Period>('daily');
  const [chartPeriod, setChartPeriod] = useState<EarningsPeriod>('24h');
  const [customHashrate, setCustomHashrate] = useState<string>('');
  const [customWatts, setCustomWatts] = useState<string>('');
  // W9.4: source-tagged J/TH headline. Polls /api/perf/efficiency every 15s.
  const [perfEfficiency, setPerfEfficiency] = useState<PerfEfficiencyResponse | null>(null);

  useEffect(() => {
    let cancelled = false;
    const fetchEfficiency = async () => {
      try {
        const r = await api.getPerfEfficiency();
        if (!cancelled) setPerfEfficiency(r);
      } catch {
        // Endpoint may be 404 on older firmware — fall back to live efficiency.
        if (!cancelled) setPerfEfficiency(null);
      }
    };
    fetchEfficiency();
    const id = window.setInterval(fetchEfficiency, 15000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, []);

  const hashrate = status?.hashrate_ghs ?? 0;
  const isMining = hashrate > 0;
  const power = stats?.power;
  const wallWatts = getLiveWallWatts(power);
  // STD-A-09 honesty: tell real power telemetry apart from the standby
  // assumption. When the daemon reports no wall watts we fall back to a nominal
  // ~25 W control-board figure, but that is an ASSUMPTION, not a measurement —
  // it must never be tagged "(live)" or rendered as an authoritative number.
  const wattsFromTelemetry = wallWatts > 0;
  const watts = wattsFromTelemetry ? wallWatts : (status != null ? 25 : 0);
  // W9.4: prefer the source-tagged J/TH from /api/perf/efficiency when
  // available; fall back to the legacy live snapshot for older firmware
  // (which doesn't yet expose the perf endpoint).
  const efficiency =
    perfEfficiency?.j_per_th != null
      ? perfEfficiency.j_per_th
      : getLivePowerEfficiencyJth(stats?.power);
  const efficiencySource = perfEfficiency?.source ?? null;
  const efficiencyConfidence = perfEfficiency?.confidence ?? null;
  const btuH = wattsFromTelemetry && power && 'btu_h' in power && typeof power.btu_h === 'number'
    ? power.btu_h
    : wattsToBtu(watts);

  // Use custom values if provided, otherwise use live data
  const effectiveHashrate = customHashrate ? parseFloat(customHashrate) * 1000 : hashrate; // Input is TH/s
  const effectiveWatts = customWatts ? parseFloat(customWatts) : watts;
  const usingManualInputs = Boolean(customHashrate || customWatts);
  // STD-A-09: the displayed Power Draw is "assumed" only when neither a manual
  // override nor real telemetry is backing it (i.e. the standby ~25 W fallback).
  const wattsAssumed = !customWatts && !wattsFromTelemetry;

  // Live value flashes — fresh-sample confirmation on key headline values.
  const wattsFlashCls = useValueFlash(usingManualInputs ? null : Math.round(watts));
  const hashrateFlashCls = useValueFlash(usingManualInputs ? null : Math.round(hashrate));

  const profit = useMemo(
    () => estimateDailyProfit(effectiveHashrate, effectiveWatts, settings.btcPrice, settings.electricityRate, networkDifficulty),
    [effectiveHashrate, effectiveWatts, settings.btcPrice, settings.electricityRate, networkDifficulty]
  );

  const multiplier = PERIOD_MULTIPLIERS[period];
  const periodSats = profit.sats * multiplier;
  const periodRevenue = profit.revenue * multiplier;
  const periodCost = profit.cost * multiplier;
  const periodProfit = profit.profit * multiplier;

  // Break-even calculation
  const breakEvenDays = useMemo(() => {
    if (profit.profit <= 0) return null;
    // Rough estimate based on miner cost ($50 for S9)
    const minerCost = 50;
    return Math.ceil(minerCost / profit.profit);
  }, [profit.profit]);

  // Break-even BTC price: the price at which mining revenue covers electricity
  const breakEvenBtcPrice = useMemo(() => {
    if (profit.cost <= 0 || effectiveHashrate <= 0) return null;
    // Revenue scales linearly with BTC price, so: breakEvenPrice = currentPrice * (cost / revenue)
    if (profit.revenue <= 0) return null;
    return Math.ceil(settings.btcPrice * (profit.cost / profit.revenue));
  }, [profit.cost, profit.revenue, settings.btcPrice, effectiveHashrate]);

  // W8.3: halving-aware projections.
  // Surface the cliff so users don't plan an ROI horizon against a reward
  // that drops 50% mid-window.
  const halvingInfo = useMemo(() => {
    const nowMs = Date.now();
    const dth = daysToHalving(nowMs);
    const next = nextHalving(nowMs);
    const currentReward = blockRewardAt(nowMs);
    const postFactor = next && currentReward > 0 ? next.rewardBtc / currentReward : 1.0;
    const dailyProfitPostHalving = profit.revenue * postFactor - profit.cost;
    const dailyRevenuePostHalving = profit.revenue * postFactor;
    const breakEvenPricePostHalving =
      profit.cost > 0 && profit.revenue > 0 && postFactor > 0
        ? Math.ceil(settings.btcPrice * (profit.cost / (profit.revenue * postFactor)))
        : null;
    const fourYear = fourYearRevenueWithHalving(profit.revenue, nowMs);
    const fourYearProfit = fourYear.revenueUsd - profit.cost * 365.25 * 4;
    return {
      daysToHalving: dth,
      nextHalvingMs: next?.epochMs ?? null,
      nextRewardBtc: next?.rewardBtc ?? null,
      currentReward,
      postFactor,
      dailyRevenuePostHalving,
      dailyProfitPostHalving,
      breakEvenPricePostHalving,
      fourYearRevenueUsd: fourYear.revenueUsd,
      fourYearProfit,
      preHalvingDays: fourYear.preDays,
      postHalvingDays: fourYear.postDays,
    };
  }, [profit.cost, profit.revenue, settings.btcPrice]);

  const halvingProminent =
    halvingInfo.daysToHalving !== null && halvingInfo.daysToHalving < 365;

  // Uptime-based cumulative estimate
  const uptimeS = status?.uptime_s ?? 0;
  const uptimeHours = uptimeS / 3600;
  const cumulativeSats = useMemo(() => {
    if (!isMining || uptimeHours < 0.01) return 0;
    return Math.round(profit.sats * (uptimeHours / 24));
  }, [profit.sats, uptimeHours, isMining]);

  // Derive an earnings-over-time series for the EarningsChart from the
  // existing hashrate history × the live sats/day estimate. When no
  // hashrate samples exist yet the chart renders its empty state.
  const earningsChartData = useMemo<EarningsPoint[]>(() => {
    if (hashrateHistory.length === 0) return [];
    // Bucket window per period.
    const windowMs = chartPeriod === '24h'
      ? 24 * 3600 * 1000
      : chartPeriod === '7d'
        ? 7 * 24 * 3600 * 1000
        : 30 * 24 * 3600 * 1000;
    const now = Date.now();
    const cutoff = now - windowMs;
    const satsPerSecond = profit.sats / 86400;
    // `hashrateHistory[].time` is stored in SECONDS (store pushRing writes
    // Date.now()/1000). EarningsChart consumes `ts` as MS epoch (it does
    // `new Date(ts)`), and the window cutoff above is MS. Convert each
    // sample's seconds→ms ONCE so the filter, the dt math, and the emitted
    // `ts` are all in the same unit. Without this the chart timestamps were
    // 1000× too small (rendering as 1970) and every sample was filtered out
    // (a seconds value is always < a ms cutoff).
    return hashrateHistory
      .map(p => ({ tsMs: p.time * 1000, value: p.value }))
      .filter(p => p.tsMs >= cutoff)
      .map((p, i, arr) => {
        const prev = i > 0 ? arr[i - 1].tsMs : p.tsMs;
        const dt = Math.max(0, p.tsMs - prev);
        // Cumulative-style: sats accumulated since the previous sample
        // weighted by the current hashrate fraction of the headline rate.
        const fraction = hashrate > 0 ? p.value / hashrate : 1;
        const sats = satsPerSecond * (dt / 1000) * fraction;
        return { ts: p.tsMs, sats };
      });
  }, [hashrateHistory, chartPeriod, profit.sats, hashrate]);

  // Average hashrate from history
  const avgHashrate = useMemo(() => {
    if (hashrateHistory.length === 0) return hashrate;
    return hashrateHistory.reduce((s, p) => s + p.value, 0) / hashrateHistory.length;
  }, [hashrateHistory, hashrate]);

  const avgHr = formatHashrateShort(avgHashrate);
  const liveHr = formatHashrateShort(hashrate);

  // CSV export
  const exportCsv = useCallback(() => {
    const rows = [
      ['Metric', 'Daily', 'Weekly', 'Monthly'],
      ['Sats Earned', profit.sats.toString(), (profit.sats * 7).toString(), (profit.sats * 30).toString()],
      ['Revenue (USD)', `$${profit.revenue.toFixed(2)}`, `$${(profit.revenue * 7).toFixed(2)}`, `$${(profit.revenue * 30).toFixed(2)}`],
      ['Electricity Cost (USD)', `$${profit.cost.toFixed(2)}`, `$${(profit.cost * 7).toFixed(2)}`, `$${(profit.cost * 30).toFixed(2)}`],
      ['Net Profit (USD)', `$${profit.profit.toFixed(2)}`, `$${(profit.profit * 7).toFixed(2)}`, `$${(profit.profit * 30).toFixed(2)}`],
      [''],
      ['Settings'],
      ['Hashrate (GH/s)', effectiveHashrate.toString()],
      ['Power (W)', effectiveWatts.toString()],
      ['Electricity Rate ($/kWh)', settings.electricityRate.toString()],
      ['BTC Price (USD)', settings.btcPrice.toString()],
      ['Efficiency (J/TH)', efficiency > 0 ? efficiency.toFixed(1) : 'N/A'],
    ];
    const csv = rows.map(r => r.join(',')).join('\n');
    const blob = new Blob([csv], { type: 'text/csv' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-earnings-${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  }, [profit, effectiveHashrate, effectiveWatts, settings, efficiency]);

  const profitableTone = periodProfit >= 0 ? 'good' : 'warn';
  const profitableLabel = periodProfit >= 0 ? 'profitable' : 'unprofitable';

  // First-load skeleton: status + stats are both null and we aren't applying
  // manual override inputs yet — there's nothing to compute against.
  if (status == null && stats == null && !usingManualInputs) {
    return <PageSkeleton data-testid="page-skeleton-earnings" />;
  }

  return (
    <div className="page-content">
      <div className="page-hero-strip">
        <div className="page-hero-summary">
          <div className="page-hero-eyebrow">PROFITABILITY</div>
          <div className="page-hero-title">Sats Earned</div>
          <div className="page-hero-stat" data-tooltip={glossaryText('earning_proof')}>
            {formatSats(cumulativeSats)}
          </div>
          <div className="page-hero-substat">
            {isMining
              ? `Session: ${uptimeHours.toFixed(1)}h · live ${liveHr.value} ${liveHr.unit}`
              : 'Standby — manual inputs may be applied below.'}
          </div>
        </div>
        <div className="hero-kpi-strip">
          <div className="hero-kpi">
            <div className="kpi-label">USD ({period})</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">${periodRevenue.toFixed(2)}</span>
            </div>
            <div className="kpi-sub">@ ${settings.btcPrice.toLocaleString()}/BTC</div>
          </div>
          <div
            className="hero-kpi"
            data-tooltip="Estimated daily Bitcoin revenue minus electricity cost, at the BTC price and $/kWh you entered below. An estimate from current conditions — not a guarantee. As a space heater the heat has value you'd pay for anyway."
          >
            <div className="kpi-label">$/day net</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {`${profit.profit >= 0 ? '+' : ''}$${profit.profit.toFixed(2)}`}
              </span>
            </div>
          </div>
          <div
            className="hero-kpi"
            data-tooltip="The electricity price used for the cost estimate. Edit it below to match your utility rate — net profit and breakeven update from this."
          >
            <div className="kpi-label">$/kWh</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">${settings.electricityRate.toFixed(3)}</span>
            </div>
          </div>
          <div className="hero-kpi" data-tooltip={glossaryText('efficiency_jth')}>
            <div className="kpi-label">J/TH</div>
            <div className="kpi-value">
              <span className="kpi-num-anim">
                {efficiency > 0 ? efficiency.toFixed(1) : '—'}
              </span>
            </div>
            {efficiencySource && (
              <div className="kpi-sub">{efficiencySource}</div>
            )}
          </div>
        </div>
      </div>

      <TaskHandoffBanner
        expectedMode="standard"
        title="Profitability task opened from Heater mode"
        copy="Review break-even and cost modeling here, then return to Heat view once you are done comparing comfort and earnings."
      />

      <section className="section">
      <div className="page-toolbar" style={{ marginBottom: 16 }}>
        <div className="section-title" style={{ margin: 0 }}>
          Earnings &amp; Profitability
          <span className={`small-tag ${profitableTone}`}>{profitableLabel}</span>
        </div>
        <div className="page-toolbar-actions">
          {/* Period selector — kit `.earn-range-pill` row
              (EarningsShares.jsx:57-61, styles.css:2497-2504). Dual-classed
              with the production `time-range-tabs`/`time-tab` hooks; wiring
              unchanged. */}
          <div className="time-range-tabs">
            {(['daily', 'weekly', 'monthly'] as Period[]).map(p => (
              <button
                key={p}
                className={`earn-range-pill time-tab ${period === p ? 'active' : ''}`}
                onClick={() => setPeriod(p)}
              >
                {p.charAt(0).toUpperCase() + p.slice(1)}
              </button>
            ))}
          </div>
          <button
            className="btn btn-secondary"
            onClick={exportCsv}
            style={{ padding: '4px 12px', fontSize: '0.75rem' }}
          >
            Export CSV
          </button>
        </div>
      </div>

      {!isMining && !usingManualInputs && (
        <StatePanel
          title="Miner is not hashing right now"
          message="These estimates are using standby control-board power until live hashrate returns. Enter manual values below if you want to model a target profile instead."
          tone="warning"
          compact
        />
      )}

      {usingManualInputs && (
        <StatePanel
          title="Using manual estimate inputs"
          message="Hashrate and/or power values were overridden in the calculator below, so this page is showing projected economics instead of strictly live miner telemetry."
          tone="info"
          compact
        />
      )}

      {/* Main earnings cards */}
      <div className="earn-kpi-strip metric-grid-auto">
        <MetricCard
          label={`${period} Sats`}
          value={formatSats(periodSats)}
          valueClassName="accent hero"
          note={`~${(periodSats / 86400 * (period === 'daily' ? 1 : period === 'weekly' ? 1 / 7 : 1 / 30)).toFixed(2)} sats/sec`}
        />
        <MetricCard
          label="Estimated Revenue"
          value={`$${periodRevenue.toFixed(2)}`}
          valueClassName="green hero"
          note={`@ $${settings.btcPrice.toLocaleString()}/BTC`}
        />
        <MetricCard
          label="Electricity"
          value={`-$${periodCost.toFixed(2)}`}
          valueClassName="red hero"
          note={`$${settings.electricityRate}/kWh`}
        />
        <MetricCard
          label="Estimated Net"
          value={`${periodProfit >= 0 ? '+' : ''}$${periodProfit.toFixed(2)}`}
          valueClassName={`${periodProfit >= 0 ? 'green' : 'red'} hero`}
          cardClassName={periodProfit >= 0 ? 'outline-positive' : 'outline-negative'}
          note={periodProfit >= 0 ? 'Profitable' : 'Unprofitable'}
        />
      </div>

      {/* Efficiency & break-even row */}
      <div className="earn-kpi-strip metric-grid-auto">
        <MetricCard
          label="Hashrate"
          value={isMining ? (
            <span className={hashrateFlashCls}>{liveHr.value} {liveHr.unit}</span>
          ) : 'Standby'}
          valueClassName="mono"
        />
        <MetricCard
          label="Power Draw"
          value={wattsAssumed
            ? '—'
            : <span className={wattsFlashCls}>{formatWatts(effectiveWatts)}</span>}
          valueClassName="mono"
          note={wattsAssumed ? 'standby (assumed)' : undefined}
        />
        <MetricCard
          label="Efficiency"
          value={efficiency > 0 ? (
            <span
              data-testid="efficiency-jth-value"
              data-source={efficiencySource ?? 'unknown'}
              data-confidence={efficiencyConfidence ?? 'unknown'}
              style={{
                fontStyle: efficiencySource === 'model' ? 'italic' : 'normal',
                color:
                  efficiencySource === 'operator' ? 'var(--green)' :
                  efficiencySource === 'pmbus' ? 'var(--accent)' :
                  efficiencySource === 'model' ? 'var(--text-secondary)' :
                  undefined,
              }}
            >
              {efficiency.toFixed(1)} J/TH
            </span>
          ) : 'N/A'}
          valueClassName={`mono ${efficiency > 0 && efficiency < 80 ? 'green' : efficiency > 120 ? 'yellow' : ''}`}
          note={efficiencySource ? (
            <span
              data-testid="efficiency-jth-source-tag"
              data-source={efficiencySource}
              data-confidence={efficiencyConfidence ?? 'unknown'}
              style={{ fontSize: '0.7rem' }}
            >
              {efficiencySource === 'operator' && 'Operator wattmeter'}
              {efficiencySource === 'pmbus' && 'PSU PMBus'}
              {efficiencySource === 'model' && 'Modeled (no wattmeter)'}
              {efficiencyConfidence && efficiencyConfidence !== 'high' && (
                <span style={{ marginLeft: 6, opacity: 0.7 }}>
                  ({efficiencyConfidence})
                </span>
              )}
              {perfEfficiency?.jth_target_active && (
                <span
                  data-testid="efficiency-jth-target-active"
                  style={{
                    marginLeft: 6,
                    padding: '0 4px',
                    borderRadius: 3,
                    background: 'var(--green)',
                    color: '#000',
                    fontWeight: 700,
                  }}
                >
                  JTH
                </span>
              )}
            </span>
          ) : undefined}
        />
        <MetricCard
          label="Break-Even"
          value={breakEvenDays ? `~${breakEvenDays}d` : breakEvenBtcPrice ? `BTC $${(breakEvenBtcPrice / 1000).toFixed(0)}K` : 'N/A'}
          valueClassName={`mono ${breakEvenDays && breakEvenDays < 60 ? 'green' : breakEvenDays ? 'yellow' : 'red'}`}
          note={breakEvenDays ? 'Assumes ~$50 S9-class hardware cost' : breakEvenBtcPrice ? 'Price needed to profit' : undefined}
        />
      </div>

      {/* Heat Value & Break-Even */}
      <div className="earn-kpi-strip metric-grid-auto">
        <MetricCard
          label="Heat Output"
          value={isMining ? formatBtu(btuH) : 'Standby'}
          valueClassName="accent"
          note={isMining ? btuComparison(btuH) : undefined}
        />
        <MetricCard
          label={`Heating Value (${period})`}
          value={isMining ? `$${(estimateHeatingOffset(effectiveWatts, 24, settings.electricityRate) * multiplier).toFixed(2)}` : 'N/A'}
          valueClassName="green"
          note="Equiv. electric heater cost"
        />
        <MetricCard
          label="Break-Even BTC"
          value={breakEvenBtcPrice ? `$${breakEvenBtcPrice.toLocaleString()}` : 'N/A'}
          valueClassName={`mono ${breakEvenBtcPrice && breakEvenBtcPrice < settings.btcPrice ? 'green' : breakEvenBtcPrice ? 'red' : ''}`}
          note={breakEvenBtcPrice && breakEvenBtcPrice < settings.btcPrice
            ? 'Currently profitable'
            : breakEvenBtcPrice
              ? 'BTC price needed to break even'
              : undefined}
        />
        <MetricCard
          label={`Electricity (${period})`}
          value={isMining ? `-$${(estimateDailyCost(effectiveWatts, settings.electricityRate) * multiplier).toFixed(2)}` : 'N/A'}
          valueClassName="mono red"
          note={isMining ? `${((effectiveWatts * 24 / 1000) * multiplier).toFixed(1)} kWh` : undefined}
        />
      </div>

      {/* Earnings over time (sats) — FE-1: this series is a PROJECTION built
          from current hashrate × the live sats/day estimate, NOT a record of
          realized on-chain payouts. Label it honestly so it never implies
          earned/credited Bitcoin. */}
      <div className="section">
        <div
          className="section-title"
          data-tooltip={glossaryText('earnings_projection_series')}
        >
          Projected earnings over time
        </div>
        <div className="page-surface">
          <EarningsChart
            period={chartPeriod}
            data={earningsChartData}
            onPeriodChange={setChartPeriod}
          />
        </div>
        <div className="page-footnote">
          Projected at the current rate — an estimate, not realized on-chain earnings.
        </div>
      </div>

      {/* Cost breakdown section */}
      <div className="section">
        <div className="section-title">Cost Breakdown</div>
        <div className="page-surface">
          <div style={{ display: 'grid', gap: 10 }}>
            {/* Power cost bar */}
            <div>
              <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.8rem', marginBottom: 4 }}>
                <span style={{ color: 'var(--text-secondary)' }}>Electricity ({period})</span>
                <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--red)' }}>
                  ${periodCost.toFixed(2)}
                </span>
              </div>
              <div style={{ height: 6, background: 'var(--bg)', borderRadius: 3, overflow: 'hidden' }}>
                <div style={{
                  height: '100%',
                  width: `${Math.min(100, periodRevenue > 0 ? (periodCost / periodRevenue) * 100 : 100)}%`,
                  background: 'var(--red)',
                  borderRadius: 3,
                  transition: 'width 0.3s',
                }} />
              </div>
            </div>

            {/* Revenue bar */}
            <div>
              <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.8rem', marginBottom: 4 }}>
                <span style={{ color: 'var(--text-secondary)' }}>Mining Revenue ({period})</span>
                <span style={{ fontFamily: "'JetBrains Mono', monospace", color: 'var(--green)' }}>
                  ${periodRevenue.toFixed(2)}
                </span>
              </div>
              <div style={{ height: 6, background: 'var(--bg)', borderRadius: 3, overflow: 'hidden' }}>
                <div style={{
                  height: '100%',
                  width: `${Math.min(100, periodCost > 0 ? (periodRevenue / Math.max(periodRevenue, periodCost)) * 100 : 0)}%`,
                  background: 'var(--green)',
                  borderRadius: 3,
                  transition: 'width 0.3s',
                }} />
              </div>
            </div>

            {/* Profit margin */}
            <div style={{
              borderTop: '1px solid var(--border)', paddingTop: 10,
              display: 'flex', justifyContent: 'space-between', alignItems: 'center',
            }}>
              <span style={{ fontSize: '0.85rem', color: 'var(--text-secondary)' }}>Profit Margin</span>
              <span style={{
                fontFamily: "var(--font-heading)",
                fontWeight: 700, fontSize: '1.1rem',
                color: periodProfit >= 0 ? 'var(--green)' : 'var(--red)',
              }}>
                {periodRevenue > 0
                  ? (() => {
                      const margin = (periodProfit / periodRevenue) * 100;
                      if (margin < -999) return '<-999%';
                      return `${margin.toFixed(1)}%`;
                    })()
                  : 'N/A'
                }
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Cumulative session earnings */}
      {isMining && (
        <div className="section">
          <div className="section-title">Session Totals</div>
          <div className="earn-kpi-strip metric-grid-auto">
            <MetricCard
              label="Session Sats (est.)"
              value={formatSats(cumulativeSats)}
              valueClassName="accent"
            />
            <MetricCard
              label="Session Duration"
              value={uptimeHours >= 24
                ? `${Math.floor(uptimeHours / 24)}d ${Math.floor(uptimeHours % 24)}h`
                : `${uptimeHours.toFixed(1)}h`}
              valueClassName="mono"
            />
            <MetricCard
              label="Avg Hashrate"
              value={`${avgHr.value} ${avgHr.unit}`}
              valueClassName="mono"
            />
          </div>
        </div>
      )}

      {/* Profitability calculator */}
      <div className="section">
        <div className="section-title">Profitability Calculator</div>
        <div className="page-surface">
          <div className="standard-grid-2" style={{ gap: 12 }}>
            <div>
              <label className="field-label">
                Hashrate (TH/s) {isMining && !customHashrate && <span style={{ color: 'var(--green)' }}>(live)</span>}
              </label>
              <input
                type="number"
                step="0.1"
                min="0"
                placeholder={isMining ? (hashrate / 1000).toFixed(2) : '13.5'}
                value={customHashrate}
                onChange={e => setCustomHashrate(e.target.value)}
                aria-label="Calculator hashrate in TH/s"
              />
            </div>
            <div>
              <label className="field-label">
                Power (Watts) {wattsFromTelemetry && !customWatts && <span style={{ color: 'var(--green)' }}>(live)</span>}
              </label>
              <input
                type="number"
                step="10"
                min="0"
                placeholder={watts > 0 ? watts.toString() : '1350'}
                value={customWatts}
                onChange={e => setCustomWatts(e.target.value)}
                aria-label="Calculator power in watts"
              />
            </div>
            <div>
              <label className="field-label">
                Electricity Rate ($/kWh)
              </label>
              <input
                type="number"
                step="0.01"
                min="0"
                value={settings.electricityRate}
                onChange={e => updateSettings({ electricityRate: Number(e.target.value) })}
                aria-label="Electricity rate in dollars per kilowatt-hour"
              />
            </div>
            <div>
              <label className="field-label">
                BTC Price (USD)
              </label>
              {/* P1-8 (§4.E): manual BTC price is always editable — the
                  removed "Auto-fetch from mempool.space" toggle used to disable
                  this field while nothing populated it (dead control). */}
              <input
                type="number"
                step="100"
                min="0"
                value={settings.btcPrice}
                onChange={e => updateSettings({ btcPrice: Number(e.target.value) })}
                aria-label="BTC price in USD"
              />
            </div>
          </div>
          {(customHashrate || customWatts) && (
            <button
              className="btn btn-secondary"
              onClick={() => { setCustomHashrate(''); setCustomWatts(''); }}
              style={{ marginTop: 12, padding: '6px 12px', fontSize: '0.8rem' }}
            >
              Reset to Live Values
            </button>
          )}
        </div>
      </div>

      {/* W8.3: Halving impact + 4-year amortization */}
      {isMining && halvingInfo.daysToHalving !== null && (
        <div className="section">
          <div
            className="section-title"
            style={{
              display: 'flex',
              alignItems: 'center',
              gap: 8,
            }}
          >
            <span>Halving impact</span>
            {halvingProminent && (
              <span
                style={{
                  fontSize: '0.65rem',
                  fontWeight: 700,
                  letterSpacing: 0.5,
                  textTransform: 'uppercase',
                  padding: '2px 8px',
                  borderRadius: 4,
                  background: 'var(--accent)',
                  color: '#000',
                }}
                title="Less than a year until the next halving — plan accordingly"
              >
                {`<${Math.ceil(halvingInfo.daysToHalving)}d`}
              </span>
            )}
          </div>
          <div className="page-surface">
            <div style={{ marginBottom: 10, fontSize: '0.85rem', color: 'var(--text-secondary)' }}>
              Current block reward: <strong>{halvingInfo.currentReward.toFixed(5)} BTC</strong>
              {halvingInfo.nextRewardBtc != null && (
                <>
                  {' · '}
                  Post-halving:{' '}
                  <strong style={{ color: 'var(--red, #EF4444)' }}>
                    {halvingInfo.nextRewardBtc.toFixed(5)} BTC
                  </strong>{' '}
                  in <strong>{Math.ceil(halvingInfo.daysToHalving)} days</strong>
                </>
              )}
            </div>
            <div className="earn-kpi-strip metric-grid-auto">
              <MetricCard
                label="Post-halving daily revenue"
                value={`$${halvingInfo.dailyRevenuePostHalving.toFixed(2)}/day`}
                valueClassName="hero"
                note={`${(halvingInfo.postFactor * 100).toFixed(0)}% of current`}
              />
              <MetricCard
                label="Post-halving daily net"
                value={`${halvingInfo.dailyProfitPostHalving >= 0 ? '+' : ''}$${halvingInfo.dailyProfitPostHalving.toFixed(2)}/day`}
                valueClassName={`${halvingInfo.dailyProfitPostHalving >= 0 ? 'green' : 'red'} hero`}
                cardClassName={halvingInfo.dailyProfitPostHalving >= 0 ? 'outline-positive' : 'outline-negative'}
                note={halvingInfo.dailyProfitPostHalving >= 0 ? 'Still profitable' : 'Unprofitable post-halving'}
              />
              <MetricCard
                label="Post-halving break-even BTC"
                value={
                  halvingInfo.breakEvenPricePostHalving
                    ? `$${halvingInfo.breakEvenPricePostHalving.toLocaleString()}`
                    : 'N/A'
                }
                valueClassName="mono"
                note="Price needed at next reward"
              />
              <MetricCard
                label="4-year amortization"
                value={`$${halvingInfo.fourYearRevenueUsd.toFixed(0)}`}
                valueClassName="mono"
                note={`${halvingInfo.preHalvingDays.toFixed(0)}d full · ${halvingInfo.postHalvingDays.toFixed(0)}d post`}
              />
            </div>
          </div>
        </div>
      )}

      {/* Disclaimer */}
      <div className="page-footnote">
        Earnings estimates are approximate. Actual revenue depends on network difficulty, pool luck, and block rewards.
        Halving dates and post-halving rewards are estimates based on the current block production rate.
      </div>
      </section>
    </div>
  );
}
