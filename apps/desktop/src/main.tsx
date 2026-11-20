import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { inTauri, type EngineClient, type Host } from "./engine/client";
import { Store, StoreContext } from "./state/store";
import { App } from "./ui/App";
import type { PanelTab } from "./ui/panel";
import { applyTheme, storedTheme } from "./ui/theme";
import "./ui/styles/tokens.css";

interface Start {
  engine: EngineClient;
  host: Host;
  project?: string;
  thread?: string;
  panel?: PanelTab;
}

/** The real engine inside the app; the scripted fake in a browser (`?scene=` picks where it starts). */
async function connect(): Promise<Start> {
  if (inTauri()) {
    const { tauriEngine, tauriHost } = await import("./engine/tauri");
    return { engine: tauriEngine, host: tauriHost };
  }
  const { buildScene, sceneNames } = await import("./engine/fake/scenes");
  const { fakeHost } = await import("./engine/fake/host");
  const asked = new URLSearchParams(location.search).get("scene");
  const name = sceneNames.find((n) => n === asked) ?? "home";
  const scene = await buildScene(name);
  return { ...scene, host: fakeHost };
}

async function boot() {
  const theme = new URLSearchParams(location.search).get("theme");
  applyTheme(theme === "dark" || theme === "system" ? theme : storedTheme());
  const start = await connect();
  const store = new Store(start.engine, start.host);
  await store.start();
  if (start.thread) await store.selectThread(start.thread);
  else if (store.get().project) store.selectProject(start.project ?? store.get().project!);
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <StoreContext.Provider value={store}>
        <App panel={start.panel} />
      </StoreContext.Provider>
    </StrictMode>,
  );
}

void boot();
