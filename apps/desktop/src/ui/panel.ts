// The right panel's state, shared so a file change in the conversation can open it.
import { createContext, useContext } from "react";

export type PanelTab = "changes" | "workflow" | "preview";

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

export const PanelContext = createContext<PanelControl>({
  panel: { open: false, tab: "changes", file: null },
  show() {},
  toggle() {},
});

export const usePanel = () => useContext(PanelContext);
