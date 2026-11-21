// The store's test transport and data: a transport that delivers as Tauri does (listening takes
// a round trip, events can be lost, a read's answer and events cross), and a small engine state.
import type { Transport } from "../engine/client";
import type { EngineCommand } from "../engine/commands";
import type { Event, Item, Numbered, Snapshot, ThreadSummary } from "../engine/types";

export const summary: ThreadSummary = {
  id: "t",
  project: "p",
  title: "Scope",
  status: { state: "working" },
  unread: false,
  forkedFrom: null,
  subagent: null,
  assignedBy: null,
  updatedAt: 1,
  settings: null,
};

export const snapshot = (seq: number, patch: Partial<Snapshot> = {}): Snapshot => ({
  codex: { state: "ready", version: "0.171.0" },
  account: { state: "signedOut" },
  models: [],
  projects: [{ id: "p", name: "project", path: "/work/project" }],
  threads: [summary],
  pending: [],
  seq,
  ...patch,
});

export const message = (id: string, text: string): Item => ({ id, body: { kind: "agentMessage", text }, done: false });
export const account: Snapshot["account"] = { state: "signedIn", email: null, plan: null };
export const signedIn: Event = { type: "account", account };

export const tick = () => new Promise((r) => setTimeout(r, 0));

/** Answers each command from `answer`; an event emitted before listening is in place is lost. */
export class Wire implements Transport {
  private handler: ((payload: Numbered | { last: number }) => void) | null = null;
  readonly asked: EngineCommand[] = [];
  constructor(private answer: (command: EngineCommand) => Promise<unknown>) {}
  async listen<T>(_event: string, handler: (payload: T) => void) {
    await tick();
    this.handler = handler as unknown as (payload: Numbered | { last: number }) => void;
    return () => void (this.handler = null);
  }
  invoke<T>(command: EngineCommand) {
    this.asked.push(command);
    return this.answer(command) as Promise<T>;
  }
  emit(seq: number, event: Event) {
    this.handler?.({ seq, event });
  }
  /** The host's mark: it sent every event through `last`. */
  mark(last: number) {
    this.handler?.({ last });
  }
}
