import React from 'react';

import type { ChipColor, ChipGrade } from '../../api/types';

export type ChipHealthTone = 'healthy' | 'degraded' | 'failing' | 'no-data';
export type ChipHealthSource = 'diagnostics' | 'autotuner';

export const CHIP_HEALTH_COLORS: Record<ChipHealthTone, string> = {
  healthy: '#22C55E',
  degraded: '#EAB308',
  failing: '#EF4444',
  'no-data': '#6B7280',
};

export const CHIP_HEALTH_LABELS: Record<ChipHealthTone, string> = {
  healthy: 'Healthy',
  degraded: 'Degraded',
  failing: 'Failing',
  'no-data': 'No data',
};

const SOURCE_LABELS: Record<ChipHealthSource, string> = {
  diagnostics: 'from diagnostics run',
  autotuner: 'from autotuner grading',
};

interface DiagnosticHealthInput {
  present: boolean;
  color: ChipColor;
  grade: ChipGrade;
  healthScore: number | null | undefined;
}

interface AutotunerHealthInput {
  status: string | null | undefined;
  healthScore: number | null | undefined;
}

export function normalizeHealthScore(value: number | null | undefined): number | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  return value > 1 ? Math.max(0, Math.min(1, value / 100)) : Math.max(0, Math.min(1, value));
}

export function formatHealthPercent(value: number | null | undefined): string {
  const normalized = normalizeHealthScore(value);
  return normalized == null ? 'n/a' : `${Math.round(normalized * 100)}%`;
}

export function chipHealthToneFromDiagnostics(input: DiagnosticHealthInput): ChipHealthTone {
  const score = normalizeHealthScore(input.healthScore);
  const grade = String(input.grade ?? '').toUpperCase();

  if (!input.present || input.color === 'Gray' || score === 0) return 'no-data';
  if (input.color === 'Red' || grade === 'D' || grade === 'F') return 'failing';
  if (input.color === 'Orange' || input.color === 'Yellow' || grade === 'C') return 'degraded';
  if (score != null) {
    if (score < 0.4) return 'failing';
    if (score < 0.75) return 'degraded';
  }
  return 'healthy';
}

export function chipHealthToneFromAutotuner(input: AutotunerHealthInput): ChipHealthTone {
  const status = String(input.status ?? '').toLowerCase();
  const score = normalizeHealthScore(input.healthScore);

  if (status.includes('fail')) return 'failing';
  if (score == null) return 'no-data';
  if (score < 0.4) return 'failing';
  if (status.includes('warn') || status.includes('degrad') || score < 0.75) return 'degraded';
  return 'healthy';
}

export function chipHealthSourceLabel(source: ChipHealthSource): string {
  return SOURCE_LABELS[source];
}

export function chipHealthTextColor(tone: ChipHealthTone): string {
  return tone === 'healthy' || tone === 'degraded' ? '#0A0F08' : '#FFFFFF';
}

interface ChipHealthLegendProps {
  source: ChipHealthSource;
  compact?: boolean;
  className?: string;
}

export function ChipHealthLegend({ source, compact = false, className }: ChipHealthLegendProps) {
  const entries: ChipHealthTone[] = ['healthy', 'degraded', 'failing', 'no-data'];
  return (
    <div
      className={className}
      data-testid="chip-health-legend"
      data-health-source={source}
      aria-label={`Chip health legend ${SOURCE_LABELS[source]}`}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: compact ? 8 : 12,
        flexWrap: 'wrap',
        fontSize: compact ? '0.66rem' : '0.7rem',
        color: 'var(--text-dim)',
      }}
    >
      <span style={{
        color: 'var(--text)',
        fontWeight: 700,
        textTransform: 'uppercase',
        letterSpacing: 0,
      }}>
        Health
      </span>
      {entries.map(tone => (
        <span key={tone} style={{ display: 'inline-flex', alignItems: 'center', gap: 5 }}>
          <span
            aria-hidden="true"
            style={{
              width: 10,
              height: 10,
              borderRadius: 2,
              background: CHIP_HEALTH_COLORS[tone],
              border: tone === 'no-data' ? '1px solid rgba(255,255,255,0.35)' : '0',
            }}
          />
          {CHIP_HEALTH_LABELS[tone]}
        </span>
      ))}
      <span style={{ color: 'var(--text-dim)', fontStyle: 'italic' }}>
        {SOURCE_LABELS[source]}
      </span>
    </div>
  );
}
