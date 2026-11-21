// The interface's one way to the engine: one method per engine method, carried
// by a host transport (Tauri in an app; recorded engine data in tests).
import { engineEvent, type EngineCommand } from "./commands";
import type {
  Answer,
  Message,
  Numbered,
  Project,
  ProjectId,
  RequestId,
  Snapshot,
  ThreadId,
  ThreadView,
  TurnId,
  TurnSettings,
  Workflow,
} from "./types";

/** One method per engine method, with the engine's own types. */
export interface EngineClient {
  snapshot(): Promise<Snapshot>;
  /** Every engine event from now on, numbered, in order, and the host's marks after them.
   * Resolves once listening, with a function that stops it. */
  subscribe(onEvent: (event: Numbered) => void, onMark?: (mark: Mark) => void): Promise<() => void>;

  signIn(): Promise<void>;
  cancelSignIn(): Promise<void>;
  signOut(): Promise<void>;

  addProject(path: string): Promise<Project>;
  removeProject(project: ProjectId): Promise<void>;
  workflows(project: ProjectId): Promise<Workflow[]>;

  readThread(thread: ThreadId): Promise<ThreadView>;
  startThread(project: ProjectId, message: Message, settings: TurnSettings): Promise<ThreadId>;
  send(thread: ThreadId, message: Message, settings: TurnSettings): Promise<void>;
  interrupt(thread: ThreadId): Promise<void>;
  fork(thread: ThreadId, through: TurnId): Promise<ThreadId>;
  revert(thread: ThreadId, from: TurnId): Promise<void>;
  rename(thread: ThreadId, title: string): Promise<void>;
  archive(thread: ThreadId): Promise<void>;
  markRead(thread: ThreadId): Promise<void>;

  answer(request: RequestId, answer: Answer): Promise<void>;
  /** Absolute paths. */
  findFiles(project: ProjectId, query: string): Promise<string[]>;
  /** A file in the project, by absolute path or relative to it. */
  readFile(project: ProjectId, path: string): Promise<string>;
}

/** The number of the last event the host sent (`chirality_engine_tauri::Mark`): past the last
 * received, some were missed. */
export interface Mark {
  last: number;
}

/** How the interface reaches its host: a command with its arguments, and the host's events. */
export interface Transport {
  invoke<T>(command: EngineCommand, args?: Record<string, unknown>): Promise<T>;
  /** Calls `handler` with each payload of `event`. Resolves once listening, with a function that stops it. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;
}

/** What the desktop itself provides: dialogs, the browser, notifications. */
export interface Host {
  pickFolder(): Promise<string | null>;
  pickFiles(): Promise<string[]>;
  openUrl(url: string): Promise<void>;
  notify(title: string, body: string): Promise<void>;
}

/** The engine over a transport: each method is one command, events arrive as `engine`. */
export function createEngineClient(t: Transport): EngineClient {
  return {
    snapshot: () => t.invoke("snapshot"),
    subscribe: (onEvent, onMark) => t.listen<Numbered | Mark>(engineEvent, (p) => ("event" in p ? onEvent(p) : onMark?.(p))),
    signIn: () => t.invoke("sign_in"),
    cancelSignIn: () => t.invoke("cancel_sign_in"),
    signOut: () => t.invoke("sign_out"),
    addProject: (path) => t.invoke("add_project", { path }),
    removeProject: (project) => t.invoke("remove_project", { project }),
    workflows: (project) => t.invoke("workflows", { project }),
    readThread: (thread) => t.invoke("read_thread", { thread }),
    startThread: (project, message, settings) => t.invoke("start_thread", { project, message, settings }),
    send: (thread, message, settings) => t.invoke("send", { thread, message, settings }),
    interrupt: (thread) => t.invoke("interrupt", { thread }),
    fork: (thread, through) => t.invoke("fork", { thread, through }),
    revert: (thread, from) => t.invoke("revert", { thread, from }),
    rename: (thread, title) => t.invoke("rename", { thread, title }),
    archive: (thread) => t.invoke("archive", { thread }),
    markRead: (thread) => t.invoke("mark_read", { thread }),
    answer: (request, answer) => t.invoke("answer", { request, answer }),
    findFiles: (project, query) => t.invoke("find_files", { project, query }),
    readFile: (project, path) => t.invoke("read_file", { project, path }),
  };
}
