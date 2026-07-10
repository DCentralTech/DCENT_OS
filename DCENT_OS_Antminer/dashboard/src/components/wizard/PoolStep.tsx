// DCENT_OS Setup Wizard — Pool step.
//
// Structural recreation of the kit `PoolStep` (ui_kits/wizard/Wizard.jsx):
// the pool-template chip grid (name + fee/Stratum meta), the Stratum URL
// field, the Worker field, and the Test-connection row with an inline
// subscribe/authorize result.
//
// Real wiring preserved: PoolConfig type export, the real
// api.testSetupPoolConnection call, the worker = "<wallet>.<workername>"
// split convention, Stratum URL validation, solo-variance awareness, and
// heater-mode recommended-pool auto-select. The kit chips are fed by the
// production POOL_TEMPLATES catalogue (all real pools, not the kit's 5).

import React, { useState, useEffect, useMemo, useRef } from 'react';
import { POOL_TEMPLATES } from '../../utils/constants';
import type { PoolTemplate } from '../../utils/constants';
import type { OperatingMode } from '../../api/types';
import { api } from '../../api/client';
import { InfoDot } from '../common/Tooltip';

export interface PoolConfig {
  url: string;
  worker: string;
  password: string;
}

interface PoolStepProps {
  value: PoolConfig;
  mode: OperatingMode | null;
  minerName: string;
  onChange: (config: PoolConfig) => void;
  estimatedHashrateThs?: number;
}

type Tab = 'pooled' | 'solo';

const SOLO_URL_FRAGMENTS = [
  'solo.ckpool.org',
  'public-pool.io',
  'ocean.xyz/solo',
  'ckpool.org',
  'solomine',
  'soloprivate',
] as const;
const POOLED_TEMPLATES = POOL_TEMPLATES.filter(p => p.category === 'pooled');
const SOLO_TEMPLATES = POOL_TEMPLATES.filter(p => p.category === 'solo');

function looksLikeSoloUrl(url: string): boolean {
  const u = url.toLowerCase();
  return SOLO_URL_FRAGMENTS.some(frag => u.includes(frag));
}

const NETWORK_TH_S = 750_000_000;
const BLOCKS_PER_YEAR = 365 * 144;

export function computeSoloVariance(thSPerSecond: number): {
  yearsBetweenBlocks: number;
  expectedBlocksPerYear: number;
  summary: string;
} {
  const ths = Math.max(0.001, thSPerSecond);
  const expectedBlocksPerYear = (ths / NETWORK_TH_S) * BLOCKS_PER_YEAR;
  const yearsBetweenBlocks = expectedBlocksPerYear > 0 ? 1 / expectedBlocksPerYear : Infinity;

  let summary: string;
  if (yearsBetweenBlocks > 5000) {
    summary = `over ${Math.round(yearsBetweenBlocks / 1000) * 1000} years`;
  } else if (yearsBetweenBlocks > 100) {
    summary = `~${Math.round(yearsBetweenBlocks / 10) * 10} years`;
  } else if (yearsBetweenBlocks > 5) {
    summary = `~${Math.round(yearsBetweenBlocks)} years`;
  } else {
    summary = `~${yearsBetweenBlocks.toFixed(1)} years`;
  }
  return { yearsBetweenBlocks, expectedBlocksPerYear, summary };
}

function validateStratumUrl(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;
  if (!/^stratum\+(tcp|ssl):\/\//i.test(trimmed)) {
    return 'Expected URL to start with stratum+tcp:// or stratum+ssl://';
  }
  const after = trimmed.replace(/^stratum\+(tcp|ssl):\/\//i, '');
  if (!/^[a-z0-9.\-]+:\d{1,5}(\/.*)?$/i.test(after)) {
    return 'Expected host:port (e.g. pool.example.com:3333)';
  }
  return null;
}

function deriveWorkerSuffix(minerName: string, fallback: string): string {
  return minerName
    ? minerName.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
    : fallback;
}

export function PoolStep({ value, mode, minerName, onChange, estimatedHashrateThs }: PoolStepProps) {
  const [activeTab, setActiveTab] = useState<Tab>('pooled');
  const [selected, setSelected] = useState<string | null>(null);
  const [testing, setTesting] = useState<'idle' | 'running' | 'ok' | 'fail'>('idle');
  const [testMsg, setTestMsg] = useState<string>('');
  const [urlBlurred, setUrlBlurred] = useState(false);
  const [soloAck, setSoloAck] = useState(false);
  const heaterDefaultAppliedRef = useRef(false);

  const isHeaterMode = mode === 'heater';
  const isSoloTab = activeTab === 'solo';
  const urlValidationError = urlBlurred ? validateStratumUrl(value.url) : null;
  const urlLooksSolo = looksLikeSoloUrl(value.url);
  const showSoloVariance = (isSoloTab || urlLooksSolo) && Boolean(value.url);
  const hashrateThs = Math.max(0.1, estimatedHashrateThs ?? 14);
  const variance = useMemo(() => computeSoloVariance(hashrateThs), [hashrateThs]);

  const templates = isSoloTab ? SOLO_TEMPLATES : POOLED_TEMPLATES;

  useEffect(() => {
    if (!showSoloVariance && soloAck) setSoloAck(false);
  }, [showSoloVariance, soloAck]);

  // Heater mode: auto-select the recommended pool once.
  useEffect(() => {
    if (!isHeaterMode || value.url || heaterDefaultAppliedRef.current) return;
    const recommended = POOLED_TEMPLATES.find(p => p.highlighted) || POOLED_TEMPLATES[0];
    if (!recommended) return;
    heaterDefaultAppliedRef.current = true;
    setSelected(recommended.name);
    const suffix = deriveWorkerSuffix(minerName, 'heater');
    const wallet = value.worker.split('.')[0] || '';
    onChange({ ...value, url: recommended.url, worker: wallet ? `${wallet}.${suffix}` : suffix });
  }, [isHeaterMode, minerName, onChange, value]);

  function selectTemplate(t: PoolTemplate) {
    setSelected(t.name);
    setTesting('idle');
    setTestMsg('');
    const suffix = deriveWorkerSuffix(minerName, 'rig1');
    const wallet = value.worker.split('.')[0] || '';
    onChange({ ...value, url: t.url, worker: wallet ? `${wallet}.${suffix}` : suffix });
  }

  async function testConnection() {
    if (!value.url || !value.worker) return;
    setTesting('running');
    setTestMsg('');
    try {
      await api.testSetupPoolConnection({
        url: value.url,
        worker: value.worker,
        password: value.password || 'x',
      });
      setTesting('ok');
      setTestMsg('subscribe ok · authorize ok');
    } catch (err) {
      setTesting('fail');
      setTestMsg(err instanceof Error ? err.message : 'connection refused · check URL and credentials');
    }
  }

  const wallet = value.worker.split('.')[0] || '';
  const workerName = value.worker.includes('.') ? value.worker.split('.').slice(1).join('.') : '';

  return (
    <div className="wiz-step-body wizard-step-pane">
      <h2 className="wiz-h2">Pool</h2>
      <p className="wiz-lede">
        Pick a pool, paste your payout address, test the connection. Pool is optional —
        you can add it (or failover pools) later. You keep the full block reward when
        you solo-mine and find a block.
      </p>

      <div className="wiz-tabs" role="tablist" aria-label="Mining type">
        {(['pooled', 'solo'] as Tab[]).map(tab => (
          <button
            key={tab}
            type="button"
            role="tab"
            aria-selected={activeTab === tab}
            className={`wiz-tab${activeTab === tab ? ' active' : ''}`}
            onClick={() => { setActiveTab(tab); setSelected(null); }}
          >
            {tab === 'pooled' ? 'Pooled mining' : 'Solo mining'}
          </button>
        ))}
      </div>

      <div className="wiz-pool-templates" role="radiogroup" aria-label="Pool selection">
        {templates.map(t => {
          const isActive = selected === t.name;
          return (
            <button
              key={t.name}
              type="button"
              role="radio"
              aria-checked={isActive}
              className={`wiz-pool-card${isActive ? ' active' : ''}${t.highlighted ? ' rec' : ''}`}
              onClick={() => selectTemplate(t)}
              title={t.description}
            >
              <div className="wiz-pool-name">{t.name}</div>
              <div className="wiz-pool-meta">
                <span>Stratum {t.sv2_supported ? 'V2' : 'V1'}</span>
                {t.highlighted && (<><span>·</span><span>recommended</span></>)}
              </div>
              <div className="wiz-pool-desc">{t.description}</div>
            </button>
          );
        })}
      </div>

      {showSoloVariance && (
        <div className="wiz-info amber" role="alert" aria-live="polite">
          <strong>Solo mining.</strong> At your hashrate ({hashrateThs.toFixed(1)} TH/s),
          expected time to find a block is <strong style={{ color: 'inherit' }}>{variance.summary}</strong>.
          Variance is astronomical — most solo miners never find a block. You still
          earn nothing until you find one (then you keep the full reward).
          {soloAck ? (
            <> Solo route confirmed — odds {(variance.expectedBlocksPerYear * 100).toFixed(4)}% per year.</>
          ) : (
            <>{' '}
              <button
                type="button"
                className="wiz-btn"
                style={{ marginTop: 8 }}
                onClick={() => setSoloAck(true)}
              >
                I understand, continue solo
              </button>
            </>
          )}
        </div>
      )}

      <div className="wiz-fld">
        <label htmlFor="wiz-pool-wallet">Your Bitcoin address</label>
        <input
          id="wiz-pool-wallet"
          className="wiz-input"
          type="text"
          value={wallet}
          onChange={e => {
            const suffixPart = value.worker.includes('.')
              ? `.${value.worker.split('.').slice(1).join('.')}`
              : minerName
                ? `.${deriveWorkerSuffix(minerName, 'rig1')}`
                : '';
            onChange({ ...value, worker: e.target.value + suffixPart });
            setTesting('idle');
          }}
          placeholder="Paste your Bitcoin address (bc1… or 1…)"
        />
        <small className="wiz-fld-hint">
          {isSoloTab
            ? 'Receives the full block reward (~3.125 BTC) — only if your miner discovers a block.'
            : 'Where the pool sends your mining rewards. Most pools pay out daily or at a minimum balance.'}
        </small>
      </div>

      <div className="wiz-fld">
        <label htmlFor="wiz-pool-url">
          Stratum URL <InfoDot term="wizard_pool_url" size={12} />
        </label>
        <input
          id="wiz-pool-url"
          className={`wiz-input${urlValidationError ? ' err' : ''}`}
          type="text"
          value={value.url}
          onChange={e => { onChange({ ...value, url: e.target.value }); setTesting('idle'); setUrlBlurred(false); }}
          onBlur={() => setUrlBlurred(true)}
          placeholder="stratum+tcp://pool.example.org:3333"
          aria-invalid={urlValidationError !== null}
          aria-describedby={urlValidationError ? 'wiz-pool-url-error' : undefined}
        />
        {urlValidationError ? (
          <span id="wiz-pool-url-error" className="wiz-err">{urlValidationError}</span>
        ) : (
          <small className="wiz-fld-hint">
            Auto-filled when you pick a pool above. You can also paste any Stratum endpoint.
          </small>
        )}
      </div>

      <div className="wiz-fld">
        <label htmlFor="wiz-pool-worker">
          Worker name <InfoDot term="wizard_worker_label" size={12} />
        </label>
        <input
          id="wiz-pool-worker"
          className="wiz-input"
          type="text"
          value={workerName}
          onChange={e => {
            const worker = e.target.value ? `${wallet}.${e.target.value}` : wallet;
            onChange({ ...value, worker });
            setTesting('idle');
          }}
          placeholder="rig1"
        />
        <small className="wiz-fld-hint">
          Identifies this device in your pool dashboard. Full worker string:{' '}
          <code>{value.worker || 'address.workername'}</code>
        </small>
      </div>

      <div className="wiz-test-row">
        <button
          type="button"
          className="wiz-btn"
          onClick={testConnection}
          disabled={testing === 'running' || !value.url || !value.worker}
        >
          {testing === 'running' ? 'Testing…' : 'Test connection'}
        </button>
        {testing === 'ok' && (
          <span className="wiz-test-ok" aria-live="polite">
            ✓ {testMsg} <InfoDot term="pool_authorized" size={12} />
          </span>
        )}
        {testing === 'fail' && <span className="wiz-test-fail" aria-live="polite">✗ {testMsg}</span>}
      </div>
    </div>
  );
}
