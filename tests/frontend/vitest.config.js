import { defineConfig } from "vitest/config";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const root = dirname(fileURLToPath(import.meta.url));
const staticLib = resolve(root, "../../static/lib");

export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["**/*.test.js"],
    coverage: {
      provider: "v8",
      include: [`${staticLib}/**/*.js`],
      exclude: ["**/*.test.js"],
      reporter: ["text", "html"],
    },
  },
});
