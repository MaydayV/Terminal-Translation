import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const devHost = process.env.TAURI_DEV_HOST;

export default defineConfig({
  base: devHost ? "/" : "./",
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
});
