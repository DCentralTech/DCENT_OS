import React, { useState, useRef, useCallback, useEffect } from 'react';
import { api } from '../../api/client';
import type { DiagnosticReportMetadata } from '../../api/types';
import { ActionButton } from '../common/ActionButton';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import { useActiveHardware } from '../../hooks/useActiveHardware';

type TestType = 'hashreport' | 'chiphealth' | 'boardhealth';

interface TestState {
  running: boolean;
  testId: string | null;
  progress: number;
  phase: string;
  message: string;
  result: Record<string, unknown> | null;
  error: string | null;
  startTime: number | null;
  eta: string | null;
}

type JsonObject = Record<string, unknown>;

function asObject(value: unknown): JsonObject | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : null;
}

function asArray(value: unknown): JsonObject[] {
  return Array.isArray(value) ? value.filter((item): item is JsonObject => !!asObject(item)) : [];
}

function getReportPath(type: TestType, testId: string): string {
  const path = type === 'hashreport'
    ? 'hashreport'
    : type === 'chiphealth'
      ? 'chip-health'
      : 'board-health';
  return `/api/diagnostics/${path}/report?test_id=${encodeURIComponent(testId)}`;
}

function reportTypeToTestType(testType: string): TestType | null {
  switch (testType) {
    case 'hashreport':
      return 'hashreport';
    case 'chip_health_snapshot':
      return 'chiphealth';
    case 'board_health_snapshot':
      return 'boardhealth';
    default:
      return null;
  }
}

function reportTypeLabel(testType: string): string {
  switch (reportTypeToTestType(testType)) {
    case 'hashreport':
      return 'Hash Rate Report';
    case 'chiphealth':
      return 'Chip Health';
    case 'boardhealth':
      return 'Board Health';
    default:
      return testType.replace(/_/g, ' ');
  }
}

function formatRecentTimestamp(value: string): string {
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return value;
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(new Date(timestamp));
}

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

const initialState: TestState = {
  running: false,
  testId: null,
  progress: 0,
  phase: '',
  message: '',
  result: null,
  error: null,
  startTime: null,
  eta: null,
};

function formatEta(startTime: number, progress: number): string {
  if (progress <= 0) return 'calculating...';
  const elapsed = (Date.now() - startTime) / 1000;
  const totalEstimated = elapsed / (progress / 100);
  const remaining = totalEstimated - elapsed;
  if (remaining < 60) return `~${Math.ceil(remaining)}s`;
  return `~${Math.ceil(remaining / 60)}m ${Math.ceil(remaining % 60)}s`;
}

// Structured result renderers
function HashReportTable({ data }: { data: Record<string, unknown> }) {
  const boards = asArray(data.boards);
  const chains = boards.length > 0 ? boards : asArray(data.chains);
  const totalHashrateGhs = typeof data.total_hashrate_ghs === 'number'
    ? data.total_hashrate_ghs
    : boards.reduce((sum, board) => sum + (typeof board.hashrate_ghs === 'number' ? board.hashrate_ghs : 0), 0);
  return (
    <div>
      <div className="dp-result-title">
        Hash Rate Report
      </div>
      {totalHashrateGhs > 0 && (
        <div className="dp-total">
          Total: {(totalHashrateGhs / 1000).toFixed(2)} TH/s
        </div>
      )}
      {typeof data.unit_grade === 'string' && (
        <div className="dp-grade-line">
          Grade {data.unit_grade}: {String(data.unit_grade_explanation ?? 'Snapshot summary')}
        </div>
      )}
      <div className="table-wrap">
        <table className="dp-table">
          <thead>
            <tr>
              <th scope="col" className="is-l">Chain</th>
              <th scope="col" className="is-r">Hashrate</th>
              <th scope="col" className="is-r">Chips</th>
              <th scope="col" className="is-r">Avg Freq</th>
              <th scope="col" className="is-r">HW Errors</th>
              <th scope="col" className="is-r">Deviation</th>
            </tr>
          </thead>
          <tbody>
            {chains.map((ch, i) => (
              <tr key={i}>
                <td className="is-chain">Chain {String(ch.chain_id ?? ch.id ?? i)}</td>
                <td className="is-r">
                  {((ch.hashrate_ghs as number) || 0).toFixed(1)} GH/s
                </td>
                <td className="is-r">{String(ch.chips_responding ?? ch.chips ?? '?')}</td>
                <td className="is-r">{String(ch.frequency_mhz ?? ch.avg_freq_mhz ?? '?')} MHz</td>
                <td className="is-r" style={{
                  color: (((ch.crc_errors as number) || (ch.hw_errors as number) || 0) > 0) ? 'var(--red)' : 'var(--green)',
                }}>
                  {String(ch.crc_errors ?? ch.hw_errors ?? 0)}
                </td>
                <td className="is-r" style={{
                  color: Math.abs((ch.deviation_pct as number) || 0) > 5 ? 'var(--yellow)' : 'var(--text-dim)',
                }}>
                  {'grade' in ch ? String(ch.grade ?? '-') : `${((ch.deviation_pct as number) || 0).toFixed(1)}%`}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ChipHealthGrid({ data }: { data: Record<string, unknown> }) {
  const chains = asArray(data.chains);
  const firstChain = asObject(chains[0]);
  const chipmap = asObject(firstChain?.chipmap);
  const chips = asArray(chipmap?.cells ?? data.chips);
  const columns = typeof chipmap?.columns === 'number' ? chipmap.columns : 21;
  const totalChips = chips.length;
  const okChips = chips.filter(c => c.status === 'ok' || c.status === 'healthy' || c.color === 'Green' || c.color === 'Yellow').length;
  const warnChips = chips.filter(c => c.status === 'warning' || c.status === 'degraded' || c.color === 'Orange').length;
  const failChips = chips.filter(c => c.status === 'failed' || c.status === 'dead' || c.color === 'Red' || c.color === 'Gray').length;

  return (
    <div>
      <div className="dp-result-title">
        Chip Health
      </div>
      {firstChain && (
        <div className="dp-grade-line">
          Chain {String(firstChain.chain_id ?? '?')} | Responding {String(firstChain.responding_chips ?? '?')}/{String(firstChain.chip_count ?? chips.length)} | Score {typeof firstChain.board_health_score === 'number' ? firstChain.board_health_score.toFixed(2) : '?'}
        </div>
      )}
      <div className="dp-summary-row">
        <span className="dp-sum-ok">OK: {okChips}</span>
        <span className="dp-sum-warn">Warn: {warnChips}</span>
        <span className="dp-sum-fail">Fail: {failChips}</span>
        <span className="dp-sum-total">Total: {totalChips}</span>
      </div>
      {chips.length > 0 && (
        <div className="dp-chip-scroll">
          <div className="dp-chip-grid" style={{
            gridTemplateColumns: `repeat(${columns}, 14px)`,
          }}>
            {chips.map((chip, i) => {
              const status = String(chip.status ?? chip.grade ?? chip.color ?? 'unknown');
              const bg = typeof chip.color === 'string'
                ? chip.color === 'Green' ? '#22c55e'
                  : chip.color === 'Yellow' ? '#eab308'
                    : chip.color === 'Orange' ? '#f97316'
                      : chip.color === 'Red' ? '#ef4444'
                        : chip.color === 'Gray' ? '#6b7280'
                          : '#333'
                : status === 'ok' || status === 'healthy' ? '#166534'
                  : status === 'warning' || status === 'degraded' ? '#EAB308'
                    : status === 'failed' || status === 'dead' ? '#FF4444'
                      : '#333';
              return (
                <div
                  key={i}
                  className="dp-chip"
                  style={{ background: bg }}
                  role="img"
                  aria-label={`Chip ${String(chip.index ?? chip.chip_id ?? i)} status ${status}${chip.health_score ? `, score ${Number(chip.health_score).toFixed(2)}` : ''}${chip.frequency_mhz ? `, ${chip.frequency_mhz} megahertz` : ''}`}
                  title={`Chip ${String(chip.index ?? chip.chip_id ?? i)}: ${status}${chip.health_score ? ` | score ${Number(chip.health_score).toFixed(2)}` : ''}${chip.frequency_mhz ? ` | ${chip.frequency_mhz} MHz` : ''}`}
                />
              );
            })}
          </div>
        </div>
      )}
      {chips.length === 0 && (
        <div className="json-response dp-json-fallback">
          {JSON.stringify(data, null, 2)}
        </div>
      )}
    </div>
  );
}

function BoardHealthChecklist({ data }: { data: Record<string, unknown> }) {
  const boards = asArray(data.boards);
  const board = asObject(boards[0] ?? data);
  const checks = asArray(board?.checks);

  // If no structured checks, render known fields
  const items = checks.length > 0 ? checks : [
    { name: 'Chip Enumeration', status: board?.chips_responding === board?.chips_expected ? 'pass' : 'warning', detail: `${String(board?.chips_responding ?? '?')}/${String(board?.chips_expected ?? '?')} responding` },
    { name: 'Voltage Verification', status: board?.voltage_ok ? 'pass' : 'warning', detail: `${String(board?.voltage_readback_v ?? '?')} V` },
    { name: 'CRC Health', status: board?.crc_ok ? 'pass' : 'warning', detail: `${String(board?.crc_errors_received ?? 0)} errors` },
    { name: 'Temperature', status: board?.temperature_ok ? 'pass' : 'warning', detail: `${String(board?.temperature_c ?? '?')} C` },
    { name: 'EEPROM', status: board?.eeprom_valid ? 'pass' : (board?.eeprom_present ? 'warning' : 'unknown'), detail: String(board?.eeprom_model ?? board?.eeprom_serial ?? 'Unavailable') },
  ].filter(c => c.status !== 'unknown' || c.detail);

  return (
    <div>
      <div className="dp-result-title">
        Board Health
      </div>
      {board && (
        <div className="dp-grade-line">
          Chain {String(board.chain_id ?? '?')} | Grade {String(board.grade ?? '?')} | {String(board.grade_explanation ?? board.status ?? 'Snapshot summary')}
        </div>
      )}
      {items.length > 0 ? (
        <div className="dp-check-list">
          {items.map((check, i) => {
            const status = check.status as string;
            const icon = status === 'pass' || status === 'ok' ? '\u2713'
              : status === 'fail' || status === 'error' ? '\u2717'
              : status === 'warning' ? '!'
              : '?';
            const color = status === 'pass' || status === 'ok' ? 'var(--green)'
              : status === 'fail' || status === 'error' ? 'var(--red)'
              : status === 'warning' ? 'var(--yellow)'
              : 'var(--text-dim)';
            return (
              <div key={i} className="dp-check">
                <span className="dp-check-icon" style={{ color }}>
                  {icon}
                </span>
                <span className="dp-check-name">{check.name as string}</span>
                {check.detail != null && (
                  <span className="dp-check-detail">
                    {String(check.detail)}
                  </span>
                )}
              </div>
            );
          })}
        </div>
      ) : (
        <div className="json-response dp-json-fallback">
          {JSON.stringify(data, null, 2)}
        </div>
      )}
    </div>
  );
}

function StructuredResult({ type, data }: { type: TestType; data: Record<string, unknown> }) {
  switch (type) {
    case 'hashreport': return <HashReportTable data={data} />;
    case 'chiphealth': return <ChipHealthGrid data={data} />;
    case 'boardhealth': return <BoardHealthChecklist data={data} />;
  }
}

export function DiagnosticsPanel() {
  const { activeChain } = useActiveHardware();
  const [chain, setChain] = useState<number | undefined>(undefined);
  const [duration, setDuration] = useState(5);
  const [recentReports, setRecentReports] = useState<DiagnosticReportMetadata[]>([]);
  const [recentReportsLoading, setRecentReportsLoading] = useState(true);
  const [recentReportsError, setRecentReportsError] = useState<string | null>(null);
  const [tests, setTests] = useState<Record<TestType, TestState>>({
    hashreport: { ...initialState },
    chiphealth: { ...initialState },
    boardhealth: { ...initialState },
  });

  const pollRefs = useRef<Record<string, ReturnType<typeof setInterval>>>({});

  // DASH-STATE-2: guard async poll/state writes against an unmounted component.
  // pollStatus() runs a 2s setInterval whose callback calls updateTest() (a
  // setState). Navigating to another Advanced tool mid-test unmounts this panel
  // but leaves the interval alive — without this guard it keeps firing setState
  // on the dead component (React warning + a zombie 2s poll). The unmount effect
  // below clears every interval; mountedRef short-circuits any in-flight tick
  // whose await resolves after the interval was cleared.
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      Object.values(pollRefs.current).forEach(clearInterval);
      pollRefs.current = {};
    };
  }, []);

  const updateTest = (type: TestType, update: Partial<TestState>) => {
    if (!mountedRef.current) return;
    setTests(prev => ({ ...prev, [type]: { ...prev[type], ...update } }));
  };

  const loadRecentReports = useCallback(async () => {
    setRecentReportsLoading(true);
    setRecentReportsError(null);
    try {
      const response = await api.getRecentDiagnosticReports(8);
      setRecentReports(response.reports || []);
    } catch (e: unknown) {
      setRecentReportsError(e instanceof Error ? e.message : 'Failed to load recent reports');
    } finally {
      setRecentReportsLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadRecentReports();
  }, [loadRecentReports]);

  const pollStatus = useCallback((type: TestType, testId: string, startTime: number) => {
    const getStatus = type === 'hashreport'
      ? api.getHashReportStatus
      : type === 'chiphealth'
        ? api.getChipHealthStatus
        : api.getBoardHealthStatus;

    const getResult = type === 'hashreport'
      ? api.getHashReportResult
      : type === 'chiphealth'
        ? api.getChipHealthResult
        : api.getBoardHealthResult;

    const iv = setInterval(async () => {
      try {
        const status = await getStatus(testId);
        // If the panel unmounted while this fetch was in flight, the unmount
        // effect already cleared the interval — stop before any further work.
        if (!mountedRef.current) { clearInterval(iv); return; }
        const eta = formatEta(startTime, status.progress_pct);
        updateTest(type, {
          progress: status.progress_pct,
          phase: status.phase,
          message: status.message,
          eta,
        });

        if (status.status === 'completed') {
          clearInterval(iv);
          delete pollRefs.current[type];
          try {
            const result = await getResult(testId);
            updateTest(type, { running: false, result: result as Record<string, unknown> });
          } catch {
            updateTest(type, { running: false });
          }
        } else if (status.status === 'cancelled') {
          clearInterval(iv);
          delete pollRefs.current[type];
          updateTest(type, {
            running: false,
            progress: status.progress_pct,
            phase: status.phase,
            message: status.message,
            error: 'Cancelled by user',
          });
        } else if (status.status === 'failed') {
          clearInterval(iv);
          delete pollRefs.current[type];
          updateTest(type, { running: false, error: status.message });
        }
      } catch (e: unknown) {
        clearInterval(iv);
        delete pollRefs.current[type];
        updateTest(type, { running: false, error: e instanceof Error ? e.message : 'Poll failed' });
      }
    }, 2000);

    pollRefs.current[type] = iv;
  }, []);

  const startTest = useCallback(async (type: TestType) => {
    const startTime = Date.now();
    updateTest(type, { ...initialState, running: true, startTime });

    const startFn = type === 'hashreport'
      ? api.startHashReport
      : type === 'chiphealth'
        ? api.startChipHealth
        : api.startBoardHealth;

      try {
        const req = {
          chain,
          ...(type === 'hashreport' ? { duration_minutes: duration } : {}),
        };
        const scope = chain == null ? '' : ` --chain ${chain}`;
        const dur = type === 'hashreport' ? ` --duration ${duration}` : '';
        echoCli(`diag run ${type}${scope}${dur}`);
        const res = await startFn(req);
        updateTest(type, { testId: res.test_id, message: res.message || 'Started...' });
        void loadRecentReports();
        pollStatus(type, res.test_id, startTime);
      } catch (e: unknown) {
        updateTest(type, {
          running: false,
          error: e instanceof Error ? e.message : 'Failed to start test',
        });
      }
  }, [chain, duration, loadRecentReports, pollStatus]);

  const reopenReport = useCallback(async (report: DiagnosticReportMetadata) => {
    const type = reportTypeToTestType(report.test_type);
    if (!type) return;

    const getResult = type === 'hashreport'
      ? api.getHashReportResult
      : type === 'chiphealth'
        ? api.getChipHealthResult
        : api.getBoardHealthResult;

    updateTest(type, { ...initialState, testId: report.report_id, message: 'Loading stored snapshot...' });

    try {
      const result = await getResult(report.report_id);
      updateTest(type, {
        running: false,
        testId: report.report_id,
        result: result as Record<string, unknown>,
        message: `Loaded persisted ${reportTypeLabel(report.test_type).toLowerCase()} snapshot.`,
      });
    } catch (e: unknown) {
      updateTest(type, {
        running: false,
        testId: report.report_id,
        error: e instanceof Error ? e.message : 'Failed to load stored snapshot',
      });
    }
  }, []);

  const cancelTest = async (type: TestType) => {
    const current = tests[type];
    if (type === 'hashreport' && current.testId) {
      try {
        await api.cancelHashReport(current.testId);
      } catch (e: unknown) {
        updateTest(type, {
          running: false,
          error: e instanceof Error ? e.message : 'Failed to cancel test',
        });
        if (pollRefs.current[type]) {
          clearInterval(pollRefs.current[type]);
          delete pollRefs.current[type];
        }
        return;
      }
    }

    if (pollRefs.current[type]) {
      clearInterval(pollRefs.current[type]);
      delete pollRefs.current[type];
    }
    updateTest(type, {
      ...initialState,
      testId: current.testId,
      message: 'Cancellation requested...',
      error: 'Cancelled by user',
    });
  };

  const runAll = async () => {
    echoCli(`diag run --all${chain == null ? '' : ` --chain ${chain}`}`);
    for (const type of ['hashreport', 'chiphealth', 'boardhealth'] as TestType[]) {
      await startTest(type);
    }
  };

  const exportReport = () => {
    const report: Record<string, unknown> = {
      timestamp: new Date().toISOString(),
      chain: chain ?? 'all',
    };
    for (const [type, state] of Object.entries(tests)) {
      report[type] = state.result || state.error || 'not run';
    }
    const blob = new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-diagnostics-${Date.now()}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const testConfigs: { type: TestType; label: string; description: string }[] = [
    {
      type: 'hashreport',
      label: 'Hash Rate Report',
      description: 'Measures per-chain and per-chip hash rate over a configurable duration. Reports consistency and identifies underperforming chips.',
    },
    {
      type: 'chiphealth',
      label: 'Chip Health Check',
      description: 'Scans each chip for response time, register integrity, CRC errors, and communication stability.',
    },
    {
      type: 'boardhealth',
      label: 'Board Health Check',
      description: 'Tests I2C bus, PIC controllers, voltage regulators, temperature sensors, and FPGA connectivity.',
    },
  ];

  const anyRunning = Object.values(tests).some(t => t.running);
  const anyResults = Object.values(tests).some(t => t.result);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// diagnostics</div>
          <h2 className="hacker-inspector-title">Structured Health Checks</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${anyRunning ? 'warning' : ''}`}>
            {anyRunning ? 'RUNNING' : anyResults ? 'COMPLETE' : 'READY'}
          </span>
          <ActionButton
            label={anyRunning ? 'Tests Running...' : 'Run All'}
            onClick={runAll}
            disabled={anyRunning}
          />
          <CliHint cmd={`diag run --all${chain == null ? '' : ` --chain ${chain}`}`} />
          {anyResults && (
            <button
              type="button"
              className="hacker-inspector-refresh"
              onClick={exportReport}
            >
              ⤓ EXPORT
            </button>
          )}
        </div>
      </header>

      <div className="hacker-inspector-body">
      {/* Controls */}
      <div className="register-inspector ds-card-hover dp-section">
        <div className="advanced-inline-actions dp-controls-row">
          <div>
            <label htmlFor="diagnostics-chain" className="advanced-control-label">
              Chain (optional)
            </label>
            <select
              id="diagnostics-chain"
              value={chain ?? 'all'}
              onChange={e => setChain(e.target.value === 'all' ? undefined : Number(e.target.value))}
            >
              <option value="all">All Chains</option>
              <option value={6}>Chain 6</option>
              <option value={7}>Chain 7</option>
              <option value={8}>Chain 8</option>
            </select>
          </div>
          <div>
            <label htmlFor="diagnostics-duration" className="advanced-control-label">
              Duration (HashReport)
            </label>
            <select id="diagnostics-duration" value={duration} onChange={e => setDuration(Number(e.target.value))}>
              <option value={1}>1 minute</option>
              <option value={5}>5 minutes</option>
              <option value={10}>10 minutes</option>
              <option value={30}>30 minutes</option>
            </select>
          </div>
        </div>
      </div>

      <div className="register-inspector ds-card-hover dp-section">
        <div className="dp-head-row">
          <div>
            <div className="dp-title">Recent Reports</div>
            <div className="dp-hint-sm">
              Reopen saved diagnostics without rerunning a new snapshot.
            </div>
          </div>
          <button
            type="button"
            className="btn btn-secondary advanced-compact-button"
            onClick={() => void loadRecentReports()}
          >
            Refresh
          </button>
        </div>

        {recentReportsError && (
          <div className="dp-err">
            {recentReportsError}
          </div>
        )}

        {recentReportsLoading ? (
          <div className="adv-state is-loading is-inline">Loading recent reports...</div>
        ) : recentReports.length === 0 ? (
          <div className="adv-empty-note">No persisted reports found yet.</div>
        ) : (
          <div className="dp-report-list">
            {recentReports.map(report => {
              const type = reportTypeToTestType(report.test_type);
              return (
                <div
                  key={report.report_id}
                  className="dp-report-row"
                >
                  <div className="dp-report-meta">
                    <div className="dp-report-titlerow">
                      <span className="dp-report-name">{reportTypeLabel(report.test_type)}</span>
                      {report.grade && (
                        <span className="dp-report-grade">Grade {report.grade}</span>
                      )}
                    </div>
                    <div className="dp-report-line">
                      {formatRecentTimestamp(report.generated_at)} | {report.report_id} | JSON {formatBytes(report.json_size_bytes)}{report.html_size_bytes > 0 ? ` | HTML ${formatBytes(report.html_size_bytes)}` : ''}
                    </div>
                    {report.firmware_version && (
                      <div className="dp-report-line">
                        Firmware {report.firmware_version}
                      </div>
                    )}
                  </div>
                  <div className="advanced-inline-actions dp-actions-shrink">
                    {type && (
                      <button
                        type="button"
                        className="btn btn-secondary advanced-compact-button"
                        onClick={() => void reopenReport(report)}
                      >
                        Reopen
                      </button>
                    )}
                    {type && report.html_size_bytes > 0 && (
                      <button
                        type="button"
                        className="btn btn-secondary advanced-compact-button"
                        onClick={() => window.open(getReportPath(type, report.report_id), '_blank', 'noopener,noreferrer')}
                        aria-label={`Open HTML report for ${reportTypeLabel(report.test_type)} in a new tab`}
                      >
                        Open HTML
                      </button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* Test cards */}
      <div className="dp-cards">
        {testConfigs.map(({ type, label, description }) => {
          const state = tests[type];
          return (
            <div key={type} className="register-inspector ds-card-hover">
              <div className="dp-head-row is-top">
                <div>
                  <div className="dp-title">{label}</div>
                  <div className="dp-hint-sm">{description}</div>
                </div>
                <div className="advanced-inline-actions dp-actions-shrink">
                  {state.testId && (
                    <button
                      type="button"
                      className="btn btn-secondary advanced-compact-button"
                      onClick={() => window.open(getReportPath(type, state.testId as string), '_blank', 'noopener,noreferrer')}
                      aria-label={`Open ${label} report in a new tab`}
                    >
                      Open Report
                    </button>
                  )}
                  {state.running && (
                    <button
                      type="button"
                      className="btn btn-danger advanced-compact-button"
                      onClick={() => cancelTest(type)}
                    >
                      Cancel
                    </button>
                  )}
                  <ActionButton
                    label={state.running ? 'Running...' : 'Start'}
                    onClick={() => startTest(type)}
                    disabled={state.running}
                  />
                </div>
              </div>

              <CliHint cmd={`diag run ${type}${chain == null ? '' : ` --chain ${chain}`}${type === 'hashreport' ? ` --duration ${duration}` : ''}`} />


              {/* Progress bar */}
              {state.running && (
                <div className="dp-progress">
                  <div className="dp-progress-row">
                    <span>{state.phase || 'Initializing...'}</span>
                    <span>{state.progress.toFixed(0)}% {state.eta ? `| ETA: ${state.eta}` : ''}</span>
                  </div>
                  <div className="dp-progress-track" role="progressbar" aria-label={`${label} progress`} aria-valuenow={Math.round(state.progress)} aria-valuemin={0} aria-valuemax={100}>
                    <div className="dp-progress-fill" style={{ width: `${state.progress}%` }} />
                  </div>
                  {state.message && (
                    <div className="dp-progress-msg">
                      {state.message}
                    </div>
                  )}
                </div>
              )}

              {/* Error */}
              {state.error && (
                <div className="dp-card-err">
                  {state.error}
                </div>
              )}

              {/* Structured result */}
              {state.result && (
                <div className="adv-mt-8">
                  <div className="dp-result-head">
                    Complete{state.testId ? ` | test_id ${state.testId}` : ''}
                  </div>
                  <StructuredResult type={type} data={state.result} />
                </div>
              )}
            </div>
          );
        })}
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{Object.keys(tests).length} test types</span>
          <span>{Object.values(tests).filter(t => t.running).length} running</span>
          <span>{Object.values(tests).filter(t => t.result).length} with results</span>
        </div>
      </footer>
    </div>
  );
}
