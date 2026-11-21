// Reads a unified diff into lines that can be shown with their numbers.

export interface DiffLine {
  kind: "add" | "del" | "context" | "hunk";
  text: string;
  old?: number;
  new?: number;
}

export interface ParsedDiff {
  lines: DiffLine[];
  added: number;
  removed: number;
  /** The file did not exist before. */
  created: boolean;
}

export function parseDiff(diff: string): ParsedDiff {
  // A new file: Codex gives its content rather than a diff.
  if (!/^@@ /m.test(diff)) {
    const text = diff.replace(/\n$/, "").split("\n");
    return { lines: text.map((t, i) => ({ kind: "add", text: t, new: i + 1 })), added: text.length, removed: 0, created: true };
  }
  const lines: DiffLine[] = [];
  let added = 0;
  let removed = 0;
  let created = false;
  let oldNo = 0;
  let newNo = 0;
  for (const raw of diff.replace(/\n$/, "").split("\n")) {
    if (raw.startsWith("--- ")) {
      created = raw.includes("/dev/null");
      continue;
    }
    if (raw.startsWith("+++ ") || raw.startsWith("diff ") || raw.startsWith("index ")) continue;
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(raw);
    if (hunk) {
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[2]);
      lines.push({ kind: "hunk", text: hunk[3].trim() });
    } else if (raw.startsWith("+")) {
      lines.push({ kind: "add", text: raw.slice(1), new: newNo++ });
      added++;
    } else if (raw.startsWith("-")) {
      lines.push({ kind: "del", text: raw.slice(1), old: oldNo++ });
      removed++;
    } else if (!raw.startsWith("\\")) {
      lines.push({ kind: "context", text: raw.slice(1), old: oldNo++, new: newNo++ });
    }
  }
  return { lines, added, removed, created };
}
