// CompanionLlmSettings — configure the companion's LLM (Settings → Companion,
// also reachable from the companion popover's Setup tab).
//
// Privacy-first: OFF by default; local providers (Ollama / LM Studio) keep
// everything on your network; cloud providers are supported but flagged
// "not recommended"; the API key is stored only in this browser's localStorage.

import React, { useState } from 'react';
import {
  loadLlmConfig, saveLlmConfig, PROVIDER_PRESETS, isCloud,
  type LlmConfig, type LlmProvider,
} from '../../companion/llm';

export function CompanionLlmSettings({ onSaved }: { onSaved?: () => void }) {
  const [cfg, setCfg] = useState<LlmConfig>(() => loadLlmConfig());
  const [saved, setSaved] = useState(false);
  const preset = PROVIDER_PRESETS[cfg.provider];

  const patch = (p: Partial<LlmConfig>) => { setCfg(c => ({ ...c, ...p })); setSaved(false); };
  const changeProvider = (provider: LlmProvider) => {
    const pr = PROVIDER_PRESETS[provider];
    patch({ provider, baseUrl: pr.baseUrl, model: pr.model });
  };
  const save = () => { saveLlmConfig(cfg); setSaved(true); onSaved?.(); };

  return (
    <div className="companion-setup">
      <label className="companion-setup-toggle">
        <input type="checkbox" checked={cfg.enabled} onChange={e => patch({ enabled: e.target.checked })} />
        <span>Enable companion chat</span>
      </label>

      <p className="companion-setup-privacy">
        Off by default. Local LLMs (<strong>Ollama</strong>, <strong>LM Studio</strong>) keep every
        prompt on your own network. Cloud providers send prompts about your miner off-box —
        <strong> not recommended</strong>. Your API key is stored only in this browser.
      </p>

      <label className="companion-setup-field">
        <span>Provider</span>
        <select value={cfg.provider} onChange={e => changeProvider(e.target.value as LlmProvider)}>
          {(Object.keys(PROVIDER_PRESETS) as LlmProvider[]).map(k => (
            <option key={k} value={k}>{PROVIDER_PRESETS[k].label}</option>
          ))}
        </select>
      </label>

      <label className="companion-setup-field">
        <span>Endpoint</span>
        <input type="text" value={cfg.baseUrl} placeholder={preset.baseUrl}
          onChange={e => patch({ baseUrl: e.target.value })} spellCheck={false} />
      </label>

      {preset.local && (
        <p className="companion-setup-hint">
          Local servers must allow browser requests. In <strong>LM Studio</strong>: Developer → Server,
          enable <strong>CORS</strong> (and <strong>Serve on Local Network</strong> if the dashboard isn't on
          this machine), then Start Server.
        </p>
      )}

      <label className="companion-setup-field">
        <span>Model</span>
        <input type="text" value={cfg.model} placeholder={preset.model}
          onChange={e => patch({ model: e.target.value })} spellCheck={false} />
      </label>

      {preset.needsKey && (
        <label className="companion-setup-field">
          <span>API key</span>
          <input type="password" value={cfg.apiKey} placeholder="stays in your browser"
            onChange={e => patch({ apiKey: e.target.value })} autoComplete="off" spellCheck={false} />
        </label>
      )}

      {isCloud(cfg.provider) && (
        <p className="companion-setup-warn">⚠ Cloud provider — prompts about your miner leave your network.</p>
      )}

      <button type="button" className="companion-setup-save" onClick={save}>
        {saved ? 'Saved ✓' : 'Save'}
      </button>
    </div>
  );
}

export default CompanionLlmSettings;
