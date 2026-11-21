// What the interface derives about subagents, on what the real engine emitted.
import { describe, expect, it } from "vitest";

import { engineData } from "../testing/replay";
import { nameOf, pendingFor, rowMark, subtreeOf, treeOf } from "./selectors";

describe("subagents in the engine's snapshot", () => {
  const data = engineData("subagent");
  const { threads } = data.after;
  const lead = threads.find((t) => !t.subagent)!;

  it("leave the project's list to their parents, each under the one that started it", () => {
    expect(treeOf(threads, lead.project).map((r) => r.thread.id)).toEqual([lead.id]);
    const below = subtreeOf(threads, lead.id);
    expect(below.map((r) => [nameOf(r.thread), r.depth])).toEqual([
      ["worker", 0],
      ["inner", 1],
    ]);
    expect(below[1].thread.subagent?.parent).toBe(below[0].thread.id);
  });

  it("put a subagent's request before its parent too, after the parent's own", () => {
    const asked = data.events.find((e) => e.type === "pending");
    if (asked?.type !== "pending") throw new Error("no request recorded");
    const worker = subtreeOf(threads, lead.id)[0].thread.id;
    expect(asked.request.thread).toBe(worker);
    const snapshot = { ...data.after, pending: [asked.request] };
    expect(pendingFor(snapshot, lead.id)).toEqual(asked.request);
    expect(pendingFor(snapshot, worker)).toEqual(asked.request);
    const own = { ...asked.request, id: "own", thread: lead.id };
    expect(pendingFor({ ...snapshot, pending: [asked.request, own] }, lead.id)).toEqual(own);
  });

  it("mark a parent's row with the strongest of its own and its subagents', but not their finishing", () => {
    const [worker, inner] = subtreeOf(threads, lead.id).map((r) => r.thread);
    const idle = { ...lead, status: { state: "idle" as const }, unread: false };
    const as = (t: typeof worker, status: typeof worker.status, unread = false) => ({ ...t, status, unread });
    const working = { state: "working" as const };
    const mark = (parent: typeof lead, ...below: (typeof worker)[]) => rowMark(parent, [parent, ...below]);
    expect(mark({ ...idle, status: working }, as(worker, { state: "needsApproval" }), inner)).toBe("waiting");
    expect(mark(idle, as(worker, working), inner)).toBe("running");
    expect(mark(idle, as(worker, { state: "idle" }, true), inner)).toBe(null);
    expect(rowMark(as(worker, working), [idle, as(worker, working), as(inner, { state: "needsAnswer" })])).toBe("running");
  });
});
