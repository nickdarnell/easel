import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// `npm run dev` serves the UI with hot reload and forwards the API to `frametv serve` on its default port.
export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: { "/api": "http://localhost:3273", "/manifest.webmanifest": "http://localhost:3273" },
  },
  build: { target: "es2022", assetsInlineLimit: 0 },
});
