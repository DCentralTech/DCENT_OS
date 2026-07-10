import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const distDir = path.join(root, 'dist');
const indexPath = path.join(distDir, 'index.html');
const gzipPath = `${indexPath}.gz`;
const shaPath = `${indexPath}.sha256`;

//  (2026-05-17): operator-authorized ceiling raise (confirmed twice
// in-chat) to fund the glassmorphism / blur / glow / animated-ASIC / motion
// redesign while staying firmware-ramdisk-loadable. Prior gate was 2 MiB raw
// / 500 KiB gzip (warn 1.8 MB). New gate 2.5 MiB raw / 600 KiB gzip (warn
// 2.35 MB). Agents must still trim dead CSS / token dupes aggressively — the
// ceiling is headroom for richness, not a licence to bloat.
// Public-beta closeout: final raw build is 2,344,995 B, so this default
// tightens the warning line without raising the prior 2,350,000 B guard.
const rawWarnBytes = Number(process.env.DASHBOARD_SIZE_WARN_BYTES ?? 2_345_500);
const rawMaxBytes = Number(process.env.DASHBOARD_SIZE_MAX_BYTES ?? Math.round(2.5 * 1024 * 1024));
const gzipMaxBytes = Number(process.env.DASHBOARD_GZIP_SIZE_MAX_BYTES ?? 600 * 1024);
const allowExtraFiles = process.env.DASHBOARD_ALLOW_EXTRA_DIST_FILES === '1';

function formatBytes(bytes) {
  if (bytes >= 1024 * 1024) {
    return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
  }
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

function listFiles(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  return entries.flatMap((entry) => {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      return listFiles(fullPath);
    }
    return [fullPath];
  });
}

if (!fs.existsSync(indexPath)) {
  console.error(`[size:guard] dist/index.html is missing. Run vite build before this guard.`);
  process.exit(1);
}

const files = listFiles(distDir);
const allowedFiles = new Set([
  path.resolve(indexPath),
  path.resolve(gzipPath),
  path.resolve(shaPath),
]);
const extraFiles = files.filter((file) => !allowedFiles.has(path.resolve(file)));
if (extraFiles.length > 0 && !allowExtraFiles) {
  console.error('[size:guard] expected single-file output plus gzip/hash sidecars, but dist contains extra files:');
  for (const file of extraFiles) {
    console.error(`  - ${path.relative(distDir, file)}`);
  }
  console.error('[size:guard] set DASHBOARD_ALLOW_EXTRA_DIST_FILES=1 only for an intentional split-output audit.');
  process.exit(1);
}

for (const required of [gzipPath, shaPath]) {
  if (!fs.existsSync(required)) {
    console.error(`[size:guard] missing ${path.relative(distDir, required)}. Run scripts/precompress-dist.mjs after vite build.`);
    process.exit(1);
  }
}

const html = fs.readFileSync(indexPath);
const gzip = fs.readFileSync(gzipPath);
const rawBytes = html.byteLength;
const gzipBytes = gzip.byteLength;
const uncompressed = gunzipSync(gzip);
if (!uncompressed.equals(html)) {
  console.error('[size:guard] dist/index.html.gz does not decompress byte-for-byte to dist/index.html.');
  process.exit(1);
}

const expectedSha = createHash('sha256').update(html).digest('hex');
const actualSha = fs.readFileSync(shaPath, 'utf8').trim().split(/\s+/)[0] ?? '';
if (actualSha !== expectedSha) {
  console.error(
    `[size:guard] dist/index.html.sha256 is stale: expected ${expectedSha}, got ${actualSha || '<empty>'}`,
  );
  process.exit(1);
}

console.log(
  `[size:guard] dist/index.html ${formatBytes(rawBytes)} raw, ${formatBytes(gzipBytes)} gzip ` +
    `(warn ${formatBytes(rawWarnBytes)}, fail ${formatBytes(rawMaxBytes)} raw / ${formatBytes(gzipMaxBytes)} gzip)`,
);

if (rawBytes > rawMaxBytes) {
  console.error(`[size:guard] dashboard bundle exceeds the ramdisk budget: ${formatBytes(rawBytes)} > ${formatBytes(rawMaxBytes)}`);
  process.exit(1);
}

if (gzipBytes > gzipMaxBytes) {
  console.error(`[size:guard] dashboard gzip bundle exceeds the ramdisk budget: ${formatBytes(gzipBytes)} > ${formatBytes(gzipMaxBytes)}`);
  process.exit(1);
}

if (rawBytes > rawWarnBytes) {
  console.warn(`[size:guard] warning: dashboard bundle is nearing the ramdisk budget: ${formatBytes(rawBytes)} > ${formatBytes(rawWarnBytes)}`);
}
