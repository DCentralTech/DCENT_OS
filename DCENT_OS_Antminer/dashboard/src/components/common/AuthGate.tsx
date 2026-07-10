import React, { useState, useEffect } from 'react';
import { api, ApiError } from '../../api/client';
import { getSessionToken } from '../../api/credentials';
import { useMinerStore } from '../../store/miner';

interface AuthGateProps {
  children: React.ReactNode;
}

export function AuthGate({ children }: AuthGateProps) {
  const authenticated = useMinerStore(s => s.authenticated);
  const setAuthenticated = useMinerStore(s => s.setAuthenticated);
  const password = useMinerStore(s => s.settings.password);
  const setupStatus = useMinerStore(s => s.setupStatus);
  const [input, setInput] = useState('');
  const [error, setError] = useState('');
  const [checking, setChecking] = useState(false);
  const hasSessionToken = Boolean(getSessionToken());
  const authRequired = Boolean(
    password ||
    setupStatus?.auth?.password_set ||
    setupStatus?.resume_requires_auth,
  );

  // Convenience lock only. The daemon remains the authorization source.
  useEffect(() => {
    if (authenticated) return;
    if (!authRequired || hasSessionToken) setAuthenticated(true);
  }, [authRequired, authenticated, hasSessionToken, setAuthenticated]);

  if (!authRequired) return <>{children}</>;

  if (authenticated || hasSessionToken) return <>{children}</>;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (checking) return;
    setChecking(true);
    setError('');

    try {
      const token = await api.createSession(input);
      if (token) {
        setAuthenticated(true);
        setInput('');
        return;
      }
      setError('The daemon rejected that password.');
      setInput('');
    } catch (err) {
      setError(
        err instanceof ApiError && err.message.trim()
          ? err.message
          : 'The daemon rejected that password.',
      );
      setInput('');
    } finally {
      setChecking(false);
    }
  };

  return (
    <div style={{
      display: 'flex', alignItems: 'center', justifyContent: 'center',
      height: '100vh', background: 'var(--bg, #0A0A0A)',
    }}>
      <form onSubmit={handleSubmit} style={{
        background: 'var(--bg, #0a0a0f)', border: '1px solid var(--border, rgba(255,255,255,0.06))',
        borderRadius: 12, padding: 32, width: 360, textAlign: 'center',
      }}>
        <div style={{
          fontFamily: "var(--font-heading)", fontWeight: 800,
          fontSize: '1.5rem', color: 'var(--accent, #00FF41)', marginBottom: 8,
        }}>
          ADVANCED MODE
        </div>
        <div style={{ color: 'var(--text-secondary, #888)', fontSize: '0.85rem', marginBottom: 24 }}>
          Server-enforced authorization. This local lock requests a daemon session.
        </div>
        <label htmlFor="auth-password" className="sr-only">Password</label>
        <input
          id="auth-password"
          type="password"
          value={input}
          onChange={e => setInput(e.target.value)}
          placeholder="Enter password"
          autoFocus
          disabled={checking}
          style={{
            width: '100%', padding: '12px 16px',
            background: 'rgba(0,0,0,0.6)', border: '1px solid var(--border, rgba(255,255,255,0.06))',
            borderRadius: 8, color: 'var(--accent, #00FF41)',
            fontFamily: "'JetBrains Mono', monospace",
            fontSize: '1rem', outline: 'none',
            marginBottom: error ? 8 : 16,
          }}
        />
        {error && (
          <div style={{ color: 'var(--red, #FF4444)', fontSize: '0.8rem', marginBottom: 8 }}>{error}</div>
        )}
        <button type="submit" style={{
          width: '100%', padding: '12px',
          background: 'var(--accent-glow, rgba(0, 255, 65, 0.1))', border: '1px solid var(--accent, #00FF41)',
          borderRadius: 8, color: 'var(--accent, #00FF41)',
          fontFamily: "'JetBrains Mono', monospace",
          fontWeight: 600, fontSize: '0.9rem', cursor: checking ? 'wait' : 'pointer',
        }}>
          {checking ? 'Checking daemon...' : 'Authenticate'}
        </button>
      </form>
    </div>
  );
}
