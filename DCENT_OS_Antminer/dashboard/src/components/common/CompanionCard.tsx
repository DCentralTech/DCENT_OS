import React, { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useMinerStore } from '../../store/miner';
import { useDashboardHealth } from '../../hooks/useDashboardHealth';
import { formatHashrate, formatUptime } from '../../utils/format';
import { getLivePowerEfficiencyJth, getLiveWallWatts } from '../../utils/power';

interface CompanionContext {
  hasStatus: boolean;
  isMining: boolean;
  hashrateGhs: number;
  accepted: number;
  rejected: number;
  uptimeS: number;
  rejectRate: number;
  maxTempC: number;
  chainCount: number;
  healthyChains: number;
  allChainsHealthy: boolean;
  poolConnected: boolean;
  wallWatts: number;
  efficiencyJth: number;
  autotunerComplete: boolean;
}

interface CompanionAchievement {
  id: string;
  name: string;
  hint: string;
  test: (ctx: CompanionContext) => boolean;
}

interface CompanionSave {
  achievements: string[];
  sharesSeen: number;
  lastAccepted: number;
}

const FACES = ['(x_x)', '(;_;)', '(-_-)', '(._.)', '(o_o)', '(^_^)', '(^o^)', '(*_*)', '(>v<)', '(\\o/)', '(\\o/)'];

const ACHIEVEMENTS: CompanionAchievement[] = [
  { id: 'first-share', name: 'First Share', hint: 'Accept one pool share.', test: c => c.accepted >= 1 },
  { id: 'pool-locked', name: 'Pool Locked', hint: 'Maintain an active pool connection.', test: c => c.poolConnected },
  { id: 'full-chain', name: 'Full Chain', hint: 'All detected chains are healthy.', test: c => c.chainCount > 0 && c.allChainsHealthy },
  { id: 'room-heater', name: 'Room Heater', hint: 'Reach 500 W live at the wall.', test: c => c.wallWatts >= 500 },
  { id: 'terahash', name: 'Terahash Club', hint: 'Cross 1 TH/s.', test: c => c.hashrateGhs >= 1000 },
  { id: 'cool-run', name: 'Cool Run', hint: 'Mine while staying under 65 C.', test: c => c.isMining && c.maxTempC > 0 && c.maxTempC < 65 },
  { id: 'clean-50', name: 'Clean 50', hint: 'Accept 50 shares with no rejects.', test: c => c.accepted >= 50 && c.rejected === 0 },
  { id: 'night-shift', name: 'Night Shift', hint: 'Stay up for 8 hours.', test: c => c.uptimeS >= 28_800 },
  { id: 'marathon', name: 'Marathon', hint: 'Stay up for 24 hours.', test: c => c.uptimeS >= 86_400 },
  { id: 'efficiency-burn', name: 'Efficiency Burn', hint: 'Run under 85 J/TH with live wall power.', test: c => c.efficiencyJth > 0 && c.efficiencyJth < 85 },
  { id: 'silicon-whisperer', name: 'Silicon Whisperer', hint: 'Complete an autotuner run.', test: c => c.autotunerComplete },
  { id: 'hot-stuff', name: 'Hot Stuff', hint: 'Survive a 65 C+ thermal event.', test: c => c.maxTempC >= 65 },
];

const EMPTY_SAVE: CompanionSave = {
  achievements: [],
  sharesSeen: 0,
  lastAccepted: 0,
};

function clamp(n: number, min: number, max: number) {
  return Math.min(max, Math.max(min, n));
}

function readSave(storageKey: string): CompanionSave {
  if (typeof window === 'undefined') return EMPTY_SAVE;

  try {
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return EMPTY_SAVE;
    const parsed = JSON.parse(raw) as Partial<CompanionSave>;
    return {
      achievements: Array.isArray(parsed.achievements)
        ? parsed.achievements.filter((id): id is string => typeof id === 'string')
        : [],
      sharesSeen: Number.isFinite(parsed.sharesSeen) ? Number(parsed.sharesSeen) : 0,
      lastAccepted: Number.isFinite(parsed.lastAccepted) ? Number(parsed.lastAccepted) : 0,
    };
  } catch {
    return EMPTY_SAVE;
  }
}

function writeSave(storageKey: string, save: CompanionSave) {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(storageKey, JSON.stringify(save));
  } catch {
    // Companion progress is cosmetic; never break dashboard telemetry if storage is unavailable.
  }
}

function sameSave(a: CompanionSave, b: CompanionSave) {
  return a.lastAccepted === b.lastAccepted
    && a.sharesSeen === b.sharesSeen
    && a.achievements.length === b.achievements.length
    && a.achievements.every((id, index) => id === b.achievements[index]);
}

function formatCompactCount(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`;
  return value.toLocaleString();
}

function stageFor(sharesSeen: number, uptimeS: number) {
  if (sharesSeen >= 5_000 || uptimeS >= 604_800) return 'Legend';
  if (sharesSeen >= 1_000 || uptimeS >= 86_400) return 'Veteran';
  if (sharesSeen >= 100 || uptimeS >= 28_800) return 'Miner';
  if (sharesSeen >= 1 || uptimeS >= 3_600) return 'Hatchling';
  return 'Egg';
}

function isPoolConnected(status: string) {
  const normalized = status.toLowerCase();
  return normalized === 'connected' || normalized === 'alive' || normalized === 'active';
}

function isPoolDisconnected(status: string) {
  const normalized = status.toLowerCase();
  return normalized === 'disconnected' || normalized === 'dead' || normalized === 'offline';
}

function moodMessage({
  ctx,
  telemetryRecent,
  poolStatus,
  model,
}: {
  ctx: CompanionContext;
  telemetryRecent: boolean;
  poolStatus: string;
  model: string;
}) {
  if (!telemetryRecent) return 'Waiting for miner telemetry. I will wake up when DCENT_OS reports in.';
  if (!ctx.hasStatus) return 'Booting my mining brain. Give me a status packet.';
  if (!ctx.isMining) return 'Resting quietly. Wake me when the room needs heat.';
  if (ctx.maxTempC >= 70) return 'Too hot. Check airflow, fans, and the board path before pushing harder.';
  if (isPoolDisconnected(poolStatus)) return 'I have hash, but no pool to feed. Check pool reachability.';
  if (ctx.rejectRate >= 5) return 'Shares are bouncing. Pool difficulty or network stability needs attention.';
  if (ctx.allChainsHealthy && ctx.accepted > 0) return `Eating SHA256 on ${model} and keeping the room warm.`;
  if (ctx.poolConnected) return 'Pool is locked. I am watching for the next accepted share.';
  return 'Mining loop is alive. Waiting for the pool to settle.';
}

export function CompanionCard({ className = '' }: { className?: string }) {
  const status = useMinerStore(s => s.status);
  const stats = useMinerStore(s => s.stats);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const settings = useMinerStore(s => s.settings);
  const autotunerStatus = useMinerStore(s => s.autotunerStatus);
  const addToast = useMinerStore(s => s.addToast);
  const health = useDashboardHealth();

  const storageId = systemInfo?.mac || systemInfo?.hostname || 'local';
  const storageKey = `dcentos-companion-v1:${storageId}`;
  const [save, setSave] = useState<CompanionSave>(() => readSave(storageKey));
  const [loadedStorageKey, setLoadedStorageKey] = useState(storageKey);
  const initializedRef = useRef(false);

  useEffect(() => {
    setSave(readSave(storageKey));
    setLoadedStorageKey(storageKey);
    initializedRef.current = false;
  }, [storageKey]);

  const chains = status?.chains ?? [];
  const hashrateGhs = status?.hashrate_ghs ?? 0;
  const accepted = status?.accepted ?? 0;
  const rejected = status?.rejected ?? 0;
  const totalShares = accepted + rejected;
  const rejectRate = totalShares > 0 ? (rejected / totalShares) * 100 : 0;
  const temps = chains.map(chain => chain.temp_c).filter(temp => temp > 0);
  const maxTempC = temps.length > 0 ? Math.max(...temps) : 0;
  const healthyChains = chains.filter(chain => {
    const chainStatus = (chain.status || '').toLowerCase();
    return chain.chips > 0 && !/(dead|error|missing|offline)/.test(chainStatus);
  }).length;
  const allChainsHealthy = chains.length > 0 && healthyChains === chains.length;
  const poolStatus = status?.pool?.status ?? '';
  const poolConnected = isPoolConnected(poolStatus);
  const power = stats?.power ?? status?.power;
  const wallWatts = getLiveWallWatts(power);
  const efficiencyJth = getLivePowerEfficiencyJth(power);
  const autotunerState = `${autotunerStatus?.state ?? ''} ${autotunerStatus?.phase ?? ''}`.toLowerCase();
  const autotunerComplete = autotunerStatus?.percent_complete === 100
    || /(complete|completed|done|stable)/.test(autotunerState);

  const context: CompanionContext = {
    hasStatus: status != null,
    isMining: hashrateGhs > 0,
    hashrateGhs,
    accepted,
    rejected,
    uptimeS: status?.uptime_s ?? systemInfo?.uptime_s ?? 0,
    rejectRate,
    maxTempC,
    chainCount: chains.length,
    healthyChains,
    allChainsHealthy,
    poolConnected,
    wallWatts,
    efficiencyJth,
    autotunerComplete,
  };

  const earnedAchievementIds = ACHIEVEMENTS
    .filter(achievement => achievement.test(context))
    .map(achievement => achievement.id);
  const earnedKey = earnedAchievementIds.join('|');

  useEffect(() => {
    if (!status) return;
    if (loadedStorageKey !== storageKey) return;

    const achievementSet = new Set([...save.achievements, ...earnedAchievementIds]);
    const nextAchievements = ACHIEVEMENTS
      .map(achievement => achievement.id)
      .filter(id => achievementSet.has(id));
    let sharesSeen = save.sharesSeen;

    if (accepted >= save.lastAccepted) {
      sharesSeen += accepted - save.lastAccepted;
    } else if (accepted > 0) {
      sharesSeen += accepted;
    }

    const next = {
      achievements: nextAchievements,
      sharesSeen,
      lastAccepted: accepted,
    };

    if (!sameSave(save, next)) {
      if (initializedRef.current && next.achievements.length > save.achievements.length) {
        const unlocked = ACHIEVEMENTS.find(achievement => !save.achievements.includes(achievement.id) && next.achievements.includes(achievement.id));
        addToast(`Companion achievement unlocked: ${unlocked?.name ?? 'New milestone'}`, 'success');
      }

      writeSave(storageKey, next);
      setSave(next);
    }

    initializedRef.current = true;
  }, [accepted, earnedKey, status, storageKey, loadedStorageKey, addToast, save]);

  const moodScore = useMemo(() => {
    if (!health.hasRecentTelemetry) return 1;

    let mood = 5;
    if (context.isMining) mood += 2;
    else mood -= 1;
    if (context.poolConnected) mood += 1;
    else if (isPoolDisconnected(poolStatus) && context.isMining) mood -= 2;
    if (context.accepted >= 10 && context.rejectRate === 0) mood += 1;
    else if (context.rejectRate >= 5) mood -= 2;
    else if (context.rejectRate >= 1) mood -= 1;
    if (context.maxTempC > 0 && context.maxTempC < 65) mood += 1;
    else if (context.maxTempC >= 70) mood -= 3;
    else if (context.maxTempC >= 65) mood -= 1;
    if (context.allChainsHealthy) mood += 1;
    if (context.autotunerComplete) mood += 1;
    return clamp(Math.round(mood), 0, 10);
  }, [context.accepted, context.allChainsHealthy, context.autotunerComplete, context.isMining, context.maxTempC, context.poolConnected, context.rejectRate, health.hasRecentTelemetry, poolStatus]);

  const unlocked = new Set(save.achievements);
  const stage = stageFor(save.sharesSeen, context.uptimeS);
  const moodTone = !health.hasRecentTelemetry ? 'mood-offline' : moodScore >= 7 ? 'mood-happy' : moodScore >= 4 ? 'mood-steady' : 'mood-sad';
  const face = FACES[clamp(moodScore, 0, FACES.length - 1)];
  const model = systemInfo?.model?.replace(/^Antminer \(([^)]+)\)$/i, '$1') || 'miner';
  const companionName = settings.minerName && settings.minerName !== 'My Miner'
    ? settings.minerName
    : systemInfo?.hostname || 'DCENT Miner';
  const message = moodMessage({
    ctx: context,
    telemetryRecent: health.hasRecentTelemetry,
    poolStatus,
    model,
  });

  const statusStats = [
    { label: 'Shares Seen', value: formatCompactCount(save.sharesSeen || accepted) },
    { label: 'Hashrate', value: context.isMining ? formatHashrate(hashrateGhs) : 'Idle' },
    { label: 'Uptime', value: context.uptimeS > 0 ? formatUptime(context.uptimeS) : 'Standby' },
    { label: 'Temp', value: maxTempC > 0 ? `${maxTempC.toFixed(0)} C` : '--' },
  ];

  return (
    <section className={`companion-card ${moodTone} ${className}`.trim()} aria-label="Miner companion">
      <div className="companion-ambient" aria-hidden="true" />
      <div className="companion-header">
        <div>
          <div className="companion-eyebrow">Companion</div>
          <h3 className="companion-name">{companionName}</h3>
        </div>
        <span className="companion-stage">{stage}</span>
      </div>

      <div className="companion-main">
        <div className="companion-face-shell">
          <div className="companion-face" aria-label={`Companion mood ${moodScore} out of 10`}>
            {face}
          </div>
          <div className="companion-face-caption">Mood {moodScore}/10</div>
        </div>
        <div className="companion-copy">
          <div className="companion-speech">{message}</div>
          <div className="companion-meter" role="meter" aria-valuemin={0} aria-valuemax={10} aria-valuenow={moodScore} aria-label="Companion mood">
            <div className="companion-meter-track">
              <div className="companion-meter-fill" style={{ width: `${moodScore * 10}%` }} />
            </div>
            <span>{moodScore}/10</span>
          </div>
        </div>
      </div>

      <div className="companion-stat-grid">
        {statusStats.map(item => (
          <div className="companion-stat" key={item.label}>
            <span>{item.label}</span>
            <strong style={{ fontVariantNumeric: 'tabular-nums' }}>{item.value}</strong>
          </div>
        ))}
      </div>

      <div className="companion-achievements">
        <div className="companion-achievement-head">
          <span>Local Achievements</span>
          <strong>{unlocked.size}/{ACHIEVEMENTS.length}</strong>
        </div>
        <div className="companion-achievement-grid">
          {ACHIEVEMENTS.map(achievement => {
            const isUnlocked = unlocked.has(achievement.id);
            return (
              <div
                key={achievement.id}
                className={`companion-achievement ${isUnlocked ? 'unlocked' : ''}`}
                title={`${achievement.name}: ${achievement.hint}`}
                aria-label={`${achievement.name}: ${isUnlocked ? 'unlocked' : 'locked'}. ${achievement.hint}`}
              >
                <span>{isUnlocked ? '*' : '-'}</span>
                <em>{achievement.name}</em>
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

/**
 * CompanionDock — the compact, always-present entry point for the companion.
 * Lives pinned at the BOTTOM of every mode's menu bar (sidebar); clicking it
 * pops open the full CompanionCard (mood, stats, quests/achievements) so the
 * companion no longer takes a slab on the page bodies themselves.
 */
export function CompanionDock() {
  const [open, setOpen] = useState(false);
  return (
    <div className="companion-dock">
      <button
        type="button"
        className={`companion-dock-trigger${open ? ' is-open' : ''}`}
        onClick={() => setOpen(o => !o)}
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label="Open miner companion"
      >
        <span className="companion-dock-face" aria-hidden="true">{'(\\o/)'}</span>
        <span className="companion-dock-text">
          <strong>Companion</strong>
          <small>mood · quests</small>
        </span>
      </button>
      {open && createPortal(
        <div className="companion-dock-backdrop" onClick={() => setOpen(false)}>
          <div className="companion-dock-popover" role="dialog" aria-label="Miner companion" onClick={(e) => e.stopPropagation()}>
            <button
              type="button"
              className="companion-dock-close"
              onClick={() => setOpen(false)}
              aria-label="Close companion"
            >
              ×
            </button>
            <CompanionCard className="companion-dock-card" />
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}
