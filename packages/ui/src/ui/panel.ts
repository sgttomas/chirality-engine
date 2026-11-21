// The side panel's state, shared so a file change in the conversation can open
// it. Absent when the application shows no side panel.
import { createContext, useContext, useMemo, useState } from "react";

/** A tab of the side panel: the standard ones are `changes`, `workflow` and `preview`. */
export type PanelTab = string;

export interface Panel {
  open: boolean;
  tab: PanelTab;
  /** The file chosen in Changes or shown in Preview. */
  file: string | null;
}

export interface PanelControl {
  panel: Panel;
  show(tab: PanelTab, file?: string | null): void;
  toggle(): void;
}

export const PanelContext = createContext<PanelControl | null>(null);

/** The side panel, or null when there is none. */
export const usePanel = () => useContext(PanelContext);

/** Side panel state for a layout to provide through `PanelContext`. */
export function usePanelState(initial?: PanelTab): PanelControl {
  const [panel, setPanel] = useState<Panel>({ open: !!initial, tab: initial ?? "changes", file: null });
  return useMemo(
    () => ({
      panel,
      show: (tab: PanelTab, file: string | null = null) => setPanel({ open: true, tab, file }),
      toggle: () => setPanel((p) => ({ ...p, open: !p.open })),
    }),
    [panel],
  );
}
