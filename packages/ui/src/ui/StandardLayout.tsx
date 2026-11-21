import { useEffect, type ReactNode } from "react";

import { markLabel, markOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Conversation } from "./Conversation";
import { FirstUse } from "./FirstUse";
import { Navigator } from "./Navigator";
import { useNotifications } from "./notify";
import { PanelContext, usePanelState, type PanelTab } from "./panel";
import { RightPanel, standardTabs, type PanelTabSpec } from "./RightPanel";
import "./styles/base.css";
import "./styles/layout.css";

export interface StandardLayoutProps {
  /** The application's name, shown in the navigator, first use and the window title. */
  brand?: string;
  /** The side panel's tabs: the standard ones by default; an application adds its own. */
  panelTabs?: PanelTabSpec[];
  /** A side panel tab to open at start. */
  panel?: PanelTab;
}

/** Projects and threads on the left, the conversation in the centre, the side panel on the
 * right; first use until the person is signed in with a project. */
export function StandardLayout({ brand = "Chirality", panelTabs = standardTabs, panel }: StandardLayoutProps) {
  const snapshot = useStore((s) => s.model?.snapshot);
  const control = usePanelState(panel);
  useNotifications();
  useWindowTitle(brand);

  if (!snapshot) return <div className="app-loading" />;
  const firstUse = snapshot.account.state !== "signedIn" || snapshot.projects.length === 0;

  return (
    <PanelContext.Provider value={control}>
      {firstUse ? (
        <FirstUse brand={brand} />
      ) : (
        <div className={`app ${control.panel.open ? "app--panel" : ""}`}>
          <Navigator brand={brand} />
          <Conversation />
          {control.panel.open && <RightPanel tabs={panelTabs} />}
        </div>
      )}
      <ErrorNotice />
    </PanelContext.Provider>
  );
}

/** The last engine call that failed, until dismissed or the next one succeeds. */
export function ErrorNotice(): ReactNode {
  const store = useStoreHandle();
  const error = useStore((s) => s.error);
  if (!error) return null;
  return (
    <div className="toast" role="alert">
      <span>{error}</span>
      <button className="quiet" aria-label="Dismiss" onClick={() => store.dismissError()}>
        ×
      </button>
    </div>
  );
}

/** The window's title: the selected thread and its mark, or the application's name. */
export function useWindowTitle(brand: string) {
  const summary = useStore((s) => s.model?.snapshot.threads.find((t) => t.id === s.thread));
  useEffect(() => {
    const mark = summary && markOf(summary);
    document.title = summary ? `${summary.title ?? "New thread"}${mark && mark !== "finished" ? ` · ${markLabel[mark]}` : ""}` : brand;
  }, [summary, brand]);
}
