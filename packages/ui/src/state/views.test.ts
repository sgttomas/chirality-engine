// A thread's view while a turn runs in it, on data the engine produced against the pinned Codex.
import { describe, expect, it } from "vitest";

import { createEngineClient } from "../engine/client";
import type { Numbered, Snapshot, ThreadView } from "../engine/types";
import { engineData } from "../testing/replay";
import { testHost } from "../testing/render";
import { Store } from "./store";
import { Wire, message, snapshot, summary, tick } from "./store.test-helpers";

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

  it("keeps the checklist Codex sent live when it reads a completed turn without it", async () => {
    // Captured with tools.update_plan.enabled: the plan is in the events, not in Codex's history.
    const { initial, firstRead, view, frames } = overlap("plan") as Overlap & { firstRead: ThreadView };
    const thread = view.summary.id;
    let reads = 0;
    const wire = new Wire(async (command) => {
      if (command === "snapshot") return initial;
      if (command === "read_thread") return reads++ === 0 ? firstRead : view;
      return [];
    });
    const store = new Store(createEngineClient(wire), testHost);
    await store.start();
    await store.selectThread(thread);
    const kinds = () => store.get().model!.views[thread].turns.at(-1)!.items.map((i) => i.body.kind);
    for (const n of frames) wire.emit(n.seq, n.event);
    await tick();
    const shown = store.get().model!.views[thread].turns.at(-1)!;
    const plan = frames.find((n) => n.event.type === "item" && n.event.item.body.kind === "plan")!.event;
    store.dispose();
    expect(reads).toBe(2);
    expect(view.turns.at(-1)!.items.some((i) => i.body.kind === "plan")).toBe(false);
    expect(kinds()).toEqual(["userMessage", "plan", "agentMessage"]);
    expect(shown.items[1]).toEqual(plan.type === "item" && plan.item);
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
