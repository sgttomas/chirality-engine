// The engine through Tauri: each method is one command, events arrive as `engine`.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { openUrl } from "@tauri-apps/plugin-opener";

import type { EngineClient, Host } from "./client";
import type { Event } from "./types";

export const tauriEngine: EngineClient = {
  snapshot: () => invoke("snapshot"),
  subscribe(onEvent) {
    const stop = listen<Event>("engine", (e) => onEvent(e.payload));
    return () => void stop.then((unlisten) => unlisten());
  },
  signIn: () => invoke("sign_in"),
  cancelSignIn: () => invoke("cancel_sign_in"),
  signOut: () => invoke("sign_out"),
  addProject: (path) => invoke("add_project", { path }),
  removeProject: (project) => invoke("remove_project", { project }),
  workflows: (project) => invoke("workflows", { project }),
  readThread: (thread) => invoke("read_thread", { thread }),
  startThread: (project, message, settings) => invoke("start_thread", { project, message, settings }),
  send: (thread, message, settings) => invoke("send", { thread, message, settings }),
  interrupt: (thread) => invoke("interrupt", { thread }),
  fork: (thread, through) => invoke("fork", { thread, through }),
  revert: (thread, from) => invoke("revert", { thread, from }),
  rename: (thread, title) => invoke("rename", { thread, title }),
  archive: (thread) => invoke("archive", { thread }),
  markRead: (thread) => invoke("mark_read", { thread }),
  answer: (request, answer) => invoke("answer", { request, answer }),
  findFiles: (project, query) => invoke("find_files", { project, query }),
};

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

export const inTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
