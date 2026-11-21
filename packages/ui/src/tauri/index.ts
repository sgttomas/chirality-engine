// The engine and the desktop through Tauri: the adapter crate's commands and
// event (`chirality-engine-tauri`), and the dialog, notification and opener plugins.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { openUrl } from "@tauri-apps/plugin-opener";

import { createEngineClient, type EngineClient, type Host, type Transport } from "../engine/client";

/** Whether the interface runs inside a Tauri app. */
export const inTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const tauriTransport: Transport = {
  invoke: <T>(command: string, args?: Record<string, unknown>) => invoke<T>(command, args),
  listen: <T>(event: string, handler: (payload: T) => void) => listen<T>(event, (e) => handler(e.payload)),
};

export const tauriEngine: EngineClient = createEngineClient(tauriTransport);

export const tauriHost: Host = {
  async pickFolder() {
    const picked = await open({ directory: true, multiple: false });
    return typeof picked === "string" ? picked : null;
  },
  async pickFiles() {
    const picked = await open({ multiple: true });
    return picked ?? [];
  },
  openUrl: (url) => openUrl(url),
  async notify(title, body) {
    if (!(await isPermissionGranted()) && (await requestPermission()) !== "granted") return;
    sendNotification({ title, body });
  },
};
