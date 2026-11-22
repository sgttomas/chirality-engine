import { useEffect, useState } from "react";

import type { Project, ProjectId, Snapshot, ThreadId, ThreadSummary } from "../../engine/types";
import { markLabel, markOf, nameOf } from "../../state/selectors";
import { Icon } from "../Icon";

export interface Mention {
  /** Where the `@` is. */
  start: number;
  /** Where the typed name ends. */
  end: number;
  query: string;
}

/** The `@` mention being typed at the caret, if any: from an `@` at the start or after a space, to the
 * caret, on one line. A title has spaces, so the name typed can too. */
export function mentionAt(text: string, caret: number): Mention | null {
  const before = text.slice(0, caret);
  const start = before.lastIndexOf("@");
  if (start < 0 || (start > 0 && !/\s/.test(before[start - 1]))) return null;
  const query = before.slice(start + 1);
  if (/^\s/.test(query) || query.includes("\n")) return null;
  return { start, end: caret, query };
}

export interface MentionGroup {
  project: Project;
  threads: ThreadSummary[];
}

/** How many of a project's threads the list shows when nothing is typed, and when something is. */
const SHOWN = 5;
const SHOWN_FOUND = 20;

/** Running or needing the person first, then finished and unread, then the rest. */
const rank = (t: ThreadSummary) => {
  const mark = markOf(t);
  return mark === "waiting" || mark === "running" ? 0 : mark === "finished" ? 1 : 2;
};

/** The threads `@` can mention, by project: the current project first, then the others by their latest
 * activity; in each, running or needing the person first, then unread, then the rest, each most recent
 * first. Subagents and the current thread are left out; `query` filters by title in every project. */
export function mentionGroups(snapshot: Snapshot, project: ProjectId | null, current: ThreadId | null, query: string): MentionGroup[] {
  const q = query.trim().toLowerCase();
  const latest = (p: Project) => Math.max(0, ...snapshot.threads.filter((t) => t.project === p.id).map((t) => t.updatedAt));
  const projects = [...snapshot.projects].sort((a, b) => Number(b.id === project) - Number(a.id === project) || latest(b) - latest(a));
  return projects
    .map((p) => ({
      project: p,
      threads: snapshot.threads
        .filter((t) => t.project === p.id && !t.subagent && t.id !== current && (!q || nameOf(t).toLowerCase().includes(q)))
        .sort((a, b) => rank(a) - rank(b) || b.updatedAt - a.updatedAt)
        .slice(0, q ? SHOWN_FOUND : SHOWN),
    }))
    .filter((g) => g.threads.length > 0);
}

/** The threads to mention, grouped by project, chosen with the arrows and Enter. */
export function MentionMenu({ groups, onPick, onClose }: { groups: MentionGroup[]; onPick(thread: ThreadSummary): void; onClose(): void }) {
  const threads = groups.flatMap((g) => g.threads);
  const [active, setActive] = useState(0);
  const key = threads.map((t) => t.id).join(" ");
  useEffect(() => setActive(0), [key]);

  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") setActive((a) => Math.min(a + 1, threads.length - 1));
      else if (e.key === "ArrowUp") setActive((a) => Math.max(a - 1, 0));
      else if ((e.key === "Enter" || e.key === "Tab") && threads[active]) onPick(threads[active]);
      else if (e.key === "Escape") onClose();
      else return;
      e.preventDefault();
    };
    document.addEventListener("keydown", keys, true);
    return () => document.removeEventListener("keydown", keys, true);
  }, [threads, active, onPick, onClose]);

  if (threads.length === 0) return null;
  return (
    <div className="menu mention-menu" role="listbox" aria-label="Threads">
      {groups.map((group) => (
        <div key={group.project.id} role="group" aria-label={group.project.name}>
          <div className="mention-project" aria-hidden>
            {group.project.name}
          </div>
          {group.threads.map((thread) => {
            const i = threads.indexOf(thread);
            const mark = markOf(thread);
            return (
              <button
                key={thread.id}
                role="option"
                aria-selected={i === active}
                className="menu-item"
                onMouseEnter={() => setActive(i)}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => onPick(thread)}
              >
                <Icon name="at" />
                <span className="mention-name">{nameOf(thread)}</span>
                {mark && <span className={`mark mark--${mark}`} title={markLabel[mark]} aria-label={markLabel[mark]} />}
              </button>
            );
          })}
        </div>
      ))}
    </div>
  );
}
