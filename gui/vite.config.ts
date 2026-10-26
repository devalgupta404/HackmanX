import { defineConfig } from "vite";

// Tauri serves the built files from dist/ and expects the dev server on a fixed port.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", outDir: "dist" },
});
