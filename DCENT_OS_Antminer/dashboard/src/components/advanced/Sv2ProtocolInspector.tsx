import React, { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../../api/client';
import type { Sv2MessageRecord, Sv2MessagesResponse, Sv2StatusResponse } from '../../api/types';
import { useModeNavigation } from '../../hooks/useModeNavigation';
import { InfoDot } from '../common/Tooltip';

// Known SV2 message type decoder — maps wire byte to human-readable name + category
const SV2_MSG_TYPES: Record<number, { label: string; category: 'setup' | 'channel' | 'job' | 'share' | 'control' }> = {
  0x00: { label: 'Setup',              category: 'setup' },
  0x01: { label: 'Setup OK',           category: 'setup' },
  0x02: { label: 'Setup Error',        category: 'setup' },
  0x10: { label: 'Open Channel',       category: 'channel' },
  0x11: { label: 'Channel Opened',     category: 'channel' },
  0x12: { label: 'Open Channel Error', category: 'channel' },
  0x1c: { label: 'Submit Share',       category: 'share' },
  0x1d: { label: 'Share Accepted',     category: 'share' },
  0x1e: { label: 'New Job',            category: 'job' },
  0x20: { label: 'New Block!',         category: 'job' },
  0x21: { label: 'Difficulty Change',  category: 'control' },
};

function decodeMessageType(msgType: number, msgName: string): string {
  const known = SV2_MSG_TYPES[msgType];
  if (known) return known.label;
  if (msgName) return msgName;
  return `0x${msgType.toString(16).padStart(2, '0').toUpperCase()}`;
}

function getMessageCategory(msgType: number): string {
  return SV2_MSG_TYPES[msgType]?.category ?? 'control';
}

// Color for each direction + category
function getRowColor(direction: 'sent' | 'recv', msgType: number): string {
  const cat = getMessageCategory(msgType);
  if (msgType === 0x02 || msgType === 0x12) return 'var(--red)';       // errors
  if (cat === 'job')   return '#e89a3c';                                // orange for jobs
  if (cat === 'share') return direction === 'sent' ? '#4ec96d' : '#5cb8ff'; // green sent, blue recv
  if (direction === 'sent') return '#4ec96d';                           // green sent
  return '#5cb8ff';                                                     // blue received
}

function getDirectionArrow(direction: 'sent' | 'recv'): string {
  return direction === 'sent' ? '\u2192' : '\u2190';
}

function formatTimestamp(ms: number): string {
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  const ms3 = String(d.getMilliseconds()).padStart(3, '0');
  return `${hh}:${mm}:${ss}.${ms3}`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KB`;
}

// Expandable field descriptions for known message types
const SV2_FIELDS: Record<number, string[]> = {
  0x00: ['protocol', 'min_version', 'max_version', 'flags', 'endpoint_host', 'endpoint_port', 'vendor', 'firmware'],
  0x01: ['used_version', 'flags'],
  0x02: ['flags', 'error_code'],
  0x10: ['request_id', 'user_identity', 'nominal_hashrate', 'max_target'],
  0x11: ['request_id', 'channel_id', 'target', 'extranonce_prefix', 'group_channel_id'],
  0x1c: ['channel_id', 'sequence_number', 'job_id', 'nonce', 'ntime', 'version'],
  0x1d: ['channel_id', 'sequence_number', 'last_sequence_number', 'new_submits_accepted_count', 'new_shares_sum'],
  0x1e: ['channel_id', 'job_id', 'future_job', 'version', 'merkle_root'],
  0x20: ['channel_id', 'job_id', 'prev_hash', 'min_ntime', 'nbits'],
  0x21: ['channel_id', 'max_target'],
};

const MAX_MESSAGES = 200;

export function Sv2ProtocolInspector() {
  const [messages, setMessages] = useState<Sv2MessageRecord[]>([]);
  const [sv2Status, setSv2Status] = useState<Sv2StatusResponse | null>(null);
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const [expandedIdx, setExpandedIdx] = useState<number | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [sv2Active, setSv2Active] = useState(true);
  const { switchMode } = useModeNavigation();

  const listRef = useRef<HTMLDivElement>(null);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

  // Fetch SV2 status for header badges
  const fetchStatus = useCallback(async () => {
    try {
      const data: Sv2StatusResponse = await api.getSv2Status();
      setSv2Status(data);
      setSv2Active(true);
    } catch {
      setSv2Active(false);
    }
  }, []);

  // Fetch messages
  const fetchMessages = useCallback(async () => {
    if (pausedRef.current) return;
    try {
      const data: Sv2MessagesResponse = await api.getSv2Messages();
      setSv2Active(true);
      setFetchError(null);
      if (data.messages) {
        setMessages(data.messages.slice(-MAX_MESSAGES));
      }
    } catch (e: unknown) {
      setFetchError(e instanceof Error ? e.message : 'Fetch failed');
    }
  }, []);

  // Poll every 2 seconds
  useEffect(() => {
    fetchStatus();
    fetchMessages();
    const statusInterval = setInterval(fetchStatus, 5000);
    const msgInterval = setInterval(fetchMessages, 2000);
    return () => {
      clearInterval(statusInterval);
      clearInterval(msgInterval);
    };
  }, [fetchStatus, fetchMessages]);

  // Auto-scroll to bottom
  useEffect(() => {
    if (autoScroll && !paused && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight;
    }
  }, [messages, autoScroll, paused]);

  const handleClear = () => {
    setMessages([]);
    setExpandedIdx(null);
  };

  const toggleExpand = (idx: number) => {
    setExpandedIdx(prev => prev === idx ? null : idx);
  };

  const session = sv2Status?.session;
  const connected = sv2Status?.connected ?? false;

  // Tally
  const sentCount = messages.filter(m => m.direction === 'sent').length;
  const recvCount = messages.filter(m => m.direction === 'recv').length;

  // ── Render ──

  if (!sv2Active) {
    return (
      <div className="advanced-page">
        <div className="advanced-page-toolbar">
          <div className="advanced-page-heading">
            <div className="section-title sv2-inactive-title">
              SV2 PROTOCOL INSPECTOR
            </div>
            <div className="advanced-page-copy">
              Inspect Stratum V2 session state, transport metadata, and recent wire messages once an SV2 pool is configured.
            </div>
          </div>
        </div>
          <div className="sv2-inactive-card">
          <div className="sv2-inactive-head adv-mono">
            SV2 NOT ACTIVE
          </div>
          <div className="sv2-inactive-body">
            Stratum V2 is not currently in use. Configure a SV2 pool in your pool settings to enable
            the protocol inspector.
          </div>
          <div className="sv2-inactive-action">
            <button
              className="btn btn-secondary"
              onClick={() => { void switchMode('standard', 'pools'); }}
            >
              Open Pool Setup
            </button>
          </div>
          {fetchError && (
            <div className="sv2-inactive-err adv-mono">
              {fetchError}
            </div>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// sv2 protocol inspector</div>
          <h2 className="hacker-inspector-title">Stratum V2 Encrypted Wire</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${connected ? '' : 'danger'}`}>
            {connected ? 'CONNECTED' : 'DISCONNECTED'}
          </span>
          {session?.cipher_suite && (
            <span className="hacker-inspector-status neutral">{session.cipher_suite}</span>
          )}
          {session && session.handshake_latency_ms > 0 && (
            <span className={`hacker-inspector-status ${session.handshake_latency_ms < 200 ? '' : 'warning'}`}>
              {session.handshake_latency_ms}ms HS
            </span>
          )}
        </div>
      </header>

      <div className="hacker-inspector-body">

      {/* Session stats row */}
      {session && (
        <div className="sv2-stats">
          <span>CH: {session.channel_id ?? '--'}</span>
          <span>TX: {session.messages_sent} ({formatBytes(session.bytes_encrypted)})</span>
          <span>RX: {session.messages_received} ({formatBytes(session.bytes_decrypted)})</span>
          <span>Nonce TX: {session.noise_nonce_tx}</span>
          <span>Nonce RX: {session.noise_nonce_rx}</span>
          {session.pool_pubkey_fingerprint && (
            <span>Pool: {session.pool_pubkey_fingerprint.slice(0, 16)}...</span>
          )}
        </div>
      )}

      {/* Controls */}
      <div className="sv2-controlbar">
        <button
          className={`btn ${paused ? 'btn-danger' : 'btn-secondary'} sv2-ctl-btn`}
          onClick={() => setPaused(!paused)}
        >
          {paused ? 'PAUSED' : 'Pause'}
        </button>
        <button
          className="btn btn-secondary sv2-ctl-btn"
          onClick={handleClear}
        >
          Clear
        </button>
        <label className="sv2-autoscroll">
          <input
            type="checkbox"
            checked={autoScroll}
            onChange={e => setAutoScroll(e.target.checked)}
            className="sv2-accent-check"
          />
          Auto-scroll
        </label>
        <div className="sv2-spacer" />
        <span className="sv2-tally adv-mono">
          {messages.length}/{MAX_MESSAGES} msgs
          {' '}|{' '}
          <span className="sv2-c-sent">{sentCount} sent</span>
          {' '}
          <span className="sv2-c-recv">{recvCount} recv</span>
        </span>
      </div>

      <div className="advanced-scroll-region">
        {/* Column header */}
        <div className="sv2-colhead adv-mono">
          <span>Time</span>
          <span></span>
          <span>Message <InfoDot term="bip320_version_rolling" /></span>
          <span className="sv2-col-right">Size</span>
        </div>

        {/* Message list */}
        <div ref={listRef} className="sv2-msglist adv-mono">
        {messages.length === 0 && (
          <div className="sv2-msg-empty">
            {paused ? 'Paused -- no new messages fetched' : 'Waiting for SV2 messages...'}
          </div>
        )}
        {messages.map((msg, idx) => {
          const isExpanded = expandedIdx === idx;
          const fields = SV2_FIELDS[msg.msg_type];
          const decoded = decodeMessageType(msg.msg_type, msg.msg_name);

          return (
            <React.Fragment key={`${msg.timestamp_ms}-${idx}`}>
              <button
                type="button"
                className={`advanced-link-button sv2-row ${isExpanded ? 'is-expanded' : ''}`}
                style={{ color: getRowColor(msg.direction, msg.msg_type) }}
                onClick={() => toggleExpand(idx)}
                title={`Type 0x${msg.msg_type.toString(16).padStart(2, '0')} | ${msg.msg_name} | ${msg.payload_size} bytes`}
                aria-expanded={isExpanded}
                aria-label={`${decoded}, ${msg.payload_size} bytes, ${msg.direction}`}
              >
                <span className="sv2-row-ts">
                  {formatTimestamp(msg.timestamp_ms)}
                </span>
                <span className="sv2-row-arrow">
                  {getDirectionArrow(msg.direction)}
                </span>
                <span className="sv2-row-msg">
                  {decoded}
                  {msg.msg_type === 0x20 && (
                    <span className="sv2-row-tag">
                      (new block detected)
                    </span>
                  )}
                </span>
                <span className="sv2-row-size">
                  {msg.payload_size} B
                </span>
              </button>
              {isExpanded && (
                <div className="sv2-expanded">
                  <div className="adv-mb-4">
                    <span className="sv2-field-k">Type: </span>
                    0x{msg.msg_type.toString(16).padStart(2, '0')}
                    {' | '}
                    <span className="sv2-field-k">Wire name: </span>
                    {msg.msg_name}
                    {' | '}
                    <span className="sv2-field-k">Direction: </span>
                    {msg.direction === 'sent' ? 'Outbound (miner -> pool)' : 'Inbound (pool -> miner)'}
                    {' | '}
                    <span className="sv2-field-k">Payload: </span>
                    {msg.payload_size} bytes
                  </div>
                  {fields ? (
                    <div>
                      <span className="sv2-field-k">Known fields: </span>
                      {fields.map((f, i) => (
                        <span key={f}>
                          <span className="sv2-field-name">{f}</span>
                          {i < fields.length - 1 ? ', ' : ''}
                        </span>
                      ))}
                    </div>
                  ) : (
                    <div className="sv2-no-decode">
                      No field decode available for this message type.
                    </div>
                  )}
                </div>
              )}
            </React.Fragment>
          );
        })}
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>{connected ? 'live' : 'disconnected'}</span>
          {session?.cipher_suite && <span>cipher: {session.cipher_suite}</span>}
          {session && session.handshake_latency_ms > 0 && <span>{session.handshake_latency_ms}ms handshake</span>}
        </div>
      </footer>
    </div>
  );
}
