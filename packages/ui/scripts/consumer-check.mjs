// Packs the package and builds a minimal Tauri interface from it, as an
// application does after a clean install: the packed files hold the licence,
// the sources and the styles, and the Tauri entry brings what it imports.
// Run from packages/ui: `npm run consumer` (installs from the npm registry).
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const here = new URL("..", import.meta.url).pathname;
const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: ["ignore", "pipe", "inherit"], encoding: "utf8" });

const root = mkdtempSync(join(tmpdir(), "engine-ui-consumer-"));
try {
  const [packed] = JSON.parse(run("npm", ["pack", "--json", "--pack-destination", root], here));
  const files = packed.files.map((f) => f.path);
  for (const needed of ["LICENSE", "package.json", "src/index.ts", "src/tauri/index.ts", "src/ui/styles/tokens.css", "src/ui/styles/page.css"]) {
    if (!files.includes(needed)) throw new Error(`the packed package lacks ${needed}`);
  }
  if (files.some((f) => f.includes(".test."))) throw new Error("the packed package holds tests");

  const app = join(root, "app");
  mkdirSync(join(app, "src"), { recursive: true });
  const write = (path, text) => writeFileSync(join(app, path), text);
  write(
    "package.json",
    JSON.stringify(
      {
        name: "consumer",
        private: true,
        type: "module",
        dependencies: { "@chirality/engine-ui": `file:${join(root, packed.filename)}`, react: "19.3.1", "react-dom": "19.3.1" },
        devDependencies: { "@types/react": "19.3.0", "@types/react-dom": "19.3.0", "@vitejs/plugin-react": "6.3.0", typescript: "7.0.2", vite: "8.3.8" },
      },
      null,
      2,
    ),
  );
  write(
    "tsconfig.json",
    JSON.stringify({
      compilerOptions: { target: "ES2022", lib: ["ES2023", "DOM", "DOM.Iterable"], module: "ESNext", moduleResolution: "bundler", jsx: "react-jsx", strict: true, skipLibCheck: true, noEmit: true, types: ["vite/client"] },
      include: ["src"],
    }),
  );
  write("vite.config.ts", `import { defineConfig } from "vite";\nimport react from "@vitejs/plugin-react";\nexport default defineConfig({ plugins: [react()] });\n`);
  write("index.html", `<!doctype html><html><body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body></html>\n`);
  write(
    "src/main.tsx",
    `import { createRoot } from "react-dom/client";
import { EngineProvider, StandardLayout, Store } from "@chirality/engine-ui";
import { tauriEngine, tauriHost } from "@chirality/engine-ui/tauri";
import "@chirality/engine-ui/tokens.css";
import "@chirality/engine-ui/page.css";

const store = new Store(tauriEngine, tauriHost);
createRoot(document.getElementById("root")!).render(
  <EngineProvider store={store}>
    <StandardLayout brand="Consumer" />
  </EngineProvider>,
);
void store.start();
`,
  );
  run("npm", ["install", "--no-audit", "--no-fund"], app);
  run("npx", ["tsc", "-p", "tsconfig.json"], app);
  run("npx", ["vite", "build", "--logLevel", "warn"], app);
  console.log(`ok: ${packed.filename} (${files.length} files) installs, typechecks and builds in a minimal Tauri interface`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
