import { defineConfig } from 'vite';
import type { PluginOption, PreviewServer, ViteDevServer } from 'vite';
import react from '@vitejs/plugin-react';
import { viteSingleFile } from 'vite-plugin-singlefile';
import path from 'path';

const dashboardProxyTarget = process.env.DCENT_DASHBOARD_PROXY_TARGET ?? 'http://127.0.0.1:8080';
const dashboardWsProxyTarget =
  process.env.DCENT_DASHBOARD_WS_PROXY_TARGET ?? dashboardProxyTarget.replace(/^http/, 'ws');

function installDiagnosticBannerStub(server: ViteDevServer | PreviewServer) {
  server.middlewares.use('/static/diagnostic-banner.js', (req, res, next) => {
    if (req.method === 'GET' || req.method === 'HEAD') {
      res.statusCode = 204;
      res.end();
      return;
    }
    next();
  });
}

function diagnosticBannerDevPlugin(): PluginOption {
  return {
    name: 'dcentos-diagnostic-banner-dev',
    configureServer(server: ViteDevServer) {
      installDiagnosticBannerStub(server);
    },
    configurePreviewServer(server: PreviewServer) {
      installDiagnosticBannerStub(server);
    },
  };
}

export default defineConfig({
  plugins: [diagnosticBannerDevPlugin(), react(), viteSingleFile()],
  esbuild: {
    pure: ['console.debug', 'console.info', 'console.warn', 'console.error'],
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  build: {
    target: 'es2022',
    modulePreload: false,
    minify: 'esbuild',
    cssMinify: true,
    assetsInlineLimit: 100000000,
    rollupOptions: {
      output: {
        manualChunks: undefined,
      },
    },
  },
  server: {
    proxy: {
      '/api': dashboardProxyTarget,
      '/ws': {
        target: dashboardWsProxyTarget,
        ws: true,
      },
    },
  },
});
