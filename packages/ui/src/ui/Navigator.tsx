import { useState, type ReactNode } from "react";

import type { Project, ProjectId, ThreadSummary } from "../engine/types";
import { ago, markLabel, nameOf, rowMark, searchTree, strongestMark } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Icon } from "./Icon";
import { SubagentGroup } from "./Subagents";
import { applyTheme, storedTheme, type Theme } from "./theme";
import "./styles/navigator.css";

/** Projects, each with its threads (forks and subagents under their parents); new thread, search,
 * add a project, and the account. */
export function Navigator({ brand = "Chirality" }: { brand?: ReactNode }) {
  const store = useStoreHandle();
  const snapshot = useStore((s) => s.model!.snapshot);
  const selected = useStore((s) => s.thread);
  const current = useStore((s) => s.project);
  const [query, setQuery] = useState("");
  const [closed, setClosed] = useState<Record<string, boolean>>({});

  const addProject = () => void store.addProjectFolder();

  const q = query.trim();

  return (
    <nav className="nav" aria-label="Projects">
      <div className="nav-top">
        <div className="nav-brand">{brand}</div>
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
            {...searchTree(snapshot.threads, project.id, q)}
            all={snapshot.threads.filter((t) => t.project === project.id)}
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
        <AccountMenu />
      </div>
    </nav>
  );
}

function ProjectGroup(props: {
  project: Project;
  rows: { thread: ThreadSummary; depth: number }[];
  /** Subagents a search found. */
  found: Set<string>;
  /** Every thread of the project, subagents too. */
  all: ThreadSummary[];
  open: boolean;
  current: boolean;
  selected: string | null;
  onToggle(): void;
}) {
  const store = useStoreHandle();
  const { project, rows, open } = props;
  const strongest = strongestMark(props.all);
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
      {open && <ThreadRows rows={rows} found={props.found} selected={props.selected} />}
    </section>
  );
}

/** A project's threads, newest first, each fork and subagent under its parent; `query` filters by name. */
export function ThreadList({ project, query = "" }: { project: ProjectId; query?: string }) {
  const threads = useStore((s) => s.model!.snapshot.threads);
  const selected = useStore((s) => s.thread);
  const { rows, found } = searchTree(threads, project, query);
  return <ThreadRows rows={rows} found={found} selected={selected} />;
}

function ThreadRows({ rows, found, selected }: { rows: { thread: ThreadSummary; depth: number }[]; found: Set<string>; selected: string | null }) {
  return (
    <ul className="nav-threads">
      {rows.map(({ thread, depth }) => (
        <ThreadAndSubagents key={thread.id} thread={thread} depth={depth} found={found} selected={selected} />
      ))}
    </ul>
  );
}

function ThreadAndSubagents({ thread, depth, found, selected }: { thread: ThreadSummary; depth: number; found: Set<string>; selected: string | null }) {
  const row = (t: ThreadSummary, d: number) => <ThreadRow key={t.id} thread={t} depth={d} selected={t.id === selected} />;
  return (
    <>
      {row(thread, depth)}
      <SubagentGroup parent={thread} depth={depth} found={found} row={row} />
    </>
  );
}

function ThreadRow({ thread, depth, selected }: { thread: ThreadSummary; depth: number; selected: boolean }) {
  const store = useStoreHandle();
  const mark = useStore((s) => rowMark(thread, s.model!.snapshot.threads));
  const quiet = !mark && !selected;
  return (
    <li>
      <button
        className={`nav-thread ${quiet ? "nav-thread--quiet" : ""} ${thread.unread ? "nav-thread--unread" : ""}`}
        aria-current={selected ? "page" : undefined}
        style={{ paddingLeft: 26 + depth * 14 }}
        onClick={() => store.selectThread(thread.id)}
      >
        {thread.subagent ? <Icon name="subagent" className="nav-fork" /> : depth > 0 && <Icon name="fork" className="nav-fork" />}
        <span className="nav-thread-title" title={thread.subagent ? [thread.subagent.nickname, thread.subagent.path].filter(Boolean).join(" · ") : undefined}>
          {nameOf(thread)}
        </span>
        {mark ? <span className={`mark mark--${mark}`} title={markLabel[mark]} aria-label={markLabel[mark]} /> : <span className="nav-when">{ago(thread.updatedAt)}</span>}
      </button>
    </li>
  );
}

/** The signed-in account: appearance and sign out. */
export function AccountMenu() {
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
