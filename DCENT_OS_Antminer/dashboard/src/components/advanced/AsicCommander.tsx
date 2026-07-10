import React, { useState } from 'react';
import { api } from '../../api/client';
import { ActionButton } from '../common/ActionButton';
import { ASIC_COMMANDS } from '../../utils/constants';
import { formatHex } from '../../utils/format';
import { useActiveHardware } from '../../hooks/useActiveHardware';
import { useSystemHealth } from '../common/proxy/SystemHealthContext';
import { CliHint } from './CliHint';
import { echoCli } from '../../hooks/useCliEcho';

// BM1387 register map (verified from live S9 probes + BraiinsOS source)
const BM1387_REGISTERS = [
  { addr: 0x00, name: 'ChipAddress', desc: 'Chip address assignment register' },
  { addr: 0x04, name: 'PLL_Param', desc: 'PLL parameter (frequency control) (unverified)' },
  { addr: 0x08, name: 'HashRate', desc: 'Hash rate counter' },
  { addr: 0x0C, name: 'PLL', desc: 'PLL register - sets chip clock frequency' },
  { addr: 0x10, name: 'HashCounting', desc: 'Hash counting / nonce counter (unverified)' },
  { addr: 0x14, name: 'MiscCtrl_14', desc: 'Misc control register 0x14 (unverified)' },
  { addr: 0x18, name: 'TicketMask', desc: 'Ticket mask for nonce filtering (difficulty)' },
  { addr: 0x1C, name: 'MiscCtrl', desc: 'Misc control: gate_block, baud, INV_CLKO' },
  { addr: 0x20, name: 'I2C_Cmd', desc: 'I2C command register (temp sensor passthrough)' },
  { addr: 0x24, name: 'I2C_Data', desc: 'I2C data register' },
  { addr: 0x28, name: 'OrderCtrl', desc: 'Nonce order control (unverified)' },
  { addr: 0x2C, name: 'FastUART', desc: 'Fast UART configuration (unverified)' },
  { addr: 0x30, name: 'Compat', desc: 'Compatibility register (unverified)' },
] as const;

// BM1387 PLL uses a proprietary lookup table, not a clean formula.
// Known register values mapped to frequencies from firmware BM1387_PLL_TABLE.
const BM1387_PLL_LOOKUP: Record<number, number> = {
  0x00680241: 200,
  0x00700241: 250,
  0x00600141: 300,
  0x00680141: 350,
  0x00700141: 400,
  0x00780141: 450,
  0x00400061: 500,
  0x00480061: 550,
  0x00500061: 600,
  0x00580061: 650,
  0x00700261: 650,  // alternate encoding
  0x00600061: 700,
  0x00680061: 750,
  0x00700061: 800,
};

function decodePLL(data: number[]): string {
  if (data.length < 4) return 'Insufficient data';
  const val = (data[0] << 24) | (data[1] << 16) | (data[2] << 8) | data[3];
  const hexStr = '0x' + (val >>> 0).toString(16).padStart(8, '0').toUpperCase();

  // Check lookup table first
  const knownFreq = BM1387_PLL_LOOKUP[val >>> 0];
  if (knownFreq !== undefined) {
    return `PLL=${hexStr} => ${knownFreq} MHz (verified)`;
  }

  // Attempt formula as rough estimate
  const fbdiv = (val >> 16) & 0xFF;
  const refdiv = (val >> 8) & 0x1F;
  const postdiv1 = (val >> 4) & 0x07;
  const postdiv2 = val & 0x07;
  const divisor = refdiv * postdiv1 * postdiv2;
  if (divisor > 0 && fbdiv > 0) {
    const estMHz = (25.0 * fbdiv) / divisor;
    return `PLL=${hexStr} => ~${estMHz.toFixed(0)} MHz (estimate, may be inaccurate). BM1387 PLL uses proprietary encoding.`;
  }

  return `PLL=${hexStr} (not in lookup table -- use register value for manual calculation)`;
}

function decodeMiscCtrl(data: number[]): string {
  if (data.length < 4) return 'Insufficient data';
  const val = (data[0] << 24) | (data[1] << 16) | (data[2] << 8) | data[3];
  const gateBlock = (val >> 15) & 1;
  const notSetBaud = (val >> 30) & 1;
  const invClock = (val >> 21) & 1;
  const baudDiv = (val >> 8) & 0x1F;
  const mmen = (val >> 7) & 1;
  return `gate_block=${gateBlock} not_set_baud=${notSetBaud} inv_clock=${invClock} baud_div=${baudDiv} mmen=${mmen}`;
}

function decodeTicketMask(data: number[]): string {
  if (data.length < 4) return 'Insufficient data';
  const val = (data[0] << 24) | (data[1] << 16) | (data[2] << 8) | data[3];
  return `Mask=${formatHex(val, 8)} (difficulty ~${Math.pow(2, 32) / (val || 1)})`;
}

function decodeResponse(regAddr: number, data: number[]): string | null {
  switch (regAddr) {
    case 0x0C: return decodePLL(data);
    case 0x1C: return decodeMiscCtrl(data);
    case 0x18: return decodeTicketMask(data);
    default: return null;
  }
}

interface CommandHistoryEntry {
  id: number;
  timestamp: number;
  chain: number;
  command: string;
  chip: number | null;
  register: string;
  regName: string;
  response: number[];
  decoded: string | null;
  error?: string;
}

export function AsicCommander() {
  const { activeChain, activeChip, setActiveChain, setActiveChip } = useActiveHardware();
  const { isProxyMode } = useSystemHealth();
  const [chain, setChain] = useState(activeChain);
  const [command, setCommand] = useState<string>(ASIC_COMMANDS[0]);
  const [chip, setChip] = useState<string>(activeChip !== null ? String(activeChip) : 'all');
  const [registerAddr, setRegisterAddr] = useState<string>('0x00');
  const [selectedBm1387, setSelectedBm1387] = useState<typeof BM1387_REGISTERS[number] | null>(BM1387_REGISTERS[0]);
  const [history, setHistory] = useState<CommandHistoryEntry[]>([]);
  const [lastResponse, setLastResponse] = useState<number[] | null>(null);
  const [lastDecoded, setLastDecoded] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [readingAll, setReadingAll] = useState(false);

  const counterRef = React.useRef(0);

  // Sync from context
  React.useEffect(() => {
    setChain(activeChain);
    if (activeChip !== null) setChip(String(activeChip));
  }, [activeChain, activeChip]);

  const handleRegisterSelect = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const addr = parseInt(e.target.value, 16);
    const reg = BM1387_REGISTERS.find(r => r.addr === addr);
    setSelectedBm1387(reg || null);
    setRegisterAddr(e.target.value);
  };

  // Read-only commands that don't need confirmation dialogs
  const READ_ONLY_COMMANDS = ['ReadReg', 'ChipID'];
  const isReadOnly = READ_ONLY_COMMANDS.includes(command);
  const commandBlocked = isProxyMode && !isReadOnly;

  const sendCommand = async (targetChain: number, targetChip: string, targetRegister: string) => {
    const chipNum = targetChip === 'all' ? undefined : Number(targetChip);
    const regAddr = parseInt(targetRegister, 16);
    const regEntry = BM1387_REGISTERS.find(r => r.addr === regAddr);

    const res = await api.sendAsicCommand({
      chain: targetChain,
      command,
      chip: chipNum,
      register: targetRegister,
      confirm: true,
    });

    const response = res.response ?? [];
    const decoded = decodeResponse(regAddr, response);
    return { res, response, decoded, regEntry };
  };

  const handleSend = async () => {
    setError('');
    setLastResponse(null);
    setLastDecoded(null);

    const chipNum = chip === 'all' ? undefined : Number(chip);
    if (commandBlocked) {
      setError('Blocked: bosminer owns ASIC hardware in proxy/hybrid mode.');
      return;
    }

    try {
      echoCli(`asic cmd ${command} ${chain}${chip !== 'all' ? `.${chip}` : ''} ${registerAddr}`);
      const { response, decoded, regEntry } = await sendCommand(chain, chip, registerAddr);

      setLastResponse(response);
      setLastDecoded(decoded);
      counterRef.current += 1;

      const entry: CommandHistoryEntry = {
        id: counterRef.current,
        timestamp: Date.now(),
        chain,
        command,
        chip: chipNum ?? null,
        register: registerAddr,
        regName: regEntry?.name || 'Unknown',
        response,
        decoded,
      };

      setHistory(prev => [entry, ...prev].slice(0, 50));
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : 'Command failed';
      setError(msg);

      counterRef.current += 1;
      const entry: CommandHistoryEntry = {
        id: counterRef.current,
        timestamp: Date.now(),
        chain,
        command,
        chip: chipNum ?? null,
        register: registerAddr,
        regName: selectedBm1387?.name || 'Unknown',
        response: [],
        decoded: null,
        error: msg,
      };
      setHistory(prev => [entry, ...prev].slice(0, 50));
    }
  };

  const handleReadAllChips = async () => {
    if (commandBlocked) {
      setError('Blocked: bosminer owns ASIC hardware in proxy/hybrid mode.');
      return;
    }
    setReadingAll(true);
    setError('');
    echoCli(`asic cmd ${command} ${chain}.* ${registerAddr}`, 'sweep all 63 chips');
    const results: CommandHistoryEntry[] = [];

    for (let i = 0; i < 63; i++) {
      try {
        const { response, decoded, regEntry } = await sendCommand(chain, String(i), registerAddr);
        counterRef.current += 1;
        results.push({
          id: counterRef.current,
          timestamp: Date.now(),
          chain,
          command,
          chip: i,
          register: registerAddr,
          regName: regEntry?.name || 'Unknown',
          response,
          decoded,
        });
      } catch (e: unknown) {
        counterRef.current += 1;
        results.push({
          id: counterRef.current,
          timestamp: Date.now(),
          chain,
          command,
          chip: i,
          register: registerAddr,
          regName: selectedBm1387?.name || 'Unknown',
          response: [],
          decoded: null,
          error: e instanceof Error ? e.message : 'Failed',
        });
      }
    }

    setHistory(prev => [...results.reverse(), ...prev].slice(0, 50));
    setReadingAll(false);
  };

  const formatTime = (ts: number) => new Date(ts).toTimeString().split(' ')[0];

  return (
    <div className="hacker-inspector">
      <header className="hacker-inspector-header">
        <div className="hacker-inspector-title-group">
          <div className="hacker-inspector-eyebrow">// asic commander</div>
          <h2 className="hacker-inspector-title">Raw ASIC Command Sender</h2>
        </div>
        <div className="hacker-inspector-actions">
          <span className={`hacker-inspector-status ${isProxyMode ? 'warning' : ''}`}>
            {isProxyMode ? 'READ ONLY' : `CHAIN ${chain}`}
          </span>
        </div>
      </header>

      <div className="hacker-inspector-body">
      <div className="register-inspector sd-block">
        {isProxyMode && (
          <div className="adv-msg is-warn adv-mb-12" style={{ fontSize: '0.78rem' }}>
            Raw ASIC writes disabled: only ReadReg and ChipID are available while bosminer owns hardware.
          </div>
        )}
        <div className="adv-row" style={{ marginBottom: 16 }}>
          <div>
            <label htmlFor="asic-commander-chain" className="advanced-control-label">
              Chain
            </label>
            <select id="asic-commander-chain" value={chain} onChange={e => { setChain(Number(e.target.value)); setActiveChain(Number(e.target.value)); }}>
              <option value={6}>Chain 6</option>
              <option value={7}>Chain 7</option>
              <option value={8}>Chain 8</option>
            </select>
          </div>

          <div>
            <label htmlFor="asic-commander-command" className="advanced-control-label">
              Command
            </label>
            <select id="asic-commander-command" value={command} onChange={e => setCommand(e.target.value)}>
              {ASIC_COMMANDS.map(cmd => (
                <option key={cmd} value={cmd}>{cmd}</option>
              ))}
            </select>
          </div>

          <div>
            <label htmlFor="asic-commander-chip" className="advanced-control-label">
              Chip (0-62)
            </label>
            <select id="asic-commander-chip" value={chip} onChange={e => {
              setChip(e.target.value);
              setActiveChip(e.target.value === 'all' ? null : Number(e.target.value));
            }}>
              <option value="all">All</option>
              {Array.from({ length: 63 }, (_, i) => (
                <option key={i} value={i}>Chip {i}</option>
              ))}
            </select>
          </div>

          <div>
            <label htmlFor="asic-commander-register-preset" className="advanced-control-label">
              BM1387 Register
            </label>
            <select
              id="asic-commander-register-preset"
              value={formatHex(parseInt(registerAddr, 16) || 0, 2)}
              onChange={handleRegisterSelect}
              className="ac-reg-select"
            >
              {BM1387_REGISTERS.map(reg => (
                <option key={reg.addr} value={formatHex(reg.addr, 2)}>
                  {formatHex(reg.addr, 2)} {reg.name}
                </option>
              ))}
              <option value={registerAddr}>Custom: {registerAddr}</option>
            </select>
          </div>

          <div>
            <label htmlFor="asic-commander-register-custom" className="advanced-control-label">
              Register (hex)
            </label>
            <input
              id="asic-commander-register-custom"
              type="text"
              value={registerAddr}
              onChange={e => setRegisterAddr(e.target.value)}
              className="adv-in-xs"
            />
          </div>

          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
            <ActionButton
              label="Send"
              onClick={handleSend}
              disabled={commandBlocked}
              confirm={isReadOnly ? undefined : `Send ${command} to chain ${chain}${chip !== 'all' ? ` chip ${chip}` : ' (all chips)'}? This sends raw commands to ASIC chips.`}
            />
            <CliHint cmd={`asic cmd ${command} ${chain}${chip !== 'all' ? `.${chip}` : ''} ${registerAddr}`} />
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
            <ActionButton
              label={readingAll ? 'Reading...' : 'Read All 63'}
              onClick={handleReadAllChips}
              variant="secondary"
              disabled={readingAll || commandBlocked}
            />
            <CliHint cmd={`asic cmd ${command} ${chain}.* ${registerAddr}`} note="sweep all 63 chips" />
          </div>
        </div>

        {/* Register description */}
        {selectedBm1387 && (
          <div className="ac-reg-desc">
            <span className="ac-reg-desc-name">{selectedBm1387.name}</span> ({formatHex(selectedBm1387.addr, 2)}): {selectedBm1387.desc}
          </div>
        )}

        {error && <div className="adv-msg is-error">{error}</div>}

        {/* Response display */}
        {lastResponse && lastResponse.length > 0 && (
          <div className="adv-mt-12">
            <div className="adv-hint adv-mb-8">Response:</div>
            <div className="table-wrap">
              <div className="hex-dump">
                {lastResponse.map(b => b.toString(16).padStart(2, '0')).join(' ')}
                {'\n'}
                {'ASCII: '}
                {lastResponse.map(b => (b >= 0x20 && b < 0x7F) ? String.fromCharCode(b) : '.').join('')}
              </div>
            </div>
            {/* Decoded value */}
            {lastDecoded && (
              <div className="ac-decoded">
                Decoded: {lastDecoded}
              </div>
            )}
          </div>
        )}
      </div>

      {/* Command History */}
      {history.length > 0 && (
        <div className="register-inspector">
          <div className="adv-flex-between adv-mb-12">
            <div className="adv-card-title" style={{ marginBottom: 0 }}>
              Command History ({history.length}/50)
            </div>
            <button
              type="button"
              className="btn btn-secondary advanced-compact-button"
              onClick={() => setHistory([])}
            >
              Clear
            </button>
          </div>
          <div className="table-wrap ac-history-scroll">
            <table className="ac-table">
              <thead>
                <tr className="ac-thead-sticky">
                  <th scope="col">Time</th>
                  <th scope="col">Ch</th>
                  <th scope="col">Cmd</th>
                  <th scope="col">Chip</th>
                  <th scope="col">Reg</th>
                  <th scope="col">Response</th>
                  <th scope="col">Decoded</th>
                </tr>
              </thead>
              <tbody>
                {history.map(entry => (
                  <tr key={entry.id}>
                    <td className="ac-td-mono ac-td-dim ac-td-nowrap">
                      {formatTime(entry.timestamp)}
                    </td>
                    <td>{entry.chain}</td>
                    <td className="ac-td-accent">{entry.command}</td>
                    <td>{entry.chip !== null ? entry.chip : 'All'}</td>
                    <td className="ac-td-yellow">
                      {entry.regName}
                    </td>
                    <td
                      className="ac-td-mono ac-td-clip ac-td-sm"
                      style={{ color: entry.error ? 'var(--red)' : 'var(--accent)' }}
                    >
                      {entry.error
                        ? entry.error
                        : entry.response.length > 0
                          ? entry.response.map(b => b.toString(16).padStart(2, '0')).join(' ')
                          : '(empty)'
                      }
                    </td>
                    <td className="ac-td-mono ac-td-clip ac-td-xs ac-td-green">
                      {entry.decoded || '---'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
      </div>

      <footer className="hacker-inspector-footer">
        <div className="hacker-inspector-stats">
          <span>chain {chain}</span>
          <span>{history.length} commands sent</span>
          <span>cmd: {command}</span>
        </div>
      </footer>
    </div>
  );
}
