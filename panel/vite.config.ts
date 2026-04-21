import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    react(),
    VitePWA({
      registerType: "autoUpdate",
      includeAssets: ["favicon.svg"],
      manifest: {
        name: "IoT-AtHome",
        short_name: "IoT-AtHome",
        description: "IoT-AtHome Command Central",
        theme_color: "#0f172a",
        background_color: "#0f172a",
        display: "standalone",
        icons: [],
      },
      workbox: {
        // Panels are offline-first (design §6.7). Cache shell + known API GETs.
        navigateFallback: "/index.html",
        runtimeCaching: [
          {
            urlPattern: /\/api\/v1\/devices/,
            handler: "StaleWhileRevalidate",
            options: { cacheName: "api-devices" },
          },
        ],
      },
    }),
  ],
  server: {
    port: 5173,
    host: "127.0.0.1",
    strictPort: true,
    proxy: {
      "/api": { target: "http://127.0.0.1:8081", changeOrigin: true },
      "/stream": {
        target: "ws://127.0.0.1:8081",
        ws: true,
        changeOrigin: true,
      },
    },
  },
});
