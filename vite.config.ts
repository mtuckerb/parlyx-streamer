import { defineConfig } from "vite";

// Tauri exposes the dev URL via TAURI_DEV_HOST when running on a non-default
// host (mobile, network testing). The default config matches `tauri dev`.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
