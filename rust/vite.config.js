import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { resolve } from 'path';

// The window is the Electron app's renderer, built from ../src/renderer (see
// src/main.jsx); Tauri serves it from the dev server while developing and
// from dist/ when built. Its imports resolve to this folder's packages, so
// the Electron app's node_modules are not needed here.
const here = (p) => resolve(__dirname, 'node_modules', p);
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  resolve: {
    dedupe: ['react', 'react-dom'],
    alias: { tailwindcss: here('tailwindcss') }
  },
  server: { port: 5173, strictPort: true, fs: { allow: [resolve(__dirname, '..')] } },
  build: { outDir: 'dist', target: 'es2022' }
});
