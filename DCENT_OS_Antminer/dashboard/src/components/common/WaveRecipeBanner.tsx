/**
 * Bosminer-handoff recipe state banner.
 *
 * Renders the live handoff-recipe env state for the PSU-spoof handoff
 * hardware class (the only hardware where the recipe applies). On other
 * hardware this is a component-level no-op (returns null) so no other
 * dashboard surface is affected.
 *
 * Visual treatment:
 *   - Green check banner: recipe intact (all required env vars applied,
 *     zero forbidden detected, daemon will mine normally).
 *   - Yellow warn: some required env vars missing (operator may have
 *     partially overridden the launcher's env).
 *   - Red high-severity banner: at least one forbidden env var is set.
 *     The dcentrald startup guard will REFUSE to start with EX_CONFIG
 *     (exit code 78) on the next boot — surfaced to the operator BEFORE
 *     the next AC-cycle re-breaks mining on the unit.
 *
 * Data source: `/api/env/recipe`. The endpoint is dev-firmware no-auth
 * (matches the existing dev MCP/dashboard posture).
 */

import { useEffect, useState } from 'react';
import { api } from '../../api/client';
import type { EnvRecipeResponse } from '../../api/types';
import { InfoDot } from './Tooltip';

const POLL_INTERVAL_MS = 15_000;
const RUNBOOK_PATH = 'https://github.com/DCentralTech/DCENT_OS';

export function WaveRecipeBanner() {
  const [recipe, setRecipe] = useState<EnvRecipeResponse | null>(null);
  const [endpointAvailable, setEndpointAvailable] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const next = await api.getEnvRecipe();
        if (cancelled) return;
        if (next == null) {
          setEndpointAvailable(false);
          setRecipe(null);
          return;
        }
        setEndpointAvailable(true);
        setRecipe(next);
      } catch {
        // Network errors are transient — keep last-known state.
      }
    };
    void tick();
    const timer = setInterval(tick, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Endpoint not present (older daemon) OR not a handoff-class unit →
  // render nothing. The component-level guard keeps every other surface
  // byte-identical (no regression).
  if (endpointAvailable === false || recipe == null || !recipe.is_xil_25_class) {
    return null;
  }

  // Forbidden env detected → red high-severity AlertBanner-style banner.
  if (recipe.forbidden_detected.length > 0) {
    return (
      <div
        role="alert"
        aria-live="assertive"
        data-testid="wave-recipe-banner"
        data-recipe-state="forbidden"
        className="cp-alert"
        data-severity="critical"
        style={{
          margin: '8px 0',
          border: '1px solid rgba(239, 68, 68, 0.55)',
          background: 'rgba(239, 68, 68, 0.12)',
          color: 'var(--text, #E8E8E8)',
          borderRadius: 8,
          padding: '10px 14px',
          fontFamily: "'Inter', sans-serif",
        }}
      >
        <div style={{ display: 'flex', gap: 10, alignItems: 'flex-start' }}>
          <span aria-hidden style={{ color: 'var(--red, #EF4444)', fontWeight: 800, fontSize: '1.05rem', lineHeight: 1 }}>
            {'▲'}
          </span>
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 800, color: 'var(--red, #EF4444)', fontSize: '0.92rem' }}>
              DANGER: Handoff recipe broken — daemon will refuse to mine.
            </div>
            <div style={{ marginTop: 4, fontSize: '0.82rem', color: 'var(--text-secondary, #B5B5BD)', lineHeight: 1.5 }}>
              {recipe.forbidden_detected.length} forbidden env var
              {recipe.forbidden_detected.length === 1 ? '' : 's'} detected (each is known to break mining on this unit):{' '}
              <code style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: '0.78rem' }}>
                {recipe.forbidden_detected.join(', ')}
              </code>
              . The runtime guard exits with EX_CONFIG (78) on this hardware.
            </div>
            <div style={{ marginTop: 6, fontSize: '0.78rem' }}>
              <a
                href={RUNBOOK_PATH}
                style={{ color: 'var(--red, #EF4444)', fontWeight: 700, textDecoration: 'none' }}
              >
                See the documentation
              </a>
              <span style={{ marginLeft: 8, color: 'var(--text-dim, #7C7C86)' }}>
                <InfoDot term="recipe_state_intact" />
              </span>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Yellow warn: some required env vars missing.
  if (recipe.missing.length > 0) {
    return (
      <div
        role="status"
        data-testid="wave-recipe-banner"
        data-recipe-state="missing"
        style={{
          margin: '8px 0',
          border: '1px solid rgba(245, 158, 11, 0.45)',
          background: 'rgba(245, 158, 11, 0.10)',
          color: 'var(--text, #E8E8E8)',
          borderRadius: 8,
          padding: '10px 14px',
          fontFamily: "'Inter', sans-serif",
        }}
      >
        <div style={{ display: 'flex', gap: 10, alignItems: 'flex-start' }}>
          <span aria-hidden style={{ color: 'var(--amber, #F59E0B)', fontWeight: 800, fontSize: '1.05rem', lineHeight: 1 }}>
            {'⚠'}
          </span>
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 700, color: 'var(--amber, #F59E0B)', fontSize: '0.9rem' }}>
              Handoff recipe incomplete: {recipe.missing.length} required env var
              {recipe.missing.length === 1 ? '' : 's'} missing.
            </div>
            <div style={{ marginTop: 4, fontSize: '0.8rem', color: 'var(--text-secondary, #B5B5BD)', lineHeight: 1.5 }}>
              The handoff launcher may have been overridden. Re-run the handoff recipe
              to restore the proven path. Missing: {recipe.missing.slice(0, 4).join(', ')}
              {recipe.missing.length > 4 ? ` (+${recipe.missing.length - 4} more)` : ''}
            </div>
            <div style={{ marginTop: 4, fontSize: '0.76rem', color: 'var(--text-dim, #7C7C86)' }}>
              <InfoDot term="recipe_state_intact" />
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Green: recipe intact.
  return (
    <div
      role="status"
      data-testid="wave-recipe-banner"
      data-recipe-state="intact"
      style={{
        margin: '8px 0',
        border: '1px solid rgba(45, 212, 160, 0.30)',
        background: 'rgba(45, 212, 160, 0.08)',
        color: 'var(--text, #E8E8E8)',
        borderRadius: 8,
        padding: '8px 14px',
        fontFamily: "'Inter', sans-serif",
      }}
    >
      <div style={{ display: 'flex', gap: 10, alignItems: 'center' }}>
        <span aria-hidden style={{ color: 'var(--green, #2DD4A0)', fontWeight: 800, fontSize: '0.95rem' }}>
          {'✓'}
        </span>
        <div style={{ flex: 1, display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
          <span style={{ fontWeight: 700, color: 'var(--green, #2DD4A0)', fontSize: '0.86rem' }}>
            Handoff recipe intact
          </span>
          <span style={{ color: 'var(--text-secondary, #B5B5BD)', fontSize: '0.78rem' }}>
            All {Object.keys(recipe.applied).length} required env vars applied, 0 forbidden detected.
          </span>
          <InfoDot term="recipe_state_intact" />
        </div>
      </div>
    </div>
  );
}
