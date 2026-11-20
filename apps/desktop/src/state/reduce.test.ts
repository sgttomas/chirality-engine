import { describe, expect, it } from "vitest";

import { FakeEngine } from "../engine/fake/engine";
import { bridge } from "../engine/fake/fixtures";
import type { Answer, Event, Request, TurnSettings } from "../engine/types";
import { applyEvent, type Model } from "./reduce";

const settings: TurnSettings = { model: "gpt-5.5", effort: "high", access: "ask" };
const yes = (r: Request): Answer => (r.kind === "question" ? { kind: "answers", answers: [[r.questions[0].id, r.questions[0].options[0]]] } : { kind: "allow" });

/** Follows an engine by events only, as the interface does. */
async function follow(engine: FakeEngine) {
  const events: Event[] = [];
  engine.subscribe((e) => events.push(e));
  let model: Model = { snapshot: await engine.snapshot(), views: {} };
  return {
    events,
    async read(thread: string) {
      model = { ...model, views: { ...model.views, [thread]: await engine.readThread(thread) } };
      events.length = 0;
    },
    model() {
      for (const e of events.splice(0)) model = applyEvent(model, e);
      return model;
    },
  };
}

describe("following the engine's events", () => {
  it("ends with exactly what the engine holds, through streaming, approvals, questions and steering", async () => {
    const engine = new FakeEngine({ instant: true, autoAnswer: yes });
    engine.seedAccount({ state: "signedIn", email: null, plan: null });
    engine.seedProject(bridge);
    const follower = await follow(engine);

    const thread = await engine.startThread(bridge.id, { text: "Draft the scope of work.", attachments: [], workflow: "wf-scope" }, settings);
    await follower.read(thread);
    await engine.send(thread, { text: "Keep it short.", attachments: [], workflow: null }, settings);
    await engine.settled(thread);
    await engine.send(thread, { text: "Now rerun the estimate.", attachments: [], workflow: null }, settings);
    await engine.settled(thread);

    const model = follower.model();
    expect(model.views[thread]).toEqual(await engine.readThread(thread));
    expect(model.snapshot).toEqual(await engine.snapshot());
  });

  it("keeps a question pending until it is answered, then clears it", async () => {
    const engine = new FakeEngine({ instant: true, autoAnswer: (r) => (r.kind === "question" ? null : { kind: "allow" }) });
    engine.seedProject(bridge);
    const follower = await follow(engine);
    const thread = await engine.startThread(bridge.id, { text: "Rerun the estimate.", attachments: [], workflow: null }, settings);
    await new Promise((r) => setTimeout(r, 0));

    const waiting = follower.model();
    expect(waiting.snapshot.pending).toHaveLength(1);
    expect(waiting.snapshot.threads[0].status).toEqual({ state: "needsAnswer" });

    await engine.answer(waiting.snapshot.pending[0].id, { kind: "answers", answers: [["class", "Class 3 (budget)"]] });
    await engine.settled(thread);
    const done = follower.model();
    expect(done.snapshot.pending).toHaveLength(0);
    expect(done.snapshot.threads[0].status).toEqual({ state: "idle" });
  });
});
