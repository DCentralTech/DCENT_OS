// CSS-drop guard (FEUX-3, beta-supremacy ).
//
// Runs `vite build` and FAILS the build if Vite/esbuild emits a
// `css-syntax-error` — the precise signal that a CSS rule was silently DROPPED
// by the esbuild "comment containing a brace/backtick" mis-lex bug (the same
// class that made the S4 light-theme toggle a no-op). esbuild only prints a
// *skippable* warning and still exits 0, so a plain `vite build` hides the
// regression; this wrapper turns that warning into a hard failure.
//
// NOTE: this is the correct repo-wide guard. A naive "scan every CSS comment
// for { } or backtick" check produces ~449 false positives across the design
// system (those comments build clean), so we gate on the actual drop signal
// instead. The per-file light-theme.css comment guard in
// src/styles/tokens.drift.test.ts stays as the origin-incident pin.
import { spawnSync } from 'node:child_process';

const r = spawnSync('node', ['node_modules/vite/bin/vite.js', 'build'], {
  encoding: 'utf8',
});

process.stdout.write(r.stdout || '');
process.stderr.write(r.stderr || '');

if (r.status !== 0) {
  process.exit(r.status === null ? 1 : r.status);
}

const out = `${r.stdout || ''}${r.stderr || ''}`;
if (/css-syntax-error/i.test(out)) {
  console.error(
    '\n[css-drop-guard] vite emitted a `css-syntax-error`: a CSS rule was likely ' +
      'DROPPED by the esbuild brace/backtick-in-comment bug. Find the offending ' +
      'CSS comment (the rule AFTER it is the one being silently removed) and ' +
      'rewrite the comment without { } or backticks.'
  );
  process.exit(1);
}
