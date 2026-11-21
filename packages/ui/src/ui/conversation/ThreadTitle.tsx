import { useEffect, useRef, useState } from "react";

import type { ThreadSummary } from "../../engine/types";
import { nameOf } from "../../state/selectors";
import { useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";

/** The thread's title, renamed in place, with a menu to rename or archive it. */
export function ThreadTitle({ thread }: { thread: ThreadSummary }) {
  const store = useStoreHandle();
  const [editing, setEditing] = useState(false);
  const [menu, setMenu] = useState(false);
  const [title, setTitle] = useState("");
  const menuRoot = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menu) return;
    const close = (e: MouseEvent) => !menuRoot.current?.contains(e.target as Node) && setMenu(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menu]);

  const startRename = () => {
    setMenu(false);
    setTitle(thread.title ?? "");
    setEditing(true);
  };
  const save = async () => {
    setEditing(false);
    const next = title.trim();
    if (next && next !== thread.title) await store.attempt(() => store.engine.rename(thread.id, next));
  };
  const archive = async () => {
    setMenu(false);
    await store.attempt(() => store.engine.archive(thread.id));
  };

  return (
    <>
      {editing ? (
        <input
          type="text"
          className="conv-rename"
          aria-label="Thread title"
          value={title}
          autoFocus
          onChange={(e) => setTitle(e.target.value)}
          onBlur={save}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
            if (e.key === "Escape") setEditing(false);
          }}
        />
      ) : (
        <h1 onDoubleClick={startRename}>{nameOf(thread)}</h1>
      )}
      <div className="conv-menu" ref={menuRoot}>
        <button className="quiet" aria-label="Thread actions" aria-expanded={menu} onClick={() => setMenu(!menu)}>
          <Icon name="more" />
        </button>
        {menu && (
          <div className="menu conv-menu-list" role="menu">
            <button role="menuitem" className="menu-item" onClick={startRename}>
              Rename
            </button>
            <button role="menuitem" className="menu-item" onClick={archive}>
              Archive
            </button>
          </div>
        )}
      </div>
    </>
  );
}
