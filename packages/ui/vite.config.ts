import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["src/test-setup.ts"],
    // The style test reads the sheets themselves.
    css: { include: [/\/styles\/[a-z-]+\.css/] },
  },
});
