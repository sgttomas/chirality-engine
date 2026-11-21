// The interface's state, followed from what the real engine emitted, ends
// where the engine itself says it is.
import { describe, expect, it } from "vitest";

import { engineData, engineDataNames } from "../testing/replay";
import type { Snapshot, ThreadView } from "../engine/types";
import { applyEvent, type Model } from "./reduce";

/** Codex keeps a turn's items in the order they ended: live, a wait for a subagent begins before
 * the subagent's finishing does, and in its history comes after it. The view is read again when
 * the turn completes; until then the same items, in the order they began, are as good. */
const byId = (view: ThreadView): ThreadView => ({ ...view, turns: view.turns.map((t) => ({ ...t, items: [...t.items].sort((a, b) => a.id.localeCompare(b.id)) })) });
const reordered = new Set(["subagent"]);

describe.each(engineDataNames)("following the engine's events: %s", (name) => {
  it("matches every view the engine read and its final snapshot", () => {
    const data = engineData(name);
    let model: Model = { snapshot: data.before, views: {} };
    const followed = new Set<string>();
    let compared = 0;
    data.events.forEach((event, i) => {
      // A thread started here is followed from its start, as the interface does
      // after starting it; a fork's history comes from reading it.
      if (event.type === "thread" && !model.views[event.thread.id] && !event.thread.forkedFrom) {
        followed.add(event.thread.id);
        model = { ...model, views: { ...model.views, [event.thread.id]: { summary: event.thread, turns: [], seq: 0 } } };
      }
      model = applyEvent(model, event);
      for (const read of data.reads.filter((r) => r.afterEvents === i + 1 && followed.has(r.view.summary.id))) {
        expect(read.view.seq).toBe(data.before.seq + i + 1);
        const view = { ...model.views[read.view.summary.id], seq: read.view.seq };
        if (reordered.has(name)) expect(byId(view)).toEqual(byId(read.view));
        else expect(view).toEqual(read.view);
        compared++;
      }
    });
    expect(data.after.seq).toBe(data.before.seq + data.events.length);
    // The same threads; the list shows them in its own order.
    const sorted = (s: Snapshot) => ({ ...s, threads: [...s.threads].sort((a, b) => a.id.localeCompare(b.id)) });
    expect(sorted({ ...model.snapshot, seq: data.after.seq })).toEqual(sorted(data.after));
    if (name !== "interrupt") expect(compared).toBeGreaterThan(0);
  });
});
