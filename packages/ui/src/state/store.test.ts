// The store against a transport that delivers as Tauri does: listening takes a
// round trip, events can be lost, and a read's answer and events cross.
import { describe, expect, it } from "vitest";

import { createEngineClient, type Transport } from "../engine/client";
import type { EngineCommand } from "../engine/commands";
import type { Event, Item, Numbered, Snapshot, ThreadSummary, ThreadView } from "../engine/types";
import { Store } from "./store";
import { testHost } from "../testing/render";

const summary: ThreadSummary = {
  id: "t",
  project: "p",
  title: "Scope",
  status: { state: "working" },
  unread: false,
  forkedFrom: null,
  updatedAt: 1,
  settings: null,
};

const snapshot = (seq: number, patch: Partial<Snapshot> = {}): Snapshot => ({
  codex: { state: "ready", version: "0.171.0" },
  account: { state: "signedOut" },
  models: [],
  projects: [{ id: "p", name: "project", path: "/work/project" }],
  threads: [summary],
  pending: [],
  seq,
  ...patch,
});

const message = (id: string, text: string): Item => ({ id, body: { kind: "agentMessage", text }, done: false });
const account: Snapshot["account"] = { state: "signedIn", email: null, plan: null };
const signedIn: Event = { type: "account", account };

const tick = () => new Promise((r) => setTimeout(r, 0));

/** Answers each command from `answer`; an event emitted before listening is in place is lost. */
class Wire implements Transport {
  private handler: ((n: Numbered) => void) | null = null;
  readonly asked: EngineCommand[] = [];
  constructor(private answer: (command: EngineCommand) => Promise<unknown>) {}
  async listen<T>(_event: string, handler: (payload: T) => void) {
    await tick();
    this.handler = handler as unknown as (n: Numbered) => void;
    return () => void (this.handler = null);
  }
  invoke<T>(command: EngineCommand) {
    this.asked.push(command);
    return this.answer(command) as Promise<T>;
  }
  emit(seq: number, event: Event) {
    this.handler?.({ seq, event });
  }
}

describe("the store follows the engine's numbered events", () => {
  it("listens before it reads the snapshot", async () => {
    const wire: Wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      // The engine moves on just after answering.
      queueMicrotask(() => wire.emit(1, signedIn));
      return snapshot(0);
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await tick();
    expect(store.get().model!.snapshot.account.state).toBe("signedIn");
    expect(store.get().model!.snapshot.seq).toBe(1);
  });

  it("reads the snapshot and the thread in view again after a gap", async () => {
    let reads = 0;
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return wire.asked.filter((c) => c === "snapshot").length === 1 ? snapshot(0) : snapshot(3, { account });
      if (command === "read_thread") return { summary, turns: [], seq: reads++ === 0 ? 0 : 3 } satisfies ThreadView;
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread("t");
    wire.emit(1, { type: "turnStarted", thread: "t", turn: "turn-1" });
    // 2 is lost.
    wire.emit(3, { type: "turnEnded", thread: "t", turn: "turn-1", state: { state: "completed" } });
    for (let i = 0; i < 5; i++) await tick();
    expect(wire.asked.filter((c) => c === "snapshot")).toHaveLength(2);
    expect(wire.asked.filter((c) => c === "read_thread")).toHaveLength(2);
    expect(store.get().model!.snapshot.account.state).toBe("signedIn");
    expect(store.get().model!.snapshot.seq).toBe(3);
    expect(store.get().thread).toBe("t");
    expect(store.get().model!.views.t.seq).toBe(3);
  });

  it("keeps the events a read missed and not those it holds", async () => {
    let answerRead: (view: ThreadView) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(0);
      if (command === "read_thread") return new Promise((resolve) => (answerRead = resolve));
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const selecting = store.selectThread("t");
    await tick();
    // The read began after 1 and holds it, and 2 as well (Codex answered after it); 3 and 4 came after.
    wire.emit(1, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", text: " world" });
    wire.emit(2, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", text: "!" });
    wire.emit(3, { type: "item", thread: "t", turn: "turn-1", item: message("b", "next") });
    wire.emit(4, { type: "itemDelta", thread: "t", turn: "turn-1", item: "b", text: " one" });
    answerRead({ summary, turns: [{ id: "turn-1", items: [message("a", "hello world!")], state: { state: "running" } }], seq: 1 });
    await selecting;
    const items = store.get().model!.views.t.turns[0].items;
    expect(items.map((i) => (i.body.kind === "agentMessage" ? i.body.text : ""))).toEqual(["hello world!", "next one"]);
    wire.emit(5, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", text: "?" });
    expect(store.get().model!.views.t.turns[0].items[0].body).toEqual({ kind: "agentMessage", text: "hello world!?" });
  });
});
