import { act, render } from "@testing-library/react";
import type { ReactNode } from "react";

import type { EngineClient, Host } from "../engine/client";
import { FakeEngine, type FakeOptions } from "../fake/engine";
import { bridge } from "../fake/fixtures";
import { EngineProvider, Store } from "../state/store";
import { StandardLayout } from "../ui/StandardLayout";

export const notices: { title: string; body: string }[] = [];

export const testHost: Host = {
  pickFolder: async () => "/Users/person/work/estimating-demo",
  pickFiles: async () => ["/Users/person/Desktop/site-photo.jpg"],
  openUrl: async () => {},
  notify: async (title, body) => void notices.push({ title, body }),
};

/** A fake engine, signed in with one project, unless `bare`. */
export function fakeEngine(options: FakeOptions & { bare?: boolean } = {}) {
  const engine = new FakeEngine({ instant: true, ...options });
  if (!options.bare) {
    engine.seedAccount({ state: "signedIn", email: "person@example.com", plan: null });
    engine.seedProject(bridge);
  }
  return engine;
}

/** Mounts `ui` (the standard layout unless given) over an engine. */
export async function mount(engine: EngineClient, ui: ReactNode = <StandardLayout />) {
  const store = new Store(engine, testHost);
  await store.start();
  const project = store.get().project;
  if (project) store.selectProject(project);
  const view = render(<EngineProvider store={store}>{ui}</EngineProvider>);
  await settle();
  return { store, ...view };
}

/** Lets pending engine work and React's updates run. */
export async function settle() {
  await act(async () => {
    for (let i = 0; i < 20; i++) await new Promise((r) => setTimeout(r, 0));
  });
}
