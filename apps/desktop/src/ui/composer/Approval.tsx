import { useEffect, useState } from "react";

import type { Answer, PendingRequest } from "../../engine/types";
import { useStoreHandle } from "../../state/store";
import { Diff, DiffStat } from "../Diff";
import { Icon } from "../Icon";

const headings = { command: "Run this command?", fileChange: "Make these changes?", permissions: "Allow more access?" } as const;

/** An approval in the composer area: Allow, Decline, or one more press for the rest. */
export function Approval({ pending }: { pending: PendingRequest }) {
  const store = useStoreHandle();
  const [more, setMore] = useState(false);
  const [sent, setSent] = useState(false);
  const request = pending.request;

  const answer = (a: Answer) => {
    if (sent) return;
    setSent(true);
    void store.attempt(() => store.engine.answer(pending.id, a)).then(() => setSent(false));
  };

  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      if (e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLInputElement) return;
      if (e.key === "Enter" && !e.metaKey) answer({ kind: "allow" });
      else if (e.key === "Escape") answer({ kind: "decline" });
    };
    document.addEventListener("keydown", keys);
    return () => document.removeEventListener("keydown", keys);
  });

  if (request.kind === "question") return null;
  return (
    <div className="ask ask--approval" role="dialog" aria-label={headings[request.kind]}>
      <div className="ask-head">{headings[request.kind]}</div>
      {request.reason && <div className="ask-reason">{request.reason}</div>}
      {request.kind === "command" && (
        <pre className="ask-command">
          <Icon name="terminal" /> {request.command}
        </pre>
      )}
      {request.kind === "fileChange" &&
        request.files.map((f) => (
          <div key={f.path} className="ask-file">
            <div className="ask-file-head">
              <span className="mono">{f.path}</span>
              <DiffStat diff={f.diff} />
            </div>
            <Diff diff={f.diff} limit={10} />
          </div>
        ))}
      <div className="ask-actions">
        <button className="primary" disabled={sent} onClick={() => answer({ kind: "allow" })}>
          Allow
        </button>
        <button disabled={sent} onClick={() => answer({ kind: "decline" })}>
          Decline
        </button>
        {more ? (
          <>
            <button className="quiet" disabled={sent} onClick={() => answer({ kind: "allowForSession" })}>
              Allow for this session
            </button>
            <button className="quiet" disabled={sent} onClick={() => answer({ kind: "cancel" })}>
              Decline and stop
            </button>
          </>
        ) : (
          <button className="quiet" aria-label="More choices" onClick={() => setMore(true)}>
            <Icon name="more" />
          </button>
        )}
      </div>
    </div>
  );
}
