// Groups a turn's items for showing: the agent's work folds between what it says.
import type { Item } from "../../engine/types";

export type Block =
  | { kind: "item"; item: Item }
  /** Consecutive reasoning, commands, tool calls and searches. */
  | { kind: "work"; items: Item[] };

const isWork = (item: Item) => ["reasoning", "command", "toolCall", "webSearch"].includes(item.body.kind);

export function blocksOf(items: Item[]): Block[] {
  const blocks: Block[] = [];
  for (const item of items) {
    const last = blocks[blocks.length - 1];
    if (isWork(item) && last?.kind === "work") last.items.push(item);
    else if (isWork(item)) blocks.push({ kind: "work", items: [item] });
    else blocks.push({ kind: "item", item });
  }
  return blocks;
}

/** One line for a piece of work. */
export function workLine(item: Item): string {
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
  const tools = count("toolCall");
  if (tools) parts.push(`used ${tools} tool${tools > 1 ? "s" : ""}`);
  const searches = count("webSearch");
  if (searches) parts.push(`searched ${searches === 1 ? "once" : `${searches} times`}`);
  const text = parts.join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}
