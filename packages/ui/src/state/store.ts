// The interface's engine state: the engine's own (kept current from events) and
// which project and thread are selected. An application keeps its own state beside it.
import { createContext, createElement, useContext, useSyncExternalStore, type ReactNode } from "react";

import type { EngineClient, Host } from "../engine/client";
import type { Event, Numbered, ProjectId, ThreadId, ThreadView, Workflow } from "../engine/types";
import { useTheme } from "../ui/theme";
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
  private eventListeners = new Set<(event: Event, before: State) => void>();
  private stop: (() => void) | null = null;
  /** Counts disposals: what an earlier start, read or listener began is dropped. */
  private generation = 0;
  private starting: Promise<void> | null = null;
  /** While the snapshot is read: the events that arrive meanwhile. */
  private held: Numbered[] | null = [];
  /** The highest number the host has marked as sent. */
  private marked = 0;
  /** Events received while the snapshot was read, until they are told to `onEvent`; kept
   * across a recovery that fails, so each is told once. */
  private untold = new Map<number, Event>();
  /** The failure of the last recovery, shown until one succeeds. */
  private recoveryError: string | null = null;
  /** While a thread is read: the events that arrive meanwhile, for each read. */
  private reads = new Set<Numbered[]>();
  /** Each thread's latest read: an earlier one that answers later, or one begun before a disposal, is dropped. */
  private latestRead = new Map<ThreadId, number>();
  private readsBegun = 0;

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

  /** Follows the engine: listens first, then reads the snapshot and applies the events after it.
   * Once: a start while one is under way is that one. */
  start(): Promise<void> {
    return (this.starting ??= this.follow(this.generation));
  }
  /** Stops following; what is under way is dropped. A later `start` follows again. */
  dispose() {
    this.generation++;
    this.starting = null;
    this.stop?.();
    this.stop = null;
    this.held = [];
    this.marked = 0;
    this.untold.clear();
    this.reads.clear();
    this.latestRead.clear();
  }

  private async follow(generation: number) {
    const current = () => generation === this.generation;
    const stop = await this.engine.subscribe(
      (n) => current() && this.receive(n),
      (m) => current() && this.mark(m.last),
    );
    if (!current()) return stop();
    this.stop = stop;
    await this.resync(generation);
    if (current() && this.state.project) void this.loadWorkflows(this.state.project);
  }

  private receive(n: Numbered) {
    for (const read of this.reads) read.push(n);
    if (this.held) return this.hold(n);
    const seq = this.state.model!.snapshot.seq;
    if (n.seq <= seq) return;
    if (n.seq !== seq + 1) {
      this.recover();
      return this.hold(n);
    }
    this.apply(n);
  }

  /** Holds an event received while the snapshot is read, to apply after it and tell `onEvent`. */
  private hold(n: Numbered) {
    this.held?.push(n);
    this.untold.set(n.seq, n.event);
  }

  /** The host sent events through `last`: any not received are read again. */
  private mark(last: number) {
    this.marked = Math.max(this.marked, last);
    if (!this.held && last > this.state.model!.snapshot.seq) this.recover();
  }

  /** Reads again after a gap or a mark ahead. A failure is shown, and tried again
   * (after 0.25 s, doubling to 4 s) while the host has marked events not received. */
  private recover(attempt = 0) {
    const generation = this.generation;
    void this.resync(generation).then(
      () => {
        if (generation !== this.generation) return;
        if (this.recoveryError && this.state.error === this.recoveryError) this.set({ error: null });
        this.recoveryError = null;
      },
      (e) => {
        if (generation !== this.generation) return;
        this.held = null;
        this.recoveryError = describe(e);
        this.set({ error: this.recoveryError });
        setTimeout(() => {
          const behind = this.state.model && this.marked > this.state.model.snapshot.seq;
          if (generation === this.generation && !this.held && behind) this.recover(attempt + 1);
        }, Math.min(250 * 2 ** attempt, 4000));
      },
    );
  }

  /** Reads the snapshot (and the thread in view) again; at the start, or once events were missed.
   * The events that arrived meanwhile are applied after it, and told to `onEvent`. A snapshot
   * that misses events received or marked is read again (after 0.25 s, doubling to 4 s). */
  private async resync(generation: number) {
    this.held ??= [];
    // Every event received from now is told to `onEvent` once, the snapshot holding it or not;
    // at the start, those after the snapshot.
    const applied = this.state.model?.snapshot.seq ?? null;
    let model: Model;
    let read = 0;
    for (let tries = 0; ; tries++) {
      if (tries) {
        await new Promise((resolve) => setTimeout(resolve, Math.min(250 * 2 ** (tries - 1), 4000)));
        if (generation !== this.generation) return;
      }
      const snapshot = await this.engine.snapshot();
      read = snapshot.seq;
      if (generation !== this.generation) return;
      const after = this.held.filter((n) => n.seq > snapshot.seq);
      const gap = after.some((n, i) => n.seq !== snapshot.seq + i + 1);
      if (gap || this.marked > snapshot.seq + after.length) continue;
      model = { snapshot, views: {} };
      for (const n of after) model = applyEvent(model, n.event, n.seq);
      break;
    }
    this.held = null;
    const told = [...this.untold].filter(([seq]) => seq > (applied ?? read)).sort(([a], [b]) => a - b);
    this.untold.clear();
    const before = this.state;
    const { project, thread } = before;
    const projects = model.snapshot.projects;
    const keptProject = projects.some((p) => p.id === project) ? project : (projects[0]?.id ?? null);
    const keptThread = model.snapshot.threads.some((t) => t.id === thread && t.project === keptProject) ? thread : null;
    this.set({ model, project: keptProject, thread: keptThread });
    for (const [, event] of told) for (const listener of this.eventListeners) listener(event, before);
    if (keptThread) await this.reload(keptThread);
  }

  private apply({ seq, event }: Numbered) {
    const before = this.state;
    this.untold.delete(seq);
    this.set({ model: applyEvent(before.model!, event, seq) });
    // Codex's history of a completed turn replaces what the view gathered while it ran; a stopped
    // or failed turn keeps it (Codex may not have kept a reply cut short).
    if (event.type === "turnEnded" && event.state.state === "completed" && event.thread === this.state.thread) void this.reload(event.thread);
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
  /** Reads the thread again, with the events after the read applied. The latest read of a thread wins. */
  async reload(thread: ThreadId) {
    const ticket = ++this.readsBegun;
    this.latestRead.set(thread, ticket);
    const during: Numbered[] = [];
    this.reads.add(during);
    const read = await this.attempt(() => this.engine.readThread(thread)).finally(() => this.reads.delete(during));
    if (!read || !this.state.model || this.latestRead.get(thread) !== ticket) return;
    const after = during.filter((n) => n.seq > read.seq);
    // A gap: the snapshot is read again, and this thread with it if it is in view.
    if (after.some((n, i) => n.seq !== read.seq + i + 1)) return;
    if (!this.state.model.snapshot.threads.some((t) => t.id === thread)) return;
    // The read holds every event through its number; the rest apply on top.
    let view: ThreadView = read;
    for (const { seq, event } of after) if (threadOf(event) === thread) view = { ...applyToView(view, event), seq };
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
 * engine to its first snapshot (`fallback` until then). Their styles and theme apply inside it. */
export function EngineProvider({ store, fallback = null, children }: { store: Store; fallback?: ReactNode; children?: ReactNode }) {
  const ready = useSyncExternalStore(store.subscribe, () => store.get().model !== null);
  const theme = useTheme();
  return createElement(StoreContext.Provider, { value: store }, createElement("div", { className: "chirality", "data-theme": theme }, ready ? children : fallback));
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
