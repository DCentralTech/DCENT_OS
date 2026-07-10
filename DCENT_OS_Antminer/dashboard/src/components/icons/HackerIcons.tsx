import React from 'react';

interface IconProps {
  size?: number;
  className?: string;
}

/** 2x2 grid of small rectangles (dashboard overview) */
export function IconOverview({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <rect x="1" y="1" width="6" height="6" rx="1" />
      <rect x="9" y="1" width="6" height="6" rx="1" />
      <rect x="1" y="9" width="6" height="6" rx="1" />
      <rect x="9" y="9" width="6" height="6" rx="1" />
    </svg>
  );
}

/** Angle bracket + underscore (>_ console prompt) */
export function IconTerminal({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="2,4 6,8 2,12" />
      <line x1="8" y1="12" x2="14" y2="12" />
    </svg>
  );
}

/** 3x3 dot grid (chip heatmap) */
export function IconChipMap({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="3" cy="3" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="8" cy="3" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="13" cy="3" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="3" cy="8" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="8" cy="8" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="13" cy="8" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="3" cy="13" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="8" cy="13" r="1.2" fill="currentColor" stroke="none" />
      <circle cx="13" cy="13" r="1.2" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Two arrows pointing opposite directions (bidirectional data) */
export function IconProtocol({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="2" y1="5" x2="14" y2="5" />
      <polyline points="10,2 14,5 10,8" />
      <line x1="14" y1="11" x2="2" y2="11" />
      <polyline points="6,8 2,11 6,14" />
    </svg>
  );
}

/** Chip outline with pins on sides */
export function IconFpga({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <rect x="4" y="4" width="8" height="8" rx="1" />
      {/* Left pins */}
      <line x1="1" y1="6" x2="4" y2="6" />
      <line x1="1" y1="10" x2="4" y2="10" />
      {/* Right pins */}
      <line x1="12" y1="6" x2="15" y2="6" />
      <line x1="12" y1="10" x2="15" y2="10" />
      {/* Top pins */}
      <line x1="6" y1="1" x2="6" y2="4" />
      <line x1="10" y1="1" x2="10" y2="4" />
      {/* Bottom pins */}
      <line x1="6" y1="12" x2="6" y2="15" />
      <line x1="10" y1="12" x2="10" y2="15" />
    </svg>
  );
}

/** Two parallel horizontal lines with connection dot (I2C bus) */
export function IconBus({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="1" y1="5" x2="15" y2="5" />
      <line x1="1" y1="11" x2="15" y2="11" />
      <circle cx="8" cy="8" r="2" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Diamond shape with inner cross (ASIC die) */
export function IconAsic({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polygon points="8,1 15,8 8,15 1,8" />
      <line x1="8" y1="4" x2="8" y2="12" />
      <line x1="4" y1="8" x2="12" y2="8" />
    </svg>
  );
}

/** Lightning bolt */
export function IconVoltage({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="9,1 4,9 8,9 7,15 12,7 8,7 9,1" />
    </svg>
  );
}

/** Curly braces { } */
export function IconApi({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M5,2 C3,2 2,3 2,5 L2,7 C2,8 1,8 1,8 C1,8 2,8 2,9 L2,11 C2,13 3,14 5,14" />
      <path d="M11,2 C13,2 14,3 14,5 L14,7 C14,8 15,8 15,8 C15,8 14,8 14,9 L14,11 C14,13 13,14 11,14" />
    </svg>
  );
}

/** Heartbeat/pulse line */
export function IconDiagnostics({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="1,8 4,8 6,3 8,13 10,6 12,10 14,8 15,8" />
    </svg>
  );
}

/** Flask/beaker */
export function IconExperiment({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M5,1 L5,5 L2,13 C1.5,14 2,15 3,15 L13,15 C14,15 14.5,14 14,13 L11,5 L11,1" />
      <line x1="4" y1="1" x2="12" y2="1" />
      <line x1="3" y1="10" x2="13" y2="10" />
    </svg>
  );
}

/** Wrench */
export function IconMaintenance({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="4" cy="4" r="3" />
      <line x1="6" y1="6" x2="13" y2="13" />
      <circle cx="14" cy="14" r="1" />
    </svg>
  );
}

/* ---- Additional icons for Standard/Network nav items ---- */

/** Water drop / pool icon */
export function IconPool({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M8,2 C8,2 3,7 3,10 C3,13 5.2,15 8,15 C10.8,15 13,13 13,10 C13,7 8,2 8,2Z" />
    </svg>
  );
}

/** Thermometer */
export function IconTemperature({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M6,10.5 L6,3 C6,1.9 6.9,1 8,1 C9.1,1 10,1.9 10,3 L10,10.5 C11.2,11.3 12,12.6 12,14 C12,14 10.2,15 8,15 C5.8,15 4,14 4,14 C4,12.6 4.8,11.3 6,10.5Z" />
      <line x1="8" y1="6" x2="8" y2="11" />
      <circle cx="8" cy="12.5" r="1.5" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Gear / cog */
export function IconTuning({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="2.5" />
      <path d="M8,1 L9,3.5 L11.5,2.5 L11,5 L13.5,5.5 L12,7.5 L15,8 L12,8.5 L13.5,10.5 L11,11 L11.5,13.5 L9,12.5 L8,15 L7,12.5 L4.5,13.5 L5,11 L2.5,10.5 L4,8.5 L1,8 L4,7.5 L2.5,5.5 L5,5 L4.5,2.5 L7,3.5Z" />
    </svg>
  );
}

/** Clock / scheduler */
export function IconScheduler({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="6.5" />
      <polyline points="8,4 8,8 11,10" />
    </svg>
  );
}

/** Leaf / green mining */
export function IconGreen({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M3,14 C3,14 3,8 8,4 C13,0 14,2 14,2 C14,2 15,6 10,10 C6,13 3,14 3,14Z" />
      <path d="M3,14 C5,11 7,9 10,7" />
    </svg>
  );
}

/** Zap / circuit check */
export function IconCircuit({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="1,8 4,8 6,4 8,12 10,6 12,10 15,8" />
      <circle cx="1" cy="8" r="0.8" fill="currentColor" stroke="none" />
      <circle cx="15" cy="8" r="0.8" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Lightning bolt with arrow (demand response) */
export function IconDemand({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="9,1 4,9 8,9 7,15 12,7 8,7 9,1" />
      <line x1="12" y1="12" x2="15" y2="15" />
      <polyline points="15,12 15,15 12,15" />
    </svg>
  );
}

/** Radar / fleet discovery */
export function IconFleet({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="2" />
      <path d="M8,1 A7,7 0 0,1 15,8" />
      <path d="M8,3.5 A4.5,4.5 0 0,1 12.5,8" />
      <circle cx="8" cy="8" r="6.5" strokeDasharray="2 2" />
    </svg>
  );
}

/** House with antenna (MQTT / Home Assistant) */
export function IconMqtt({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <polyline points="1,8 8,2 15,8" />
      <polyline points="3,9 3,15 13,15 13,9" />
      <line x1="12" y1="2" x2="12" y2="5" />
    </svg>
  );
}

/** Download arrow / data export */
export function IconExport({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="8" y1="2" x2="8" y2="11" />
      <polyline points="4,8 8,12 12,8" />
      <line x1="2" y1="14" x2="14" y2="14" />
    </svg>
  );
}

/** Stacked layers / profiles */
export function IconProfiles({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="2" y1="4" x2="14" y2="4" />
      <line x1="2" y1="8" x2="14" y2="8" />
      <line x1="2" y1="12" x2="14" y2="12" />
    </svg>
  );
}

/** Sliders / settings */
export function IconSettings({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="2" y1="4" x2="14" y2="4" />
      <circle cx="10" cy="4" r="1.5" fill="currentColor" />
      <line x1="2" y1="8" x2="14" y2="8" />
      <circle cx="5" cy="8" r="1.5" fill="currentColor" />
      <line x1="2" y1="12" x2="14" y2="12" />
      <circle cx="11" cy="12" r="1.5" fill="currentColor" />
    </svg>
  );
}

/** Bitcoin symbol */
export function IconEarnings({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <path d="M5,3 L9,3 C11,3 12,4 12,5.5 C12,7 11,7.5 11,7.5" />
      <path d="M5,8 L10,8 C12,8 13,9 13,10.5 C13,12 11,13 9,13 L5,13" />
      <line x1="5" y1="3" x2="5" y2="13" />
      <line x1="7" y1="1" x2="7" y2="3" />
      <line x1="9" y1="1" x2="9" y2="3" />
      <line x1="7" y1="13" x2="7" y2="15" />
      <line x1="9" y1="13" x2="9" y2="15" />
    </svg>
  );
}

/** Bar chart / shares */
export function IconShares({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <rect x="1" y="9" width="3" height="6" rx="0.5" />
      <rect x="5.5" y="5" width="3" height="10" rx="0.5" />
      <rect x="10" y="1" width="3" height="14" rx="0.5" />
    </svg>
  );
}

/** Info circle */
export function IconAbout({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="6.5" />
      <line x1="8" y1="7" x2="8" y2="12" />
      <circle cx="8" cy="4.5" r="0.5" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Lock / SV2 security */
export function IconLock({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <rect x="3" y="7" width="10" height="8" rx="1.5" />
      <path d="M5,7 L5,5 C5,2.8 6.3,1 8,1 C9.7,1 11,2.8 11,5 L11,7" />
      <circle cx="8" cy="11" r="1" fill="currentColor" stroke="none" />
    </svg>
  );
}

/** Pickaxe / job declaration */
export function IconPickaxe({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <line x1="3" y1="13" x2="10" y2="6" />
      <path d="M10,6 L8,2 L14,4 L12,6 L14,8 L10,6Z" />
    </svg>
  );
}

/** PID controller knob */
export function IconPid({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <circle cx="8" cy="8" r="5" />
      <line x1="8" y1="8" x2="8" y2="4" />
      <line x1="8" y1="13" x2="8" y2="15" />
      <line x1="3" y1="8" x2="1" y2="8" />
      <line x1="15" y1="8" x2="13" y2="8" />
    </svg>
  );
}

/** Bug / debug */
export function IconDebug({ size = 16, className }: IconProps) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none"
         stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"
         strokeLinejoin="round" className={className}>
      <ellipse cx="8" cy="10" rx="4" ry="5" />
      <line x1="1" y1="7" x2="4" y2="8" />
      <line x1="15" y1="7" x2="12" y2="8" />
      <line x1="1" y1="12" x2="4" y2="11" />
      <line x1="15" y1="12" x2="12" y2="11" />
      <circle cx="6.5" cy="8" r="0.8" fill="currentColor" stroke="none" />
      <circle cx="9.5" cy="8" r="0.8" fill="currentColor" stroke="none" />
    </svg>
  );
}
