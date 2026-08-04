import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    // 5173 belongs to RunTime's dev server; PlayBack sits one up.
    port: 5174,
    strictPort: true,
    // Forward state-sync WebSocket and media streaming to the app's built-in
    // web server so the web control/output views work against a dev build.
    proxy: {
      '/ws': {
        target: 'ws://localhost:3100',
        ws: true,
      },
      '/media': {
        target: 'http://localhost:3100',
      },
      '/help': {
        target: 'http://localhost:3100',
      },
    },
  },
})
