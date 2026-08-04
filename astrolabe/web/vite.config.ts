import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/ws": { target: "ws://127.0.0.1:7979", ws: true },
      "/healthz": { target: "http://127.0.0.1:7979" },
    },
  },
});
