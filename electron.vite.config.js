import { defineConfig, externalizeDepsPlugin } from 'electron-vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { resolve } from 'path';

// Main and preload are plain CommonJS Node code (the services are shared with
// the Falcon backend and written that way); the renderer is React + Tailwind.
export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    build: {
      // The main-process code is CommonJS (shared with the Falcon backend).
      // Vite only converts require() inside node_modules unless told
      // otherwise, which left "./settings" as a runtime require that does not
      // exist in the installed app.
      commonjsOptions: { include: [/node_modules/, /src[\\/]main/], transformMixedEsModules: true },
      rollupOptions: { input: { index: resolve(__dirname, 'src/main/index.js') } }
    }
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    build: {
      commonjsOptions: { include: [/node_modules/, /src[\\/]preload/], transformMixedEsModules: true },
      rollupOptions: { input: { index: resolve(__dirname, 'src/preload/index.js') } }
    }
  },
  renderer: {
    root: resolve(__dirname, 'src/renderer'),
    plugins: [react(), tailwindcss()],
    build: {
      rollupOptions: { input: { index: resolve(__dirname, 'src/renderer/index.html') } }
    }
  }
});
