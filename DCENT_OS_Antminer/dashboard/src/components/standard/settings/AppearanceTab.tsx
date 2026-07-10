import React from 'react';
import { useMinerStore } from '../../../store/miner';
import { useTranslation } from '../../../i18n/i18n';
import { InfoDot } from '../../common/Tooltip';
import { AccentColorPicker } from '../../common/AccentColorPicker';
import { LedSettings } from '../../common/LedSettings';

export function AppearanceTab() {
  const settings = useMinerStore(s => s.settings);
  const updateSettings = useMinerStore(s => s.updateSettings);
  const { t } = useTranslation();
  const isLightAppearance = settings.appearance === 'light';

  return (
    <>
      <div className="section">
        <div className="section-title">Appearance</div>
        <div style={{
          background: 'var(--card-bg)', borderRadius: 'var(--radius)',
          padding: 16, border: '1px solid var(--border)',
        }}>
          <div style={{ marginBottom: 16 }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 8 }}>
              <label style={{ fontSize: '0.75rem', color: 'var(--text-dim)' }}>
                {t('settings.appearance.theme')}
              </label>
              <InfoDot term="appearance_theme" label={t('settings.appearance.theme')} />
            </div>
            <div
              role="group"
              aria-label={t('settings.appearance.theme')}
              style={{ display: 'inline-flex', gap: 8 }}
            >
              <button
                type="button"
                aria-pressed={!isLightAppearance}
                className={`btn ${!isLightAppearance ? 'btn-primary' : 'btn-secondary'}`}
                onClick={() => updateSettings({ appearance: 'dark' })}
              >
                {t('settings.appearance.dark')}
              </button>
              <button
                type="button"
                aria-pressed={isLightAppearance}
                className={`btn ${isLightAppearance ? 'btn-primary' : 'btn-secondary'}`}
                onClick={() => updateSettings({ appearance: 'light' })}
              >
                {t('settings.appearance.light')}
              </button>
            </div>
          </div>

          <AccentColorPicker />
        </div>
      </div>

      <div className="section">
        <LedSettings />
      </div>
    </>
  );
}
