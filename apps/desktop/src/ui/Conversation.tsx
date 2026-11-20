import { useEffect, useLayoutEffect, useRef } from "react";

import { isRunning, markLabel, markOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Composer } from "./composer/Composer";
import { ThreadTitle } from "./conversation/ThreadTitle";
import { TurnView } from "./conversation/TurnView";
import { Icon } from "./Icon";
import { usePanel } from "./panel";
import "./styles/conversation.css";

export function Conversation() {
  const store = useStoreHandle();
  const threadId = useStore((s) => s.thread);
  const projectId = useStore((s) => s.project);
  const view = useStore((s) => (s.thread ? s.model!.snapshot && s.model!.views[s.thread] : undefined));
  const summary = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === s.thread));
  const project = useStore((s) => s.model!.snapshot.projects.find((p) => p.id === s.project));
  const parent = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === summary?.forkedFrom));
  const workflows = useStore((s) => (s.project ? s.workflows[s.project] : undefined)) ?? [];
  const { panel, toggle } = usePanel();
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const running = isRunning(view);

  useEffect(() => {
    const mark = summary && markOf(summary);
    document.title = summary ? `${summary.title ?? "New thread"}${mark && mark !== "finished" ? ` · ${markLabel[mark]}` : ""}` : "Chirality";
  }, [summary]);

  useLayoutEffect(() => {
    pinned.current = true;
  }, [threadId]);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && pinned.current) el.scrollTop = el.scrollHeight;
  });

  return (
    <main className="conv">
      <header className="conv-head">
        <div className="conv-title">
          {summary ? <ThreadTitle thread={summary} /> : <h1>{project?.name ?? ""}</h1>}
          {parent && (
            <button className="conv-parent" onClick={() => store.selectThread(parent.id)}>
              <Icon name="fork" /> {parent.title ?? "Parent thread"}
            </button>
          )}
          {!summary && project && <span className="conv-path">{project.path}</span>}
        </div>
        <button className={`quiet ${panel.open ? "is-on" : ""}`} aria-label="Side panel" aria-pressed={panel.open} onClick={toggle}>
          <Icon name="panel" />
        </button>
      </header>
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
            <TurnView key={turn.id} thread={view.summary.id} turn={turn} workflows={workflows} canAct={!running} />
          ))}
          {!threadId && projectId && <div className="conv-empty" />}
        </div>
      </div>
      {projectId && <Composer key={threadId ?? projectId} />}
    </main>
  );
}
