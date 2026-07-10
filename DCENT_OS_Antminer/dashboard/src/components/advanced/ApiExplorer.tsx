import React, { useState } from 'react';
import { apiFetch } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';

interface Endpoint {
  method: 'GET' | 'POST';
  path: string;
  category: string;
  description: string;
  sampleBody?: string;
}

const ENDPOINTS: Endpoint[] = [
  // Core
  { method: 'GET', path: '/api/status', category: 'Core', description: 'Current miner status, hashrate, chains, fans, pool' },
  { method: 'GET', path: '/api/config', category: 'Core', description: 'Current configuration' },
  { method: 'POST', path: '/api/config', category: 'Core', description: 'Update configuration', sampleBody: '{"mode": {"active": "hacker"}}' },
  { method: 'GET', path: '/api/system/info', category: 'Core', description: 'System information (firmware, model, chip type)' },
  { method: 'GET', path: '/api/system/asic', category: 'Core', description: 'ASIC chain information' },

  // Stats
  { method: 'GET', path: '/api/stats', category: 'Stats', description: 'Detailed stats with power and per-chain data' },
  { method: 'GET', path: '/api/history', category: 'Stats', description: 'Historical data points for charts' },

  // Pools
  { method: 'GET', path: '/api/pools', category: 'Pools', description: 'Pool configuration and status' },
  { method: 'POST', path: '/api/pools', category: 'Pools', description: 'Configure pool', sampleBody: '{"url": "stratum+tcp://pool.d-central.tech:3333", "worker": "myworker", "password": "x"}' },

  // Profiles
  { method: 'GET', path: '/api/profiles', category: 'Profiles', description: 'Tuning profiles' },
  { method: 'POST', path: '/api/profiles', category: 'Profiles', description: 'Save tuning profile', sampleBody: '{"name": "efficient", "frequency_mhz": 550, "voltage_mv": 8500}' },

  // Heater
  { method: 'GET', path: '/api/home/status', category: 'Home', description: 'Heater mode status (BTU, noise, sats)' },
  { method: 'POST', path: '/api/home/target', category: 'Home', description: 'Set heater power target', sampleBody: '{"preset": "medium"}' },
  { method: 'GET', path: '/api/home/presets', category: 'Home', description: 'Available heater presets' },
  { method: 'POST', path: '/api/home/room-temp', category: 'Home', description: 'Set room temperature', sampleBody: '{"temp_c": 22.5}' },
  { method: 'GET', path: '/api/home/night-mode', category: 'Home', description: 'Night mode configuration' },
  { method: 'POST', path: '/api/home/night-mode', category: 'Home', description: 'Configure night mode', sampleBody: '{"enabled": true, "start_hour": 22, "end_hour": 7}' },
  { method: 'GET', path: '/api/home/history', category: 'Home', description: 'Heater history data' },

  // Actions
  { method: 'POST', path: '/api/action/restart', category: 'Actions', description: 'Restart mining daemon' },
  { method: 'POST', path: '/api/action/reboot', category: 'Actions', description: 'Reboot the miner' },
  { method: 'POST', path: '/api/action/sleep', category: 'Actions', description: 'Enter sleep/curtailment mode' },
  { method: 'POST', path: '/api/action/wake', category: 'Actions', description: 'Wake from sleep mode' },

  // Debug
  { method: 'GET', path: '/api/debug/registers?chain=6&offset=0x0000&count=4', category: 'Debug', description: 'Read FPGA registers' },
  { method: 'POST', path: '/api/debug/registers', category: 'Debug', description: 'Write FPGA register', sampleBody: '{"chain": 6, "offset": "0x0000", "value": "0x00000000", "confirm": true}' },
  { method: 'GET', path: '/api/debug/i2c?bus=0&addr=0x55', category: 'Debug', description: 'Read I2C device' },
  { method: 'POST', path: '/api/debug/i2c', category: 'Debug', description: 'Write I2C device', sampleBody: '{"bus": 0, "addr": "0x55", "data": [85, 170, 6], "confirm": true}' },
  { method: 'POST', path: '/api/debug/asic-command', category: 'Debug', description: 'Send raw ASIC command', sampleBody: '{"chain": 6, "command": "ChipID", "confirm": true}' },
  { method: 'GET', path: '/api/debug/pid-state', category: 'Debug', description: 'Read PID controller state' },
  { method: 'POST', path: '/api/debug/pid-params', category: 'Debug', description: 'Set PID parameters', sampleBody: '{"kp": 2.0, "ki": 0.1, "kd": 0.5, "setpoint": 55, "confirm": true}' },
  { method: 'POST', path: '/api/debug/chip/frequency', category: 'Debug', description: 'Set chip frequency', sampleBody: '{"chain": 6, "chip": 0, "freq_mhz": 600, "confirm": true}' },
  { method: 'POST', path: '/api/debug/chip/voltage', category: 'Debug', description: 'Set chain voltage', sampleBody: '{"chain": 6, "pic_value": 100, "confirm": true}' },

  // Diagnostics
  { method: 'POST', path: '/api/diagnostics/hashreport/start', category: 'Diagnostics', description: 'Start hash rate report', sampleBody: '{"duration_minutes": 5}' },
  { method: 'POST', path: '/api/diagnostics/chip-health/start', category: 'Diagnostics', description: 'Start chip health check', sampleBody: '{}' },
  { method: 'POST', path: '/api/diagnostics/board-health/start', category: 'Diagnostics', description: 'Start board health check', sampleBody: '{}' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/network', category: 'Diagnostics', description: 'Network troubleshoot' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/psu', category: 'Diagnostics', description: 'PSU troubleshoot' },
  { method: 'GET', path: '/api/diagnostics/troubleshoot/fpga', category: 'Diagnostics', description: 'FPGA troubleshoot' },
];

/** Walk a parsed API response and gather every field the daemon flagged as an
 *  AxeOS/pyasic compatibility placeholder — `unsupported_metrics` (REST, e.g.
 *  /api/system/info) and nested `_DCENTUnsupported` arrays (CGMiner shim). Those
 *  fields read 0 only for ecosystem interop and are NOT real telemetry, so the
 *  raw 0s in the response must be flagged honestly as "n/a". (Omega P3-8.) */
function collectUnsupportedMetrics(value: unknown, out: Set<string>): void {
  if (!value || typeof value !== 'object') return;
  if (Array.isArray(value)) {
    for (const item of value) collectUnsupportedMetrics(item, out);
    return;
  }
  const obj = value as Record<string, unknown>;
  for (const flagKey of ['unsupported_metrics', '_DCENTUnsupported']) {
    const arr = obj[flagKey];
    if (Array.isArray(arr)) {
      for (const k of arr) if (typeof k === 'string' && k.length > 0) out.add(k);
    }
  }
  for (const v of Object.values(obj)) collectUnsupportedMetrics(v, out);
}

export function ApiExplorer() {
  const { isProxyMode } = useSystemHealth();
  const [selected, setSelected] = useState<Endpoint>(ENDPOINTS[0]);
  const [body, setBody] = useState(ENDPOINTS[0].sampleBody || '');
  const [response, setResponse] = useState('');
  const [unsupported, setUnsupported] = useState<string[]>([]);
  const [statusCode, setStatusCode] = useState<number | null>(null);
  const [timing, setTiming] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);

  const categories = [...new Set(ENDPOINTS.map(e => e.category))];

  const selectEndpoint = (ep: Endpoint) => {
    setSelected(ep);
    setBody(ep.sampleBody || '');
    setResponse('');
    setUnsupported([]);
    setStatusCode(null);
    setTiming(null);
  };

  const proxyBlocked = isProxyMode && selected.method === 'POST';

  const handleSend = async () => {
    if (proxyBlocked) {
      setStatusCode(0);
      setTiming(null);
      setResponse('Blocked: bosminer owns hardware in proxy/hybrid mode.');
      return;
    }

    setLoading(true);
    setResponse('');
    setUnsupported([]);
    setStatusCode(null);
    setTiming(null);

    const start = performance.now();
    try {
      const opts: RequestInit = { method: selected.method };
      if (selected.method === 'POST' && body.trim()) {
        opts.headers = { 'Content-Type': 'application/json' };
        opts.body = body;
      }

      const res = await apiFetch(selected.path, opts);
      const elapsed = performance.now() - start;
      setStatusCode(res.status);
      setTiming(elapsed);

      const contentType = res.headers.get('content-type') || '';
      if (contentType.includes('json')) {
        const json = await res.json();
        setResponse(JSON.stringify(json, null, 2));
        const flagged = new Set<string>();
        collectUnsupportedMetrics(json, flagged);
        setUnsupported(Array.from(flagged));
      } else {
        const text = await res.text();
        setResponse(text);
      }
    } catch (e: unknown) {
      const elapsed = performance.now() - start;
      setTiming(elapsed);
      setStatusCode(0);
      setResponse(e instanceof Error ? e.message : 'Request failed');
    }
    setLoading(false);
  };

  return (
    <div className="advanced-page">
      <div className="advanced-page-toolbar">
        <div className="advanced-page-heading">
          <div className="section-title ax-title">
            API EXPLORER
          </div>
          <div className="advanced-page-copy">
            Call DCENT_OS endpoints directly, inspect raw responses, and validate request bodies without leaving the dashboard.
          </div>
        </div>
      </div>

      <div className="advanced-split-layout">
        {/* Endpoint list */}
        <div className="register-inspector advanced-scroll-card">
          {categories.map(cat => (
            <div key={cat} className="ax-cat">
              <div className="ax-cat-title">
                {cat}
              </div>
              {ENDPOINTS.filter(e => e.category === cat).map((ep, i) => (
                <button
                  key={`${ep.method}-${ep.path}-${i}`}
                  onClick={() => selectEndpoint(ep)}
                  className={`ax-ep-row${selected === ep ? ' is-active' : ''}`}
                >
                  <span className={`ax-method ${ep.method === 'GET' ? 'is-get' : 'is-post'}`}>
                    {ep.method}
                  </span>
                  <span className="ax-ep-path">
                    {ep.path.split('?')[0]}
                  </span>
                </button>
              ))}
            </div>
          ))}
        </div>

        {/* Request/Response panel */}
        <div className="api-explorer">
          {/* Selected endpoint info */}
          <div className="register-inspector ds-card-hover adv-mb-16">
            <div className="ax-sel-row">
              <span className={`ax-method-pill ${selected.method === 'GET' ? 'is-get' : 'is-post'}`}>
                {selected.method}
              </span>
              <span className="ax-sel-path">
                {selected.path}
              </span>
            </div>
            <div className="ax-sel-desc">
              {selected.description}
            </div>
            {proxyBlocked && (
              <div className="ax-proxy-warn">
                POST requests disabled: bosminer owns hardware in proxy/hybrid mode.
              </div>
            )}
          </div>

          {/* Request body */}
          {selected.method === 'POST' && (
            <div className="adv-mb-16">
              <div className="ax-field-label">
                Request Body (JSON)
              </div>
              <textarea
                value={body}
                onChange={e => setBody(e.target.value)}
                rows={6}
              />
            </div>
          )}

          {/* Send button */}
          <div className="advanced-inline-actions adv-mb-16">
            <ActionButton
              label={loading ? 'Sending...' : 'Send Request'}
              onClick={handleSend}
              disabled={loading || proxyBlocked}
            />
            {statusCode !== null && (
              <span className="ax-status">
                <span
                  className="ax-status-code"
                  style={{ color: statusCode >= 200 && statusCode < 300 ? 'var(--green)' : 'var(--red)' }}
                >
                  {statusCode}
                </span>
                {timing !== null && (
                  <span className="ax-status-time">
                    {timing.toFixed(0)}ms
                  </span>
                )}
              </span>
            )}
          </div>

          {/* Response */}
          {response && (
            <div>
              <div className="ax-resp-head">
                <div className="ax-field-label">
                  Response
                </div>
                <button
                  onClick={() => {
                    navigator.clipboard.writeText(response).then(() => {
                      setCopied(true);
                      setTimeout(() => setCopied(false), 2000);
                    });
                  }}
                  className={`ax-copy-btn${copied ? ' is-copied' : ''}`}
                >
                  {copied ? 'Copied!' : 'Copy'}
                </button>
              </div>
              {unsupported.length > 0 && (
                <div className="adv-empty-note adv-mb-8">
                  {unsupported.length} compatibility placeholder
                  {unsupported.length === 1 ? '' : 's'} in this response always read 0
                  for AxeOS/pyasic interop — they are not real telemetry (treat as n/a):{' '}
                  {unsupported.join(', ')}.
                </div>
              )}
              <div className="json-response">
                {response}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
