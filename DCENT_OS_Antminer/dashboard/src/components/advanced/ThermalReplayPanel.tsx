import React, { useEffect, useMemo, useState } from 'react';
import { api } from '../../api/client';
import type { HistoryPoint } from '../../api/types';
import { SvgChart, type ChartSeries } from '../common/SvgChart';
import { TIME_RANGES } from '../../utils/constants';
import { useTemp } from '../../hooks/useTemp';
import { useFlightRecorder } from '../../hooks/useFlightRecorder';

function classifyThermalPosture(tempC: number) {
  if (tempC >= 75) return { label: 'Critical', tone: 'danger' as const };
  if (tempC >= 65) return { label: 'Hot', tone: 'warning' as const };
  if (tempC >= 55) return { label: 'Warm', tone: 'info' as const };
  if (tempC >= 40) return { label: 'Comfortable', tone: 'success' as const };
  return { label: 'Cool', tone: 'neutral' as const };
}

function formatTime(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleString('en-US', {
    hour12: false,
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function downloadJson(filename: string, payload: unknown) {
  const json = JSON.stringify(payload, null, 2);
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
}

function formatTempDelta(deltaC: number, unit: 'C' | 'F', symbol: string) {
  const value = unit === 'F' ? (deltaC * 9) / 5 : deltaC;
  return `${value >= 0 ? '+' : ''}${value.toFixed(1)}${symbol}`;
}

export function ThermalReplayPanel() {
  const temp = useTemp();
  const { recordAction } = useFlightRecorder();
  const [history, setHistory] = useState<HistoryPoint[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [timeRangeIndex, setTimeRangeIndex] = useState(2);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [playing, setPlaying] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError('');
    api.getHistory()
      .then(response => {
        if (cancelled) return;
        setHistory(response.history ?? []);
      })
      .catch((nextError: unknown) => {
        if (cancelled) return;
        setError(nextError instanceof Error ? nextError.message : 'Failed to load thermal history');
      })
      .finally(() => {
        if (!cancelled) {
          setLoading(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, []);

  const filteredHistory = useMemo(() => {
    const range = TIME_RANGES[timeRangeIndex];
    const latestTimestamp = history[history.length - 1]?.timestamp ?? 0;
    const cutoff = latestTimestamp - range.seconds;
    return history.filter(point => point.timestamp >= cutoff);
  }, [history, timeRangeIndex]);

  useEffect(() => {
    setSelectedIndex(filteredHistory.length > 0 ? filteredHistory.length - 1 : 0);
  }, [filteredHistory]);

  useEffect(() => {
    if (!playing || filteredHistory.length <= 1) {
      return;
    }

    const intervalId = window.setInterval(() => {
      setSelectedIndex(current => (current + 1 >= filteredHistory.length ? 0 : current + 1));
    }, 850);

    return () => window.clearInterval(intervalId);
  }, [filteredHistory.length, playing]);

  const selectedPoint = filteredHistory[selectedIndex] ?? null;
  const previousPoint = selectedIndex > 0 ? filteredHistory[selectedIndex - 1] : null;

  const replaySeries = useMemo<ChartSeries[]>(() => {
    if (filteredHistory.length === 0) {
      return [];
    }

    return [
      {
        data: filteredHistory.map(point => ({ time: point.timestamp, value: point.temp_c })),
        color: 'var(--red, #EF4444)',
        label: 'Temperature',
        yAxis: 'left',
      },
      {
        data: filteredHistory.map(point => ({ time: point.timestamp, value: point.power_watts })),
        color: 'var(--accent, #FAA500)',
        label: 'Power',
        dashed: true,
        yAxis: 'right',
      },
    ];
  }, [filteredHistory]);

  // SR summary derived from the exact replay series the chart renders, so
  // screen-reader truth tracks visible truth on this safety-relevant readout.
  const replaySummary = useMemo(() => {
    const rangeLabel = TIME_RANGES[timeRangeIndex].label;
    if (filteredHistory.length === 0) {
      return `Temperature and power replay chart, ${rangeLabel} window, no data yet`;
    }
    let sum = 0;
    let count = 0;
    for (const point of filteredHistory) {
      if (Number.isFinite(point.temp_c)) { sum += point.temp_c; count++; }
    }
    if (count === 0) return `Temperature and power replay chart, ${rangeLabel} window, no data yet`;
    const avg = sum / count;
    return `Temperature replay over last ${rangeLabel}: ${temp.format(avg)} average across ${count} samples`;
  }, [filteredHistory, timeRangeIndex, temp]);

  const posture = selectedPoint ? classifyThermalPosture(selectedPoint.temp_c) : { label: 'Idle', tone: 'neutral' as const };
  const deltaTemp = selectedPoint && previousPoint ? selectedPoint.temp_c - previousPoint.temp_c : 0;
  const deltaPower = selectedPoint && previousPoint ? selectedPoint.power_watts - previousPoint.power_watts : 0;
  const deltaFan = selectedPoint && previousPoint ? selectedPoint.fan_rpm - previousPoint.fan_rpm : 0;

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// thermal replay</div>
          <h2 className="hacker-inspector-title">Temp & Power Scrubber</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${posture.tone === 'success' ? '' : posture.tone}`}>
            {String(posture.label).toUpperCase()}
          </span>
          <button
            className="hacker-inspector-help"
            onClick={() => {
              setPlaying(value => !value);
              recordAction('thermal_replay_toggled', { playing: !playing });
            }}
            disabled={filteredHistory.length <= 1}
          >
            {playing ? '⏸ PAUSE' : '▶ PLAY'}
          </button>
          <button
            className="hacker-inspector-refresh"
            onClick={() => {
              downloadJson(`dcentos-thermal-replay-${Date.now()}.json`, {
                exportedAt: new Date().toISOString(),
                timeRange: TIME_RANGES[timeRangeIndex],
                points: filteredHistory,
              });
              recordAction('thermal_replay_exported', { points: filteredHistory.length, range: TIME_RANGES[timeRangeIndex].label });
            }}
            disabled={filteredHistory.length === 0}
          >
            ⤓ EXPORT
          </button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="register-inspector ds-card-hover adv-mb-16">
        <div className="time-range-tabs" role="group" aria-label="Thermal replay time range">
          {TIME_RANGES.slice(0, 4).map((range, index) => (
            <button
              key={range.label}
              type="button"
              className={`time-tab ${timeRangeIndex === index ? 'active' : ''}`}
              onClick={() => setTimeRangeIndex(index)}
            >
              {range.label}
            </button>
          ))}
        </div>
      </div>

      {loading ? (
        <div className="register-inspector"><div className="adv-state is-loading is-inline">Loading thermal replay...</div></div>
      ) : error ? (
        <div className="register-inspector"><div className="adv-state is-error is-inline">{error}</div></div>
      ) : filteredHistory.length === 0 ? (
        <div className="register-inspector"><div className="adv-empty-note">No thermal history available yet.</div></div>
      ) : (
        <>
          <div className="register-inspector ds-card-hover adv-mb-16">
            <SvgChart
              series={replaySeries}
              height={260}
              formatValue={(value, seriesIndex) => seriesIndex === 0 ? temp.format(value) : `${value.toFixed(0)} W`}
              summaryText={replaySummary}
              style={{ borderRadius: 'var(--radius)', overflow: 'hidden' }}
            />
            <div className="legend-row tr-legend">
              <span className="legend-pill"><span className="legend-dot tr-dot-temp" /> Temperature</span>
              <span className="legend-pill"><span className="legend-dot tr-dot-power" /> Power</span>
            </div>
          </div>

          <div className="register-inspector ds-card-hover adv-mb-16">
            <div className="tr-scrubber-wrap">
              <input
                type="range"
                min={0}
                max={Math.max(0, filteredHistory.length - 1)}
                value={selectedIndex}
                onChange={event => {
                  setSelectedIndex(Number(event.target.value));
                  setPlaying(false);
                }}
              />
              <div className="tr-time-row">
                <span>{formatTime(filteredHistory[0].timestamp)}</span>
                <span>{selectedPoint ? formatTime(selectedPoint.timestamp) : '--'}</span>
                <span>{formatTime(filteredHistory[filteredHistory.length - 1].timestamp)}</span>
              </div>
            </div>
          </div>

          {selectedPoint && (
            <div className="adv-stat-grid is-mb">
              {[
                { label: 'Replay frame', value: formatTime(selectedPoint.timestamp), tone: 'var(--accent-orange)' },
                { label: 'Temperature', value: temp.format(selectedPoint.temp_c), tone: selectedPoint.temp_c >= 65 ? 'var(--yellow)' : 'var(--accent)' },
                { label: 'Power', value: `${selectedPoint.power_watts.toFixed(0)} W`, tone: 'var(--accent)' },
                { label: 'Fan RPM', value: selectedPoint.fan_rpm.toLocaleString(), tone: 'var(--text)' },
                { label: 'Hashrate', value: `${(selectedPoint.hashrate_ghs / 1000).toFixed(2)} TH/s`, tone: 'var(--accent-orange)' },
                { label: 'Thermal posture', value: posture.label, tone: posture.tone === 'danger' ? 'var(--red)' : posture.tone === 'warning' ? 'var(--yellow)' : 'var(--green)' },
              ].map(card => (
                <div key={card.label} className="glass-card adv-stat-card ds-card-hover">
                  <span className="adv-stat-label">{card.label}</span>
                  <span className="adv-stat-value" style={{ color: card.tone }}>{card.value}</span>
                </div>
              ))}
            </div>
          )}

          {selectedPoint && (
            <div className="register-inspector ds-card-hover adv-mt-16">
              <div className="tr-delta-title">Frame Delta</div>
              <div className="adv-stat-grid is-min-180">
                <div className="glass-card adv-stat-card is-pad-10">
                  <div className="adv-stat-label no-track">Temp delta</div>
                  <div className="tr-delta-val" style={{ color: deltaTemp >= 0 ? 'var(--yellow)' : 'var(--green)' }}>{formatTempDelta(deltaTemp, temp.unit, temp.symbol)}</div>
                </div>
                <div className="glass-card adv-stat-card is-pad-10">
                  <div className="adv-stat-label no-track">Power delta</div>
                  <div className="tr-delta-val" style={{ color: deltaPower >= 0 ? 'var(--yellow)' : 'var(--green)' }}>{deltaPower >= 0 ? '+' : ''}{deltaPower.toFixed(0)} W</div>
                </div>
                <div className="glass-card adv-stat-card is-pad-10">
                  <div className="adv-stat-label no-track">Fan delta</div>
                  <div className="tr-delta-val" style={{ color: deltaFan >= 0 ? 'var(--yellow)' : 'var(--green)' }}>{deltaFan >= 0 ? '+' : ''}{deltaFan.toFixed(0)} RPM</div>
                </div>
              </div>
            </div>
          )}
        </>
      )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{filteredHistory.length} samples</span>
          <span>range: {TIME_RANGES[timeRangeIndex].label}</span>
          <span>{playing ? 'playing' : 'paused'}</span>
        </div>
      </footer>
    </div>
  );
}
