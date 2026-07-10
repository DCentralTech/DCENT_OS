import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const stylesDir = path.join(root, 'src', 'styles');
const forbidden = /[{}\x60]/;

function listCssFiles(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) return listCssFiles(fullPath);
    return entry.isFile() && entry.name.endsWith('.css') ? [fullPath] : [];
  });
}

function lineForOffset(text, offset) {
  return text.slice(0, offset).split(/\r?\n/).length;
}

const offenders = [];

for (const file of listCssFiles(stylesDir)) {
  const css = fs.readFileSync(file, 'utf8');
  const commentPattern = /\/\*[\s\S]*?\*\//g;
  let match;
  while ((match = commentPattern.exec(css)) !== null) {
    if (!forbidden.test(match[0])) continue;
    offenders.push({
      file: path.relative(root, file),
      line: lineForOffset(css, match.index),
      preview: match[0].replace(/\s+/g, ' ').slice(0, 120),
    });
  }
}

if (offenders.length > 0) {
  console.error('[css-comments] CSS comments must not contain braces or backticks.');
  console.error('[css-comments] Rewrite the comment text before the next CSS rule can be dropped by esbuild.');
  for (const offender of offenders) {
    console.error(`  ${offender.file}:${offender.line} ${offender.preview}`);
  }
  process.exit(1);
}

console.log(`[css-comments] scanned ${listCssFiles(stylesDir).length} CSS files; comments are safe.`);
