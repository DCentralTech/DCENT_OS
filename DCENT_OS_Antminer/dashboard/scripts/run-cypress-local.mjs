import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');
const isWindows = process.platform === 'win32';
const viteBin = path.join(root, 'node_modules', 'vite', 'bin', 'vite.js');
const previewUrl = 'http://127.0.0.1:4173';

function prefixStream(stream, prefix) {
  stream.setEncoding('utf8');
  stream.on('data', (chunk) => {
    for (const line of chunk.split(/\r?\n/)) {
      if (line.trim()) {
        console.log(`${prefix} ${line}`);
      }
    }
  });
}

async function waitForPreview(proc) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (proc.exitCode != null) {
      throw new Error(`vite preview exited early with code ${proc.exitCode}`);
    }
    try {
      const res = await fetch(previewUrl, { cache: 'no-store' });
      if (res.ok) return;
    } catch {
      // Server is still booting.
    }
    await delay(500);
  }
  throw new Error(`timed out waiting for ${previewUrl}`);
}

function killProcessTree(proc) {
  if (!proc.pid || proc.exitCode != null) return;
  if (isWindows) {
    spawnSync('taskkill', ['/pid', String(proc.pid), '/T', '/F'], {
      stdio: 'ignore',
      windowsHide: true,
    });
    return;
  }
  proc.kill('SIGTERM');
}

async function runCypress() {
  const preview = spawn(
    process.execPath,
    [viteBin, 'preview', '--host', '127.0.0.1', '--port', '4173', '--strictPort'],
    {
      cwd: root,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    },
  );

  prefixStream(preview.stdout, '[preview]');
  prefixStream(preview.stderr, '[preview]');

  const cleanup = () => killProcessTree(preview);
  process.once('exit', cleanup);
  process.once('SIGINT', () => {
    cleanup();
    process.exit(130);
  });
  process.once('SIGTERM', () => {
    cleanup();
    process.exit(143);
  });

  try {
    await waitForPreview(preview);
    const cypress = spawn(
      isWindows ? 'cmd.exe' : 'npm',
      isWindows ? ['/d', '/s', '/c', 'npm run cypress:run'] : ['run', 'cypress:run'],
    {
      cwd: root,
      stdio: 'inherit',
      windowsHide: true,
    });
    const [code] = await once(cypress, 'exit');
    process.exitCode = code ?? 1;
  } finally {
    cleanup();
    process.removeListener('exit', cleanup);
  }
}

runCypress().catch((err) => {
  console.error(err instanceof Error ? err.message : err);
  process.exitCode = 1;
});
