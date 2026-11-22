import { createContext, useContext, useState } from "react";

import type { Workflow } from "../../engine/types";
import { sourceName } from "../../state/selectors";
import { Icon } from "../Icon";
import { Dropdown } from "./Dropdown";

/** The workflow names the picker lists before anything is typed, besides the project's own;
 * a search finds every workflow. Unset, the picker lists them all. */
export const FeaturedWorkflows = createContext<readonly string[] | undefined>(undefined);

/** The workflows listed for a query: every match, or with none typed the featured ones and the
 * project's own (all of them when nothing is featured). */
export function listedWorkflows(workflows: Workflow[], query: string, featured: readonly string[] | undefined): Workflow[] {
  const q = query.trim().toLowerCase();
  if (q) return workflows.filter((w) => w.name.toLowerCase().includes(q) || w.summary.toLowerCase().includes(q));
  if (!featured) return workflows;
  return workflows.filter((w) => w.source === "project" || featured.includes(w.name));
}

/** Picks a workflow into the next message. */
export function WorkflowPicker({ workflows, picked, onPick }: { workflows: Workflow[]; picked: Workflow | null; onPick(w: Workflow | null): void }) {
  const [query, setQuery] = useState("");
  const featured = useContext(FeaturedWorkflows);
  const shown = listedWorkflows(workflows, query, featured);
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
      options={shown.map((w) => ({ value: w, label: w.name, detail: `${w.summary} · ${sourceName(w.source)}` }))}
      onPick={(w) => {
        onPick(w);
        setQuery("");
      }}
    />
  );
}
