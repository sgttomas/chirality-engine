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

/** The mention just opened by typing `@` at the caret (over a selection too), at the start or after a
 * space: only an `@` typed fresh opens one, never one already in the text. A deletion is no typing. */
export function typedMention(before: string, after: string, caret: number): Mention | null {
  const start = caret - 1;
  const rest = after.slice(caret);
  // The `@` replaced nothing, or the selection it was typed over.
  if (after[start] !== "@" || after.slice(0, start) !== before.slice(0, start) || !before.endsWith(rest) || before.length - rest.length < start) return null;
  if (start > 0 && !/\s/.test(after[start - 1])) return null;
  return { start, end: caret, query: "" };
}

/** An open mention as the text now is: the name typed from its `@` to the caret, across spaces, on
 * one line; none once the `@` is gone, the caret is before it, or the name starts with a space. */
export function mentionFrom(text: string, start: number, caret: number): Mention | null {
  if (text[start] !== "@" || caret <= start) return null;
  const query = text.slice(start + 1, caret);
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

/** Running or needing the person first, then unread (finished or failed), then the rest. */
const rank = (t: ThreadSummary) => {
  const mark = markOf(t);
  return mark === "waiting" || mark === "running" ? 0 : t.unread ? 1 : 2;
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

/** The threads to mention, grouped by project; `active` is the one Enter picks. The composer handles
 * the keys. */
export function MentionMenu({ groups, active, onActive, onPick }: { groups: MentionGroup[]; active: number; onActive(i: number): void; onPick(thread: ThreadSummary): void }) {
  const threads = groups.flatMap((g) => g.threads);
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
                onMouseEnter={() => onActive(i)}
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
