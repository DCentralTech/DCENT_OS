import React, { useState, useEffect, useRef, useCallback } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { formatHex } from '../../utils/format';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { InfoDot } from '../common/Tooltip';
import { glossaryText } from '../../utils/glossary';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

// Full FPGA register map (BraiinsOS IP core, verified from live S9 probes)
const FPGA_REGISTER_MAP = [
  { name: 'CTRL', offset: 0x0000, size: 4, desc: 'Control register (enable, reset, midstate count)', bitfields: [
    { bit: 0, name: 'ENABLE', desc: 'IP core enable' },
    { bit: 1, name: 'MIDSTATE_1', desc: 'Single midstate mode' },
    { bit: 2, name: 'MIDSTATE_2', desc: '2-midstate ASICBoost' },
    { bit: 3, name: 'MIDSTATE_4', desc: '4-midstate ASICBoost' },
  ]},
  { name: 'BUILD_ID', offset: 0x0004, size: 4, desc: 'FPGA build timestamp' },
  { name: 'BAUD', offset: 0x0008, size: 4, desc: 'UART baud divisor. Baud = 200M/(16*(div+1)). 0x6C=115200, 0x07=1.5MHz', bitfields: [
    { bit: 0, name: 'DIV[7:0]', desc: 'Baud rate divisor' },
  ]},
  { name: 'STAT', offset: 0x000C, size: 4, desc: 'Status register (work/cmd FIFO status, IRQ)', bitfields: [
    { bit: 0, name: 'CMD_RX_EMPTY', desc: 'Command RX FIFO empty' },
    { bit: 1, name: 'CMD_RX_FULL', desc: 'Command RX FIFO full' },
    { bit: 2, name: 'WORK_RX_EMPTY', desc: 'Work RX FIFO empty' },
    { bit: 3, name: 'WORK_RX_FULL', desc: 'Work RX FIFO full' },
    { bit: 4, name: 'WORK_TX_EMPTY', desc: 'Work TX FIFO empty' },
    { bit: 5, name: 'WORK_TX_FULL', desc: 'Work TX FIFO full' },
    { bit: 6, name: 'IRQ_PENDING', desc: 'Interrupt pending' },
  ]},
  { name: 'ERR_CNT', offset: 0x0010, size: 4, desc: 'Error counter (CRC errors, UART framing)' },
  { name: 'WORK_ID', offset: 0x0014, size: 4, desc: 'Current work ID counter' },
  { name: 'WORK_TIME', offset: 0x0018, size: 4, desc: 'Work time register. Nonce range / clock cycles per work item', bitfields: [] },
  { name: 'TICKET_MASK', offset: 0x001C, size: 4, desc: 'Ticket mask for hardware difficulty filtering' },
  { name: 'HASH_CNT_LO', offset: 0x0020, size: 4, desc: 'Hash counting register (low 32 bits)' },
  { name: 'HASH_CNT_HI', offset: 0x0024, size: 4, desc: 'Hash counting register (high 32 bits)' },
] as const;

// Known BM1387 register decode maps (for ASIC register values displayed in hex dumps)
interface KnownRegisterDecode {
  name: string;
  decode: (value: number) => { label: string; value: string; color?: string }[];
}

const KNOWN_REGISTER_DECODES: Record<number, KnownRegisterDecode> = {
  // CTRL_REG at offset 0x0000
  0x0000: {
    name: 'CTRL_REG',
    decode: (value: number) => {
      const enable = value & 0x01;
      const baudSelect = (value >> 1) & 0x01;
      const midstateCnt = (value >> 2) & 0x03;
      const midstateLabel = midstateCnt === 0 ? '1 midstate' : midstateCnt === 1 ? '2 midstates' : midstateCnt === 2 ? '4 midstates' : `${midstateCnt} (unknown)`;
      return [
        { label: 'ENABLE', value: enable ? '1 (ON)' : '0 (OFF)', color: enable ? '#00FF41' : '#FF4444' },
        { label: 'BAUD_SELECT', value: baudSelect ? '1 (high)' : '0 (low)', color: baudSelect ? '#F7931A' : '#3b82f6' },
        { label: 'MIDSTATE_CNT', value: `${midstateCnt} (${midstateLabel})`, color: '#F7931A' },
      ];
    },
  },
  // MiscCtrl (BM1387 chip register, accessed via ASIC commands)
  // This is a synthetic entry for display when the user reads ASIC MiscCtrl values
};

// Inline bit-field decoder for arbitrary register values shown in hex dumps
function decodeRegisterValue(offset: number, value: number): { label: string; value: string; color?: string }[] | null {
  const decoder = KNOWN_REGISTER_DECODES[offset];
  if (decoder) return decoder.decode(value);
  return null;
}

// Decode MiscCtrl register (BM1387 internal register, 32-bit)
function decodeMiscCtrl(value: number): { label: string; value: string; color?: string }[] {
  const invClk = value & 0x01;
  const baudDiv = (value >> 1) & 0x0FFF;
  const gateBlock = (value >> 13) & 0x01;
  const baudRate = baudDiv > 0 ? Math.round(25000000 / (baudDiv * 8)) : 0;
  return [
    { label: 'INV_CLK', value: invClk ? '1' : '0', color: '#3b82f6' },
    { label: 'BAUD_DIV', value: `${baudDiv} (${baudRate > 0 ? `~${baudRate} baud` : 'N/A'})`, color: '#F7931A' },
    { label: 'GATE_BLOCK', value: gateBlock ? '1 (BLOCKED)' : '0 (OPEN)', color: gateBlock ? '#FF4444' : '#00FF41' },
  ];
}

// The inline decode overlay component
function BitFieldDecodeOverlay({ offset, value }: { offset: number; value: number }) {
  const decoded = decodeRegisterValue(offset, value);
  if (!decoded) return null;

  return (
    <div className="adv-decode-overlay">
      <div className="adv-decode-overlay-title">
        BIT DECODE: {formatHex(offset, 4)}
      </div>
      <div className="adv-decode-overlay-value">
        Value: {formatHex(value, 8)}
      </div>
      <div className="adv-decode-overlay-fields">
        {decoded.map(d => (
          <span key={d.label} style={{ color: d.color || '#00FF41' }}>
            {d.label}={d.value}
          </span>
        ))}
      </div>
    </div>
  );
}

// MiscCtrl decode overlay
function MiscCtrlDecodeOverlay({ value }: { value: number }) {
  const decoded = decodeMiscCtrl(value);

  return (
    <div className="adv-decode-overlay">
      <div className="adv-decode-overlay-title">
        BIT DECODE: MiscCtrl
      </div>
      <div className="adv-decode-overlay-value">
        Value: {formatHex(value, 8)}
      </div>
      <div className="adv-decode-overlay-fields">
        {decoded.map(d => (
          <span key={d.label} style={{ color: d.color || '#00FF41' }}>
            {d.label}={d.value}
          </span>
        ))}
      </div>
    </div>
  );
}

interface WriteHistoryEntry {
  id: number;
  timestamp: number;
  chain: number;
  offset: string;
  value: string;
  success: boolean;
}

function hexDump(offset: number, values: number[]): { addr: string; hex: string; ascii: string }[] {
  const rows: { addr: string; hex: string; ascii: string }[] = [];
  for (let i = 0; i < values.length; i += 16) {
    const chunk = values.slice(i, i + 16);
    const addr = formatHex(offset + i, 4);
    const hex = chunk.map(v => v.toString(16).padStart(2, '0')).join(' ');
    const ascii = chunk.map(v => (v >= 0x20 && v < 0x7F) ? String.fromCharCode(v) : '.').join('');
    rows.push({ addr, hex: hex.padEnd(47), ascii });
  }
  return rows;
}

function decodeBitfields(value: number, bitfields: readonly { bit: number; name: string; desc: string }[]) {
  return bitfields.map(bf => ({
    ...bf,
    set: Boolean((value >> bf.bit) & 1),
  }));
}

// Extract a 32-bit value from a byte array at a given offset
function extract32(values: number[], byteOffset: number): number | null {
  if (byteOffset + 4 > values.length) return null;
  return (values[byteOffset] << 24) | (values[byteOffset + 1] << 16) |
         (values[byteOffset + 2] << 8) | values[byteOffset + 3];
}

export function RegisterInspector() {
  const { activeChain, setActiveChain } = useActiveHardware();
  const { isProxyMode } = useSystemHealth();
  const [chain, setChain] = useState(activeChain);
  const [readOffset, setReadOffset] = useState('0x0000');
  const [readCount, setReadCount] = useState(16);
  const [readResult, setReadResult] = useState<number[] | null>(null);
  const [readResultOffset, setReadResultOffset] = useState(0);
  const [readError, setReadError] = useState('');

  const [writeOffset, setWriteOffset] = useState('0x0000');
  const [writeValue, setWriteValue] = useState('0x00000000');
  const [writeError, setWriteError] = useState('');
  const [writeSuccess, setWriteSuccess] = useState('');
  const [writeHistory, setWriteHistory] = useState<WriteHistoryEntry[]>([]);
  const writeCounterRef = useRef(0);

  const [autoPoll, setAutoPoll] = useState(false);
  const [pollInterval, setPollInterval] = useState(2000);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const [selectedReg, setSelectedReg] = useState<typeof FPGA_REGISTER_MAP[number] | null>(null);
  const [regValue, setRegValue] = useState<number | null>(null);

  const [compareMode, setCompareMode] = useState(false);
  const [compareResults, setCompareResults] = useState<Record<number, number[] | null>>({});

  // MiscCtrl decode state
  const [miscCtrlValue, setMiscCtrlValue] = useState<number | null>(null);
  const [showMiscCtrlDecode, setShowMiscCtrlDecode] = useState(false);

  // Sync chain from context
  useEffect(() => {
    setChain(activeChain);
  }, [activeChain]);

  const handleRead = useCallback(async () => {
    setReadError('');
    setReadResult(null);
    try {
      const res = await api.readRegisters(chain, readOffset, readCount);
      setReadResult(res.values ?? []);
      setReadResultOffset(parseInt(readOffset, 16));
    } catch (e: unknown) {
      setReadError(e instanceof Error ? e.message : 'Read failed');
    }
  }, [chain, readOffset, readCount]);

  // Auto-poll
  useEffect(() => {
    if (autoPoll) {
      pollRef.current = setInterval(handleRead, pollInterval);
    }
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [autoPoll, pollInterval, handleRead]);

  const handleWrite = async () => {
    setWriteError('');
    setWriteSuccess('');
    if (isProxyMode) {
      setWriteError('Blocked: bosminer owns hardware in proxy/hybrid mode.');
      return;
    }
    const entry: WriteHistoryEntry = {
      id: ++writeCounterRef.current,
      timestamp: Date.now(),
      chain,
      offset: writeOffset,
      value: writeValue,
      success: false,
    };
    try {
      echoCli(`reg write ${chain} ${writeOffset} ${writeValue}`);
      await api.writeRegister({ chain, offset: writeOffset, value: writeValue, confirm: true });
      entry.success = true;
      setWriteSuccess(`Wrote ${writeValue} to chain ${chain} offset ${writeOffset}`);
    } catch (e: unknown) {
      setWriteError(e instanceof Error ? e.message : 'Write failed');
    }
    setWriteHistory(prev => [entry, ...prev].slice(0, 20));
  };

  const loadPreset = (reg: typeof FPGA_REGISTER_MAP[number]) => {
    setReadOffset(formatHex(reg.offset, 4));
    setReadCount(reg.size);
    setSelectedReg(reg);
  };

  const readRegForDecode = async (reg: typeof FPGA_REGISTER_MAP[number]) => {
    setSelectedReg(reg);
    try {
      const res = await api.readRegisters(chain, formatHex(reg.offset, 4), reg.size);
      const values = res.values ?? [];
      if (values.length >= 4) {
        const val = (values[0] << 24) | (values[1] << 16) | (values[2] << 8) | values[3];
        setRegValue(val);
      } else if (values.length > 0) {
        setRegValue(values[0]);
      }
    } catch {
      setRegValue(null);
    }
  };

  const handleCompare = async () => {
    echoCli(`reg dump 6,7,8 ${readOffset} ${readCount}`, 'compare all chains');
    const results: Record<number, number[] | null> = {};
    for (const ch of [6, 7, 8]) {
      try {
        const res = await api.readRegisters(ch, readOffset, readCount);
        results[ch] = res.values ?? [];
      } catch {
        results[ch] = null;
      }
    }
    setCompareResults(results);
  };

  // Try to auto-decode known registers from the read result
  const getInlineDecodes = (): { offset: number; value: number }[] => {
    if (!readResult) return [];
    const decodes: { offset: number; value: number }[] = [];
    const baseOffset = readResultOffset;

    // Check if the read range covers any known registers
    for (const reg of FPGA_REGISTER_MAP) {
      const relOffset = reg.offset - baseOffset;
      if (relOffset >= 0 && relOffset + 4 <= readResult.length) {
        const val = extract32(readResult, relOffset);
        if (val !== null && KNOWN_REGISTER_DECODES[reg.offset]) {
          decodes.push({ offset: reg.offset, value: val });
        }
      }
    }
    return decodes;
  };

  const dump = readResult ? hexDump(readResultOffset, readResult) : [];
  const inlineDecodes = getInlineDecodes();
  const formatTime = (ts: number) => new Date(ts).toTimeString().split(' ')[0];

  const [showHelp, setShowHelp] = useState(false);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// register inspector</div>
          <h2 className="hacker-inspector-title">FPGA Register Map</h2>
        </div>
        <div className="hacker-inspector-actions adv-help-anchor">
          <span className={`hacker-inspector-status ${autoPoll ? '' : 'neutral'}`}>
            {autoPoll ? `POLL ${pollInterval / 1000}s` : 'IDLE'}
          </span>
          <button
            type="button"
            className="hacker-inspector-help"
            onClick={() => setShowHelp(v => !v)}
            aria-expanded={showHelp}
            aria-label="Show register inspector context help"
            title="Context help"
          >
            ?
          </button>
          {showHelp && (
            <div
              role="dialog"
              aria-label="Register inspector help"
              className="adv-help-pop"
            >
              <div className="adv-help-pop-title">
                FPGA Register Inspector — Quick Reference
              </div>
              <div className="adv-help-pop-sub adv-mb-4">Chain base addresses:</div>
              <div>0x43C00000 — Chain 6</div>
              <div>0x43C10000 — Chain 7</div>
              <div>0x43C20000 — Chain 8</div>
              <div className="adv-help-pop-sub adv-mt-8">Access notes:</div>
              <div>32-bit (word) access for control regs (CTRL, BAUD, WORK_TIME, etc.).</div>
              <div>Hex dump shows bytes — big-endian byte order on the wire.</div>
              <div>Offsets are 4-byte aligned. Use `Count` = 4 for one 32-bit register.</div>
              <div className="adv-help-pop-warn">
                Writes are guarded; bosminer ownership blocks writes in proxy/hybrid mode.
              </div>
            </div>
          )}
          <button
            type="button"
            className={`btn ${autoPoll ? 'btn-primary' : 'btn-secondary'} advanced-compact-button`}
            onClick={() => setAutoPoll(!autoPoll)}
            aria-pressed={autoPoll}
          >
            {autoPoll ? `POLLING (${pollInterval / 1000}s)` : 'AUTO-POLL OFF'}
          </button>
          {autoPoll && (
            <select
              value={pollInterval}
              onChange={e => setPollInterval(Number(e.target.value))}
              style={{ fontSize: '0.7rem', padding: '2px 4px' }}
            >
              <option value={1000}>1s</option>
              <option value={2000}>2s</option>
              <option value={5000}>5s</option>
              <option value={10000}>10s</option>
            </select>
          )}
          <button
            type="button"
            className={`btn ${compareMode ? 'btn-primary' : 'btn-secondary'} advanced-compact-button`}
            onClick={() => setCompareMode(!compareMode)}
            aria-pressed={compareMode}
          >
            {compareMode ? 'COMPARE ON' : 'COMPARE'}
          </button>
          <button
            type="button"
            className="hacker-inspector-refresh"
            onClick={() => { void handleRead(); }}
            title="Read registers now"
          >
            ⟳ READ
          </button>
        </div>
      </header>

      <div className="hacker-inspector-body">
      {/* Register shortcuts */}
      <div className="adv-flex-wrap adv-mb-12">
        <span className="adv-hint is-xs adv-flex-center" style={{ alignSelf: 'center' }}>
          FPGA register
          <InfoDot term="fpga_register" />
        </span>
        {FPGA_REGISTER_MAP.map(reg => (
          <button
            key={reg.name}
            type="button"
            className={`btn ${selectedReg?.name === reg.name ? 'btn-primary' : 'btn-secondary'} adv-mini-btn`}
            onClick={() => loadPreset(reg)}
            onDoubleClick={() => readRegForDecode(reg)}
            title={`${formatHex(reg.offset, 4)} - ${reg.desc} (dblclick to decode)`}
            aria-pressed={selectedReg?.name === reg.name}
          >
            {reg.name}
          </button>
        ))}
        {/* MiscCtrl decode button */}
        <button
          type="button"
          className={`btn ${showMiscCtrlDecode ? 'btn-primary' : 'btn-secondary'} adv-mini-btn`}
          onClick={() => setShowMiscCtrlDecode(!showMiscCtrlDecode)}
          title="Decode a MiscCtrl value (BM1387 ASIC register)"
          aria-pressed={showMiscCtrlDecode}
        >
          MiscCtrl
        </button>
      </div>

      {/* MiscCtrl manual decode */}
      {showMiscCtrlDecode && (
        <div className="glass-card adv-card adv-mb-16">
          <div className="adv-card-title is-sm-tight">
            MiscCtrl Decoder (BM1387)
          </div>
          <div className="adv-hint is-xs adv-mb-8">
            Enter a MiscCtrl register value to decode bit fields. Common values: 0x00200180 (normal), 0x00208180 (gate_block=1).
          </div>
          <div className="adv-flex-center adv-mb-8">
            <input
              id="register-inspector-miscctrl"
              type="text"
              placeholder="0x00200180"
              value={miscCtrlValue !== null ? formatHex(miscCtrlValue, 8) : ''}
              onChange={e => {
                const parsed = parseInt(e.target.value, 16);
                setMiscCtrlValue(isNaN(parsed) ? null : parsed);
              }}
              className="adv-in-xl"
            />
            <span className="adv-hint is-xs">Hex value</span>
          </div>
          {miscCtrlValue !== null && <MiscCtrlDecodeOverlay value={miscCtrlValue} />}
          {miscCtrlValue === null && (
            <div className="adv-miscctrl-hint">
              Bit 13 = GATE_BLOCK{'\n'}
              Bit 12:1 = BAUD_DIV{'\n'}
              Bit 0 = INV_CLK
            </div>
          )}
        </div>
      )}

      {/* Bit-field decoder panel */}
      {selectedReg && 'bitfields' in selectedReg && selectedReg.bitfields && selectedReg.bitfields.length > 0 && (
        <div className="glass-card adv-card adv-mb-16">
          <div className="adv-flex-between adv-mb-8">
            <div className="adv-card-title is-sm" style={{ marginBottom: 0 }}>
              {selectedReg.name} Bit Decoder ({formatHex(selectedReg.offset, 4)})
            </div>
            <button
              type="button"
              className="btn btn-secondary advanced-compact-button"
              onClick={() => readRegForDecode(selectedReg)}
            >
              Read & Decode
            </button>
          </div>
          <div className="adv-hint adv-mb-8">
            {selectedReg.desc}
          </div>
          {regValue !== null && (
            <>
              <div className="adv-code-readout">
                Raw: {formatHex(regValue, 8)} = {regValue} (0b{regValue.toString(2).padStart(32, '0')})
              </div>
              {/* Inline human-readable decode for known registers */}
              {KNOWN_REGISTER_DECODES[selectedReg.offset] && (
                <BitFieldDecodeOverlay offset={selectedReg.offset} value={regValue} />
              )}
              <div className="adv-grid-fill">
                {decodeBitfields(regValue, 'bitfields' in selectedReg ? selectedReg.bitfields : []).map(bf => (
                  <div key={bf.bit} className={`adv-bf-cell ${bf.set ? 'is-set' : ''}`}>
                    <div className="adv-bf-cell-name">
                      [{bf.bit}] {bf.name}: {bf.set ? '1' : '0'}
                    </div>
                    <div className="adv-bf-cell-desc">{bf.desc}</div>
                  </div>
                ))}
              </div>
            </>
          )}
          {regValue === null && (
            <div className="adv-hint">
              Double-click a register button or click "Read & Decode" to see bit fields.
            </div>
          )}
        </div>
      )}

      <div className="adv-grid-2">
        {/* Read section */}
        <div className="glass-card adv-card">
          <div className="adv-card-title">Read Registers</div>

          <div className="adv-row">
            <div>
              <label htmlFor="register-inspector-chain" className="advanced-control-label">
                Chain
              </label>
              <select id="register-inspector-chain" value={chain} onChange={e => { setChain(Number(e.target.value)); setActiveChain(Number(e.target.value)); }}>
                <option value={6}>Chain 6</option>
                <option value={7}>Chain 7</option>
                <option value={8}>Chain 8</option>
              </select>
            </div>
            <div>
              <label htmlFor="register-inspector-read-offset" className="advanced-control-label">
                Offset (hex)
              </label>
              <input
                id="register-inspector-read-offset"
                type="text"
                value={readOffset}
                onChange={e => setReadOffset(e.target.value)}
                className="adv-in-md"
              />
            </div>
            <div>
              <label htmlFor="register-inspector-read-count" className="advanced-control-label">
                Count
              </label>
              <input
                id="register-inspector-read-count"
                type="number"
                value={readCount}
                onChange={e => setReadCount(Number(e.target.value))}
                min={1}
                max={256}
                className="adv-in-xs"
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
              <ActionButton label="Read" onClick={handleRead} />
              <CliHint cmd={`reg dump ${chain} ${readOffset} ${readCount}`} />
            </div>
            {compareMode && (
              <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
                <ActionButton label="Compare All" onClick={handleCompare} variant="secondary" />
                <CliHint cmd={`reg dump 6,7,8 ${readOffset} ${readCount}`} note="compare all chains" />
              </div>
            )}
          </div>

          {readError && (
            <div className="adv-msg is-error">{readError}</div>
          )}

          {dump.length > 0 && (
            <>
              <div className="table-wrap">
                <div className="hex-dump">
                  {dump.map((row, i) => (
                    <div key={i}>
                      <span className="addr">{row.addr}</span>
                      {'  '}
                      <span className="byte">{row.hex}</span>
                      {'  '}
                      <span className="ascii">|{row.ascii}|</span>
                    </div>
                  ))}
                </div>
              </div>

              {/* Auto-decoded inline bit fields below hex dump */}
              {inlineDecodes.map(d => (
                <BitFieldDecodeOverlay key={d.offset} offset={d.offset} value={d.value} />
              ))}
            </>
          )}

          {/* Chain comparison */}
          {compareMode && Object.keys(compareResults).length > 0 && (
            <div className="adv-mt-12">
              <div className="adv-compare-title">
                Chain Comparison at {readOffset}
              </div>
              {[6, 7, 8].map(ch => (
                <div key={ch} className="adv-compare-row">
                  <span className="adv-compare-ch">Ch{ch}:</span>
                  {compareResults[ch]
                    ? <span className="adv-compare-ok">{compareResults[ch]!.map(b => b.toString(16).padStart(2, '0')).join(' ')}</span>
                    : <span className="adv-compare-fail">read failed</span>
                  }
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Write section */}
        <div className="glass-card adv-card">
          <div className="adv-card-title is-danger">Write Register</div>
          {isProxyMode && (
            <div className="adv-msg is-warn adv-mb-12" style={{ fontSize: '0.78rem' }}>
              Raw register writes disabled: bosminer owns hardware in proxy/hybrid mode.
            </div>
          )}

          <div className="adv-row">
            <div>
              <label htmlFor="register-inspector-write-offset" className="advanced-control-label">
                Offset (hex)
              </label>
              <input
                id="register-inspector-write-offset"
                type="text"
                value={writeOffset}
                onChange={e => setWriteOffset(e.target.value)}
                className="adv-in-md"
              />
            </div>
            <div>
              <label htmlFor="register-inspector-write-value" className="advanced-control-label">
                Value (hex)
              </label>
              <input
                id="register-inspector-write-value"
                type="text"
                value={writeValue}
                onChange={e => setWriteValue(e.target.value)}
                className="adv-in-lg"
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
              <ActionButton
                label="Write"
                onClick={handleWrite}
                variant="danger"
                disabled={isProxyMode}
                confirm={`Write ${writeValue} to chain ${chain} at offset ${writeOffset}? This directly modifies FPGA registers and can damage hardware.`}
              />
              <CliHint cmd={`reg write ${chain} ${writeOffset} ${writeValue}`} />
            </div>
          </div>

          {/* Live decode of write value */}
          {(() => {
            const wOffset = parseInt(writeOffset, 16);
            const wValue = parseInt(writeValue, 16);
            if (!isNaN(wOffset) && !isNaN(wValue) && KNOWN_REGISTER_DECODES[wOffset]) {
              return <BitFieldDecodeOverlay offset={wOffset} value={wValue} />;
            }
            return null;
          })()}

          {writeError && (
            <div className="adv-msg is-error is-mt">{writeError}</div>
          )}
          {writeSuccess && (
            <div className="adv-msg is-success is-mt">{writeSuccess}</div>
          )}

          {/* Write history */}
          {writeHistory.length > 0 && (
            <div className="adv-mt-12">
              <div className="adv-writelog-title">
                Write History (last {writeHistory.length})
              </div>
              {writeHistory.map(entry => (
                <div
                  key={entry.id}
                  className={`adv-writelog-row ${entry.success ? '' : 'is-fail'}`}
                >
                  [{formatTime(entry.timestamp)}] Ch{entry.chain} {entry.offset}={entry.value} {entry.success ? 'OK' : 'FAIL'}
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Quick reference for CTRL_REG and MiscCtrl bit layouts */}
      <div className="glass-card adv-card adv-card-mt">
        <div className="adv-card-title is-sm">
          Register Bit-Field Reference
        </div>
        <div className="adv-ref-grid">
          <div className="adv-ref-cell">
            <div className="adv-ref-cell-title">CTRL_REG (0x0000)</div>
            <div className="adv-ref-cell-body">
              <div>Bit 3:2 = MIDSTATE_CNT</div>
              <div className="adv-ref-sub">0=1, 1=2, 2=4 midstates</div>
              <div>Bit 1   = BAUD_SELECT</div>
              <div>Bit 0   = ENABLE</div>
            </div>
          </div>
          <div className="adv-ref-cell">
            <div className="adv-ref-cell-title">MiscCtrl (BM1387)</div>
            <div className="adv-ref-cell-body">
              <div>Bit 13  = GATE_BLOCK</div>
              <div className="adv-ref-sub">1=cores blocked, 0=open</div>
              <div>Bit 12:1 = BAUD_DIV</div>
              <div>Bit 0   = INV_CLK</div>
            </div>
          </div>
        </div>
      </div>
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>chain {chain}</span>
          <span>{readResult ? `${readResult.length} bytes read` : 'no read yet'}</span>
          <span>{writeHistory.length} writes logged</span>
          {autoPoll && <span>polling {pollInterval / 1000}s</span>}
        </div>
      </footer>
    </div>
  );
}
