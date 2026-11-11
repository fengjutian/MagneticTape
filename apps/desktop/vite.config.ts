import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**", "**/.*.tmpdir/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
});

