// The interface's engine state: the engine's own (kept current from events) and
// which project and thread are selected. An application keeps its own state beside it.
import { createContext, createElement, useContext, useSyncExternalStore, type ReactNode } from "react";

import type { EngineClient, Host } from "../engine/client";
import type { Event, Numbered, ProjectId, ThreadId, ThreadView, Workflow } from "../engine/types";
import { applyEvent, applyToView, threadOf, type Model } from "./reduce";

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
  /** While the snapshot is read: the events that arrive meanwhile. */
  private held: Numbered[] | null = [];
  /** While a thread is read: the events that arrive meanwhile, for each read. */
  private reads = new Set<Numbered[]>();

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

  /** Follows the engine: listens first, then reads the snapshot and applies the events after it. */
  async start() {
    this.stop = await this.engine.subscribe((n) => this.receive(n));
    await this.resync();
    if (this.state.project) void this.loadWorkflows(this.state.project);
  }
  dispose() {
    this.stop?.();
  }

  private receive(n: Numbered) {
    for (const read of this.reads) read.push(n);
    if (this.held) return void this.held.push(n);
    const seq = this.state.model!.snapshot.seq;
    if (n.seq <= seq) return;
    if (n.seq !== seq + 1) return void this.resync();
    this.apply(n);
  }

  /** Reads the snapshot (and the thread in view) again; at the start, or once events were missed. */
  private async resync() {
    this.held ??= [];
    let model: Model;
    for (;;) {
      const snapshot = await this.engine.snapshot();
      model = { snapshot, views: {} };
      const after = this.held.filter((n) => n.seq > snapshot.seq);
      if (after.some((n, i) => n.seq !== snapshot.seq + i + 1)) {
        this.held = [];
        continue;
      }
      for (const n of after) model = numbered(applyEvent(model, n.event), n.seq);
      break;
    }
    this.held = null;
    const { project, thread } = this.state;
    const projects = model.snapshot.projects;
    const keptProject = projects.some((p) => p.id === project) ? project : (projects[0]?.id ?? null);
    const keptThread = model.snapshot.threads.some((t) => t.id === thread && t.project === keptProject) ? thread : null;
    this.set({ model, project: keptProject, thread: keptThread });
    if (keptThread) await this.reload(keptThread);
  }

  private apply({ seq, event }: Numbered) {
    const before = this.state;
    this.set({ model: numbered(applyEvent(before.model!, event), seq) });
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

  /** Asks the person for a folder, adds it as a project and selects it. */
  async addProjectFolder() {
    const path = await this.host.pickFolder();
    if (!path) return;
    const project = await this.attempt(() => this.engine.addProject(path));
    if (project) this.selectProject(project.id);
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
  /** Reads the thread again, with the events after the read applied. */
  async reload(thread: ThreadId) {
    const during: Numbered[] = [];
    this.reads.add(during);
    const read = await this.attempt(() => this.engine.readThread(thread)).finally(() => this.reads.delete(during));
    if (!read || !this.state.model) return;
    const after = during.filter((n) => n.seq > read.seq);
    // A gap: the snapshot is read again, and this thread with it if it is in view.
    if (after.some((n, i) => n.seq !== read.seq + i + 1)) return;
    if (!this.state.model.snapshot.threads.some((t) => t.id === thread)) return;
    const had = new Set(read.turns.flatMap((t) => t.items.map((i) => i.id)));
    let view: ThreadView = read;
    for (const { event } of after) {
      if (threadOf(event) !== thread) continue;
      // Text the read already holds; the item's last event brings all of it in any case.
      if (event.type === "itemDelta" && had.has(event.item)) continue;
      view = applyToView(view, event);
    }
    this.set({ model: { ...this.state.model, views: { ...this.state.model.views, [thread]: view } } });
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

/** The model as of event `seq`. */
const numbered = (model: Model, seq: number): Model => ({ ...model, snapshot: { ...model.snapshot, seq } });

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

/** The root of the engine's parts: gives them the store, and shows them once it has followed the
 * engine to its first snapshot (`fallback` until then). Their styles apply inside it. */
export function EngineProvider({ store, fallback = null, children }: { store: Store; fallback?: ReactNode; children?: ReactNode }) {
  const ready = useSyncExternalStore(store.subscribe, () => store.get().model !== null);
  return createElement(StoreContext.Provider, { value: store }, createElement("div", { className: "chirality" }, ready ? children : fallback));
}

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
