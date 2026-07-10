import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const fontsDir = path.join(root, 'src', 'assets', 'fonts');
const outFile = path.join(root, 'src', 'styles', 'fonts.css');
const coreRange = 'U+0020-007E, U+00A0, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215';

const faces = [
  {
    family: 'Inter',
    file: 'inter-core-400-700.woff2',
    weight: '400 700',
    fallback: {
      family: 'Inter Fallback',
      locals: ['Arial'],
      sizeAdjust: '107%',
      ascentOverride: '90%',
      descentOverride: '22.5%',
      lineGapOverride: '0%',
    },
  },
  {
    family: 'JetBrains Mono',
    file: 'jetbrains-mono-core-400-700.woff2',
    weight: '400 700',
    fallback: {
      family: 'JetBrains Mono Fallback',
      locals: ['Consolas'],
      sizeAdjust: '110%',
      ascentOverride: '92%',
      descentOverride: '24%',
      lineGapOverride: '0%',
    },
  },
];

function quote(value) {
  return value.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
}

function fontFace(face) {
  const file = path.join(fontsDir, face.file);
  const bytes = fs.readFileSync(file);
  const encoded = bytes.toString('base64');
  return [
    '@font-face {',
    `  font-family: '${quote(face.family)}';`,
    '  font-style: normal;',
    `  font-weight: ${face.weight};`,
    '  font-display: swap;',
    `  src: url(data:font/woff2;base64,${encoded}) format('woff2');`,
    `  unicode-range: ${coreRange};`,
    '}',
  ].join('\n');
}

function fallbackFace(face) {
  const fallback = face.fallback;
  return [
    '@font-face {',
    `  font-family: '${quote(fallback.family)}';`,
    `  src: ${fallback.locals.map(local => `local('${quote(local)}')`).join(', ')};`,
    `  size-adjust: ${fallback.sizeAdjust};`,
    `  ascent-override: ${fallback.ascentOverride};`,
    `  descent-override: ${fallback.descentOverride};`,
    `  line-gap-override: ${fallback.lineGapOverride};`,
    '}',
  ].join('\n');
}

const css = `${faces.map(face => `${fontFace(face)}\n\n${fallbackFace(face)}`).join('\n\n')}\n`;
fs.writeFileSync(outFile, css);

for (const face of faces) {
  const file = path.join(fontsDir, face.file);
  const raw = fs.statSync(file).size;
  const b64 = Buffer.from(fs.readFileSync(file)).toString('base64').length;
  console.log(`[fonts] ${face.file}: ${raw} B raw, ${b64} B base64`);
}
console.log(`[fonts] wrote ${path.relative(root, outFile)}: ${Buffer.byteLength(css)} B raw, ${gzipSync(css, { level: 9 }).length} B gzip`);
