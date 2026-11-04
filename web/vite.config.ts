import { svelte } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// `npm run dev` serves the UI on :5173 and forwards API calls to a RustCloud
// server running on :8080. `npm run build` writes web/dist, which the server
// embeds into its binary.
export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/health': 'http://localhost:8080',
    },
  },
});
