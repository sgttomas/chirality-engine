import { useLayoutEffect, useRef, type ReactNode } from "react";

import { isRunning, nameOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { AssignedBy } from "./Assignments";
import { Composer } from "./composer/Composer";
import { ThreadTitle } from "./conversation/ThreadTitle";
import { TurnView } from "./conversation/TurnView";
import { Icon } from "./Icon";
import { usePanel } from "./panel";
import "./styles/conversation.css";

/** The selected thread (or a new one in the selected project): heading, turns and composer. */
export function Conversation({ actions, className }: { actions?: ReactNode; className?: string }) {
  const projectId = useStore((s) => s.project);
  // A project's new thread: the composer sits under the middle of the page, not at its foot.
  const fresh = useStore((s) => !!s.project && !s.thread);
  return (
    <main className={`conv ${fresh ? "conv--new" : ""} ${className ?? ""}`}>
      <ConversationHeader actions={actions} />
      <Turns />
      {projectId && <Composer />}
    </main>
  );
}

/** The thread's title and the thread it was forked from, or the project's name; then `actions`
 * and, when there is a side panel, its toggle. */
export function ConversationHeader({ actions }: { actions?: ReactNode }) {
  const panel = usePanel();
  const summary = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === s.thread));
  const project = useStore((s) => s.model!.snapshot.projects.find((p) => p.id === s.project));
  return (
    <header className="conv-head">
      <div className="conv-title">
        {summary ? (
          <ThreadHeading />
        ) : (
          <>
            <h1>{project?.name ?? ""}</h1>
            {project && <span className="conv-path">{project.path}</span>}
          </>
        )}
      </div>
      {actions}
      {panel && (
        <button className={`quiet ${panel.panel.open ? "is-on" : ""}`} aria-label="Side panel" aria-pressed={panel.panel.open} onClick={panel.toggle}>
          <Icon name="panel" />
        </button>
      )}
    </header>
  );
}

/** The selected thread's title, renamed in place, and a way back to the thread it was forked from,
 * to the thread whose agent started it, or to the thread that assigned it. */
export function ThreadHeading() {
  const store = useStoreHandle();
  const summary = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === s.thread));
  const parent = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === (summary?.subagent?.parent ?? summary?.forkedFrom)));
  if (!summary) return <h1>New thread</h1>;
  return (
    <>
      <ThreadTitle thread={summary} />
      {parent && (
        <button className="conv-parent" onClick={() => store.selectThread(parent.id)}>
          <Icon name={summary.subagent ? "subagent" : "fork"} /> {nameOf(parent)}
        </button>
      )}
      <AssignedBy thread={summary} />
    </>
  );
}

/** The selected thread's turns, kept at the newest unless the person scrolls back. */
export function Turns() {
  const threadId = useStore((s) => s.thread);
  const projectId = useStore((s) => s.project);
  const view = useStore((s) => (s.thread ? s.model!.views[s.thread] : undefined));
  const workflows = useStore((s) => (s.project ? s.workflows[s.project] : undefined)) ?? [];
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const running = isRunning(view);

  useLayoutEffect(() => {
    pinned.current = true;
  }, [threadId]);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  });

  return (
    <div
      className="conv-scroll"
      ref={scroller}
      onScroll={(e) => {
        const el = e.currentTarget;
        pinned.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
      }}
    >
      <div className="conv-turns">
        {view?.turns.map((turn) => (
          <TurnView key={turn.id} thread={view.summary.id} turn={turn} workflows={workflows} canAct={!running && !view.summary.subagent} />
        ))}
        {!threadId && projectId && <div className="conv-empty" />}
      </div>
    </div>
  );
}
