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

/** What waits on the person in the thread: its own request first, else one of its subagents'. */
export function pendingFor(snapshot: Snapshot, thread: ThreadId | null): PendingRequest | undefined {
  if (!thread) return undefined;
  const own = snapshot.pending.find((p) => p.thread === thread);
  if (own) return own;
  const below = new Set(subtreeOf(snapshot.threads, thread).map((r) => r.thread.id));
  return snapshot.pending.find((p) => below.has(p.thread));
}

/** A thread row's mark: a subagent's own; any other thread's, the strongest of its own and its
 * subagents' (whose finishing is their parent's to tell). */
export function rowMark(thread: ThreadSummary, threads: ThreadSummary[]): Mark {
  if (thread.subagent) return markOf(thread);
  const below = subtreeOf(threads, thread.id).map((r) => markOf(r.thread)).filter((m) => m !== "finished");
  return strongest([markOf(thread), ...below]);
}

/** A thread's name: its title, a subagent's task name, or "New thread". */
export function nameOf(thread: ThreadSummary): string {
  return thread.title ?? thread.subagent?.path.split("/").filter(Boolean).at(-1) ?? "New thread";
}

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

/** Threads of a project, each fork placed under the thread it came from; subagents are under
 * their parents (`subtreeOf`), not here. */
export function treeOf(threads: ThreadSummary[], project: string): { thread: ThreadSummary; depth: number }[] {
  const mine = threads.filter((t) => t.project === project && !t.subagent).sort((a, b) => b.updatedAt - a.updatedAt);
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

/** A project's threads as `treeOf` gives them, searched by name: a thread is in it if it or one of its
 * subagents matches; `found` holds the subagents that match, to be shown. */
export function searchTree(threads: ThreadSummary[], project: string, query: string): { rows: { thread: ThreadSummary; depth: number }[]; found: Set<ThreadId> } {
  const q = query.trim().toLowerCase();
  const rows = treeOf(threads, project);
  if (!q) return { rows, found: new Set() };
  const matches = (t: ThreadSummary) => nameOf(t).toLowerCase().includes(q);
  const found = new Set(threads.filter((t) => t.subagent && t.project === project && matches(t)).map((t) => t.id));
  return { rows: rows.filter((r) => matches(r.thread) || subtreeOf(threads, r.thread.id).some((s) => found.has(s.thread.id))), found };
}

/** A thread's subagents at every depth, each under the one that started it, in the order they started. */
export function subtreeOf(threads: ThreadSummary[], parent: ThreadId): { thread: ThreadSummary; depth: number }[] {
  const out: { thread: ThreadSummary; depth: number }[] = [];
  const visit = (id: ThreadId, depth: number, seen: Set<ThreadId>) => {
    const children = threads.filter((t) => t.subagent?.parent === id && !seen.has(t.id)).sort((a, b) => a.id.localeCompare(b.id, undefined, { numeric: true }));
    for (const child of children) {
      seen.add(child.id);
      out.push({ thread: child, depth });
      visit(child.id, depth + 1, seen);
    }
  };
  visit(parent, 0, new Set([parent]));
  return out;
}

/** The mark a collapsed group shows: the strongest of its threads'. */
export function strongestMark(threads: ThreadSummary[]): Mark {
  return strongest(threads.map(markOf));
}

function strongest(marks: Mark[]): Mark {
  return (["waiting", "failed", "finished", "running"] as const).find((m) => marks.includes(m)) ?? null;
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
