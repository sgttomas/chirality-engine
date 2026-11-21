// The interface's state, followed from what the real engine emitted, ends
// where the engine itself says it is.
import { describe, expect, it } from "vitest";

import { engineData, engineDataNames } from "../testing/replay";
import { applyEvent, type Model } from "./reduce";

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
        expect({ ...model.views[read.view.summary.id], seq: read.view.seq }).toEqual(read.view);
        compared++;
      }
    });
    expect(data.after.seq).toBe(data.before.seq + data.events.length);
    expect({ ...model.snapshot, seq: data.after.seq }).toEqual(data.after);
    if (name !== "interrupt") expect(compared).toBeGreaterThan(0);
  });
});
