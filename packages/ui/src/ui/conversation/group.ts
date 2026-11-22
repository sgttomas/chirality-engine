// Groups a turn's items for showing: the agent's work folds between what it says, and an
// item the application shows its own way stands on its own.
import type { ReactNode } from "react";

import type { Item, ThreadId } from "../../engine/types";
import type { ItemViewer } from "./itemView";

export type Block =
  | { kind: "item"; item: Item }
  /** An item the application's view gave a node. */
  | { kind: "view"; item: Item; node: ReactNode }
  /** Consecutive reasoning, commands, tool calls, searches, waits for subagents, and listings and reads of threads. */
  | { kind: "work"; items: Item[] };

const isWork = (item: Item) =>
  ["reasoning", "command", "toolCall", "webSearch", "subagentTool"].includes(item.body.kind) || (item.body.kind === "threadTool" && (item.body.tool === "threads" || item.body.tool === "read"));

const shown = ["userMessage", "agentMessage", "plan", "fileChange", "subagent", "threadTool", "error"];

/** The blocks to show; `nodes` holds the application's node for each item, by index. An item
 * with no node and no standard view is left out, so it neither splits a fold nor ends one. */
export function blocksOf(items: Item[], nodes: readonly ReturnType<ItemViewer>[] = []): Block[] {
  const blocks: Block[] = [];
  items.forEach((item, i) => {
    const node = nodes[i];
    const last = blocks[blocks.length - 1];
    if (node != null) blocks.push({ kind: "view", item, node });
    else if (isWork(item) && last?.kind === "work") last.items.push(item);
    else if (isWork(item)) blocks.push({ kind: "work", items: [item] });
    else if (shown.includes(item.body.kind)) blocks.push({ kind: "item", item });
  });
  return blocks;
}

/** One line for a piece of work; `name` names a thread it worked with. */
export function workLine(item: Item, name: (id: ThreadId | null) => string = () => "a thread"): string {
  const b = item.body;
  switch (b.kind) {
    case "reasoning":
      return b.summary.split(/(?<=\.)\s/)[0] || "Thinking";
    case "command":
      return b.command;
    case "toolCall":
      return b.server ? `${b.server} · ${b.tool}` : b.tool;
    case "webSearch":
      return `Searched “${b.query}”`;
    case "subagentTool":
      if (b.tool === "wait") return b.status === "running" ? "Waiting for subagents" : "Waited for subagents";
      if (b.tool === "spawnAgent" && b.status === "failed") return "Could not start a subagent";
      return b.tool;
    case "threadTool":
      if (b.tool === "threads") return b.status === "failed" ? "Could not list the threads" : "Listed the threads";
      if (b.tool === "read") return b.status === "failed" ? `Could not read ${name(b.thread)}` : b.status === "running" ? `Reading ${name(b.thread)}` : `Read ${name(b.thread)}`;
      return b.status === "failed" ? `Could not ${b.tool}` : b.tool;
    default:
      return "";
  }
}

/** "Thought, ran 3 commands": what a folded block holds. */
export function workSummary(items: Item[]): string {
  const count = (kind: string) => items.filter((i) => i.body.kind === kind).length;
  const parts: string[] = [];
  if (count("reasoning")) parts.push("Thought");
  const commands = count("command");
  if (commands) parts.push(`ran ${commands} command${commands > 1 ? "s" : ""}`);
  const waits = items.filter((i) => i.body.kind === "subagentTool" && i.body.tool === "wait").length;
  const tools = count("toolCall") + count("subagentTool") + count("threadTool") - waits;
  if (tools) parts.push(`used ${tools} tool${tools > 1 ? "s" : ""}`);
  if (waits) parts.push("waited for subagents");
  const searches = count("webSearch");
  if (searches) parts.push(`searched ${searches === 1 ? "once" : `${searches} times`}`);
  const text = parts.join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}
