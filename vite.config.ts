import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

// Three pages, one bundle. The Rust side loads them by these fixed paths:
//   /overlay/index.html   the floating pill
//   /settings/index.html  the preferences window
//   /first-run/index.html the setup wizard
const page = (name: string) => fileURLToPath(new URL(`./ui/${name}/index.html`, import.meta.url));

export default defineConfig({
  root: "ui",
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    target: ["es2022", "chrome110", "safari16"],
    rollupOptions: {
      input: {
        overlay: page("overlay"),
        settings: page("settings"),
        "first-run": page("first-run"),
      },
      output: {
        manualChunks(id) {
          if (id.includes("node_modules/react") || id.includes("node_modules/scheduler")) return "react";
          if (id.includes("/ui/shared/")) return "shared";
          return undefined;
        },
      },
    },
  },
});
