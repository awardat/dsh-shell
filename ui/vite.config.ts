import { defineConfig } from "vite";
import { resolve } from "node:path";

export default defineConfig({
  // UI 源码位于 ui/ 目录
  root: __dirname,
  // Tauri expects a fixed port; fails if unavailable.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
      },
    },
  },
});
