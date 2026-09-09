import { defineConfig } from "vite";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

// ESM 安全：package.json 为 "type": "module"，不用 __dirname（无该全局量）
const rootDir = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  // UI 源码位于 ui/ 目录
  root: rootDir,
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
        main: resolve(rootDir, "index.html"),
      },
    },
  },
});
