import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Dev server proxies API calls to a locally running nexus so the SPA can use
// relative URLs in both dev and production (where nexus serves everything).
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8000',
      '/dev': 'http://127.0.0.1:8000',
    },
  },
});
