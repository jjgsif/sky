import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vite.dev/config/
export default defineConfig({
  base: "/app",
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    hmr: {
      // When running behind the Sky gateway proxy (localhost:8080),
      // tell Vite's HMR client to connect WebSocket to the proxy port.
      // Sky tunnels it transparently to this dev server.
      clientPort: 8080,
    },
  },
});
