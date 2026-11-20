import { useState } from "react";

import type { Workflow } from "../../engine/types";
import { Icon } from "../Icon";
import { Dropdown } from "./Dropdown";

/** Picks a workflow into the next message. */
export function WorkflowPicker({ workflows, picked, onPick }: { workflows: Workflow[]; picked: Workflow | null; onPick(w: Workflow | null): void }) {
  const [query, setQuery] = useState("");
  const q = query.trim().toLowerCase();
  const shown = workflows.filter((w) => !q || w.name.toLowerCase().includes(q) || w.summary.toLowerCase().includes(q));
  if (workflows.length === 0) return null;
  return (
    <Dropdown
      title="Workflow"
      wide
      label={
        <>
          <Icon name="book" /> Workflow
        </>
      }
      value={picked}
      header={<input type="search" className="dropdown-search" placeholder="Find a workflow" value={query} autoFocus onChange={(e) => setQuery(e.target.value)} />}
      options={shown.map((w) => ({ value: w, label: w.name, detail: `${w.summary} · ${w.source}` }))}
      onPick={(w) => {
        onPick(w);
        setQuery("");
      }}
    />
  );
}
