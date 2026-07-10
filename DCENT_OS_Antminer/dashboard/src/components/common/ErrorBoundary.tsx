import React, { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

interface ErrorBoundaryProps {
  children: ReactNode;
  /**
   * When this value changes, the boundary clears its error state. Pass the
   * current route/page here so a crash on one screen doesn't wedge every
   * other screen until a hard reload — navigating away should recover.
   */
  resetKey?: string | number;
}

interface ErrorBoundaryState {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  constructor(props: ErrorBoundaryProps) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Log to console for debugging — in production this would go to a logging service
    console.error('[DCENTos ErrorBoundary]', error, info.componentStack);
  }

  componentDidUpdate(prevProps: ErrorBoundaryProps) {
    // Route changed while in an error state — recover so navigating away from
    // a broken page works without a hard reload.
    if (this.state.hasError && prevProps.resetKey !== this.props.resetKey) {
      this.setState({ hasError: false, error: null });
    }
  }

  handleReload = () => {
    window.location.reload();
  };

  handleReset = () => {
    this.setState({ hasError: false, error: null });
  };

  render() {
    if (this.state.hasError) {
      return (
        <div style={{
          display: 'flex',
          flexDirection: 'column',
          alignItems: 'center',
          justifyContent: 'center',
          height: '100vh',
          background: 'var(--bg, #0a0a0f)',
          color: 'var(--text, #E8E8E8)',
          fontFamily: "'Inter', sans-serif",
          padding: 24,
          textAlign: 'center',
        }}>
          <div style={{
            fontFamily: "var(--font-heading)",
            fontWeight: 800,
            fontSize: '2rem',
            color: 'var(--accent, #FAA500)',
            marginBottom: 16,
          }}>
            DCENT_OS
          </div>
          <div style={{
            fontSize: '1.2rem',
            fontWeight: 600,
            marginBottom: 8,
            color: 'var(--red, #EF4444)',
          }}>
            The dashboard hit a snag
          </div>
          <div style={{
            fontSize: '0.85rem',
            color: 'var(--text-dim, #6B7280)',
            marginBottom: 24,
            maxWidth: 400,
          }}>
            Your miner is still mining — the dashboard UI had an issue. Try reloading.
          </div>
          {this.state.error && (
            <div style={{
              fontFamily: "'JetBrains Mono', monospace",
              fontSize: '0.75rem',
              color: 'var(--red, #EF4444)',
              background: 'rgba(239, 68, 68, 0.1)',
              border: '1px solid rgba(239, 68, 68, 0.3)',
              borderRadius: 8,
              padding: 12,
              maxWidth: 500,
              marginBottom: 24,
              wordBreak: 'break-word',
            }}>
              {this.state.error.message}
            </div>
          )}
          <div style={{ display: 'flex', gap: 12 }}>
            <button
              onClick={this.handleReset}
              style={{
                padding: '10px 24px',
                borderRadius: 8,
                border: '1px solid var(--border, #333)',
                background: 'var(--card-bg, #242432)',
                color: 'var(--text, #E8E8E8)',
                fontWeight: 600,
                cursor: 'pointer',
                fontSize: '0.9rem',
              }}
            >
              Try Again
            </button>
            <button
              onClick={this.handleReload}
              style={{
                padding: '10px 24px',
                borderRadius: 8,
                border: 'none',
                background: 'var(--accent, #FAA500)',
                color: 'var(--accent-ink, #1a0f00)',
                fontWeight: 600,
                cursor: 'pointer',
                fontSize: '0.9rem',
              }}
            >
              Reload Page
            </button>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
