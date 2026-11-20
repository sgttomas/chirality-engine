// The interface's one way to the engine. Two implementations: the Tauri
// adapter over the real engine, and a scripted fake for the browser and tests.
import type {
  Answer,
  Event,
  Message,
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
  /** Every engine event from now on, in order. Returns a function that stops it. */
  subscribe(onEvent: (event: Event) => void): () => void;

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

/** What the desktop itself provides: dialogs, the browser, notifications. */
export interface Host {
  pickFolder(): Promise<string | null>;
  pickFiles(): Promise<string[]>;
  openUrl(url: string): Promise<void>;
  notify(title: string, body: string): Promise<void>;
}

/** Whether the interface runs inside the desktop app. */
export const inTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
