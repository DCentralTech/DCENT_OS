/**
 * CLI-echo bus (design-handoff ).
 *
 * The design's Hacker-mode identity move: every GUI action shows its
 * `$ dcent …` equivalent, and invoking the GUI action echoes that command
 * into the live console — "the user learns the commands while they explore."
 *
 * This is a tiny, dependency-free singleton pub/sub so the pattern can be
 * wired without touching the shared Zustand store or threading a context
 * through ~30 advanced tools. `CliHint` emits on click; `Console` subscribes
 * and renders the echoed command as a `$ …` line. No telemetry, no
 * persistence, no contract surface — purely a UI learning aid.
 *
 * Truth-contract: the echoed string is the COMMAND EQUIVALENT of the action
 * the operator just took in the GUI. It is a label, not a claim that a CLI
 * round-trip occurred — the GUI handler still performs the real API call.
 */
export interface CliEchoEvent {
  /** The `dcent …` command equivalent (without the leading `$ `). */
  cmd: string;
  /** Optional one-line note appended after the command. */
  note?: string;
}

type Listener = (e: CliEchoEvent) => void;

const listeners = new Set<Listener>();

export const CliEchoBus = {
  subscribe(fn: Listener): () => void {
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  },
  emit(e: CliEchoEvent): void {
    listeners.forEach(fn => {
      try {
        fn(e);
      } catch {
        /* a broken listener must never break the GUI action */
      }
    });
  },
};

/** Convenience: emit a CLI-echo for a GUI action. Safe to call anywhere. */
export function echoCli(cmd: string, note?: string): void {
  if (cmd) CliEchoBus.emit({ cmd, note });
}
