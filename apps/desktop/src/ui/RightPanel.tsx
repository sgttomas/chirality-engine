import { useEffect, useState } from "react";

import { changesOf, shortPath, sourceName, workflowInUse, type Change } from "../state/selectors";
import { describe, useProjectRoot, useStore, useStoreHandle } from "../state/store";
import { Markdown } from "./conversation/Markdown";
import { Diff, DiffStat } from "./Diff";
import { Icon } from "./Icon";
import { usePanel, type PanelTab } from "./panel";
import "./styles/panel.css";

const tabs: { tab: PanelTab; label: string }[] = [
  { tab: "changes", label: "Changes" },
  { tab: "workflow", label: "Workflow" },
  { tab: "preview", label: "Preview" },
];

const pathsOf = (changes: Change[]) => [...new Set(changes.map((c) => c.path))];

export function RightPanel() {
  const { panel, show, toggle } = usePanel();
  const view = useStore((s) => (s.thread ? s.model!.views[s.thread] : undefined));
  const workflows = useStore((s) => (s.project ? s.workflows[s.project] : undefined));
  const changes = changesOf(view);
  return (
    <aside className="panel" aria-label="Side panel">
      <div className="panel-tabs" role="tablist">
        {tabs.map(({ tab, label }) => (
          <button key={tab} role="tab" aria-selected={panel.tab === tab} className="panel-tab" onClick={() => show(tab, panel.file)}>
            {label}
            {tab === "changes" && changes.length > 0 && <span className="panel-count">{pathsOf(changes).length}</span>}
          </button>
        ))}
        <span className="composer-spacer" />
        <button className="quiet" aria-label="Close side panel" onClick={toggle}>
          <Icon name="x" />
        </button>
      </div>
      <div className="panel-body">
        {panel.tab === "changes" && <Changes changes={changes} file={panel.file} />}
        {panel.tab === "workflow" && <WorkflowTab workflow={workflowInUse(view, workflows)} />}
        {panel.tab === "preview" && <Preview changes={changes} file={panel.file} />}
      </div>
    </aside>
  );
}

function Changes({ changes, file }: { changes: Change[]; file: string | null }) {
  const { show } = usePanel();
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
  const { show } = usePanel();
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
