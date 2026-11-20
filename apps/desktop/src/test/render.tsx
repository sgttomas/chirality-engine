import { act, render } from "@testing-library/react";

import { FakeEngine, type FakeOptions } from "../engine/fake/engine";
import { bridge } from "../engine/fake/fixtures";
import type { Host } from "../engine/client";
import { Store, StoreContext } from "../state/store";
import { App } from "../ui/App";

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

/** Mounts the whole interface over an engine. */
export async function mount(engine: FakeEngine) {
  const store = new Store(engine, testHost);
  await store.start();
  const project = store.get().project;
  if (project) store.selectProject(project);
  const view = render(
    <StoreContext.Provider value={store}>
      <App />
    </StoreContext.Provider>,
  );
  await settle();
  return { store, ...view };
}

/** Lets the fake engine's pending work and React's updates run. */
export async function settle() {
  await act(async () => {
    for (let i = 0; i < 20; i++) await new Promise((r) => setTimeout(r, 0));
  });
}
