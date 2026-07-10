// TOU (Time-of-Use) Rate Scheduler — Visual weekly grid for electricity rate scheduling
// Feature no competitor has: drag-paint schedule with per-tier mining behavior

import React, { useState, useCallback, useRef } from 'react';
import type { RateTier, MiningBehavior, TouTierConfig, TouScheduleBlock } from '../../api/feature-types';
import { api } from '../../api/client';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { InfoDot } from '../common/Tooltip';
import { getLiveWallWatts } from '../../utils/power';

const PLANNING_LOAD_WATTS = 1100;

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
const HOURS = Array.from({ length: 24 }, (_, i) => i);
const TIER_COLORS: Record<RateTier, string> = {
  'off-peak': 'var(--feat-green)',
  'mid-peak': 'var(--feat-yellow)',
  'on-peak': 'var(--feat-red)',
};
const TIER_BG: Record<RateTier, string> = {
  'off-peak': 'var(--feat-green-dim)',
  'mid-peak': 'var(--feat-yellow-dim)',
  'on-peak': 'var(--feat-red-dim)',
};

const DEFAULT_TIERS: TouTierConfig[] = [
  { tier: 'off-peak', rate: 0.06, behavior: 'full' },
  { tier: 'mid-peak', rate: 0.10, behavior: 'reduced' },
  { tier: 'on-peak', rate: 0.18, behavior: 'sleep' },
];

function buildDefaultSchedule(): TouScheduleBlock[] {
  const blocks: TouScheduleBlock[] = [];
  for (let day = 0; day < 7; day++) {
    for (let hour = 0; hour < 24; hour++) {
      // Weekends all off-peak, weekdays: off-peak 19-7, mid-peak 7-11 & 17-19, on-peak 11-17
      let tier: RateTier = 'off-peak';
      if (day >= 1 && day <= 5) {
        if (hour >= 11 && hour < 17) tier = 'on-peak';
        else if ((hour >= 7 && hour < 11) || (hour >= 17 && hour < 19)) tier = 'mid-peak';
      }
      blocks.push({ day, hour, tier });
    }
  }
  return blocks;
}

export function TouScheduler() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);
  const settings = useMinerStore(s => s.settings);

  const [enabled, setEnabled] = useState(false);
  const [schedule, setSchedule] = useState<TouScheduleBlock[]>(buildDefaultSchedule);
  const [tiers, setTiers] = useState<TouTierConfig[]>(DEFAULT_TIERS);
  const [activeBrush, setActiveBrush] = useState<RateTier>('off-peak');
  const [painting, setPainting] = useState(false);
  const gridRef = useRef<HTMLDivElement>(null);

  const getTier = useCallback((day: number, hour: number): RateTier => {
    const block = schedule.find(b => b.day === day && b.hour === hour);
    return block?.tier ?? 'off-peak';
  }, [schedule]);

  const paintCell = useCallback((day: number, hour: number) => {
    setSchedule(prev => prev.map(b =>
      b.day === day && b.hour === hour ? { ...b, tier: activeBrush } : b
    ));
  }, [activeBrush]);

  const handleMouseDown = (day: number, hour: number) => {
    setPainting(true);
    paintCell(day, hour);
  };

  const handleMouseEnter = (day: number, hour: number) => {
    if (painting) paintCell(day, hour);
  };

  const handleMouseUp = () => setPainting(false);

  const updateTierRate = (tier: RateTier, rate: number) => {
    setTiers(prev => prev.map(t => t.tier === tier ? { ...t, rate } : t));
  };

  const updateTierBehavior = (tier: RateTier, behavior: MiningBehavior) => {
    setTiers(prev => prev.map(t => t.tier === tier ? { ...t, behavior } : t));
  };

  // Estimate daily cost: count hours per tier, multiply by rate * watts
  const liveWatts = useMinerStore(s => getLiveWallWatts(s.stats?.power));
  const wattsIsLive = liveWatts > 0;
  const watts = wattsIsLive ? liveWatts : PLANNING_LOAD_WATTS;
  const estimateDailyCost = (): number => {
    // Average over 7 days
    let totalCost = 0;
    for (const tierConfig of tiers) {
      const hoursPerWeek = schedule.filter(b => b.tier === tierConfig.tier).length;
      const avgHoursPerDay = hoursPerWeek / 7;
      const powerMultiplier = tierConfig.behavior === 'full' ? 1.0
        : tierConfig.behavior === 'reduced' ? 0.5 : 0.02; // sleep = ~25W
      totalCost += avgHoursPerDay * (watts * powerMultiplier / 1000) * tierConfig.rate;
    }
    return totalCost;
  };

  // behavior → target_watts for the daemon TOU schedule. The backend
  // (rest.rs hour_in_schedule_slot + the slot consumer) treats target_watts<=0
  // as the NO-CAP sentinel (it skips the slot → mining runs at full power) and
  // any positive value as a power cap. So 'full' maps to 0 (no cap, distinct
  // from 'sleep') and 'sleep' maps to a real low curtailment target (~25 W),
  // making the two behaviors distinguishable in the saved schedule.
  const behaviorToWatts = (behavior: MiningBehavior): number =>
    behavior === 'full' ? 0
      : behavior === 'reduced' ? 800
      : 25;

  const handleSave = async () => {
    // The daemon TOU schedule is hour-of-day only — there is no day-of-week
    // dimension in /api/tou/schedule (see hour_in_schedule_slot() in rest.rs).
    // We therefore collapse the painted 7-day grid into a single 24-hour
    // profile using the most-common tier per hour across all painted days,
    // then emit ONE slot per contiguous same-tier block — faithful to the
    // grid's block structure within the backend's hour-of-day model (no
    // invented per-day backend). 'full' (no-cap) blocks are not emitted: they
    // are exactly the gaps the daemon already runs at full power between slots.
    const behaviorFor = (tier: RateTier): MiningBehavior =>
      tiers.find(tc => tc.tier === tier)?.behavior ?? 'full';

    const tierAt = (day: number, hour: number): RateTier =>
      schedule.find(b => b.day === day && b.hour === hour)?.tier ?? 'off-peak';

    // Most-common tier per hour-of-day across all 7 painted days.
    const hourTier: RateTier[] = HOURS.map(hour => {
      const counts: Record<RateTier, number> = { 'off-peak': 0, 'mid-peak': 0, 'on-peak': 0 };
      for (let day = 0; day < 7; day++) counts[tierAt(day, hour)] += 1;
      return (['off-peak', 'mid-peak', 'on-peak'] as RateTier[]).reduce(
        (best, tier) => (counts[tier] > counts[best] ? tier : best),
        'off-peak' as RateTier,
      );
    });

    // Walk hours 0..23, emitting one slot per contiguous same-tier run whose
    // behavior actually caps power (target_watts > 0).
    const slots: { start_hour: number; end_hour: number; target_watts: number; label: string }[] = [];
    let blockStart = 0;
    for (let hour = 1; hour <= 24; hour++) {
      if (hour === 24 || hourTier[hour] !== hourTier[blockStart]) {
        const tier = hourTier[blockStart];
        const watts = behaviorToWatts(behaviorFor(tier));
        if (watts > 0) {
          slots.push({
            start_hour: blockStart,
            end_hour: hour % 24,
            target_watts: watts,
            label: tier,
          });
        }
        blockStart = hour;
      }
    }

    try {
      const data = await api.saveTouSchedule({
        enabled,
        slots,
        timezone_offset_hours: new Date().getTimezoneOffset() / -60,
        ramp_duration_s: 60,
      });
      addAlert('info', data.message || 'TOU schedule saved.');
    } catch {
      addAlert('warning', 'Failed to save TOU schedule.');
    }
  };

  const behaviorLabel = (b: MiningBehavior): string => {
    if (b === 'full') return t('tou.full');
    if (b === 'reduced') return t('tou.reduced');
    return t('tou.sleep');
  };

  const tierLabel = (tier: RateTier): string => {
    if (tier === 'off-peak') return t('tou.offPeak');
    if (tier === 'mid-peak') return t('tou.midPeak');
    return t('tou.onPeak');
  };

  const touEnabledLabelId = 'tou-enabled-label';

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title">
          {t('tou.title')}
          <InfoDot
            placement="bottom"
            label="What time-of-use scheduling does"
            content={
              <>
                Paint a weekly grid of when your electricity is cheap (off-peak),
                medium (mid-peak), or expensive (on-peak). DCENT_OS then mines
                full-speed on cheap power and backs off or sleeps during pricey
                hours — so the heater earns most when power is cheapest. Drag to
                paint blocks with the selected rate tier.
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('tou.subtitle')}</p>
      </div>

      {/* Enable toggle */}
      <div className="feat-card">
        <label className="feat-toggle-row">
          <span className="feat-toggle-label" id={touEnabledLabelId}>{t('common.enabled')}</span>
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            aria-labelledby={touEnabledLabelId}
            className={`feat-toggle ${enabled ? 'active' : ''}`}
            onClick={() => setEnabled(!enabled)}
          >
            <span className="feat-toggle-knob" />
          </button>
        </label>
      </div>

      {/* Tier Configuration */}
      <div className="feat-card">
        <h3 className="feat-card-title">Rate Tiers</h3>
        <div className="feat-tier-grid">
          {tiers.map(tierConfig => (
            <div key={tierConfig.tier} className="feat-tier-row">
              <div
                className="feat-tier-badge"
                style={{ background: TIER_BG[tierConfig.tier], color: TIER_COLORS[tierConfig.tier] }}
              >
                {tierConfig.tier === 'off-peak' ? t('tou.offPeak')
                  : tierConfig.tier === 'mid-peak' ? t('tou.midPeak')
                  : t('tou.onPeak')}
              </div>
              <div className="feat-tier-inputs">
                <div className="feat-input-group">
                  <label className="feat-label-sm">{t('tou.rate')}</label>
                  <input
                    type="number"
                    step="0.01"
                    min="0"
                    value={tierConfig.rate}
                    onChange={e => updateTierRate(tierConfig.tier, Number(e.target.value))}
                    className="feat-input feat-input-sm"
                    aria-label={`${tierConfig.tier} electricity rate`}
                  />
                </div>
                <div className="feat-input-group">
                  <label className="feat-label-sm">{t('tou.behavior')}</label>
                  <select
                    value={tierConfig.behavior}
                    onChange={e => updateTierBehavior(tierConfig.tier, e.target.value as MiningBehavior)}
                    className="feat-input feat-input-sm"
                    aria-label={`${tierConfig.tier} mining behavior`}
                  >
                    <option value="full">{t('tou.full')}</option>
                    <option value="reduced">{t('tou.reduced')}</option>
                    <option value="sleep">{t('tou.sleep')}</option>
                  </select>
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Brush selector */}
      <div className="feat-card">
        <h3 className="feat-card-title">{t('tou.weeklySchedule')}</h3>
        <p className="feat-hint">{t('tou.clickDrag')}</p>

        <div className="feat-brush-row">
          {(['off-peak', 'mid-peak', 'on-peak'] as RateTier[]).map(tier => (
              <button
                type="button"
                key={tier}
                className={`feat-brush-btn ${activeBrush === tier ? 'active' : ''}`}
                aria-pressed={activeBrush === tier}
                aria-label={`Brush: ${tierLabel(tier)}`}
                style={{
                borderColor: activeBrush === tier ? TIER_COLORS[tier] : 'var(--border)',
                background: activeBrush === tier ? TIER_BG[tier] : 'var(--card-bg)',
                color: activeBrush === tier ? TIER_COLORS[tier] : 'var(--text-secondary)',
              }}
              onClick={() => setActiveBrush(tier)}
            >
              <span className="feat-brush-dot" style={{ background: TIER_COLORS[tier] }} />
              {tier === 'off-peak' ? t('tou.offPeak')
                : tier === 'mid-peak' ? t('tou.midPeak')
                : t('tou.onPeak')}
            </button>
          ))}
        </div>

        {/* Schedule grid */}
        <div
          className="feat-schedule-grid"
          ref={gridRef}
          onMouseUp={handleMouseUp}
          onMouseLeave={handleMouseUp}
        >
          {/* Hour headers */}
          <div className="feat-grid-header">
            <div className="feat-grid-label" />
            {HOURS.map(h => (
              <div key={h} className="feat-grid-hour">
                {h.toString().padStart(2, '0')}
              </div>
            ))}
          </div>

          {/* Day rows */}
          {DAYS.map((dayName, dayIdx) => (
            <div key={dayIdx} className="feat-grid-row">
              <div className="feat-grid-label">{dayName}</div>
              {HOURS.map(hour => {
                const tier = getTier(dayIdx, hour);
                return (
                    <div
                      key={hour}
                      className="feat-grid-cell"
                      role="button"
                      tabIndex={0}
                      aria-label={`${dayName} ${hour.toString().padStart(2, '0')}:00. Current tier ${tierLabel(tier)}. Apply ${tierLabel(activeBrush)}.`}
                      aria-pressed={tier === activeBrush}
                      style={{ background: TIER_COLORS[tier] }}
                      onMouseDown={() => handleMouseDown(dayIdx, hour)}
                      onMouseEnter={() => handleMouseEnter(dayIdx, hour)}
                      onKeyDown={e => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault();
                          paintCell(dayIdx, hour);
                        }
                      }}
                      onFocus={() => {
                        if (painting) {
                          paintCell(dayIdx, hour);
                        }
                      }}
                    />
                );
              })}
            </div>
          ))}
        </div>

        {/* Honest-state note (STD-B-02): this grid is an editable template that
            always starts from the default rate plan on load. The daemon stores
            an hour-of-day power schedule (no day-of-week dimension), so a
            previously saved schedule is NOT loaded back into this weekly
            painting — pressing Save overwrites the active daemon schedule with
            what is shown here. */}
        <p className="feat-hint" style={{ marginTop: 10 }}>
          This grid is an editable template seeded from the default rate plan — it does
          not reflect the schedule currently running on the miner. The daemon stores an
          hour-of-day schedule, so a saved schedule is not re-loaded here. Saving overwrites
          the active schedule with the grid above.
        </p>
      </div>

      {/* Cost Preview */}
      <div className="feat-card feat-cost-preview">
        <div className="feat-cost-row">
          <span className="feat-cost-label">
            {wattsIsLive ? t('tou.estimatedDailyCost') : 'Planning Daily Cost'}
          </span>
          <span className="feat-cost-value">${estimateDailyCost().toFixed(2)}</span>
        </div>
        <div className="feat-cost-row">
          <span className="feat-cost-label">
            {wattsIsLive ? 'Monthly Estimate' : 'Planning Monthly Cost'}
          </span>
          <span className="feat-cost-value">${(estimateDailyCost() * 30).toFixed(2)}</span>
        </div>
        {!wattsIsLive && (
          <p className="feat-hint">
            Planning cost uses a 1,100 W load assumption because live wall-power telemetry
            is unavailable; it is not a measured current draw.
          </p>
        )}
        <div className="feat-cost-detail">
          {tiers.map(tierConfig => {
            const hoursPerWeek = schedule.filter(b => b.tier === tierConfig.tier).length;
            return (
              <div key={tierConfig.tier} className="feat-cost-tier">
                <span
                  className="feat-cost-dot"
                  style={{ background: TIER_COLORS[tierConfig.tier] }}
                />
                <span>{hoursPerWeek}h/wk @ ${tierConfig.rate}/kWh</span>
                <span className="feat-cost-behavior">{behaviorLabel(tierConfig.behavior)}</span>
              </div>
            );
          })}
        </div>
      </div>

      {/* Save */}
      <div className="feat-actions">
        <button type="button" className="feat-btn feat-btn-primary" onClick={handleSave}>
          {t('common.save')}
        </button>
      </div>
    </div>
  );
}
