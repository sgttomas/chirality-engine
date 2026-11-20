// The interface's state: the engine's (kept current from events) and what is selected.
import { createContext, useContext, useSyncExternalStore } from "react";

import type { EngineClient, Host } from "../engine/client";
import type { Event, ProjectId, ThreadId, Workflow } from "../engine/types";
import { applyEvent, type Model } from "./reduce";

export interface State {
  model: Model | null;
  project: ProjectId | null;
  thread: ThreadId | null;
  workflows: Record<ProjectId, Workflow[]>;
  error: string | null;
}

export class Store {
  private state: State = { model: null, project: null, thread: null, workflows: {}, error: null };
  private listeners = new Set<() => void>();
  private stop: (() => void) | null = null;
  private eventListeners = new Set<(event: Event, before: State) => void>();

  constructor(
    readonly engine: EngineClient,
    readonly host: Host,
  ) {}

  get = () => this.state;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  };
  /** Every engine event after it is applied, with the state before it. */
  onEvent(listener: (event: Event, before: State) => void) {
    this.eventListeners.add(listener);
    return () => void this.eventListeners.delete(listener);
  }

  /** Follows the engine: listens first, then reads the snapshot, then applies what came meanwhile. */
  async start() {
    const early: Event[] = [];
    this.stop = this.engine.subscribe((event) => {
      if (!this.state.model) return void early.push(event);
      this.apply(event);
    });
    const snapshot = await this.engine.snapshot();
    let model: Model = { snapshot, views: {} };
    for (const event of early) model = applyEvent(model, event);
    const project = this.state.project ?? snapshot.projects[0]?.id ?? null;
    this.set({ model, project });
  }
  dispose() {
    this.stop?.();
  }

  private apply(event: Event) {
    const before = this.state;
    const model = applyEvent(before.model!, event);
    if (model !== before.model) this.set({ model });
    if (event.type === "thread" && event.thread.id === this.state.thread && event.thread.unread) this.markRead(event.thread.id);
    if (event.type === "threadRemoved" && event.thread === this.state.thread) this.set({ thread: null });
    if (event.type === "projects" && !event.projects.some((p) => p.id === this.state.project)) this.set({ project: event.projects[0]?.id ?? null, thread: null });
    for (const listener of this.eventListeners) listener(event, before);
  }

  private set(patch: Partial<State>) {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }

  /** Runs an engine call; a failure is shown once. */
  async attempt<T>(call: () => Promise<T>): Promise<T | undefined> {
    try {
      const value = await call();
      if (this.state.error) this.set({ error: null });
      return value;
    } catch (e) {
      this.set({ error: describe(e) });
      return undefined;
    }
  }
  dismissError() {
    this.set({ error: null });
  }

  selectProject(project: ProjectId) {
    this.set({ project, thread: null });
    void this.loadWorkflows(project);
  }
  async selectThread(thread: ThreadId) {
    const summary = this.state.model?.snapshot.threads.find((t) => t.id === thread);
    this.set({ thread, project: summary?.project ?? this.state.project });
    if (summary) void this.loadWorkflows(summary.project);
    if (!this.state.model?.views[thread]) await this.reload(thread);
    if (summary?.unread) this.markRead(thread);
  }
  async reload(thread: ThreadId) {
    const view = await this.attempt(() => this.engine.readThread(thread));
    if (view && this.state.model) this.set({ model: { ...this.state.model, views: { ...this.state.model.views, [thread]: view } } });
  }
  async loadWorkflows(project: ProjectId) {
    if (this.state.workflows[project]) return;
    const list = await this.attempt(() => this.engine.workflows(project));
    if (list) this.set({ workflows: { ...this.state.workflows, [project]: list } });
  }
  private markRead(thread: ThreadId) {
    void this.attempt(() => this.engine.markRead(thread));
  }
}

export function describe(error: unknown): string {
  if (error && typeof error === "object" && "kind" in error) {
    const e = error as { kind: string; message?: string; what?: string };
    if (e.kind === "notFound") return `Not found: ${e.what}`;
    if (e.kind === "codexUnavailable") return `Codex isn't running: ${e.message}`;
    return e.message ?? String(error);
  }
  return error instanceof Error ? error.message : String(error);
}

export const StoreContext = createContext<Store | null>(null);

export function useStoreHandle(): Store {
  const store = useContext(StoreContext);
  if (!store) throw new Error("No store");
  return store;
}

export function useStore<T>(select: (state: State) => T): T {
  const store = useStoreHandle();
  return useSyncExternalStore(store.subscribe, () => select(store.get()));
}

/** The folder of the project in view. */
export const useProjectRoot = () => useStore((s) => s.model?.snapshot.projects.find((p) => p.id === s.project)?.path);
