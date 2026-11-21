import { useEffect, useState, type ReactNode } from "react";

import type { ThreadView, Workflow } from "../engine/types";
import { changesOf, shortPath, sourceName, workflowInUse, type Change } from "../state/selectors";
import { describe, useProjectRoot, useStore, useStoreHandle } from "../state/store";
import { Markdown } from "./conversation/Markdown";
import { Diff, DiffStat } from "./Diff";
import { Icon } from "./Icon";
import { usePanel, type PanelTab } from "./panel";
import "./styles/panel.css";

/** What a side panel tab is given: the selected thread, if it has been read, and its workflows. */
export interface PanelView {
  view: ThreadView | undefined;
  workflows: Workflow[] | undefined;
  changes: Change[];
  file: string | null;
}

export interface PanelTabSpec {
  tab: PanelTab;
  label: string;
  /** A number shown beside the label, if any. */
  count?(panel: PanelView): number;
  render(panel: PanelView): ReactNode;
}

const pathsOf = (changes: Change[]) => [...new Set(changes.map((c) => c.path))];

/** The diff of the thread's changes, the workflow in use, and a preview of a file it changed. */
export const standardTabs: PanelTabSpec[] = [
  { tab: "changes", label: "Changes", count: (p) => pathsOf(p.changes).length, render: (p) => <Changes changes={p.changes} file={p.file} /> },
  { tab: "workflow", label: "Workflow", render: (p) => <WorkflowTab workflow={workflowInUse(p.view, p.workflows)} /> },
  { tab: "preview", label: "Preview", render: (p) => <Preview changes={p.changes} file={p.file} /> },
];

/** The side panel, inside a `PanelContext`: the standard tabs, or the ones given. */
export function RightPanel({ tabs = standardTabs }: { tabs?: PanelTabSpec[] }) {
  const control = usePanel();
  const view = useStore((s) => (s.thread ? s.model!.views[s.thread] : undefined));
  const workflows = useStore((s) => (s.project ? s.workflows[s.project] : undefined));
  if (!control) return null;
  const { panel, show, toggle } = control;
  const shown: PanelView = { view, workflows, changes: changesOf(view), file: panel.file };
  const current = tabs.find((t) => t.tab === panel.tab) ?? tabs[0];
  return (
    <aside className="panel" aria-label="Side panel">
      <div className="panel-tabs" role="tablist">
        {tabs.map((t) => {
          const count = t.count?.(shown) ?? 0;
          return (
            <button key={t.tab} role="tab" aria-selected={current?.tab === t.tab} className="panel-tab" onClick={() => show(t.tab, panel.file)}>
              {t.label}
              {count > 0 && <span className="panel-count">{count}</span>}
            </button>
          );
        })}
        <span className="panel-spacer" />
        <button className="quiet" aria-label="Close side panel" onClick={toggle}>
          <Icon name="x" />
        </button>
      </div>
      <div className="panel-body">{current?.render(shown)}</div>
    </aside>
  );
}

function Changes({ changes, file }: { changes: Change[]; file: string | null }) {
  const show = usePanel()!.show;
  const root = useProjectRoot();
  if (changes.length === 0) return <p className="panel-empty">No changes yet</p>;
  const shown = file ? changes.filter((c) => c.path === file) : changes;
  return (
    <div className="panel-changes">
      <ul className="panel-files">
        {pathsOf(changes).map((path) => (
          <li key={path}>
            <button aria-current={path === file ? "true" : undefined} className="panel-file" onClick={() => show("changes", path === file ? null : path)}>
              <span className="panel-file-name">{path.split("/").pop()}</span>
              <span className="panel-file-dir">{shortPath(path, root).split("/").slice(0, -1).join("/")}</span>
              <DiffStat diff={changes.filter((c) => c.path === path).map((c) => c.diff).join("\n")} />
            </button>
          </li>
        ))}
      </ul>
      {shown.map((c, i) => (
        <section key={`${c.turn}-${c.path}-${i}`} className="panel-diff">
          <header className="panel-diff-head">
            <span className="mono">{shortPath(c.path, root)}</span>
            <button className="quiet" onClick={() => show("preview", c.path)}>
              Preview
            </button>
          </header>
          <Diff diff={c.diff} />
        </section>
      ))}
    </div>
  );
}

function WorkflowTab({ workflow }: { workflow: ReturnType<typeof workflowInUse> }) {
  if (!workflow) return <p className="panel-empty">No workflow in this thread</p>;
  return (
    <div className="panel-workflow">
      <div className="panel-workflow-source">{sourceName(workflow.source)}</div>
      <h2>{workflow.name}</h2>
      <p>{workflow.summary}</p>
      <div className="mono faint panel-workflow-path">{workflow.path}</div>
    </div>
  );
}

/** A file the agent changed, as it is now in the project. */
function Preview({ changes, file }: { changes: Change[]; file: string | null }) {
  const store = useStoreHandle();
  const show = usePanel()!.show;
  const root = useProjectRoot();
  const project = useStore((s) => s.project);
  const paths = pathsOf(changes);
  const path = file ?? paths.at(-1) ?? null;
  const [content, setContent] = useState<{ path: string; text: string } | { path: string; error: string } | null>(null);

  useEffect(() => {
    if (!path || !project) return;
    let current = true;
    store.engine.readFile(project, path).then(
      (text) => current && setContent({ path, text }),
      (e) => current && setContent({ path, error: describe(e) }),
    );
    return () => {
      current = false;
    };
  }, [store, project, path]);

  if (!path) return <p className="panel-empty">No file to preview</p>;
  const shown = content?.path === path ? content : null;
  return (
    <div className="panel-preview">
      {paths.length > 1 && (
        <div className="panel-preview-pick">
          {paths.map((p) => (
            <button key={p} className={`quiet ${p === path ? "is-on" : ""}`} onClick={() => show("preview", p)}>
              {p.split("/").pop()}
            </button>
          ))}
        </div>
      )}
      <div className="mono faint panel-preview-path">{shortPath(path, root)}</div>
      {shown && "error" in shown && <p className="panel-empty">{shown.error}</p>}
      {shown && "text" in shown && (/\.(md|markdown)$/i.test(path) ? <Markdown text={shown.text} /> : <pre className="panel-preview-text">{shown.text}</pre>)}
    </div>
  );
}
