import React, { useState, useCallback, useRef, useEffect } from 'react';
import { useMinerStore } from '../../store/miner';
import { FAN_CURVE_PRESETS, type FanCurvePoint } from '../../utils/constants';

const SVG_W = 480;
const SVG_H = 280;
const PAD_L = 50;
const PAD_R = 20;
const PAD_T = 20;
const PAD_B = 40;
const PLOT_W = SVG_W - PAD_L - PAD_R;
const PLOT_H = SVG_H - PAD_T - PAD_B;

// Scale: temp range 20-80C, home fan cap 0-30 PWM.
const TEMP_MIN = 20;
const TEMP_MAX = 80;
const PWM_MIN = 0;
const PWM_MAX = 30;

function tempToX(temp: number): number {
  return PAD_L + ((temp - TEMP_MIN) / (TEMP_MAX - TEMP_MIN)) * PLOT_W;
}

function pwmToY(pwm: number): number {
  return PAD_T + PLOT_H - ((pwm - PWM_MIN) / (PWM_MAX - PWM_MIN)) * PLOT_H;
}

function xToTemp(x: number): number {
  return TEMP_MIN + ((x - PAD_L) / PLOT_W) * (TEMP_MAX - TEMP_MIN);
}

function yToPwm(y: number): number {
  return PWM_MAX - ((y - PAD_T) / PLOT_H) * (PWM_MAX - PWM_MIN);
}

export function FanCurveEditor() {
  const fans = useMinerStore(s => s.status?.fans);
  const chains = useMinerStore(s => s.status?.chains ?? []);
  const addAlert = useMinerStore(s => s.addAlert);

  const [points, setPoints] = useState<FanCurvePoint[]>(() => {
    try {
      const saved = localStorage.getItem('dcentos-fan-curve');
      if (saved) {
        const parsed = JSON.parse(saved);
        if (Array.isArray(parsed) && parsed.length >= 2) {
          return parsed.map((p: FanCurvePoint) => ({
            temp: p.temp,
            pwm: Math.max(PWM_MIN, Math.min(PWM_MAX, Number(p.pwm) || 0)),
          }));
        }
      }
    } catch { /* ignore parse errors */ }
    return [...FAN_CURVE_PRESETS.balanced];
  });
  const [dragging, setDragging] = useState<number | null>(null);
  const [activePreset, setActivePreset] = useState<string>(() => {
    try {
      const saved = localStorage.getItem('dcentos-fan-curve');
      if (saved) return ''; // custom saved curve, no preset active
    } catch { /* ignore */ }
    return 'balanced';
  });
  const svgRef = useRef<SVGSVGElement>(null);

  const pwm = fans?.pwm ?? 0;
  const pwmPct = Math.round(pwm);

  // Current operating temperature (average across chains)
  const chainTemps = chains.filter(c => c.temp_c > 0).map(c => c.temp_c);
  const currentTemp = chainTemps.length > 0
    ? chainTemps.reduce((s, t) => s + t, 0) / chainTemps.length
    : 0;

  // Build SVG path from points
  const sortedPoints = [...points].sort((a, b) => a.temp - b.temp);
  const pathD = sortedPoints.map((p, i) => {
    const x = tempToX(p.temp);
    const y = pwmToY(p.pwm);
    return `${i === 0 ? 'M' : 'L'} ${x} ${y}`;
  }).join(' ');

  // Fill path (area under curve)
  const fillD = pathD
    + ` L ${tempToX(sortedPoints[sortedPoints.length - 1]?.temp ?? TEMP_MAX)} ${pwmToY(0)}`
    + ` L ${tempToX(sortedPoints[0]?.temp ?? TEMP_MIN)} ${pwmToY(0)} Z`;

  const handlePreset = (name: string) => {
    const preset = FAN_CURVE_PRESETS[name];
    if (preset) {
      setPoints([...preset]);
      setActivePreset(name);
    }
  };

  const getSvgCoords = useCallback((e: React.MouseEvent | MouseEvent): { x: number; y: number } => {
    if (!svgRef.current) return { x: 0, y: 0 };
    const rect = svgRef.current.getBoundingClientRect();
    const scaleX = SVG_W / rect.width;
    const scaleY = SVG_H / rect.height;
    return {
      x: (e.clientX - rect.left) * scaleX,
      y: (e.clientY - rect.top) * scaleY,
    };
  }, []);

  const handleMouseDown = useCallback((idx: number) => (e: React.MouseEvent) => {
    e.preventDefault();
    setDragging(idx);
    setActivePreset('');
  }, []);

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    if (dragging === null) return;
    const { x, y } = getSvgCoords(e);
    const newTemp = Math.round(Math.max(TEMP_MIN, Math.min(TEMP_MAX, xToTemp(x))));
    const newPwm = Math.round(Math.max(PWM_MIN, Math.min(PWM_MAX, yToPwm(y))));
    setPoints(prev => prev.map((p, i) =>
      i === dragging ? { temp: newTemp, pwm: newPwm } : p
    ));
  }, [dragging, getSvgCoords]);

  const handleMouseUp = useCallback(() => {
    setDragging(null);
  }, []);

  // Add global mouse up listener for drag end
  useEffect(() => {
    if (dragging !== null) {
      const up = () => setDragging(null);
      window.addEventListener('mouseup', up);
      return () => window.removeEventListener('mouseup', up);
    }
  }, [dragging]);

  const addPoint = useCallback((e: React.MouseEvent) => {
    if (dragging !== null) return;
    // Only add if clicking on the plot area (not on existing points)
    const { x, y } = getSvgCoords(e);
    if (x < PAD_L || x > SVG_W - PAD_R || y < PAD_T || y > SVG_H - PAD_B) return;
    const newTemp = Math.round(xToTemp(x));
    const newPwm = Math.round(yToPwm(y));
    setPoints(prev => [...prev, { temp: newTemp, pwm: newPwm }]);
    setActivePreset('');
  }, [dragging, getSvgCoords]);

  const removePoint = useCallback((idx: number) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (points.length > 2) {
      setPoints(prev => prev.filter((_, i) => i !== idx));
      setActivePreset('');
    }
  }, [points.length]);

  const applyCurve = async () => {
    try {
      // Persist to localStorage so curve survives navigation and page reloads
      localStorage.setItem('dcentos-fan-curve', JSON.stringify(sortedPoints));
      addAlert('info', `Fan curve saved as draft in browser (${sortedPoints.length} points). Connect to a miner to apply.`);
    } catch {
      addAlert('warning', 'Failed to save fan curve');
    }
  };

  // Grid lines
  const tempGridLines = [20, 30, 40, 50, 60, 70, 80];
  const pwmGridLines = [0, 10, 20, 30];

  return (
    <div style={{
      background: 'var(--card-bg)',
      borderRadius: 'var(--radius)',
      padding: 16,
      border: '1px solid var(--border)',
    }}>
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        alignItems: 'center', marginBottom: 12,
      }}>
        <div style={{
          fontFamily: "var(--font-heading)",
          fontWeight: 700, fontSize: '1rem',
          color: 'var(--text)',
        }}>
          Fan Curve Editor
        </div>
        <div style={{ display: 'flex', gap: 6 }}>
          {Object.keys(FAN_CURVE_PRESETS).map(name => (
            <button
              key={name}
              className={`time-tab ${activePreset === name ? 'active' : ''}`}
              onClick={() => handlePreset(name)}
              style={{ fontSize: '0.7rem', padding: '3px 10px', textTransform: 'capitalize' }}
            >
              {name}
            </button>
          ))}
        </div>
      </div>

      {/* SVG canvas */}
      <svg
        ref={svgRef}
        viewBox={`0 0 ${SVG_W} ${SVG_H}`}
        style={{
          width: '100%',
          height: 'auto',
          cursor: dragging !== null ? 'grabbing' : 'crosshair',
          userSelect: 'none',
        }}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onDoubleClick={addPoint}
      >
        {/* Plot background */}
        <rect x={PAD_L} y={PAD_T} width={PLOT_W} height={PLOT_H} fill="#1a1a22" rx="4" />

        {/* Grid lines */}
        {tempGridLines.map(t => (
          <g key={`tg-${t}`}>
            <line
              x1={tempToX(t)} y1={PAD_T}
              x2={tempToX(t)} y2={PAD_T + PLOT_H}
              stroke="rgba(51,51,51,0.6)" strokeWidth="1"
            />
            <text
              x={tempToX(t)} y={SVG_H - 10}
              fill="#6B7280" fontSize="10" textAnchor="middle"
              fontFamily="'JetBrains Mono', monospace"
            >
              {t}C
            </text>
          </g>
        ))}
        {pwmGridLines.map(p => (
          <g key={`pg-${p}`}>
            <line
              x1={PAD_L} y1={pwmToY(p)}
              x2={PAD_L + PLOT_W} y2={pwmToY(p)}
              stroke="rgba(51,51,51,0.6)" strokeWidth="1"
            />
            <text
              x={PAD_L - 8} y={pwmToY(p) + 4}
              fill="#6B7280" fontSize="10" textAnchor="end"
              fontFamily="'JetBrains Mono', monospace"
            >
              {p}
            </text>
          </g>
        ))}

        {/* Axis labels */}
        <text
          x={PAD_L + PLOT_W / 2} y={SVG_H - 2}
          fill="#9CA3AF" fontSize="11" textAnchor="middle"
          fontFamily="'JetBrains Mono', monospace"
        >
          Temperature (C)
        </text>
        <text
          x={12} y={PAD_T + PLOT_H / 2}
          fill="#9CA3AF" fontSize="11" textAnchor="middle"
          fontFamily="'JetBrains Mono', monospace"
          transform={`rotate(-90 12 ${PAD_T + PLOT_H / 2})`}
        >
          Fan PWM (0–30 cap)
        </text>

        {/* Thermal threshold zones */}
        {/* Hot zone (65+) */}
        <rect
          x={tempToX(65)} y={PAD_T}
          width={tempToX(80) - tempToX(65)} height={PLOT_H}
          fill="rgba(239, 68, 68, 0.06)"
        />
        {/* Warm zone (55-65) */}
        <rect
          x={tempToX(55)} y={PAD_T}
          width={tempToX(65) - tempToX(55)} height={PLOT_H}
          fill="rgba(234, 179, 8, 0.04)"
        />

        {/* Fill area under curve */}
        <path d={fillD} fill="rgba(247, 147, 26, 0.08)" />

        {/* Curve line */}
        <path d={pathD} fill="none" stroke="var(--accent)" strokeWidth="2.5" strokeLinejoin="round" />

        {/* Current operating point */}
        {currentTemp > 0 && (
          <>
            {/* Crosshair */}
            <line
              x1={tempToX(currentTemp)} y1={PAD_T}
              x2={tempToX(currentTemp)} y2={PAD_T + PLOT_H}
              stroke="var(--green)" strokeWidth="1" strokeDasharray="4 4" opacity="0.6"
            />
            <line
              x1={PAD_L} y1={pwmToY(pwmPct)}
              x2={PAD_L + PLOT_W} y2={pwmToY(pwmPct)}
              stroke="var(--green)" strokeWidth="1" strokeDasharray="4 4" opacity="0.6"
            />
            {/* Current point */}
            <circle
              cx={tempToX(currentTemp)}
              cy={pwmToY(pwmPct)}
              r="6"
              fill="var(--green)"
              stroke="#1a1a22"
              strokeWidth="2"
            />
            <text
              x={Math.min(tempToX(currentTemp) + 10, SVG_W - 70)}
              y={pwmToY(pwmPct) - 8}
              fill="var(--green)"
              fontSize="10"
              fontFamily="'JetBrains Mono', monospace"
            >
              NOW: {currentTemp.toFixed(0)}C / {pwmPct}%
            </text>
          </>
        )}

        {/* Draggable control points */}
        {sortedPoints.map((p, i) => {
          const origIdx = points.indexOf(p);
          return (
            <g key={i}>
              <circle
                cx={tempToX(p.temp)}
                cy={pwmToY(p.pwm)}
                r={dragging === origIdx ? 8 : 6}
                fill="var(--accent)"
                stroke="#1a1a22"
                strokeWidth="2"
                cursor="grab"
                onMouseDown={handleMouseDown(origIdx)}
                onContextMenu={removePoint(origIdx)}
                style={{ transition: dragging === origIdx ? 'none' : 'r 0.1s' }}
              />
              {/* Point label */}
              <text
                x={tempToX(p.temp)}
                y={pwmToY(p.pwm) - 12}
                fill="var(--text-dim)"
                fontSize="9"
                textAnchor="middle"
                fontFamily="'JetBrains Mono', monospace"
                pointerEvents="none"
              >
                {p.temp}C/{p.pwm}%
              </text>
            </g>
          );
        })}
      </svg>

      {/* Instructions + apply */}
      <div style={{
        display: 'flex', justifyContent: 'space-between',
        alignItems: 'center', marginTop: 8,
      }}>
        <div style={{ fontSize: '0.7rem', color: 'var(--text-dim)' }}>
          Drag points to adjust. Double-click to add. Right-click to remove.
          {currentTemp > 0 && (
            <span style={{ color: 'var(--green)', marginLeft: 8 }}>
              Current: {currentTemp.toFixed(1)}C / PWM {pwm} ({pwmPct}%)
            </span>
          )}
        </div>
        <button
          className="btn btn-secondary"
          onClick={applyCurve}
          style={{ padding: '6px 16px', fontSize: '0.8rem' }}
        >
          Save Draft
        </button>
      </div>
    </div>
  );
}
