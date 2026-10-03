import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// base "./" so the built app loads from file:// inside Electron.
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  server: { port: 5174 },
});
