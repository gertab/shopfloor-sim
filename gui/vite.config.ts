import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Port 5174 so this app can run alongside another app on Vite's default port (5173).
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5174,
    strictPort: true,
  },
});
