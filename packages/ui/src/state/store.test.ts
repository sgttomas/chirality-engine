// The store against a transport that delivers as Tauri does: listening takes a
// round trip, events can be lost, and a read's answer and events cross.
import { describe, expect, it, vi } from "vitest";

import { createEngineClient, type Transport } from "../engine/client";
import type { EngineCommand } from "../engine/commands";
import type { Event, Item, Numbered, Snapshot, ThreadSummary, ThreadView } from "../engine/types";
import { engineData } from "../testing/replay";
import { noticeFor } from "../ui/notify";
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

  it("applies the events after a read's number, and none it holds", async () => {
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
    // The read holds 1 and 2 (its number is 2); 3 and 4 came after.
    wire.emit(1, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: " world" });
    wire.emit(2, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: "!" });
    wire.emit(3, { type: "item", thread: "t", turn: "turn-1", item: message("b", "next") });
    wire.emit(4, { type: "itemDelta", thread: "t", turn: "turn-1", item: "b", kind: "agentMessage", text: " one" });
    answerRead({ summary, turns: [{ id: "turn-1", items: [message("a", "hello world!")], state: { state: "running" } }], seq: 2 });
    await selecting;
    const items = store.get().model!.views.t.turns[0].items;
    expect(items.map((i) => (i.body.kind === "agentMessage" ? i.body.text : ""))).toEqual(["hello world!", "next one"]);
    wire.emit(5, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: "?" });
    expect(store.get().model!.views.t.turns[0].items[0].body).toEqual({ kind: "agentMessage", text: "hello world!?" });
  });
});

describe("the store recovers what it missed, and starts and stops cleanly", () => {
  const turn = "turn-1";
  const ended = { type: "turnEnded", thread: "t", turn, state: { state: "interrupted" } } as const;

  it("keeps a chunk received during a read when Stop ends the turn without finishing the item", async () => {
    let answerRead: (view: ThreadView) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(0);
      if (command === "read_thread") return new Promise((resolve) => (answerRead = resolve));
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.emit(1, { type: "item", thread: "t", turn, item: message("a", "") });
    const reading = store.selectThread("t");
    wire.emit(2, { type: "itemDelta", thread: "t", turn, item: "a", kind: "agentMessage", text: "partial response" });
    // The engine's read holds the live item through event 1.
    answerRead({ summary, turns: [{ id: turn, items: [message("a", "")], state: { state: "running" } }], seq: 1 });
    await reading;
    wire.emit(3, ended);
    expect(store.get().model?.views.t.turns[0].items[0].body).toEqual({ kind: "agentMessage", text: "partial response" });
    expect(store.get().model?.views.t.turns[0].state.state).toBe("interrupted");
  });

  it("reads again when the host marks an event it never received: a lost last turnEnded", async () => {
    let seq = 0;
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(seq);
      if (command === "read_thread") return { summary, seq, turns: [{ id: turn, state: { state: seq === 2 ? "interrupted" : "running" }, items: [] }] };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.emit(++seq, { type: "turnStarted", thread: "t", turn });
    await store.selectThread("t");
    ++seq; // The engine sent turnEnded; it never arrived.
    wire.mark(seq);
    for (let i = 0; i < 3; i++) await tick();
    expect(store.get().model?.views.t.turns[0].state.state).toBe("interrupted");
    expect(store.get().model?.snapshot.seq).toBe(2);
  });

  it("recovers from a second gap while the snapshot of the first is read", async () => {
    let reads = 0;
    let answerSnapshot: (s: Snapshot) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      reads++;
      if (reads === 1) return snapshot(0);
      if (reads === 2) return new Promise((resolve) => (answerSnapshot = resolve));
      return snapshot(4, { account });
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.emit(2, { type: "codex", status: { state: "starting" } });
    wire.emit(4, signedIn);
    answerSnapshot(snapshot(2));
    // The snapshot misses 3: it is read again after a moment.
    await vi.waitFor(() => expect(store.get().model?.snapshot.seq).toBe(4), { timeout: 1000 });
    expect(reads).toBe(3);
    expect(store.get().model?.snapshot.account.state).toBe("signedIn");
  });

  it("keeps the latest read of a thread when an earlier one answers last", async () => {
    const answers: Array<(v: ThreadView) => void> = [];
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(0);
      if (command === "read_thread") return new Promise((resolve) => answers.push(resolve));
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const older = store.selectThread("t");
    const newer = store.reload("t");
    answers[1]({ seq: 0, summary, turns: [{ id: turn, items: [message("a", "new")], state: { state: "running" } }] });
    await newer;
    answers[0]({ seq: 0, summary, turns: [] });
    await older;
    expect(store.get().model?.views.t.turns).toHaveLength(1);
  });

  it("starts once however often it is asked, and again after it is disposed (React's StrictMode)", async () => {
    const answers: Array<(s: Snapshot) => void> = [];
    const wire = new Wire(async (command) => (command === "snapshot" ? new Promise<Snapshot>((resolve) => answers.push(resolve)) : []));
    const store = new Store(createEngineClient(wire), testHost);
    expect(store.start()).toBe(store.start());
    const first = store.start();
    store.dispose();
    const second = store.start();
    await tick();
    await tick();
    // The first start was dropped before it read the snapshot; the second reads it.
    expect(answers).toHaveLength(1);
    answers[0](snapshot(0));
    await first;
    await second;
    wire.emit(1, signedIn);
    expect(store.get().model?.snapshot.seq).toBe(1);
    expect(store.get().model?.snapshot.account.state).toBe("signedIn");
  });

  it("drops a read the store was disposed during", async () => {
    let answerRead: (view: ThreadView) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(0);
      if (command === "read_thread") return new Promise((resolve) => (answerRead = resolve));
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const reading = store.selectThread("t");
    store.dispose();
    answerRead({ seq: 0, summary, turns: [] });
    await reading;
    expect(store.get().model?.views.t).toBeUndefined();
  });

  it("tells onEvent the events that arrived while it read the snapshot again", async () => {
    const answers: Array<(s: Snapshot) => void> = [];
    let first = true;
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      if (first) {
        first = false;
        return snapshot(0);
      }
      return new Promise<Snapshot>((resolve) => answers.push(resolve));
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const listener = vi.fn();
    store.onEvent(listener);
    wire.emit(2, signedIn);
    const background = { type: "thread", thread: { ...summary, status: { state: "needsApproval" } } } as const;
    wire.emit(3, background);
    answers[0](snapshot(2));
    await tick();
    expect(store.get().model?.snapshot.threads[0].status.state).toBe("needsApproval");
    expect(listener).toHaveBeenCalledWith(background, expect.anything());
  });
});

describe("the store's views and recovery, on the engine's recorded data", () => {
  it("leaves a view read after an event as it is when that event arrives late: no doubled text", async () => {
    const data = engineData("text_turn");
    const read = data.reads[0].view;
    const at = data.events.findIndex((e) => e.type === "itemDelta");
    const delta = data.events[at];
    if (delta.type !== "itemDelta") throw new Error("expected a recorded delta");
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return { ...data.before, seq: data.before.seq + at, projects: data.after.projects, threads: [read.summary] };
      if (command === "read_thread") return read;
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread(read.summary.id);
    wire.emit(data.before.seq + at + 1, delta);
    const shown = store.get().model?.views[read.summary.id].turns[0].items.find((i) => i.id === delta.item);
    expect(shown?.body).toEqual(read.turns[0].items.find((i) => i.id === delta.item)?.body);
  });

  it("leaves a view read after a delta to an item still streaming as it is when the delta arrives late", async () => {
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(4);
      // The engine had sent 5 when the read began; the store had not yet received it.
      if (command === "read_thread") return { summary, seq: 5, turns: [{ id: "turn-1", items: [message("a", "Half")], state: { state: "running" } }] };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread("t");
    wire.emit(5, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: "Half" });
    wire.emit(6, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: " more" });
    expect(store.get().model?.views.t.turns[0].items[0].body).toEqual({ kind: "agentMessage", text: "Half more" });
    expect(store.get().model?.views.t.seq).toBe(6);
  });

  it("takes no text for an item the read has as done, from an event numbered after the read", async () => {
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(4);
      // The read began after 4; Codex finished the item before it answered.
      if (command === "read_thread") return { summary, seq: 4, turns: [{ id: "turn-1", items: [{ ...message("a", "Whole"), done: true }], state: { state: "running" } }] };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread("t");
    wire.emit(5, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: "Whole" });
    expect(store.get().model?.views.t.turns[0].items[0].body).toEqual({ kind: "agentMessage", text: "Whole" });
  });

  it("tells onEvent an approval that revealed a gap, though the snapshot holds it", async () => {
    const data = engineData("command_approved");
    const at = data.events.findIndex((e) => e.type === "thread" && e.thread.status.state === "needsApproval");
    const approval = data.events[at];
    if (approval.type !== "thread") throw new Error("expected a recorded approval");
    const working = data.events.slice(0, at).reverse().find((e) => e.type === "thread" && e.thread.id === approval.thread.id && e.thread.status.state === "working");
    if (working?.type !== "thread") throw new Error("expected a recorded running thread");
    let reads = 0;
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      if (reads++ === 0) return { ...data.before, seq: data.before.seq + at - 1, projects: data.after.projects, threads: [working.thread] };
      return { ...data.before, seq: data.before.seq + at + 1, projects: data.after.projects, threads: [approval.thread] };
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const notices: unknown[] = [];
    store.onEvent((event, before) => {
      const notice = noticeFor(event, before, false);
      if (notice) notices.push(notice);
    });
    wire.emit(data.before.seq + at + 1, approval);
    await tick();
    expect(store.get().model?.snapshot.threads[0].status.state).toBe("needsApproval");
    expect(notices).toHaveLength(1);
  });

  it("leaves a store started again alone when an earlier recovery fails", async () => {
    let reads = 0;
    let reject: (e: Error) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      if (++reads === 2) return new Promise((_, refuse) => (reject = refuse));
      return snapshot(reads === 1 ? 0 : 2);
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.mark(2);
    store.dispose();
    await store.start();
    reject(new Error("disposed transport"));
    await tick();
    expect(store.get().error).toBeNull();
  });

  it("keeps an earlier recovery from clearing what a new start holds", async () => {
    let calls = 0;
    let rejectOld: (error: Error) => void = () => {};
    let resolveNew: (value: Snapshot) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      switch (++calls) {
        case 1:
          return snapshot(0);
        case 2:
          return new Promise((_, reject) => (rejectOld = reject));
        case 3:
          return new Promise((resolve) => (resolveNew = resolve));
        default:
          throw new Error("unexpected snapshot");
      }
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.mark(2);
    store.dispose();
    const starting = store.start();
    await tick();
    rejectOld(new Error("the old snapshot, cancelled with its transport"));
    await tick();
    resolveNew(snapshot(2));
    await expect(starting).resolves.toBeUndefined();
    expect(store.get().model?.snapshot.seq).toBe(2);
  });

  it("tries a failed recovery again while the host has marked events not received", async () => {
    let calls = 0;
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      switch (++calls) {
        case 1:
          return snapshot(0);
        case 2:
          throw new Error("temporary snapshot failure");
        default:
          return snapshot(2);
      }
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.mark(2);
    await tick();
    expect(store.get().error).toBe("temporary snapshot failure");
    await new Promise((resolve) => setTimeout(resolve, 600));
    expect(store.get().model?.snapshot.seq).toBe(2);
    expect(store.get().error).toBeNull();
  });
});

describe("recovery keeps what it received, and backs off", () => {
  it("tells an approval received during a recovery whose first snapshot fails", async () => {
    const data = engineData("command_approved");
    const at = data.events.findIndex((e) => e.type === "thread" && e.thread.status.state === "needsApproval");
    const approval = data.events[at];
    if (approval.type !== "thread") throw new Error("expected a recorded approval");
    const working = data.events.slice(0, at).reverse().find((e) => e.type === "thread" && e.thread.id === approval.thread.id && e.thread.status.state === "working");
    if (working?.type !== "thread") throw new Error("expected a recorded running thread");
    let calls = 0;
    const base = { ...data.before, projects: data.after.projects, threads: [working.thread], seq: data.before.seq + at - 1 };
    const latest = { ...base, threads: [approval.thread], seq: base.seq + 2 };
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      if (++calls === 1) return base;
      if (calls === 2) throw new Error("temporary snapshot failure");
      return latest;
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const notices: unknown[] = [];
    store.onEvent((event, before) => {
      const notice = noticeFor(event, before, false);
      if (notice) notices.push(notice);
    });
    wire.emit(latest.seq, approval);
    wire.mark(latest.seq);
    await vi.waitFor(() => expect(store.get().model?.snapshot.seq).toBe(latest.seq), { timeout: 1000 });
    store.dispose();
    expect(notices).toHaveLength(1);
  });

  it("backs off while the host returns a snapshot behind its mark", async () => {
    let count = 0;
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      return snapshot(++count < 20 ? 0 : 2);
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.mark(2);
    await tick();
    store.dispose();
    expect(count).toBeLessThan(5);
  });

  it("leaves a previous generation's recovery from clearing a new one's recovery error", async () => {
    let calls = 0;
    let answerOld: (value: Snapshot) => void = () => {};
    const wire = new Wire(async (command) => {
      if (command !== "snapshot") return [];
      switch (++calls) {
        case 1:
          return snapshot(0);
        case 2:
          return new Promise((resolve) => (answerOld = resolve));
        case 3:
          return snapshot(2);
        default:
          throw new Error("the new recovery failed");
      }
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    wire.mark(2);
    store.dispose();
    await store.start();
    wire.mark(4);
    await tick();
    expect(store.get().error).toBe("the new recovery failed");
    answerOld(snapshot(2));
    await tick();
    const error = store.get().error;
    store.dispose();
    expect(error).toBe("the new recovery failed");
  });
});

describe("a view and the turn running in it", () => {
  it("keeps an item finished when its start event arrives after the read", async () => {
    const data = engineData("text_turn");
    const read = data.reads[0].view;
    const at = data.events.findIndex((e) => e.type === "item" && e.item.body.kind === "agentMessage" && !e.item.done);
    const started = data.events[at];
    if (started.type !== "item") throw new Error("expected a recorded item start");
    const initial = { ...data.before, seq: data.before.seq + at, projects: data.after.projects, threads: [read.summary] };
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return initial;
      if (command === "read_thread") return { ...read, seq: initial.seq };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread(read.summary.id);
    wire.emit(initial.seq + 1, started);
    const delta = data.events[at + 1];
    if (delta.type !== "itemDelta") throw new Error("expected a recorded delta");
    wire.emit(initial.seq + 2, delta);
    const item = store.get().model?.views[read.summary.id].turns[0].items.find((i) => i.id === started.item.id);
    store.dispose();
    expect(item).toEqual(read.turns[0].items.find((i) => i.id === started.item.id));
  });

  // The engine's own reads and events against the pinned Codex and its stub, a turn running while
  // the thread is read (captured for the fourth review of the engine; the project path made fixed).
  interface Overlap {
    initial: Snapshot;
    view: ThreadView;
    frames: Numbered[];
  }
  const overlaps = import.meta.glob<Overlap>("../testing/overlap-data/*.json", { eager: true, import: "default" });
  const overlap = (name: string) => structuredClone(overlaps[`../testing/overlap-data/${name}.json`]);

  it.each(["legacy", "paginated"])("shows one exchange when a turn of a %s thread completes during a read", async (mode) => {
    const { initial, view, frames } = overlap(mode);
    const answers: Array<(value: ThreadView) => void> = [];
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return initial;
      if (command === "read_thread") return new Promise((resolve) => answers.push(resolve));
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    const reading = store.selectThread(view.summary.id);
    await tick();
    for (const frame of frames) wire.emit(frame.seq, frame.event);
    // The first read is Codex's history from before the turn's events; each after the turn's end, from after them.
    answers.forEach((answer, i) => answer(i === 0 ? view : { ...view, seq: frames.at(-1)!.seq }));
    await reading;
    await tick();
    const shown = store.get().model?.views[view.summary.id].turns[0].items.map((item) => item.body.kind);
    store.dispose();
    expect(answers.length).toBeGreaterThan(1);
    expect(shown).toEqual(["userMessage", "agentMessage"]);
  });

  it("keeps a stopped reply's streamed text in the view: a stopped turn is not read again", async () => {
    let reads = 0;
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return snapshot(0);
      if (command === "read_thread") return reads++ === 0 ? { summary, seq: 0, turns: [{ id: "turn-1", items: [], state: { state: "running" } }] } : { summary, seq: 4, turns: [] };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread("t");
    wire.emit(1, { type: "item", thread: "t", turn: "turn-1", item: message("a", "") });
    wire.emit(2, { type: "itemDelta", thread: "t", turn: "turn-1", item: "a", kind: "agentMessage", text: "A partial" });
    // An item the view only has from its text.
    wire.emit(3, { type: "itemDelta", thread: "t", turn: "turn-1", item: "b", kind: "agentMessage", text: " reply" });
    wire.emit(4, { type: "turnEnded", thread: "t", turn: "turn-1", state: { state: "interrupted" } });
    await tick();
    const turn = store.get().model?.views.t.turns[0];
    store.dispose();
    expect(reads).toBe(1);
    expect(turn?.state.state).toBe("interrupted");
    expect(turn?.items.map((i) => i.body)).toEqual([
      { kind: "agentMessage", text: "A partial" },
      { kind: "agentMessage", text: " reply" },
    ]);
  });

  it("streams text into a thread opened mid-reply, and settles on the turn's end", async () => {
    const { initial, view, frames } = overlap("streaming");
    let reads = 0;
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return initial;
      if (command === "read_thread") return reads++ === 0 ? view : { ...view, seq: frames.at(-1)!.seq };
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread(view.summary.id);
    const after = frames.filter((n) => n.seq > view.seq);
    const agent = () =>
      store
        .get()
        .model?.views[view.summary.id].turns[0].items.flatMap((item) => (item.body.kind === "agentMessage" ? [item.body.text] : []));
    const delta = after.findIndex((n) => n.event.type === "itemDelta");
    for (const frame of after.slice(0, delta + 1)) wire.emit(frame.seq, frame.event);
    expect(agent()).toEqual([" Second half."]);
    for (const frame of after.slice(delta + 1)) wire.emit(frame.seq, frame.event);
    expect(agent()).toEqual(["First half. Second half."]);
    await tick();
    expect(reads).toBe(2);
    store.dispose();
  });
});
