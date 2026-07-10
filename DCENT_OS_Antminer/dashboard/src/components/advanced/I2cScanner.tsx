import React, { useState, useEffect, useRef } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { formatHex } from '../../utils/format';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { InfoDot } from '../common/Tooltip';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

interface I2cDevice {
  addr: number;
  addrHex: string;
  device: string;
  status: 'found' | 'nack' | 'error';
  data?: number[];
  picDecode?: string;
  responseMs?: number;
}

interface TransactionEntry {
  id: number;
  timestamp: number;
  bus: number;
  addr: string;
  direction: 'read' | 'write';
  data: number[];
  responseMs: number;
  error?: string;
}

// Known I2C device identifiers for Antminer S9
const KNOWN_DEVICES: Record<number, string> = {
  0x55: 'PIC (Chain 6 voltage controller)',
  0x56: 'PIC (Chain 7 voltage controller)',
  0x57: 'PIC (Chain 8 voltage controller)',
  0x48: 'TMP75 (control board sensor)',
  0x49: 'TMP75 (control board sensor)',
  0x4A: 'TMP75 (control board sensor)',
};

// Hash board temp sensors — accessed via BM1387 I2C passthrough register 0x20, NOT directly on the I2C bus.
// These addresses cannot be scanned directly from the control board I2C bus.
const PASSTHROUGH_SENSORS: Array<{ addr: number; addrHex: string; device: string }> = [
  { addr: 0x98, addrHex: '0x98', device: 'TMP451 (hash board temp, via ASIC I2C passthrough, not directly scannable)' },
  { addr: 0x9A, addrHex: '0x9A', device: 'ADT7461 (hash board temp, via ASIC I2C passthrough, not directly scannable)' },
  { addr: 0x9C, addrHex: '0x9C', device: 'NCT218 (hash board temp, via ASIC I2C passthrough, not directly scannable)' },
];

// PIC address to chain mapping
const PIC_CHAIN_MAP: Record<number, number> = {
  0x55: 6,
  0x56: 7,
  0x57: 8,
};

// Decode PIC raw I2C response byte
function decodePicResponse(addr: number, data: number[]): string {
  if (data.length === 0) return 'No response';
  const byte = data[0];

  const chainId = PIC_CHAIN_MAP[addr];
  const chainStr = chainId ? ` (Chain ${chainId})` : '';

  switch (byte) {
    case 0x60: return `APP MODE${chainStr} - PIC running application firmware`;
    case 0xCC: return `BOOTLOADER${chainStr} - PIC in bootloader, needs JUMP(0x06)`;
    case 0x00: return `DEAD${chainStr} - PIC not responding (no pull-up? power issue?)`;
    case 0xFF: return `BUS ERROR${chainStr} - SDA stuck high (check I2C pull-ups)`;
    case 0x03: return `VERSION 0x03${chainStr} - BraiinsOS PIC firmware (24 commands)`;
    case 0x56: return `VERSION 0x56${chainStr} - Stock Bitmain PIC firmware`;
    case 0x5A: return `VERSION 0x5A${chainStr} - Stock Bitmain PIC firmware (newer)`;
    case 0x5E: return `VERSION 0x5E${chainStr} - Stock Bitmain PIC firmware (latest)`;
    default:
      if (byte >= 0x01 && byte <= 0x23) {
        return `CMD RESPONSE 0x${byte.toString(16)}${chainStr} - BraiinsOS command response`;
      }
      return `Unknown: 0x${byte.toString(16)} (${byte})${chainStr}`;
  }
}

function isPicAddress(addr: number): boolean {
  return addr === 0x55 || addr === 0x56 || addr === 0x57;
}

export function I2cScanner() {
  const { activeChain } = useActiveHardware();
  const { isProxyMode } = useSystemHealth();
  const [bus, setBus] = useState(0);
  const [scanning, setScanning] = useState(false);
  const [devices, setDevices] = useState<I2cDevice[]>([]);
  const [scanComplete, setScanComplete] = useState(false);

  const [readAddr, setReadAddr] = useState('0x55');
  const [readReg, setReadReg] = useState('');
  const [readResult, setReadResult] = useState<number[] | null>(null);
  const [readError, setReadError] = useState('');

  const [writeAddr, setWriteAddr] = useState('0x55');
  const [writeData, setWriteData] = useState('');
  const [writeError, setWriteError] = useState('');
  const [writeSuccess, setWriteSuccess] = useState('');

  const [transactionLog, setTransactionLog] = useState<TransactionEntry[]>([]);
  const txCounterRef = useRef(0);

  const [autoRefresh, setAutoRefresh] = useState(false);
  const autoRefreshRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const addTransaction = (entry: Omit<TransactionEntry, 'id' | 'timestamp'>) => {
    txCounterRef.current += 1;
    setTransactionLog(prev => [{
      ...entry,
      id: txCounterRef.current,
      timestamp: Date.now(),
    }, ...prev].slice(0, 100));
  };

  const handleScan = async () => {
    echoCli(`i2c scan --bus ${bus}`);
    setScanning(true);
    setScanComplete(false);
    setDevices([]);

    const found: I2cDevice[] = [];
    const knownAddrs = Object.keys(KNOWN_DEVICES).map(Number);

    // Scan known addresses first, then sweep 0x03-0x77
    for (const addr of knownAddrs) {
      const start = performance.now();
      try {
        const res = await api.readI2c(bus, formatHex(addr, 2));
        const elapsed = performance.now() - start;
        const resData = res.data ?? [];
        const isPic = isPicAddress(addr);
        found.push({
          addr,
          addrHex: formatHex(addr, 2),
          device: KNOWN_DEVICES[addr] || 'Unknown',
          status: resData.length > 0 ? 'found' : 'nack',
          data: resData.length > 0 ? resData : undefined,
          picDecode: isPic ? decodePicResponse(addr, resData) : undefined,
          responseMs: elapsed,
        });
        addTransaction({
          bus, addr: formatHex(addr, 2), direction: 'read',
          data: resData, responseMs: elapsed,
        });
      } catch {
        const elapsed = performance.now() - start;
        found.push({
          addr,
          addrHex: formatHex(addr, 2),
          device: KNOWN_DEVICES[addr] || 'Unknown',
          status: 'nack',
          responseMs: elapsed,
          picDecode: isPicAddress(addr) ? decodePicResponse(addr, [0x00]) : undefined,
        });
      }
    }

    // Sweep remaining addresses
    for (let addr = 0x03; addr <= 0x77; addr++) {
      if (knownAddrs.includes(addr)) continue;
      const start = performance.now();
      try {
        const res = await api.readI2c(bus, formatHex(addr, 2));
        const elapsed = performance.now() - start;
        const resData = res.data ?? [];
        if (resData.length > 0) {
          found.push({
            addr,
            addrHex: formatHex(addr, 2),
            device: 'Unknown',
            status: 'found',
            data: resData,
            responseMs: elapsed,
          });
          addTransaction({
            bus, addr: formatHex(addr, 2), direction: 'read',
            data: resData, responseMs: elapsed,
          });
        }
      } catch {
        // NACK -- no device at this address
      }
    }

    setDevices(found);
    setScanning(false);
    setScanComplete(true);
  };

  // Auto-refresh PIC status
  useEffect(() => {
    if (autoRefresh) {
      autoRefreshRef.current = setInterval(async () => {
        const picAddrs = [0x55, 0x56, 0x57];
        const updated = [...devices];
        for (const addr of picAddrs) {
          const start = performance.now();
          try {
            const res = await api.readI2c(bus, formatHex(addr, 2));
            const elapsed = performance.now() - start;
            const resData = res.data ?? [];
            const idx = updated.findIndex(d => d.addr === addr);
            if (idx >= 0) {
              updated[idx] = {
                ...updated[idx],
                data: resData.length > 0 ? resData : undefined,
                status: resData.length > 0 ? 'found' : 'nack',
                picDecode: decodePicResponse(addr, resData),
                responseMs: elapsed,
              };
            }
          } catch {
            // skip
          }
        }
        setDevices(updated);
      }, 3000);
    }
    return () => {
      if (autoRefreshRef.current) clearInterval(autoRefreshRef.current);
    };
  }, [autoRefresh, bus, devices]);

  const handleRead = async () => {
    setReadError('');
    setReadResult(null);
    const start = performance.now();
    try {
      echoCli(`i2c read ${readAddr}${readReg ? ` ${readReg}` : ''} --bus ${bus}`);
      const res = await api.readI2c(bus, readAddr, readReg || undefined);
      const elapsed = performance.now() - start;
      const resData = res.data ?? [];
      setReadResult(resData);
      addTransaction({
        bus, addr: readAddr, direction: 'read',
        data: resData, responseMs: elapsed,
      });
    } catch (e: unknown) {
      const elapsed = performance.now() - start;
      const msg = e instanceof Error ? e.message : 'Read failed';
      setReadError(msg);
      addTransaction({
        bus, addr: readAddr, direction: 'read',
        data: [], responseMs: elapsed, error: msg,
      });
    }
  };

  const handleWrite = async () => {
    setWriteError('');
    setWriteSuccess('');
    if (isProxyMode) {
      setWriteError('Blocked: bosminer owns I2C in proxy/hybrid mode.');
      return;
    }
    const start = performance.now();
    try {
      const data = writeData.split(/[\s,]+/).map(s => parseInt(s, 16)).filter(n => !isNaN(n));
      echoCli(`i2c write ${writeAddr} ${data.map(b => '0x' + b.toString(16).padStart(2, '0')).join(' ')} --bus ${bus}`);
      await api.writeI2c({ bus, addr: writeAddr, data, confirm: true });
      const elapsed = performance.now() - start;
      setWriteSuccess(`Wrote ${data.length} bytes to ${writeAddr}`);
      addTransaction({
        bus, addr: writeAddr, direction: 'write',
        data, responseMs: elapsed,
      });
    } catch (e: unknown) {
      const elapsed = performance.now() - start;
      const msg = e instanceof Error ? e.message : 'Write failed';
      setWriteError(msg);
      addTransaction({
        bus, addr: writeAddr, direction: 'write',
        data: [], responseMs: elapsed, error: msg,
      });
    }
  };

  const formatTime = (ts: number) => new Date(ts).toTimeString().split(' ')[0];

  const [showHelp, setShowHelp] = useState(false);

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// i2c scanner</div>
          <h2 className="hacker-inspector-title">I²C Bus Scanner</h2>
        </div>
        <div className="hacker-inspector-actions adv-help-anchor">
          <span className={`hacker-inspector-status ${autoRefresh ? '' : 'neutral'}`}>
            {scanComplete ? `${devices.filter(d => d.status === 'found').length} ACK` : autoRefresh ? 'AUTO' : 'IDLE'}
          </span>
          <button
            type="button"
            className="hacker-inspector-help"
            onClick={() => setShowHelp(v => !v)}
            aria-expanded={showHelp}
            aria-label="Show I2C scanner context help"
            title="Context help"
          >
            ?
          </button>
          <button
            type="button"
            className="hacker-inspector-refresh"
            onClick={() => setAutoRefresh(!autoRefresh)}
            aria-pressed={autoRefresh}
            title="Auto-refresh PIC addresses"
          >
            {autoRefresh ? '⏸ AUTO' : '⟳ AUTO'}
          </button>
          {showHelp && (
            <div
              role="dialog"
              aria-label="I2C scanner help"
              className="adv-help-pop adv-help-pop-wide"
            >
              <div className="adv-help-pop-title">
                I2C Bus Scanner — Quick Reference
              </div>
              <div className="adv-help-pop-sub adv-mb-4">Bus numbering:</div>
              <div>Bus 0 — FPGA AXI IIC (S9 PICs, control board sensors).</div>
              <div>Bus 1 — Secondary I2C bus (platform-dependent).</div>
              <div className="adv-help-pop-sub adv-mt-8">PIC addresses (S9 / BM1387):</div>
              <div>0x55 — Chain 6 voltage controller (PIC16F1704)</div>
              <div>0x56 — Chain 7 voltage controller</div>
              <div>0x57 — Chain 8 voltage controller</div>
              <div className="adv-help-pop-sub adv-mt-8">Address conventions:</div>
              <div>Addresses are <strong>7-bit</strong> (0x03–0x77 scannable range).</div>
              <div>Linux `i2cdetect` 8-bit form (e.g. 0xAA) = (7-bit &lt;&lt; 1) | R/W bit.</div>
              <div>Raw byte 0x60 = PIC APP mode. 0xCC = PIC bootloader. 0xFF = bus stuck.</div>
              <div className="adv-help-pop-warn">
                Hash-board temp sensors (0x98/0x9A/0x9C) are not directly scannable — access via BM1387 register 0x20.
              </div>
            </div>
          )}
        </div>
      </header>

      <div className="hacker-inspector-body">
      {/* Scan controls */}
      <div className="register-inspector sd-block">
        <div className="adv-row is-tight is-center" style={{ marginBottom: 16 }}>
          <div>
            <label htmlFor="i2c-scanner-bus" className="advanced-control-label">
              I2C Bus
              <InfoDot term="i2c_address" />
            </label>
            <select id="i2c-scanner-bus" value={bus} onChange={e => setBus(Number(e.target.value))}>
              <option value={0}>Bus 0 (FPGA AXI IIC)</option>
              <option value={1}>Bus 1</option>
            </select>
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
            <ActionButton
              label={scanning ? 'Scanning...' : 'Scan Bus'}
              onClick={handleScan}
              disabled={scanning}
            />
            <CliHint cmd={`i2c scan --bus ${bus}`} />
          </div>
          {scanning && (
            <span className="adv-hint" style={{ fontSize: '0.8rem' }}>
              Scanning 0x03-0x77...
            </span>
          )}
        </div>

        {/* Scan results table */}
        {scanComplete && (
          <div className="table-wrap">
            <table className="i2c-table">
              <thead>
                <tr>
                  <th scope="col">Address</th>
                  <th scope="col">Device</th>
                  <th scope="col">Status</th>
                  <th scope="col">Data</th>
                  <th scope="col">PIC Decode</th>
                  <th scope="col" className="i2c-th-right">Response</th>
                </tr>
              </thead>
              <tbody>
                {devices.map(dev => (
                  <tr key={dev.addr}>
                    <td className="i2c-td-mono i2c-td-accent">
                      {dev.addrHex}
                    </td>
                    <td>{dev.device}</td>
                    <td>
                      <span
                        className="i2c-status"
                        style={{
                          color: dev.status === 'found' ? 'var(--green)' : dev.status === 'nack' ? 'var(--yellow)' : 'var(--red)',
                        }}
                      >
                        {dev.status.toUpperCase()}
                      </span>
                    </td>
                    <td className="i2c-td-mono i2c-td-sm">
                      {dev.data ? dev.data.map(b => b.toString(16).padStart(2, '0')).join(' ') : '---'}
                    </td>
                    <td
                      className="i2c-td-sm"
                      style={{
                        color: dev.picDecode?.includes('DEAD') ? 'var(--red)'
                          : dev.picDecode?.includes('BOOTLOADER') ? 'var(--yellow)'
                          : dev.picDecode?.includes('APP MODE') ? 'var(--green)'
                          : 'var(--text-dim)',
                      }}
                    >
                      {dev.picDecode || '---'}
                    </td>
                    <td
                      className="i2c-td-mono i2c-td-sm i2c-td-right"
                      style={{ color: (dev.responseMs ?? 0) > 100 ? 'var(--red)' : 'var(--text-dim)' }}
                    >
                      {dev.responseMs !== undefined ? `${dev.responseMs.toFixed(1)}ms` : '---'}
                    </td>
                  </tr>
                ))}
                {/* Hash board temp sensors accessible via ASIC I2C passthrough */}
                {devices.length > 0 && PASSTHROUGH_SENSORS.map(sensor => (
                  <tr key={sensor.addr} className="i2c-row-passthrough">
                    <td className="i2c-td-mono i2c-td-dim">
                      {sensor.addrHex}
                    </td>
                    <td className="i2c-td-dim i2c-td-sm">{sensor.device}</td>
                    <td>
                      <span className="i2c-status i2c-td-dim i2c-td-sm">N/A</span>
                    </td>
                    <td className="i2c-td-mono i2c-td-sm i2c-td-dim">---</td>
                    <td className="i2c-td-sm i2c-td-dim">Requires BM1387 reg 0x20</td>
                    <td className="i2c-td-right i2c-td-dim">---</td>
                  </tr>
                ))}
                {devices.length === 0 && (
                  <tr>
                    <td colSpan={6} className="i2c-td-empty">
                      No devices found on bus {bus}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <div className="adv-grid-2">
        {/* Read */}
        <div className="register-inspector">
          <div className="adv-card-title">Read Device</div>
          <div className="adv-row">
            <div>
              <label htmlFor="i2c-read-addr" className="advanced-control-label">
                Address (hex)
              </label>
              <input id="i2c-read-addr" type="text" value={readAddr} onChange={e => setReadAddr(e.target.value)} className="adv-in-100" />
            </div>
            <div>
              <label htmlFor="i2c-read-reg" className="advanced-control-label">
                Register (hex, optional)
              </label>
              <input id="i2c-read-reg" type="text" value={readReg} onChange={e => setReadReg(e.target.value)} className="adv-in-100" placeholder="e.g. 0x00" />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
              <ActionButton label="Read" onClick={handleRead} />
              <CliHint cmd={`i2c read ${readAddr}${readReg ? ` ${readReg}` : ''} --bus ${bus}`} />
            </div>
          </div>
          {readError && <div className="adv-msg is-error">{readError}</div>}
          {readResult && (
            <div>
              <div className="table-wrap">
                <div className="hex-dump">
                  {readResult.map(b => b.toString(16).padStart(2, '0')).join(' ')}
                </div>
              </div>
              {/* PIC decode for known PIC addresses */}
              {isPicAddress(parseInt(readAddr, 16)) && readResult.length > 0 && (
                <div className="i2c-pic-decode">
                  PIC: {decodePicResponse(parseInt(readAddr, 16), readResult)}
                </div>
              )}
            </div>
          )}
        </div>

        {/* Write */}
        <div className="register-inspector">
          <div className="adv-card-title is-danger">Write Device</div>
          {isProxyMode && (
            <div className="adv-msg is-warn adv-mb-12" style={{ fontSize: '0.78rem' }}>
              Raw I2C writes disabled: bosminer owns hardware in proxy/hybrid mode.
            </div>
          )}
          <div className="adv-row">
            <div>
              <label htmlFor="i2c-write-addr" className="advanced-control-label">
                Address (hex)
              </label>
              <input id="i2c-write-addr" type="text" value={writeAddr} onChange={e => setWriteAddr(e.target.value)} className="adv-in-100" />
            </div>
            <div style={{ flex: 1 }}>
              <label htmlFor="i2c-write-data" className="advanced-control-label">
                Data (hex bytes, space-separated)
              </label>
              <input id="i2c-write-data" type="text" value={writeData} onChange={e => setWriteData(e.target.value)} placeholder="55 AA 06" />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
              <ActionButton
                label="Write"
                onClick={handleWrite}
                variant="danger"
                disabled={isProxyMode}
                confirm={`Write data to I2C device at ${writeAddr}? This directly communicates with hardware.`}
              />
              <CliHint cmd={`i2c write ${writeAddr} ${writeData || '<bytes>'} --bus ${bus}`} />
            </div>
          </div>
          {writeError && <div className="adv-msg is-error">{writeError}</div>}
          {writeSuccess && <div className="adv-msg is-success">{writeSuccess}</div>}
        </div>
      </div>

      {/* Transaction log */}
      {transactionLog.length > 0 && (
        <div className="register-inspector adv-card-mt">
          <div className="adv-flex-between adv-mb-12">
            <div className="adv-card-title" style={{ marginBottom: 0 }}>
              Transaction Log ({transactionLog.length})
            </div>
            <button
              type="button"
              className="btn btn-secondary advanced-compact-button"
              onClick={() => setTransactionLog([])}
            >
              Clear
            </button>
          </div>
          <div className="i2c-txlog">
            {transactionLog.map(tx => (
              <div
                key={tx.id}
                className="i2c-tx-row"
                style={{ color: tx.error ? 'var(--red)' : tx.direction === 'write' ? 'var(--yellow)' : 'var(--accent)' }}
              >
                <span className="i2c-tx-ts">[{formatTime(tx.timestamp)}]</span>
                <span
                  className="i2c-tx-dir"
                  style={{ color: tx.direction === 'write' ? 'var(--yellow)' : 'var(--green)' }}
                >
                  {tx.direction === 'write' ? 'WR' : 'RD'}
                </span>
                <span className="i2c-tx-bus">bus{tx.bus}</span>
                <span className="i2c-tx-addr">{tx.addr}</span>
                <span className="i2c-tx-data">
                  {tx.error || (tx.data.length > 0
                    ? tx.data.map(b => b.toString(16).padStart(2, '0')).join(' ')
                    : '(empty)')}
                </span>
                <span
                  className="i2c-tx-ms"
                  style={{ color: tx.responseMs > 100 ? 'var(--red)' : 'var(--text-dim)' }}
                >
                  {tx.responseMs.toFixed(0)}ms
                </span>
              </div>
            ))}
          </div>
        </div>
      )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>bus {bus}</span>
          <span>{devices.filter(d => d.status === 'found').length} devices found</span>
          <span>{scanComplete ? 'scan complete' : 'no scan yet'}</span>
        </div>
      </footer>
    </div>
  );
}
