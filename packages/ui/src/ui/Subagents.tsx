// Subagents: under their parent in the thread list, where the agent delegated in its
// conversation, whose request waits on the person, and what the person can do in one.
import { useState, type ReactNode } from "react";

import type { Item, ThreadSummary } from "../engine/types";
import { isRunning, markLabel, markOf, nameOf, pendingFor, strongestMark, subtreeOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Approval } from "./composer/Approval";
import { Question } from "./composer/Question";
import { Icon } from "./Icon";

/** A thread's subagents, folded unless one runs, waits on the person, or is selected. */
export function SubagentGroup({ parent, depth, row }: { parent: ThreadSummary; depth: number; row(thread: ThreadSummary, depth: number): ReactNode }) {
  const threads = useStore((s) => s.model!.snapshot.threads);
  const selected = useStore((s) => s.thread);
  const [chosen, setChosen] = useState<boolean | null>(null);
  const rows = subtreeOf(threads, parent.id);
  if (!rows.length) return null;
  const busy = rows.some((r) => ["running", "waiting"].includes(markOf(r.thread) ?? "") || r.thread.id === selected);
  const open = chosen ?? busy;
  const mark = strongestMark(rows.map((r) => r.thread));
  return (
    <>
      <li>
        <button className="nav-subagents" aria-expanded={open} style={{ paddingLeft: 26 + depth * 14 }} onClick={() => setChosen(!open)}>
          <Icon name={open ? "chevronDown" : "chevronRight"} />
          <span className="nav-thread-title">{rows.length === 1 ? "1 subagent" : `${rows.length} subagents`}</span>
          {!open && mark && mark !== "finished" && <span className={`mark mark--${mark}`} title={markLabel[mark]} aria-label={markLabel[mark]} />}
        </button>
      </li>
      {open && rows.map((r) => row(r.thread, depth + 1 + r.depth))}
    </>
  );
}

/** Where the agent started, messaged or stopped a subagent, or one finished; opens it. */
export function Delegation({ item }: { item: Item }) {
  const store = useStoreHandle();
  const body = item.body.kind === "subagent" ? item.body : null;
  const child = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === body?.thread));
  if (!body) return null;
  const name = child ? nameOf(child) : (body.path.split("/").filter(Boolean).at(-1) ?? body.path);
  const said = { started: `Started ${name}`, messaged: `Messaged ${name}`, interrupted: `Stopped ${name}`, finished: `${name} finished` }[body.activity];
  const mark = child && body.activity === "started" ? markOf(child) : null;
  return (
    <button className="delegation" disabled={!child} title={body.path} onClick={() => child && store.selectThread(child.id)}>
      <Icon name="subagent" />
      <span className="delegation-text">{said}</span>
      {mark && <span className={`mark mark--${mark}`} title={markLabel[mark]} aria-label={markLabel[mark]} />}
    </button>
  );
}

/** In a subagent's conversation, in place of the composer (Codex takes no message from the person
 * for one): what waits on the person, and Stop while it runs. */
export function SubagentControls() {
  const store = useStoreHandle();
  const thread = useStore((s) => s.thread)!;
  const pending = useStore((s) => pendingFor(s.model!.snapshot, s.thread));
  const running = useStore((s) => isRunning(s.model!.views[thread]));
  if (!pending && !running) return null;
  return (
    <div className="composer-area">
      {pending?.request.kind === "question" ? <Question key={pending.id} pending={pending} /> : pending ? <Approval key={pending.id} pending={pending} /> : null}
      {running && !pending && (
        <div className="composer composer--subagent">
          <span className="composer-spacer" />
          <button className="send send--stop" aria-label="Stop" title="Stop" onClick={() => store.attempt(() => store.engine.interrupt(thread))}>
            <Icon name="stop" />
          </button>
        </div>
      )}
    </div>
  );
}
