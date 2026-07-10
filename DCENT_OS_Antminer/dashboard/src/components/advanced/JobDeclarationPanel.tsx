import React, { useEffect, useState } from 'react';
import { api } from '../../api/client';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';
import type { JobDeclarationConfig, JobDeclarationStatus } from '../../api/types';

const DEFAULT_CONFIG: JobDeclarationConfig = {
  enabled: false,
  mode: 'coinbase_only',
  bitcoind_rpc_url: 'http://127.0.0.1:8332',
  bitcoind_rpc_user: '',
  bitcoind_rpc_password: '',
  bitcoind_rpc_cookie: '',
  template_provider_url: 'sv2+tcp://127.0.0.1:8442',
  job_declarator_url: '',
  coinbase_output_address: '',
  template_refresh_interval_s: 30,
  fallback_to_pool_templates: true,
  declare_tx_data: false,
  coinbase_output_max_additional_size: 512,
  coinbase_output_max_additional_sigops: 0,
};

function formatSats(value?: number): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 'Unavailable';
  return `${value.toLocaleString()} sats`;
}

export function JobDeclarationPanel() {
  const [config, setConfig] = useState<JobDeclarationConfig>({ ...DEFAULT_CONFIG });
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<JobDeclarationStatus | null>(null);
  const [statusMsg, setStatusMsg] = useState('');
  const [errorMsg, setErrorMsg] = useState('');

  useEffect(() => {
    api.getJdStatus()
      .then(res => {
        setStatus(res);
        setConfig(prev => ({
          ...prev,
          ...(res.config ?? {}),
          enabled: res.enabled ?? res.config?.enabled ?? prev.enabled,
          mode: res.mode ?? res.config?.mode ?? prev.mode,
          bitcoind_rpc_url: res.config?.bitcoind_rpc_url ?? res.bitcoind_url ?? prev.bitcoind_rpc_url,
          template_provider_url: res.config?.template_provider_url ?? res.template_provider_url ?? prev.template_provider_url,
          job_declarator_url: res.config?.job_declarator_url ?? res.job_declarator_url ?? prev.job_declarator_url,
          bitcoind_rpc_password: '',
        }));
      })
      .catch(e => setErrorMsg(e instanceof Error ? e.message : 'Failed to load Job Declaration status'))
      .finally(() => setLoading(false));
  }, []);

  const update = <K extends keyof JobDeclarationConfig>(key: K, value: JobDeclarationConfig[K]) => {
    setConfig(prev => ({ ...prev, [key]: value }));
  };

  const testConnection = async () => {
    setTesting(true);
    setStatusMsg('');
    setErrorMsg('');
    echoCli('jobd test');
    try {
      const result = await api.testJdConnection(config);
      const failed = Array.isArray(result.checks)
        ? result.checks.filter((check: { ok?: boolean }) => !check.ok).length
        : 0;
      if (failed > 0) {
        setErrorMsg(result.message ?? `${failed} check${failed === 1 ? '' : 's'} failed`);
      } else {
        setStatusMsg(result.message ?? 'Bitcoin Core template RPC and configured endpoints are ready.');
      }
    } catch (e: unknown) {
      setErrorMsg(e instanceof Error ? e.message : 'Connection test failed');
    }
    setTesting(false);
  };

  const refreshStatus = async () => {
    const next = await api.getJdStatus();
    setStatus(next);
  };

  const saveConfig = async () => {
    setSaving(true);
    setStatusMsg('');
    setErrorMsg('');
    echoCli(`jobd config save --mode ${config.mode ?? 'coinbase_only'} --${config.enabled ? 'enable' : 'disable'}`);
    try {
      await api.postJdConfig(config);
      await refreshStatus();
      setStatusMsg('Configuration saved. Restart dcentrald to apply runtime endpoints.');
    } catch (e: unknown) {
      setErrorMsg(e instanceof Error ? e.message : 'Failed to save configuration');
    }
    setSaving(false);
  };

  const liveRuntime = status?.live_jdc_runtime === true || status?.connected === true;
  const configured = status?.configured ?? (config.enabled === true);
  const headerTone = liveRuntime
    ? 'var(--green)'
    : configured
      ? 'var(--yellow, #f59e0b)'
      : 'var(--text-dim)';
  const headerLabel = liveRuntime ? 'live' : configured ? 'configured' : 'available';

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// job declaration</div>
          <h2 className="hacker-inspector-title">Self-Built Block Templates</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${liveRuntime ? '' : configured ? 'warning' : 'neutral'}`}>
            {loading ? 'LOADING' : headerLabel.toUpperCase()}
          </span>
        </div>
      </header>

      <div className="hacker-inspector-toolbar">
        <span className="adv-hint" style={{ fontSize: '0.74rem' }}>
          Build your own block templates. Choose which transactions to include. Requires a local Bitcoin Core full node.
        </span>
      </div>

      <div className="hacker-inspector-body">
      <div className="adv-grid-2">
        {/* Configuration form */}
        <div className="register-inspector">
          <div className="adv-card-title jd-section-title">
            Configuration
          </div>

          {/* Enable toggle */}
          <div className="jd-enable-row">
            <div>
              <div className="jd-enable-name">Enable Job Declaration</div>
              <div className="adv-hint is-xs">
                Construct block templates from your own mempool
              </div>
            </div>
            <button
              onClick={() => update('enabled', !config.enabled)}
              className={`jd-toggle ${config.enabled ? 'is-on' : ''}`}
              role="switch"
              aria-checked={config.enabled}
              aria-label="Enable Job Declaration"
            >
              <div className="jd-toggle-knob" />
            </button>
          </div>

          <div className="adv-mt-12">
            <label className="jd-label">Mode</label>
            <select
              value={config.mode ?? 'coinbase_only'}
              onChange={e => update('mode', e.target.value)}
              className="jd-input jd-input-auto"
            >
              <option value="coinbase_only">Coinbase-only</option>
              <option value="full_template">Full template</option>
            </select>
          </div>

          <div className="adv-mt-12">
            <label className="jd-label">Template Provider URL</label>
            <input
              type="text"
              value={config.template_provider_url ?? ''}
              onChange={e => update('template_provider_url', e.target.value)}
              placeholder="sv2+tcp://127.0.0.1:8442"
              className="jd-input"
            />
          </div>

          <div className="adv-mt-12">
            <label className="jd-label">Job Declarator URL</label>
            <input
              type="text"
              value={config.job_declarator_url ?? ''}
              onChange={e => update('job_declarator_url', e.target.value)}
              placeholder="sv2+tcp://pool-jds.example:34255"
              className="jd-input"
            />
          </div>

          {/* RPC URL */}
          <div className="adv-mt-12">
            <label className="jd-label">Bitcoin Core RPC URL</label>
            <input
              type="text"
              value={config.bitcoind_rpc_url ?? ''}
              onChange={e => update('bitcoind_rpc_url', e.target.value)}
              placeholder="http://127.0.0.1:8332"
              className="jd-input"
            />
          </div>

          <div className="adv-mt-12">
            <label className="jd-label">RPC Cookie File</label>
            <input
              type="text"
              value={config.bitcoind_rpc_cookie ?? ''}
              onChange={e => update('bitcoind_rpc_cookie', e.target.value)}
              placeholder="/data/bitcoin/.cookie"
              className="jd-input"
            />
          </div>

          {/* RPC Username */}
          <div className="adv-mt-12">
            <label className="jd-label">RPC Username</label>
            <input
              type="text"
              value={config.bitcoind_rpc_user ?? ''}
              onChange={e => update('bitcoind_rpc_user', e.target.value)}
              placeholder="rpcuser"
              className="jd-input"
            />
          </div>

          {/* RPC Password */}
          <div className="adv-mt-12">
            <label className="jd-label">RPC Password</label>
            <input
              type="password"
              value={config.bitcoind_rpc_password ?? ''}
              onChange={e => update('bitcoind_rpc_password', e.target.value)}
              placeholder="rpcpassword"
              className="jd-input"
            />
          </div>

          {/* Coinbase Output Address */}
          <div className="adv-mt-12">
            <label className="jd-label">Coinbase Output Address</label>
            <input
              type="text"
              value={config.coinbase_output_address ?? ''}
              onChange={e => update('coinbase_output_address', e.target.value)}
              placeholder="bc1q..."
              className="jd-input"
            />
          </div>

          {/* Template Refresh Interval */}
          <div className="adv-mt-12">
            <label className="jd-label">Template Refresh Interval (seconds)</label>
            <input
              type="number"
              min={5}
              max={300}
              value={config.template_refresh_interval_s ?? 30}
              onChange={e => update('template_refresh_interval_s', Number(e.target.value))}
              className="jd-input jd-input-120"
            />
          </div>

          <div className="jd-grid-2">
            <div>
              <label className="jd-label">Coinbase Reserve Bytes</label>
              <input
                type="number"
                min={0}
                max={1000000}
                value={config.coinbase_output_max_additional_size ?? 512}
                onChange={e => update('coinbase_output_max_additional_size', Number(e.target.value))}
                className="jd-input"
              />
            </div>
            <div>
              <label className="jd-label">Coinbase Reserve Sigops</label>
              <input
                type="number"
                min={0}
                max={65535}
                value={config.coinbase_output_max_additional_sigops ?? 0}
                onChange={e => update('coinbase_output_max_additional_sigops', Number(e.target.value))}
                className="jd-input"
              />
            </div>
          </div>

          <div className="jd-check-group">
            <label className="jd-label jd-check-label">
              <input
                type="checkbox"
                checked={config.fallback_to_pool_templates !== false}
                onChange={e => update('fallback_to_pool_templates', e.target.checked)}
                className="jd-check"
              />
              Fall back to pool templates
            </label>
            <label className="jd-label jd-check-label">
              <input
                type="checkbox"
                checked={config.mode === 'full_template' || config.declare_tx_data === true}
                onChange={e => {
                  update('declare_tx_data', e.target.checked);
                  update('mode', e.target.checked ? 'full_template' : 'coinbase_only');
                }}
                className="jd-check"
              />
              Declare transaction data
            </label>
          </div>

          {/* Buttons */}
          <div className="jd-btn-row">
            <button
              className="btn btn-secondary jd-btn"
              onClick={testConnection}
              disabled={testing}
            >
              {testing ? 'Testing...' : 'Test Connection'}
            </button>
            <button
              className="btn btn-primary jd-btn"
              onClick={saveConfig}
              disabled={saving}
            >
              {saving ? 'Saving...' : 'Save'}
            </button>
          </div>
          <CliHint cmd={`jobd config save --mode ${config.mode ?? 'coinbase_only'} --${config.enabled ? 'enable' : 'disable'}`} />
          <CliHint cmd="jobd test" />

          {statusMsg && <div className="adv-msg is-success is-sm is-mt">{statusMsg}</div>}
          {errorMsg && <div className="adv-msg is-error is-sm is-mt">{errorMsg}</div>}
        </div>

        {/* Status section */}
        <div className="register-inspector">
          <div className="adv-card-title jd-section-title">
            Status
          </div>

          <div className="jd-status-grid">
            <div className="jd-stat-row">
              <span className="jd-stat-k">Runtime</span>
              <span style={{ color: liveRuntime ? 'var(--green)' : configured ? 'var(--yellow)' : 'var(--text-dim)' }}>
                {status?.runtime_state ?? (loading ? 'Loading' : 'Disabled')}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Mode</span>
              <span className="jd-stat-v">{status?.mode ?? config.mode ?? 'coinbase_only'}</span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Template Provider</span>
              <span className="jd-stat-ellipsis" style={{ color: status?.template_provider_url ? 'var(--text)' : 'var(--text-dim)' }}>
                {status?.template_provider_url ?? config.template_provider_url ?? 'unset'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">TP Setup</span>
              <span style={{ color: status?.template_provider_connected ? 'var(--green)' : 'var(--text-dim)' }}>
                {status?.template_provider_connected ? 'OK' : 'Pending'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Job Declarator</span>
              <span className="jd-stat-ellipsis" style={{ color: status?.job_declarator_url ? 'var(--text)' : 'var(--text-dim)' }}>
                {status?.job_declarator_url ?? config.job_declarator_url ?? 'unset'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">JDS Setup</span>
              <span style={{ color: status?.job_declarator_connected ? 'var(--green)' : 'var(--text-dim)' }}>
                {status?.job_declarator_connected ? 'OK' : 'Pending'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Mining Job Token</span>
              <span style={{ color: status?.mining_job_token_available ? 'var(--green)' : 'var(--text-dim)' }}>
                {status?.mining_job_token_available ? 'OK' : 'Pending'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Template PrevHash</span>
              <span style={{ color: status?.template_prev_hash_ready ? 'var(--green)' : 'var(--text-dim)' }}>
                {status?.template_prev_hash_ready ? 'OK' : 'Pending'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Custom Job Candidate</span>
              <span style={{ color: status?.custom_job_candidate_ready ? 'var(--green)' : 'var(--yellow, #f59e0b)' }}>
                {status?.custom_job_candidate_ready ? 'Ready' : 'Gated'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Mining Bridge</span>
              <span style={{ color: status?.custom_job_injection_active ? 'var(--green)' : status?.custom_job_injection_ready ? 'var(--yellow, #f59e0b)' : 'var(--text-dim)' }}>
                {status?.custom_job_injection_active ? 'Active' : status?.custom_job_injection_ready ? 'Ready' : 'Pending'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Bridge State</span>
              <span style={{ color: status?.custom_job_bridge?.status === 'accepted' ? 'var(--green)' : status?.custom_job_bridge?.status === 'rejected' ? 'var(--red)' : 'var(--text-dim)' }}>
                {status?.custom_job_bridge?.status ?? 'Unavailable'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Declared Job</span>
              <span style={{ color: status?.last_declared_job_id !== undefined ? 'var(--text)' : 'var(--text-dim)' }}>
                {status?.last_declared_job_id ?? 'Unavailable'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Templates Constructed</span>
              <span className="jd-stat-v">{status?.templates_constructed ?? 0}</span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Current Template</span>
              <span style={{ color: status?.current_template_id !== undefined ? 'var(--text)' : 'var(--text-dim)' }}>
                {status?.current_template_id ?? 'Unavailable'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Template Tx</span>
              <span style={{ color: status?.current_tx_count !== undefined ? 'var(--text)' : 'var(--text-dim)' }}>
                {status?.current_tx_count ?? 'Unavailable'}
              </span>
            </div>
            <div className="jd-stat-row">
              <span className="jd-stat-k">Coinbase Remaining</span>
              <span style={{ color: status?.coinbase_value_remaining_sats !== undefined ? 'var(--text)' : 'var(--text-dim)' }}>
                {formatSats(status?.coinbase_value_remaining_sats)}
              </span>
            </div>
            {status?.last_error && (
              <div className="jd-stat-row jd-stat-row-err">
                <span className="jd-stat-k">Last Error</span>
                <span className="jd-stat-ellipsis jd-stat-err">{status.last_error}</span>
              </div>
            )}
            <div className="jd-stat-row">
              <span className="jd-stat-k">Enabled</span>
              <span style={{ color: config.enabled ? 'var(--green)' : 'var(--text-dim)' }}>
                {config.enabled ? 'Yes' : 'No'}
              </span>
            </div>
          </div>

          <div className={`jd-reason ${liveRuntime ? 'is-live' : ''}`}>
            {status?.reason ?? 'Job Declaration status has not been loaded yet.'}
          </div>
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{liveRuntime ? 'live runtime' : configured ? 'configured' : 'available'}</span>
          {status?.reason && <span>{status.reason}</span>}
        </div>
      </footer>
    </div>
  );
}
