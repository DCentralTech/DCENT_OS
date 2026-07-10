import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const distDir = path.join(root, 'dist');
const indexPath = path.join(distDir, 'index.html');
const gzipPath = `${indexPath}.gz`;
const shaPath = `${indexPath}.sha256`;
const bannerTag = '<script src="/static/diagnostic-banner.js" defer></script>';
const legacyBannerTag = '<script src="/static/diagnostic-banner.js"></script>';

function formatBytes(bytes) {
  if (bytes >= 1024 * 1024) {
    return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
  }
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

if (!fs.existsSync(indexPath)) {
  console.error('[precompress] dist/index.html is missing. Run vite build first.');
  process.exit(1);
}

let html = fs.readFileSync(indexPath, 'utf8');
if (html.includes(legacyBannerTag)) {
  html = html.replaceAll(legacyBannerTag, bannerTag);
}
if (!html.includes(bannerTag)) {
  if (html.includes('</body>')) {
    html = html.replace('</body>', `${bannerTag}</body>`);
  } else {
    html += bannerTag;
  }
}

const finalHtml = Buffer.from(html);
fs.writeFileSync(indexPath, finalHtml);

// Brotli is intentionally omitted. Python stdlib and BusyBox can recreate gzip
// sidecars in post-build fallbacks, and the LAN savings from Brotli are small.
const gzip = gzipSync(finalHtml, { level: 9 });
const sha256 = createHash('sha256').update(finalHtml).digest('hex');

fs.writeFileSync(gzipPath, gzip);
fs.writeFileSync(shaPath, `${sha256}\n`);

console.log(
  `[precompress] index.html ${formatBytes(finalHtml.byteLength)} raw, ` +
    `${formatBytes(gzip.byteLength)} gzip, sha256 ${sha256.slice(0, 16)}`,
);
