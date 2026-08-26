import { defineConfig } from "vite";

// Standard Tauri + Vite setup: index.html and JS live in src/, bundled output goes to dist/.
export default defineConfig({
  root: "src",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
  server: {
    port: 1420,
    strictPort: true,
  },
  clearScreen: false,
});
