// A folded work line says what happened, a refused call included: the `threads` call the engine
// recorded in its assignment scenario, as Codex answered it and as a refusal.
import { describe, expect, it } from "vitest";

import type { Item } from "../../engine/types";
import { engineData } from "../../testing/replay";
import { workLine } from "./group";

function recordedThreadsCall(): Item {
  const items: Item[] = [];
  const walk = (v: unknown): void => {
    if (Array.isArray(v)) return v.forEach(walk);
    if (!v || typeof v !== "object") return;
    const o = v as Record<string, unknown>;
    const body = o.body as { kind?: string; tool?: string } | undefined;
    if (body?.kind === "threadTool" && body.tool === "threads" && o.done === true) items.push(o as unknown as Item);
    Object.values(o).forEach(walk);
  };
  walk(engineData("assignment"));
  if (!items.length) throw new Error("the assignment scenario has no finished threads call");
  return items[0];
}

describe("a thread tool's work line", () => {
  it("says the threads were listed only when they were", () => {
    const call = recordedThreadsCall();
    expect(workLine(call)).toBe("Listed the threads");
    const refused = { ...call, body: { ...call.body, status: "failed" } } as Item;
    expect(workLine(refused)).toBe("Could not list the threads");
  });
});
