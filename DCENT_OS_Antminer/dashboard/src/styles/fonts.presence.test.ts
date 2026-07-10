import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { theme } from './theme';

const FONT_CSS = readFileSync(
  fileURLToPath(new URL('./fonts.css', import.meta.url)),
  'utf8',
);
const TOKENS_CSS = readFileSync(
  fileURLToPath(new URL('./tokens.css', import.meta.url)),
  'utf8',
);

function cssVar(name: string): string {
  const match = new RegExp(`${name}\\s*:\\s*([^;]+);`).exec(TOKENS_CSS);
  return match?.[1].trim() ?? '';
}

function faceBlock(family: string): string {
  const escaped = family.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = new RegExp(`@font-face\\s*\\{[\\s\\S]*?font-family:\\s*'${escaped}'[\\s\\S]*?\\}`, 'm').exec(FONT_CSS);
  return match?.[0] ?? '';
}

describe('self-hosted dashboard fonts', () => {
  it('embeds one real face and one metrics fallback for every shipped family', () => {
    for (const family of ['Inter', 'JetBrains Mono']) {
      expect(faceBlock(family), `${family} @font-face`).toContain('data:font/woff2;base64,');
      const fallback = faceBlock(`${family} Fallback`);
      expect(fallback, `${family} metrics fallback`).toContain('size-adjust:');
      expect(fallback).toContain('ascent-override:');
      expect(fallback).toContain('descent-override:');
      expect(fallback).toContain('line-gap-override:');
    }
  });

  it('keeps token stacks on self-hosted faces before system fallbacks', () => {
    expect(cssVar('--font-ui')).toBe("'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif");
    expect(cssVar('--font-heading')).toBe("'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif");
    expect(cssVar('--font-heading')).not.toContain('Barlow Condensed');
    expect(cssVar('--font-mono')).toBe("'JetBrains Mono', 'JetBrains Mono Fallback', ui-monospace, Consolas, monospace");
  });

  it('mirrors the same stacks in theme.ts', () => {
    expect(theme.font.ui).toBe("'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif");
    expect(theme.font.heading).toBe("'Inter', 'Inter Fallback', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, sans-serif");
    expect(theme.font.heading).not.toContain('Barlow Condensed');
    expect(theme.font.mono).toBe("'JetBrains Mono', 'JetBrains Mono Fallback', ui-monospace, Consolas, monospace");
  });
});
