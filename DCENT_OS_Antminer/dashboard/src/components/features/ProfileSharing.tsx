// Community Tuning Profiles — Share and import tuning configurations
// Feature no competitor has: community-driven profile sharing built into firmware

import React, { useState, useCallback, useRef } from 'react';
import type { CommunityProfile } from '../../api/feature-types';
import { useTranslation } from '../../i18n/i18n';
import { useMinerStore } from '../../store/miner';
import { getLivePowerEfficiencyJth } from '../../utils/power';
import { InfoDot } from '../common/Tooltip';
import { ActionButton } from '../common/ActionButton';
import { api } from '../../api/client';

const PROFILE_VERSION = '1.0';

function validateProfile(data: unknown): data is CommunityProfile {
  if (!data || typeof data !== 'object') return false;
  const p = data as Record<string, unknown>;
  return (
    typeof p.name === 'string' &&
    typeof p.author === 'string' &&
    typeof p.model === 'string' &&
    typeof p.target === 'string' &&
    ['efficiency', 'performance', 'quiet', 'balanced'].includes(p.target as string) &&
    typeof p.frequencyMhz === 'number' &&
    typeof p.voltageMv === 'number' &&
    typeof p.fanMode === 'string'
  );
}

export function ProfileSharing() {
  const { t } = useTranslation();
  const addAlert = useMinerStore(s => s.addAlert);
  const systemInfo = useMinerStore(s => s.systemInfo);
  const stats = useMinerStore(s => s.stats);

  const [profiles, setProfiles] = useState<CommunityProfile[]>([]);
  const [showExport, setShowExport] = useState(false);
  const [showImport, setShowImport] = useState(false);
  const [importJson, setImportJson] = useState('');
  const [importError, setImportError] = useState<string | null>(null);

  // Export form
  const [exportName, setExportName] = useState('');
  const [exportAuthor, setExportAuthor] = useState('');
  const [exportTarget, setExportTarget] = useState<CommunityProfile['target']>('balanced');
  const [exportDesc, setExportDesc] = useState('');

  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const buildExportProfile = (): CommunityProfile => {
    const chain0 = stats?.chains?.[0];
    const liveEfficiencyJth = getLivePowerEfficiencyJth(stats?.power);
    return {
      name: exportName || 'My Profile',
      author: exportAuthor || 'Anonymous',
      model: systemInfo?.model ?? 'Unknown',
      target: exportTarget,
      frequencyMhz: chain0?.frequency_mhz ?? 650,
      voltageMv: chain0?.voltage_mv ?? 9100,
      fanMode: 'balanced',
      description: exportDesc || '',
      version: PROFILE_VERSION,
      createdAt: new Date().toISOString(),
      hashrateThs: stats?.hashrate_ths ?? null,
      efficiencyJth: liveEfficiencyJth > 0 ? liveEfficiencyJth : null,
    };
  };

  const handleExport = useCallback(() => {
    const profile = buildExportProfile();
    const json = JSON.stringify(profile, null, 2);

    // Copy to clipboard
    navigator.clipboard.writeText(json).then(() => {
      addAlert('info', 'Profile JSON copied to clipboard.');
    }).catch(() => {
      // Fallback: select textarea
      if (textareaRef.current) {
        textareaRef.current.value = json;
        textareaRef.current.select();
      }
    });

    // Also trigger download
    const blob = new Blob([json], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `dcentos-profile-${exportName || 'export'}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  }, [exportName, exportAuthor, exportTarget, exportDesc, stats, systemInfo, addAlert]);

  const handleImport = useCallback(() => {
    setImportError(null);
    try {
      const parsed = JSON.parse(importJson);
      if (!validateProfile(parsed)) {
        setImportError(t('profiles.invalidJson'));
        return;
      }
      setProfiles(prev => [...prev, parsed]);
      setImportJson('');
      setShowImport(false);
      addAlert('info', t('profiles.imported'));
    } catch {
      setImportError(t('profiles.invalidJson'));
    }
  }, [importJson, t, addAlert]);

  // Save a shared/imported profile to the on-device profile LIBRARY
  // (POST /api/profiles). This does NOT change the running miner: the daemon
  // boots its config from dcentrald.toml, not the profile library — a saved
  // profile becomes available to activate from Tuning. `post()` throws on a
  // non-ok response, so a resolved promise means the library write succeeded.
  // (Earlier copy claimed this "changes the LIVE hash boards" / takes effect
  // on restart — both false; saveProfile only persists a JSON the daemon does
  // not read at boot.)
  const handleSaveProfile = async (profile: CommunityProfile) => {
    try {
      await api.saveProfile({
        name: profile.name || 'imported',
        frequency_mhz: profile.frequencyMhz,
        voltage_mv: profile.voltageMv,
        fan_mode: profile.fanMode || 'balanced',
      });
      addAlert(
        'info',
        `Saved "${profile.name}" (${profile.frequencyMhz} MHz, `
          + `${(profile.voltageMv / 1000).toFixed(2)}V) to your profile library. `
          + `This does not change the running miner — activate it from Tuning to use it.`,
      );
    } catch {
      addAlert('warning', `Failed to save profile "${profile.name}" — the daemon rejected the write or is unreachable.`);
    }
  };

  const handleRemoveProfile = (idx: number) => {
    setProfiles(prev => prev.filter((_, i) => i !== idx));
  };

  const targetColor = (target: CommunityProfile['target']): string => {
    switch (target) {
      case 'efficiency': return 'var(--feat-green)';
      case 'performance': return 'var(--feat-red)';
      case 'quiet': return 'var(--feat-blue)';
      case 'balanced': return 'var(--feat-orange)';
    }
  };

  return (
    <div className="feat-page">
      <div className="feat-header">
        <h2 className="feat-title">
          {t('profiles.title')}
          <InfoDot
            placement="bottom"
            label="What community profiles are"
            content={
              <>
                A tuning profile is just a portable JSON of frequency / voltage /
                fan settings plus reported hashrate and live wall-power-backed
                J/TH efficiency when available.
                Share a known-good config or import one — no license server, no
                cloud account. Always re-validate an imported profile on your own
                hardware before trusting it; silicon varies chip to chip.
              </>
            }
          />
        </h2>
        <p className="feat-subtitle">{t('profiles.subtitle')}</p>
      </div>

      {/* Action buttons */}
      <div className="feat-card" style={{ display: 'flex', gap: 12 }}>
        <button
          className="feat-btn feat-btn-primary"
          onClick={() => { setShowExport(!showExport); setShowImport(false); }}
        >
          {t('profiles.share')}
        </button>
        <button
          className="feat-btn feat-btn-secondary"
          onClick={() => { setShowImport(!showImport); setShowExport(false); }}
        >
          {t('profiles.import')}
        </button>
      </div>

      {/* Export panel */}
      {showExport && (
        <div className="feat-card">
          <h3 className="feat-card-title">{t('profiles.exportJson')}</h3>
          <div className="feat-form-grid">
            <div className="feat-input-group">
              <label className="feat-label">{t('profiles.name')}</label>
              <input
                type="text"
                value={exportName}
                onChange={e => setExportName(e.target.value)}
                placeholder="e.g. Quiet S9 Efficiency"
                className="feat-input"
              />
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{t('profiles.author')}</label>
              <input
                type="text"
                value={exportAuthor}
                onChange={e => setExportAuthor(e.target.value)}
                placeholder="Your name or handle"
                className="feat-input"
              />
            </div>
            <div className="feat-input-group">
              <label className="feat-label">{t('profiles.target')}</label>
              <select
                value={exportTarget}
                onChange={e => setExportTarget(e.target.value as CommunityProfile['target'])}
                className="feat-input"
              >
                <option value="efficiency">{t('profiles.efficiency')}</option>
                <option value="performance">{t('profiles.performance')}</option>
                <option value="quiet">{t('profiles.quiet')}</option>
                <option value="balanced">{t('profiles.balanced')}</option>
              </select>
            </div>
            <div className="feat-input-group feat-span-2">
              <label className="feat-label">Description</label>
              <input
                type="text"
                value={exportDesc}
                onChange={e => setExportDesc(e.target.value)}
                placeholder="Brief description of this profile"
                className="feat-input"
              />
            </div>
          </div>

          <div className="feat-profile-preview">
            <div className="feat-profile-preview-label">Preview:</div>
            <pre className="feat-profile-json mono">
              {JSON.stringify(buildExportProfile(), null, 2)}
            </pre>
          </div>

          <textarea ref={textareaRef} style={{ position: 'absolute', left: -9999 }} readOnly />

          <div className="feat-actions" style={{ marginTop: 12 }}>
            <button className="feat-btn feat-btn-primary" onClick={handleExport}>
              {t('profiles.exportJson')}
            </button>
          </div>
        </div>
      )}

      {/* Import panel */}
      {showImport && (
        <div className="feat-card">
          <h3 className="feat-card-title">{t('profiles.importJson')}</h3>
          <p className="feat-hint">{t('profiles.pasteJson')}</p>
          <textarea
            value={importJson}
            onChange={e => { setImportJson(e.target.value); setImportError(null); }}
            placeholder='{"name": "...", "frequencyMhz": 650, ...}'
            className="feat-textarea"
            rows={8}
          />
          {importError && (
            <div className="feat-error">{importError}</div>
          )}
          <div className="feat-actions" style={{ marginTop: 12 }}>
            <button
              className="feat-btn feat-btn-primary"
              onClick={handleImport}
              disabled={!importJson.trim()}
            >
              {t('profiles.importJson')}
            </button>
          </div>
        </div>
      )}

      {/* Profile list */}
      {profiles.length > 0 && (
        <div className="feat-card">
          <h3 className="feat-card-title">Saved Profiles</h3>
          <div className="feat-profile-list">
            {profiles.map((profile, idx) => (
              <div key={idx} className="feat-profile-card">
                <div className="feat-profile-header">
                  <div className="feat-profile-name">{profile.name}</div>
                  <span
                    className="feat-profile-target"
                    style={{ color: targetColor(profile.target), borderColor: targetColor(profile.target) }}
                  >
                    {profile.target}
                  </span>
                </div>
                <div className="feat-profile-meta">
                  <span>by {profile.author}</span>
                  <span>{profile.model}</span>
                  {profile.hashrateThs && <span>{profile.hashrateThs.toFixed(1)} TH/s</span>}
                  {profile.efficiencyJth && <span>{profile.efficiencyJth.toFixed(1)} J/TH</span>}
                </div>
                <div className="feat-profile-specs">
                  <span>{profile.frequencyMhz} MHz</span>
                  <span>{profile.voltageMv} mV</span>
                  <span>Fan: {profile.fanMode}</span>
                </div>
                {profile.description && (
                  <div className="feat-profile-desc">{profile.description}</div>
                )}
                <div className="feat-profile-actions">
                  <ActionButton
                    label={t('common.save')}
                    className="feat-btn-sm"
                    onClick={() => handleSaveProfile(profile)}
                  />
                  <button
                    className="feat-btn feat-btn-secondary feat-btn-sm"
                    onClick={() => handleRemoveProfile(idx)}
                  >
                    {t('common.delete')}
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
