import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig(({ mode }) => {
  const environment = loadEnv(mode, process.cwd(), '');
  const serverTarget = environment.DEEPSEEK_SERVER_URL ?? 'http://127.0.0.1:3000';

  return {
    plugins: [react()],
    build: {
      emptyOutDir: true,
      outDir: '../crates/deepseek-custom/src/web/assets',
    },
    server: {
      proxy: {
        '/api': {
          changeOrigin: false,
          target: serverTarget,
        },
      },
    },
    test: {
      environment: 'node',
    },
  };
});
