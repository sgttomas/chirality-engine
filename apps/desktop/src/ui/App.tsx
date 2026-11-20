import { useMemo, useState } from "react";

import { useStore, useStoreHandle } from "../state/store";
import { Conversation } from "./Conversation";
import { FirstUse } from "./FirstUse";
import { Navigator } from "./Navigator";
import { useNotifications } from "./notify";
import { PanelContext, type Panel, type PanelTab } from "./panel";
import { RightPanel } from "./RightPanel";
import "./styles/layout.css";

export function App({ panel: initialTab }: { panel?: PanelTab }) {
  const store = useStoreHandle();
  const snapshot = useStore((s) => s.model?.snapshot);
  const error = useStore((s) => s.error);
  const [panel, setPanel] = useState<Panel>({ open: !!initialTab, tab: initialTab ?? "changes", file: null });
  const control = useMemo(
    () => ({
      panel,
      show: (tab: PanelTab, file: string | null = null) => setPanel({ open: true, tab, file }),
      toggle: () => setPanel((p) => ({ ...p, open: !p.open })),
    }),
    [panel],
  );
  useNotifications();

  if (!snapshot) return <div className="app-loading" />;
  const firstUse = snapshot.account.state !== "signedIn" || snapshot.projects.length === 0;

  return (
    <PanelContext.Provider value={control}>
      {firstUse ? (
        <FirstUse />
      ) : (
        <div className={`app ${panel.open ? "app--panel" : ""}`}>
          <Navigator />
          <Conversation />
          {panel.open && <RightPanel />}
        </div>
      )}
      {error && (
        <div className="toast" role="alert">
          <span>{error}</span>
          <button className="quiet" aria-label="Dismiss" onClick={() => store.dismissError()}>
            ×
          </button>
        </div>
      )}
    </PanelContext.Provider>
  );
}
