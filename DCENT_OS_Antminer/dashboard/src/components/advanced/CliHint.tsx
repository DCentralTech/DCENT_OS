import React from 'react';
import { echoCli } from '../../hooks/useCliEcho';

/**
 * `$ dcent …` command-equivalent caption shown under a Hacker-mode GUI
 * action (design-handoff ). Clicking the caption copies the command
 * and echoes it into the console — so the operator can learn/replay the CLI
 * even without invoking the GUI action. The owning button's handler also
 * calls `echoCli(cmd)` so doing the action mirrors it too.
 *
 * Presentational + self-contained: no shared store/context. Safe to drop
 * under any control without touching that tool's contract.
 */
export function CliHint({ cmd, note }: { cmd: string; note?: string }) {
  if (!cmd) return null;

  const onClick = () => {
    try {
      void navigator.clipboard?.writeText(`dcent ${cmd}`);
    } catch {
      /* clipboard optional — echo still fires */
    }
    echoCli(cmd, note);
  };

  return (
    <button
      type="button"
      className="hacker-cli-hint"
      onClick={onClick}
      title="Click to copy & echo this command into the console"
      aria-label={`Command equivalent: dcent ${cmd}. Click to copy and echo into the console.`}
    >
      <span className="hacker-cli-hint-sigil" aria-hidden="true">$</span>
      <span className="hacker-cli-hint-cmd">dcent {cmd}</span>
    </button>
  );
}
