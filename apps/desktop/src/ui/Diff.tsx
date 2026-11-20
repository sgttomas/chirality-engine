import { useMemo } from "react";

import { parseDiff } from "./unifiedDiff";
import "./styles/diff.css";

export function DiffStat({ diff }: { diff: string }) {
  const { added, removed } = useMemo(() => parseDiff(diff), [diff]);
  return (
    <span className="diff-stat">
      {added > 0 && <span className="diff-stat-add">+{added}</span>}
      {removed > 0 && <span className="diff-stat-del">−{removed}</span>}
    </span>
  );
}

/** A unified diff, line by line; `limit` shows only the first lines. */
export function Diff({ diff, limit }: { diff: string; limit?: number }) {
  const { lines } = useMemo(() => parseDiff(diff), [diff]);
  const shown = limit ? lines.slice(0, limit) : lines;
  return (
    <div className="diff" role="table">
      {shown.map((line, i) =>
        line.kind === "hunk" ? (
          i === 0 && !line.text ? null : (
          <div key={i} className="diff-hunk" role="row">
            {line.text || " "}
          </div>
          )
        ) : (
          <div key={i} className={`diff-line diff-${line.kind}`} role="row">
            <span className="diff-no">{line.old ?? ""}</span>
            <span className="diff-no">{line.new ?? ""}</span>
            <span className="diff-sign">{line.kind === "add" ? "+" : line.kind === "del" ? "−" : ""}</span>
            <span className="diff-text">{line.text}</span>
          </div>
        ),
      )}
      {shown.length < lines.length && <div className="diff-more">…</div>}
    </div>
  );
}
