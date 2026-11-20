import { changesOf, workflowInUse, type Change } from "../state/selectors";
import { useStore } from "../state/store";
import { Markdown } from "./conversation/Markdown";
import { contentAfter } from "./unifiedDiff";
import { Diff, DiffStat } from "./Diff";
import { Icon } from "./Icon";
import { usePanel, type PanelTab } from "./panel";
import "./styles/panel.css";

const tabs: { tab: PanelTab; label: string }[] = [
  { tab: "changes", label: "Changes" },
  { tab: "workflow", label: "Workflow" },
  { tab: "preview", label: "Preview" },
];

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
            {tab === "changes" && changes.length > 0 && <span className="panel-count">{new Set(changes.map((c) => c.path)).size}</span>}
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
  if (changes.length === 0) return <p className="panel-empty">No changes yet</p>;
  const shown = file ? changes.filter((c) => c.path === file) : changes;
  return (
    <div className="panel-changes">
      <ul className="panel-files">
        {[...new Set(changes.map((c) => c.path))].map((path) => (
          <li key={path}>
            <button aria-current={path === file ? "true" : undefined} className="panel-file" onClick={() => show("changes", path === file ? null : path)}>
              <span className="panel-file-name">{path.split("/").pop()}</span>
              <span className="panel-file-dir">{path.split("/").slice(0, -1).join("/")}</span>
              <DiffStat diff={changes.filter((c) => c.path === path).map((c) => c.diff).join("\n")} />
            </button>
          </li>
        ))}
      </ul>
      {shown.map((c, i) => (
        <section key={`${c.turn}-${c.path}-${i}`} className="panel-diff">
          <header className="panel-diff-head">
            <span className="mono">{c.path}</span>
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
      <div className="panel-workflow-source">{workflow.source}</div>
      <h2>{workflow.name}</h2>
      <p>{workflow.summary}</p>
      <div className="mono faint panel-workflow-path">{workflow.path}</div>
    </div>
  );
}

/** A file the agent produced, from the change that made it. */
function Preview({ changes, file }: { changes: Change[]; file: string | null }) {
  const { show } = usePanel();
  const created = changes.filter((c) => contentAfter(c.diff) !== null);
  const path = file ?? created.at(-1)?.path ?? null;
  const change = changes.filter((c) => c.path === path).at(-1);
  if (!path || !change) return <p className="panel-empty">No file to preview</p>;
  const content = contentAfter(change.diff);
  return (
    <div className="panel-preview">
      {created.length > 1 && (
        <div className="panel-preview-pick">
          {created.map((c) => (
            <button key={c.path} className={`quiet ${c.path === path ? "is-on" : ""}`} onClick={() => show("preview", c.path)}>
              {c.path.split("/").pop()}
            </button>
          ))}
        </div>
      )}
      <div className="mono faint panel-preview-path">{path}</div>
      {content === null ? <Diff diff={change.diff} /> : /\.md$/i.test(path) ? <Markdown text={content} /> : <pre className="panel-preview-text">{content}</pre>}
    </div>
  );
}
