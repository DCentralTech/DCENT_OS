import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { FxEvent } from '../../fx/rewardBus';
import { rewardBus } from '../../fx/rewardBus';
import { useMinerStore } from '../../store/miner';
import type { MiningWorkPostureResponse, RecentShareEvent, StatusResponse } from '../../api/types';
import { TransportChip } from './TransportChip';

const WATCH_KEY = 'dcentos-first-share-watch';
const BASELINE_KEY = 'dcentos-first-share-watch-baseline';
const REFRESH_MS = 5000;

type WatchFlag = 'pending' | 'done' | 'dismissed' | null;

interface WatchBaseline {
  accepted: number;
  rejected: number;
  capturedAt: number;
}

interface CompletionState {
  poolHost: string;
  difficulty?: number;
  targetDifficulty?: number;
}

interface WatchStep {
  id: string;
  label: string;
  done: boolean;
  detail: string;
}

function storageAvailable(): boolean {
  return typeof window !== 'undefined' && typeof window.localStorage !== 'undefined';
}

function readFlag(): WatchFlag {
  if (!storageAvailable()) return null;
  const value = window.localStorage.getItem(WATCH_KEY);
  if (value === 'pending' || value === 'done' || value === 'dismissed') return value;
  return null;
}

function writeFlag(value: Exclude<WatchFlag, null>): void {
  if (!storageAvailable()) return;
  window.localStorage.setItem(WATCH_KEY, value);
}

function readBaseline(): WatchBaseline | null {
  if (!storageAvailable()) return null;
  try {
    const raw = window.localStorage.getItem(BASELINE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<WatchBaseline>;
    if (typeof parsed.accepted !== 'number' || typeof parsed.rejected !== 'number') return null;
    return {
      accepted: Math.max(0, parsed.accepted),
      rejected: Math.max(0, parsed.rejected),
      capturedAt: typeof parsed.capturedAt === 'number' ? parsed.capturedAt : Date.now(),
    };
  } catch {
    return null;
  }
}

function writeBaseline(value: WatchBaseline): void {
  if (!storageAvailable()) return;
  window.localStorage.setItem(BASELINE_KEY, JSON.stringify(value));
}

export function armFirstShareWatch(): void {
  if (!storageAvailable()) return;
  window.localStorage.setItem(WATCH_KEY, 'pending');
  window.localStorage.removeItem(BASELINE_KEY);
}

function poolHost(url?: string | null): string {
  if (!url) return 'the active pool';
  const match = url.match(/:\/\/([^/]+)/);
  return match?.[1] || url;
}

function finiteNumber(value: number | null | undefined): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function formatDifficulty(value: number | undefined): string {
  if (value === undefined) return 'not reported';
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function normalizePoolStatus(status?: string | null): string {
  return (status ?? '').toLowerCase();
}

function isPoolConnected(status?: string | null): boolean {
  const normalized = normalizePoolStatus(status);
  return ['connected', 'authorized', 'mining_capable', 'mining', 'active', 'alive', 'donating'].includes(normalized);
}

function isPoolAuthorized(status?: string | null, posture?: MiningWorkPostureResponse | null): boolean {
  if (posture?.pool.published_authorized === true) return true;
  const normalized = normalizePoolStatus(status);
  return ['authorized', 'mining_capable', 'mining', 'active'].includes(normalized);
}

function sharesUnresolved(status?: StatusResponse | null): number {
  const failover = status?.pool?.failover;
  const value = failover?.shares_unresolved ?? failover?.unresolved_submit_count ?? 0;
  return typeof value === 'number' && Number.isFinite(value) ? Math.max(0, value) : 0;
}

function latestAcceptedEvent(events: RecentShareEvent[]): RecentShareEvent | null {
  return events
    .filter(event => {
      const result = String(event.result ?? '').toLowerCase();
      return result === 'accepted' || result === 'lucky';
    })
    .sort((a, b) => b.timestamp_ms - a.timestamp_ms)[0] ?? null;
}

function completionFromEvent(event: FxEvent, status?: StatusResponse | null): CompletionState {
  return {
    poolHost: poolHost(status?.pool?.url),
    difficulty: finiteNumber(event.difficulty),
    targetDifficulty: finiteNumber(event.targetDifficulty),
  };
}

function completionFromCounters(
  status: StatusResponse | null,
  posture: MiningWorkPostureResponse | null,
  history: RecentShareEvent[],
): CompletionState {
  const event = latestAcceptedEvent([
    ...history,
    ...(posture?.shares.recent_events ?? []),
  ]);
  return {
    poolHost: poolHost(status?.pool?.url ?? posture?.pool.url),
    difficulty: finiteNumber(event?.difficulty),
    targetDifficulty: finiteNumber(event?.target_difficulty),
  };
}

function buildSteps(
  status: StatusResponse | null,
  posture: MiningWorkPostureResponse | null,
  baseline: WatchBaseline | null,
  history: RecentShareEvent[],
): WatchStep[] {
  const poolStatus = status?.pool?.status ?? posture?.pool.status ?? null;
  const accepted = status?.accepted ?? posture?.shares.accepted_total ?? 0;
  const rejected = status?.rejected ?? posture?.shares.rejected_total ?? 0;
  const authFailed = normalizePoolStatus(poolStatus) === 'auth_failed';
  const baselineAccepted = baseline?.accepted ?? 0;
  const baselineRejected = baseline?.rejected ?? 0;
  const hasRecentShareEvent = history.length > 0 || (posture?.shares.recent_events?.length ?? 0) > 0;
  const submitted = sharesUnresolved(status) > 0
    || accepted > baselineAccepted
    || rejected > baselineRejected
    || hasRecentShareEvent;

  return [
    {
      id: 'telemetry',
      label: 'Connecting',
      done: Boolean(status || posture),
      detail: status || posture ? 'Daemon telemetry is reachable.' : 'Waiting for daemon telemetry.',
    },
    {
      id: 'connected',
      label: 'Connected',
      done: isPoolConnected(poolStatus),
      detail: poolStatus ? `Pool state: ${poolStatus}.` : 'Waiting for pool state.',
    },
    {
      id: 'authorized',
      label: 'Authorized',
      done: isPoolAuthorized(poolStatus, posture),
      detail: authFailed
        ? 'Pool rejected the worker credentials.'
        : 'Bound to pool authorization state.',
    },
    {
      id: 'job',
      label: 'Job received',
      done: Boolean(
        posture?.jobs.current_job_available
        || posture?.jobs.latest_observed_job_id
        || typeof posture?.jobs.latest_observed_job_age_s === 'number',
      ),
      detail: posture?.jobs.latest_observed_job_id
        ? `Latest job ${posture.jobs.latest_observed_job_id}.`
        : 'Waiting for a persisted pool job.',
    },
    {
      id: 'submitted',
      label: 'First share submitted',
      done: submitted,
      detail: sharesUnresolved(status) > 0
        ? `${sharesUnresolved(status)} share submission awaiting pool response.`
        : 'Waiting for a real share result or pending submission counter.',
    },
    {
      id: 'accepted',
      label: 'First share accepted',
      done: accepted > baselineAccepted,
      detail: accepted > baselineAccepted
        ? `${accepted - baselineAccepted} new accepted share${accepted - baselineAccepted === 1 ? '' : 's'} observed.`
        : 'Waiting for the pool to accept a share.',
    },
  ];
}

export function FirstShareWatchCard() {
  const status = useMinerStore(s => s.status);
  const setCurrentPage = useMinerStore(s => s.setCurrentPage);
  const [flag, setFlag] = useState<WatchFlag>(() => readFlag());
  const [baseline, setBaseline] = useState<WatchBaseline | null>(() => readBaseline());
  const [posture, setPosture] = useState<MiningWorkPostureResponse | null>(null);
  const [history, setHistory] = useState<RecentShareEvent[]>([]);
  const [completion, setCompletion] = useState<CompletionState | null>(null);
  const [hidden, setHidden] = useState(false);
  const needsCompletionEvidence = completion !== null && completion.difficulty === undefined;

  const finish = useCallback((next: CompletionState, show = true) => {
    writeFlag('done');
    setFlag('done');
    if (show) {
      setCompletion(next);
    } else {
      setHidden(true);
    }
  }, []);

  useEffect(() => {
    if (!completion || completion.difficulty !== undefined) return;
    const enriched = completionFromCounters(status, posture, history);
    if (enriched.difficulty === undefined && enriched.targetDifficulty === undefined) return;
    setCompletion(prev => prev
      ? {
          ...prev,
          difficulty: enriched.difficulty ?? prev.difficulty,
          targetDifficulty: enriched.targetDifficulty ?? prev.targetDifficulty,
        }
      : prev);
  }, [completion, history, posture, status]);

  useEffect(() => {
    if (flag !== 'pending') return;
    if (!status) return;

    const accepted = status.accepted ?? 0;
    const rejected = status.rejected ?? 0;
    if (!baseline) {
      if (accepted > 0) {
        finish({
          poolHost: poolHost(status.pool?.url),
        }, false);
        return;
      }
      const next = { accepted, rejected, capturedAt: Date.now() };
      writeBaseline(next);
      setBaseline(next);
      return;
    }

    if (accepted > baseline.accepted) {
      finish(completionFromCounters(status, posture, history));
    }
  }, [baseline, finish, flag, history, posture, status]);

  useEffect(() => {
    if (flag !== 'pending' && !needsCompletionEvidence) return;
    let cancelled = false;

    const load = async () => {
      const [postureResult, historyResult] = await Promise.allSettled([
        api.getMiningWorkPosture(),
        api.getShareHistory(),
      ]);
      if (cancelled) return;
      if (postureResult.status === 'fulfilled') setPosture(postureResult.value);
      if (historyResult.status === 'fulfilled') setHistory(historyResult.value.events ?? []);
    };

    void load();
    const timer = window.setInterval(() => void load(), REFRESH_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [flag, needsCompletionEvidence]);

  useEffect(() => {
    if (flag !== 'pending' || !baseline || completion) return;
    const accepted = posture?.shares.accepted_total;
    if (typeof accepted === 'number' && accepted > baseline.accepted) {
      finish(completionFromCounters(status, posture, history));
    }
  }, [baseline, completion, finish, flag, history, posture, status]);

  useEffect(() => {
    if (flag !== 'pending') return;
    return rewardBus.subscribe(event => {
      if (event.kind !== 'first-share' || event.intensity <= 0) return;
      finish(completionFromEvent(event, status));
    });
  }, [finish, flag, status]);

  const steps = useMemo(
    () => buildSteps(status, posture, baseline, history),
    [baseline, history, posture, status],
  );
  const currentStep = steps.find(step => !step.done) ?? steps[steps.length - 1];

  const dismiss = () => {
    writeFlag(completion ? 'done' : 'dismissed');
    setFlag(completion ? 'done' : 'dismissed');
    setHidden(true);
  };

  if (hidden || (flag !== 'pending' && !completion)) return null;
  if (flag === 'pending' && !completion && !status) return null;

  return (
    <section
      className="page-surface dcfx-first-share-watch"
      data-testid="first-share-watch-card"
      aria-label="First share watch"
    >
      <div className="page-surface-header dcfx-first-share-watch-header">
        <div>
          <div className="page-surface-title">
            {completion ? 'First Share Accepted' : 'First-Share Watch'}
          </div>
          <div className="page-surface-copy">
            {completion
              ? 'Pool acceptance confirmed for this setup session.'
              : 'Advances only on daemon telemetry, pool posture, share history, or live share events.'}
          </div>
        </div>
        <div className="dcfx-first-share-watch-pills">
          <TransportChip className="transport-chip" />
          <span className="ds-chip ds-neutral">post-setup</span>
        </div>
      </div>

      {completion ? (
        <div className="dcfx-first-share-watch-success" role="status" aria-live="polite">
          <strong>First share accepted by {completion.poolHost} - you are mining.</strong>
          <span>
            Achieved difficulty {formatDifficulty(completion.difficulty)}
            {completion.targetDifficulty !== undefined
              ? ` / pool target ${formatDifficulty(completion.targetDifficulty)}`
              : ''}
          </span>
        </div>
      ) : (
        <>
          <div className="dcfx-watch-ladder" aria-label="First share watch ladder">
            {steps.map(step => {
              const isCurrent = step.id === currentStep.id && !step.done;
              const cls = [
                'dcfx-watch-step',
                step.done ? 'is-done' : '',
                isCurrent ? 'is-current' : '',
              ].filter(Boolean).join(' ');
              return (
                <div key={step.id} className={cls} data-testid={`first-share-step-${step.id}`}>
                  <span className="dcfx-watch-step-dot" aria-hidden="true" />
                  <div>
                    <strong>{step.label}</strong>
                    <span>{step.detail}</span>
                  </div>
                </div>
              );
            })}
          </div>
          <p className="dcfx-first-share-watch-note">
            Holding at {currentStep.label.toLowerCase()}. Open Pools to test credentials or review share history.
          </p>
        </>
      )}

      <div className="dcfx-first-share-watch-actions">
        <button type="button" className="ds-btn sm" onClick={() => setCurrentPage('pools')}>
          Open Pools
        </button>
        <button type="button" className="ds-btn sm secondary" onClick={() => setCurrentPage('pools/shares')}>
          Share History
        </button>
        <button type="button" className="ds-btn sm ghost" onClick={dismiss}>
          Dismiss
        </button>
      </div>
    </section>
  );
}
