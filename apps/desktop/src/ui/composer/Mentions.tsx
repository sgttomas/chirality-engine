import { useEffect, useState } from "react";

import type { ProjectId } from "../../engine/types";
import { useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";

export interface Mention {
  /** Where the `@` is. */
  start: number;
  /** Where the typed reference ends. */
  end: number;
  query: string;
}

/** The `@reference` being typed at the caret, if any. */
export function mentionAt(text: string, caret: number): Mention | null {
  const match = /(^|\s)@([^\s@]*)$/.exec(text.slice(0, caret));
  if (!match) return null;
  const start = caret - match[2].length - 1;
  return { start, end: caret, query: match[2] };
}

/** Files matching the reference, chosen with the arrows and Enter. */
export function MentionMenu({ project, mention, onPick, onClose }: { project: ProjectId; mention: Mention; onPick(path: string): void; onClose(): void }) {
  const store = useStoreHandle();
  const [files, setFiles] = useState<string[]>([]);
  const [active, setActive] = useState(0);

  useEffect(() => {
    let current = true;
    void store.attempt(() => store.engine.findFiles(project, mention.query)).then((found) => {
      if (!current || !found) return;
      setFiles(found);
      setActive(0);
    });
    return () => {
      current = false;
    };
  }, [store, project, mention.query]);

  useEffect(() => {
    const keys = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") setActive((a) => Math.min(a + 1, files.length - 1));
      else if (e.key === "ArrowUp") setActive((a) => Math.max(a - 1, 0));
      else if ((e.key === "Enter" || e.key === "Tab") && files[active]) onPick(files[active]);
      else if (e.key === "Escape") onClose();
      else return;
      e.preventDefault();
    };
    document.addEventListener("keydown", keys, true);
    return () => document.removeEventListener("keydown", keys, true);
  }, [files, active, onPick, onClose]);

  if (files.length === 0) return null;
  return (
    <div className="menu mention-menu" role="listbox" aria-label="Files">
      {files.map((path, i) => (
        <button
          key={path}
          role="option"
          aria-selected={i === active}
          className="menu-item"
          onMouseEnter={() => setActive(i)}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => onPick(path)}
        >
          <Icon name="file" />
          <span className="mention-name">{path.split("/").pop()}</span>
          <span className="mention-dir">{path.split("/").slice(0, -1).join("/")}</span>
        </button>
      ))}
    </div>
  );
}
