import { useState } from "react";

import type { Project, ThreadSummary } from "../engine/types";
import { ago, markLabel, markOf, treeOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Icon } from "./Icon";
import { applyTheme, storedTheme, type Theme } from "./theme";
import "./styles/navigator.css";

export function Navigator() {
  const store = useStoreHandle();
  const snapshot = useStore((s) => s.model!.snapshot);
  const selected = useStore((s) => s.thread);
  const current = useStore((s) => s.project);
  const [query, setQuery] = useState("");
  const [closed, setClosed] = useState<Record<string, boolean>>({});

  const addProject = async () => {
    const path = await store.host.pickFolder();
    if (!path) return;
    const project = await store.attempt(() => store.engine.addProject(path));
    if (project) store.selectProject(project.id);
  };

  const q = query.trim().toLowerCase();
  const matches = (t: ThreadSummary) => !q || (t.title ?? "").toLowerCase().includes(q);

  return (
    <nav className="nav" aria-label="Projects">
      <div className="nav-top">
        <div className="nav-brand">Chirality</div>
        <button className="quiet nav-new" onClick={() => current && store.selectProject(current)} disabled={!current} title="New thread">
          <Icon name="compose" /> New thread
        </button>
        <input type="search" className="nav-search" placeholder="Search" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <div className="nav-projects">
        {snapshot.projects.map((project) => (
          <ProjectGroup
            key={project.id}
            project={project}
            rows={treeOf(snapshot.threads, project.id).filter((r) => matches(r.thread))}
            open={!closed[project.id] || !!q}
            current={current === project.id && !selected}
            selected={selected}
            onToggle={() => setClosed((c) => ({ ...c, [project.id]: !c[project.id] }))}
          />
        ))}
      </div>
      <div className="nav-foot">
        <button className="quiet" onClick={addProject}>
          <Icon name="folderPlus" /> Add project
        </button>
        <Account />
      </div>
    </nav>
  );
}

function ProjectGroup(props: {
  project: Project;
  rows: { thread: ThreadSummary; depth: number }[];
  open: boolean;
  current: boolean;
  selected: string | null;
  onToggle(): void;
}) {
  const store = useStoreHandle();
  const { project, rows, open } = props;
  const marks = rows.map((r) => markOf(r.thread));
  const strongest = (["waiting", "failed", "finished", "running"] as const).find((m) => marks.includes(m));
  return (
    <section className="nav-group">
      <div className={`nav-project ${props.current ? "nav-project--current" : ""}`}>
        <button className="nav-chevron" aria-label={open ? "Collapse" : "Expand"} aria-expanded={open} onClick={props.onToggle}>
          <Icon name={open ? "chevronDown" : "chevronRight"} />
        </button>
        <button className="nav-project-name" title={project.path} onClick={() => store.selectProject(project.id)}>
          {project.name}
        </button>
        {!open && strongest && <span className={`mark mark--${strongest}`} title={markLabel[strongest]} />}
        <button className="quiet nav-project-new" aria-label={`New thread in ${project.name}`} onClick={() => store.selectProject(project.id)}>
          <Icon name="plus" />
        </button>
      </div>
      {open && (
        <ul className="nav-threads">
          {rows.map(({ thread, depth }) => (
            <ThreadRow key={thread.id} thread={thread} depth={depth} selected={thread.id === props.selected} />
          ))}
        </ul>
      )}
    </section>
  );
}

function ThreadRow({ thread, depth, selected }: { thread: ThreadSummary; depth: number; selected: boolean }) {
  const store = useStoreHandle();
  const mark = markOf(thread);
  const quiet = !mark && !selected;
  return (
    <li>
      <button
        className={`nav-thread ${quiet ? "nav-thread--quiet" : ""} ${thread.unread ? "nav-thread--unread" : ""}`}
        aria-current={selected ? "page" : undefined}
        style={{ paddingLeft: 26 + depth * 14 }}
        onClick={() => store.selectThread(thread.id)}
      >
        {depth > 0 && <Icon name="fork" className="nav-fork" />}
        <span className="nav-thread-title">{thread.title ?? "New thread"}</span>
        {mark ? <span className={`mark mark--${mark}`} title={markLabel[mark]} aria-label={markLabel[mark]} /> : <span className="nav-when">{ago(thread.updatedAt)}</span>}
      </button>
    </li>
  );
}

function Account() {
  const store = useStoreHandle();
  const account = useStore((s) => s.model!.snapshot.account);
  const [open, setOpen] = useState(false);
  const [theme, setTheme] = useState<Theme>(storedTheme);
  if (account.state !== "signedIn") return null;
  return (
    <div className="nav-account">
      <button className="quiet nav-account-button" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="nav-avatar">{(account.email ?? "?").slice(0, 1).toUpperCase()}</span>
        <span className="nav-account-email">{account.email}</span>
      </button>
      {open && (
        <div className="menu nav-account-menu" role="menu">
          {account.plan && <div className="menu-note">{account.plan}</div>}
          <div className="menu-segment" role="group" aria-label="Appearance">
            {(["light", "dark", "system"] as const).map((t) => (
              <button
                key={t}
                aria-pressed={theme === t}
                onClick={() => {
                  applyTheme(t);
                  setTheme(t);
                }}
              >
                {t === "light" ? "Light" : t === "dark" ? "Dark" : "System"}
              </button>
            ))}
          </div>
          <button role="menuitem" className="menu-item" onClick={() => store.attempt(() => store.engine.signOut())}>
            Sign out
          </button>
        </div>
      )}
    </div>
  );
}
