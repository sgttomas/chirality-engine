// What the interface derives from the engine's state.
import type { FileDiff, PendingRequest, Snapshot, ThreadId, ThreadSummary, ThreadView, TurnId, Workflow } from "../engine/types";

/** The one mark a thread shows: what the person most needs to know. */
export type Mark = "waiting" | "running" | "failed" | "finished" | null;

export function markOf(thread: ThreadSummary): Mark {
  switch (thread.status.state) {
    case "needsApproval":
    case "needsAnswer":
      return "waiting";
    case "working":
      return "running";
    case "failed":
      return "failed";
    case "idle":
      return thread.unread ? "finished" : null;
  }
}

export const markLabel: Record<Exclude<Mark, null>, string> = {
  waiting: "Needs you",
  running: "Running",
  failed: "Failed",
  finished: "Finished",
};

export const isRunning = (view: ThreadView | undefined) => view?.turns.some((t) => t.state.state === "running") ?? false;

export const pendingFor = (snapshot: Snapshot, thread: ThreadId | null): PendingRequest | undefined =>
  snapshot.pending.find((p) => p.thread === thread);

/** The workflow most recently picked into the thread. */
export function workflowInUse(view: ThreadView | undefined, workflows: Workflow[] | undefined): Workflow | undefined {
  if (!view || !workflows) return undefined;
  for (const turn of [...view.turns].reverse())
    for (const item of [...turn.items].reverse())
      if (item.body.kind === "userMessage" && item.body.workflow) return workflows.find((w) => w.id === (item.body as { workflow: string }).workflow);
  return undefined;
}

export interface Change extends FileDiff {
  turn: TurnId;
}

/** Every file change the thread made, in order. */
export function changesOf(view: ThreadView | undefined): Change[] {
  if (!view) return [];
  return view.turns.flatMap((turn) =>
    turn.items.flatMap((item) => (item.body.kind === "fileChange" && item.done ? item.body.files.map((f) => ({ ...f, turn: turn.id })) : [])),
  );
}

/** Threads of a project, each fork placed under the thread it came from. */
export function treeOf(threads: ThreadSummary[], project: string): { thread: ThreadSummary; depth: number }[] {
  const mine = threads.filter((t) => t.project === project).sort((a, b) => b.updatedAt - a.updatedAt);
  const ids = new Set(mine.map((t) => t.id));
  const children = new Map<string, ThreadSummary[]>();
  for (const t of mine) if (t.forkedFrom && ids.has(t.forkedFrom)) children.set(t.forkedFrom, [...(children.get(t.forkedFrom) ?? []), t]);
  const out: { thread: ThreadSummary; depth: number }[] = [];
  const visit = (t: ThreadSummary, depth: number) => {
    out.push({ thread: t, depth });
    for (const c of children.get(t.id) ?? []) visit(c, depth + 1);
  };
  for (const t of mine) if (!t.forkedFrom || !ids.has(t.forkedFrom)) visit(t, 0);
  return out;
}

/** "now", "5m", "3h", "2d": how long ago. */
export function ago(seconds: number, now = Date.now() / 1000): string {
  const s = Math.max(0, now - seconds);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/** A path as the person thinks of it: inside the project, relative to it. */
export function shortPath(path: string, root: string | undefined): string {
  if (root && path.startsWith(`${root}/`)) return path.slice(root.length + 1);
  return path;
}

/** Where a workflow comes from, in a word. */
export function sourceName(source: string): string {
  if (source === "project") return "Project";
  const parts = source.split("/").filter(Boolean);
  return (parts.at(-1) === "workflows" ? parts.at(-2) : parts.at(-1)) ?? source;
}
