import React, { useState, useRef, useEffect, useCallback } from 'react';
import { CliEchoBus } from '../../hooks/useCliEcho';
import { useMinerStore, type LogEntry } from '../../store/miner';
import { api } from '../../api/client';

type SeverityLevel = 'info' | 'warn' | 'error' | 'debug';

const HELP_TEXT = `Available commands:
  read <chain> <offset> [count]   Read FPGA registers (e.g. read 6 0x0000 4)
  i2c <bus> <addr> [reg]          Read I2C device (e.g. i2c 0 0x55)
  asic <chain> <cmd> [chip] [reg] Send ASIC command (e.g. asic 6 ReadReg 0 0x0C)
  status                          Show miner status summary
  chains                          Show chain status
  logs [lines]                    Fetch recent daemon logs (default: 50)
  clear                           Clear console output
  help                            Show this help message
`;

interface CommandResult {
  id: number;
  timestamp: number;
  level: 'info' | 'warn' | 'error' | 'debug';
  source: 'mining' | 'system';
  message: string;
}

const COMMAND_HISTORY_KEY = 'dcentos-hacker-console-history';
const COMMAND_HISTORY_CAP = 100;

function loadCommandHistory(): string[] {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return [];
    const raw = window.localStorage.getItem(COMMAND_HISTORY_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(s => typeof s === 'string').slice(-COMMAND_HISTORY_CAP);
  } catch {
    return [];
  }
}

function persistCommandHistory(history: string[]): void {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return;
    window.localStorage.setItem(
      COMMAND_HISTORY_KEY,
      JSON.stringify(history.slice(-COMMAND_HISTORY_CAP)),
    );
  } catch {
    // localStorage may be unavailable (privacy mode, embedded contexts) — silently no-op
  }
}

export function Console() {
  const allLogs = useMinerStore(s => s.logEntries);
  // `s.status` is already typed `StatusResponse | null` in the store — the old
  // `as any` cast threw that away (SLOP-TOOL-05). Keep the real type so the
  // console's `status.*` field reads stay type-checked.
  const status = useMinerStore(s => s.status);
  const [activeTab, setActiveTab] = useState<'mining' | 'system'>('mining');
  const [search, setSearch] = useState('');
  const [activeFilters, setActiveFilters] = useState<SeverityLevel[]>(['info', 'warn', 'error', 'debug']);
  const [showTimestamps, setShowTimestamps] = useState(true);
  const [wordWrap, setWordWrap] = useState(true);
  const [paused, setPaused] = useState(false);
  const [pausedSnapshot, setPausedSnapshot] = useState<LogEntry[]>([]);
  const outputRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Command prompt state
  const [commandInput, setCommandInput] = useState('');
  const [commandHistory, setCommandHistory] = useState<string[]>(() => loadCommandHistory());
  const [historyIndex, setHistoryIndex] = useState(-1);
  const [commandResults, setCommandResults] = useState<CommandResult[]>([]);
  const cmdCounterRef = useRef(0);

  // Tail -f autoscroll state
  const [autoscrollPaused, setAutoscrollPaused] = useState(false);
  const [pendingLineCount, setPendingLineCount] = useState(0);
  const filteredCountRef = useRef(0);

  // When pausing, snapshot current logs; when resuming, clear snapshot
  useEffect(() => {
    if (paused) {
      setPausedSnapshot(allLogs);
    }
  }, [paused]);

  const logs = paused ? pausedSnapshot : allLogs;

  // Merge log entries with command results
  const allEntries = [...logs, ...commandResults].sort((a, b) => a.timestamp - b.timestamp);

  const toggleLevel = useCallback((level: SeverityLevel) => {
    setActiveFilters(prev =>
      prev.includes(level) ? prev.filter(l => l !== level) : [...prev, level]
    );
  }, []);

  // Filter by source tab, level, and search
  const filtered = allEntries.filter(l => {
    if (l.source !== activeTab) return false;
    if (!activeFilters.includes(l.level as SeverityLevel)) return false;
    if (search && !l.message.toLowerCase().includes(search.toLowerCase())) return false;
    return true;
  });

  // Counters
  const errorCount = allEntries.filter(l => l.level === 'error').length;
  const warnCount = allEntries.filter(l => l.level === 'warn').length;

  // Auto-scroll when log stream not paused AND tail autoscroll not paused
  useEffect(() => {
    if (!paused && !autoscrollPaused && outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, [allLogs, commandResults, paused, autoscrollPaused]);

  // Track new lines while autoscroll is paused so we can surface a "N new" pill
  useEffect(() => {
    const total = filtered.length;
    if (autoscrollPaused) {
      const delta = total - filteredCountRef.current;
      if (delta > 0) setPendingLineCount(prev => prev + delta);
    } else {
      setPendingLineCount(0);
    }
    filteredCountRef.current = total;
    // We intentionally compare against filtered.length only — feedback loop guarded by autoscrollPaused.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filtered.length, autoscrollPaused]);

  // Detect user scroll: if user scrolls up away from bottom, pause autoscroll
  const handleOutputScroll = useCallback(() => {
    const el = outputRef.current;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - (el.scrollTop + el.clientHeight);
    const nearBottom = distanceFromBottom <= 8;
    if (nearBottom) {
      if (autoscrollPaused) {
        setAutoscrollPaused(false);
        setPendingLineCount(0);
      }
    } else if (!autoscrollPaused) {
      setAutoscrollPaused(true);
    }
  }, [autoscrollPaused]);

  const resumeAutoscroll = useCallback(() => {
    setAutoscrollPaused(false);
    setPendingLineCount(0);
    const el = outputRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  const addResult = useCallback((level: CommandResult['level'], message: string) => {
    cmdCounterRef.current += 1;
    const entry: CommandResult = {
      id: cmdCounterRef.current + 1000000, // avoid collision with log IDs
      timestamp: Date.now(),
      level,
      source: activeTab,
      message,
    };
    setCommandResults(prev => [...prev, entry].slice(-200));
  }, [activeTab]);

  // Design-handoff : mirror GUI actions into the console as their
  // `$ dcent …` command equivalent (the "learn the CLI while you explore"
  // identity move). Additive — does not change any existing console path.
  useEffect(() => {
    return CliEchoBus.subscribe(e => {
      addResult('info', `$ dcent ${e.cmd}${e.note ? `  # ${e.note}` : ''}`);
    });
  }, [addResult]);

  const executeCommand = useCallback(async (raw: string) => {
    const trimmed = raw.trim();
    if (!trimmed) return;

    // Add to history (persisted to localStorage, capped at 100)
    setCommandHistory(prev => {
      const next = prev.filter(c => c !== trimmed);
      next.push(trimmed);
      const capped = next.slice(-COMMAND_HISTORY_CAP);
      persistCommandHistory(capped);
      return capped;
    });
    setHistoryIndex(-1);

    addResult('info', `$ ${trimmed}`);

    const parts = trimmed.split(/\s+/);
    const cmd = parts[0].toLowerCase();

    try {
      switch (cmd) {
        case 'help': {
          addResult('info', HELP_TEXT);
          break;
        }
        case 'clear': {
          setCommandResults([]);
          break;
        }
        case 'status': {
          if (status) {
            const chains = Array.isArray(status.chains) ? status.chains : [];
            const fanRpm = status.fans?.rpm ?? 'n/a';
            const fanPwm = status.fans?.pwm ?? 'n/a';
            const poolUrl = status.pool?.url ?? 'unknown';
            const poolStatus = status.pool?.status ?? 'unknown';
            addResult('info', [
              `Hashrate: ${(status.hashrate_ghs / 1000).toFixed(2)} TH/s`,
              `Chains: ${chains.length}`,
              `Accepted: ${status.accepted}  Rejected: ${status.rejected}`,
              `Fan: ${fanRpm} RPM (PWM ${fanPwm})`,
              `Pool: ${poolUrl} (${poolStatus})`,
              `Uptime: ${Math.floor(status.uptime_s / 3600)}h ${Math.floor((status.uptime_s % 3600) / 60)}m`,
            ].join('\n'));
          } else {
            addResult('warn', 'No status data available — dcentrald may not be running');
          }
          break;
        }
        case 'chains': {
          const chains = Array.isArray(status?.chains) ? status.chains : [];
          if (chains.length > 0) {
            for (const ch of chains) {
              addResult('info', `Chain ${ch.id}: ${ch.chips} chips, ${ch.frequency_mhz} MHz, ${(ch.voltage_mv / 1000).toFixed(3)}V, ${ch.temp_c.toFixed(1)}C, ${ch.hashrate_ghs.toFixed(1)} GH/s, ${ch.errors} errors, ${ch.status}`);
            }
          } else {
            addResult('warn', 'No chain data available');
          }
          break;
        }
        case 'logs': {
          const lineCount = parseInt(parts[1]) || 50;
          addResult('info', `Fetching last ${lineCount} log lines...`);
          try {
            const data = await api.getDebugLog(lineCount);
            const lines = data.lines || data.log || [];
            if (Array.isArray(lines) && lines.length > 0) {
              for (const line of lines.slice(-lineCount)) {
                const clean = typeof line === 'string' ? line.replace(/\x1B\[[0-9;]*m/g, '').trim() : JSON.stringify(line);
                if (clean) addResult(clean.includes('ERROR') ? 'error' : clean.includes('WARN') ? 'warn' : 'info', clean);
              }
            } else {
              addResult('warn', 'No log lines returned');
            }
          } catch (e: any) {
            addResult('error', `Failed to fetch logs: ${e.message}`);
          }
          break;
        }
        case 'read': {
          const chain = parseInt(parts[1]);
          const offset = parts[2] || '0x0000';
          const count = parseInt(parts[3]) || 4;
          if (isNaN(chain)) {
            addResult('error', 'Usage: read <chain> <offset> [count]');
            break;
          }
          const res = await api.readRegisters(chain, offset, count);
          const hex = (res.values ?? []).map(b => b.toString(16).padStart(2, '0')).join(' ');
          addResult('info', `[Ch${chain}] ${offset}: ${hex}`);
          break;
        }
        case 'i2c': {
          const bus = parseInt(parts[1]);
          const addr = parts[2];
          const reg = parts[3];
          if (isNaN(bus) || !addr) {
            addResult('error', 'Usage: i2c <bus> <addr> [reg]');
            break;
          }
          const res = await api.readI2c(bus, addr, reg);
          const hex = (res.data ?? []).map(b => b.toString(16).padStart(2, '0')).join(' ');
          addResult('info', `[I2C bus${bus} ${addr}${reg ? ' reg=' + reg : ''}]: ${hex || '(no data)'}`);
          break;
        }
        case 'asic': {
          const chain = parseInt(parts[1]);
          const asicCmd = parts[2];
          const chip = parts[3] ? parseInt(parts[3]) : undefined;
          const reg = parts[4] || '0x00';
          if (isNaN(chain) || !asicCmd) {
            addResult('error', 'Usage: asic <chain> <command> [chip] [register]');
            break;
          }
          const res = await api.sendAsicCommand({
            chain,
            command: asicCmd,
            chip: isNaN(chip as number) ? undefined : chip,
            register: reg,
            confirm: true,
          });
          const hex = (res.response ?? []).map(b => b.toString(16).padStart(2, '0')).join(' ');
          addResult('info', `[ASIC Ch${chain} ${asicCmd}${chip !== undefined ? ` chip${chip}` : ''}]: ${hex || '(empty)'}`);
          break;
        }
        default: {
          addResult('error', `Unknown command: ${cmd}. Type 'help' for available commands.`);
        }
      }
    } catch (e: unknown) {
      addResult('error', `Error: ${e instanceof Error ? e.message : 'Command failed'}`);
    }
  }, [status, addResult]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      executeCommand(commandInput);
      setCommandInput('');
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (commandHistory.length > 0) {
        const nextIdx = historyIndex === -1 ? commandHistory.length - 1 : Math.max(0, historyIndex - 1);
        setHistoryIndex(nextIdx);
        setCommandInput(commandHistory[nextIdx]);
      }
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (historyIndex === -1) return;
      const nextIdx = historyIndex + 1;
      if (nextIdx >= commandHistory.length) {
        setHistoryIndex(-1);
        setCommandInput('');
      } else {
        setHistoryIndex(nextIdx);
        setCommandInput(commandHistory[nextIdx]);
      }
    }
  };

  const handleExport = () => {
    const lines = filtered.map(entry => {
      const d = new Date(entry.timestamp);
      const ts = d.toTimeString().split(' ')[0] + '.' + String(d.getMilliseconds()).padStart(3, '0');
      return `[${ts}] [${entry.level.toUpperCase().padEnd(5)}] ${entry.message}`;
    });
    const blob = new Blob([lines.join('\n')], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-console-${Date.now()}.log`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return d.toTimeString().split(' ')[0] + '.' + String(d.getMilliseconds()).padStart(3, '0');
  };

  const levelClass = (level: string) => {
    switch (level) {
      case 'error': return 'log-error';
      case 'warn': return 'log-warn';
      case 'debug': return 'log-debug';
      default: return 'log-info';
    }
  };

  const hasAnyLogs = allEntries.length > 0;

  return (
    <div className="console-page">
      <div className="console-header">
        <div className="section-title console-header-title">
          LIVE CONSOLE
        </div>
        <div className="console-header-meta">
          {errorCount > 0 && (
            <span className="console-header-count console-header-count-err">
              {errorCount} ERR
            </span>
          )}
          {warnCount > 0 && (
            <span className="console-header-count console-header-count-warn">
              {warnCount} WARN
            </span>
          )}
          <button
            type="button"
            className="btn btn-secondary advanced-compact-button"
            onClick={handleExport}
            aria-label={`Export ${activeTab} console entries`}
          >
            Export
          </button>
        </div>
      </div>

      {/* Sub-tabs */}
      <div className="tab-bar console-tab-bar" role="tablist" aria-label="Console log tabs">
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === 'mining'}
          className={`tab ${activeTab === 'mining' ? 'active' : ''}`}
          onClick={() => setActiveTab('mining')}
        >
          Mining Log
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={activeTab === 'system'}
          className={`tab ${activeTab === 'system' ? 'active' : ''}`}
          onClick={() => setActiveTab('system')}
        >
          System Log
        </button>
      </div>

      <div className="console">
        {/* Toolbar */}
        <div className="console-toolbar">
          <input
            type="text"
            placeholder="Search logs..."
            value={search}
            onChange={e => setSearch(e.target.value)}
            aria-label={`Search ${activeTab} log output`}
          />

          {/* Severity toggle buttons */}
          <div className="console-severity-group" role="group" aria-label="Filter log severities">
            {(['info', 'warn', 'error', 'debug'] as const).map(level => {
              const isActive = activeFilters.includes(level);
              return (
                <button
                  key={level}
                  type="button"
                  className={`advanced-toggle-button console-severity-btn console-severity-btn-${level}${isActive ? ' is-active' : ''}`}
                  onClick={() => toggleLevel(level)}
                  aria-pressed={isActive}
                  aria-label={`${isActive ? 'Hide' : 'Show'} ${level} log entries`}
                >
                  {level[0].toUpperCase()}
                </button>
              );
            })}
          </div>

          {/* Timestamp toggle */}
          <button
            type="button"
            onClick={() => setShowTimestamps(!showTimestamps)}
            className="advanced-toggle-button"
            aria-pressed={showTimestamps}
            aria-label={`${showTimestamps ? 'Hide' : 'Show'} timestamps`}
            title="Toggle timestamps"
          >
            TS
          </button>

          {/* Wrap toggle */}
          <button
            type="button"
            onClick={() => setWordWrap(!wordWrap)}
            className="advanced-toggle-button"
            aria-pressed={wordWrap}
            aria-label={`${wordWrap ? 'Disable' : 'Enable'} word wrap`}
            title="Toggle word wrap"
          >
            Wrap
          </button>

          <button
            type="button"
            className={`btn ${paused ? 'btn-danger' : 'btn-secondary'} console-pause-btn`}
            onClick={() => setPaused(!paused)}
            aria-pressed={paused}
          >
            {paused ? 'PAUSED' : 'Pause'}
          </button>
          <span className="advanced-toolbar-status" aria-live="polite">
            {filtered.length} lines
          </span>
        </div>

        {/* Output (relative, so pill anchors to bottom) */}
        <div className="console-output-shell">
        <div
          className={`console-output console-output-shell-inner ${wordWrap ? 'is-wrapped' : 'is-unwrapped'}`}
          ref={outputRef}
          onScroll={handleOutputScroll}
          role="log"
          aria-label={`${activeTab} console output`}
          aria-live={paused ? 'off' : 'polite'}
          aria-relevant="additions text"
        >
          {filtered.map(entry => (
            <div key={entry.id} className={levelClass(entry.level)}>
              {showTimestamps && (
                <>
                  <span className="console-line-ts">[{formatTime(entry.timestamp)}]</span>
                  {' '}
                </>
              )}
              <span className="console-line-level">[{entry.level.toUpperCase().padEnd(5)}]</span>
              {' '}
              {entry.message}
            </div>
          ))}
          {filtered.length === 0 && (
            <div className="console-output-empty">
              {!hasAnyLogs
                ? 'Waiting for log messages from dcentrald...'
                : `No ${activeTab} log entries match filter`}
            </div>
          )}
        </div>

        {/* Tail-f autoscroll pill — sticky at bottom when user scrolled up */}
        {autoscrollPaused && pendingLineCount > 0 && (
          <button
            type="button"
            className="console-autoscroll-pill"
            onClick={resumeAutoscroll}
            aria-label={`Resume autoscroll — ${pendingLineCount} new lines below`}
          >
            {`> +${pendingLineCount} new ${pendingLineCount === 1 ? 'line' : 'lines'} — click to resume`}
          </button>
        )}
        </div>

        {/* Command input */}
        <div className="console-prompt">
          <span className="console-prompt-sigil">$</span>
          <input
            ref={inputRef}
            type="text"
            value={commandInput}
            onChange={e => setCommandInput(e.target.value)}
            onKeyDown={handleKeyDown}
            placeholder="Type command... (help for list)"
            aria-label="Console command input"
            className="console-prompt-input"
          />
          {commandInput === '' && (
            <span
              className="ds-cursor-blink console-prompt-cursor"
              aria-hidden="true"
            >
              _
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
