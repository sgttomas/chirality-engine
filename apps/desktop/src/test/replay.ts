// Data the real engine produced from Codex's recorded transcripts
// (crates/engine/src/testkit/export.rs), and a client that plays it back.
import type { EngineClient } from "../engine/client";
import type { Answer, Event, Snapshot, ThreadId, ThreadView } from "../engine/types";

export interface EngineData {
  before: Snapshot;
  events: Event[];
  after: Snapshot;
  /** Each thread view the scenario read, after how many of the events. */
  reads: { afterEvents: number; view: ThreadView }[];
}

const files = import.meta.glob<EngineData>("./engine-data/*.json", { eager: true, import: "default" });

export function engineData(name: string): EngineData {
  const data = files[`./engine-data/${name}.json`];
  if (!data) throw new Error(`no engine data ${name}`);
  return structuredClone(data);
}

export const engineDataNames = Object.keys(files).map((f) => f.replace(/^.*\/(.*)\.json$/, "$1"));

/** Plays recorded events on demand; records what the interface asks of it. */
export class ReplayEngine implements EngineClient {
  private listeners = new Set<(event: Event) => void>();
  private at = 0;
  readonly answers: { request: string; answer: Answer }[] = [];
  readonly calls: string[] = [];
  readonly reads: string[] = [];

  constructor(
    readonly data: EngineData,
    private patch: (s: Snapshot) => Snapshot = (s) => s,
  ) {}

  /** Emits events until `stop` accepts one (it is emitted too), or to the end. */
  play(stop: (event: Event) => boolean = () => false) {
    while (this.at < this.data.events.length) {
      const event = this.data.events[this.at++];
      for (const listener of this.listeners) listener(structuredClone(event));
      if (stop(event)) return event;
    }
    return undefined;
  }
  get done() {
    return this.at >= this.data.events.length;
  }

  async snapshot() {
    return this.patch(structuredClone(this.data.before));
  }
  subscribe(onEvent: (event: Event) => void) {
    this.listeners.add(onEvent);
    return () => void this.listeners.delete(onEvent);
  }
  /** The engine's own read when there is one by now; a thread not yet read has no turns. */
  async readThread(thread: ThreadId): Promise<ThreadView> {
    const read = this.data.reads.filter((r) => r.view.summary.id === thread && r.afterEvents <= this.at).at(-1);
    if (read) return structuredClone(read.view);
    const summary = [...this.data.events.slice(0, this.at)].reverse().find((e) => e.type === "thread" && e.thread.id === thread);
    if (summary?.type !== "thread") throw { kind: "notFound", what: thread };
    return { summary: structuredClone(summary.thread), turns: [] };
  }
  async answer(request: string, answer: Answer) {
    this.answers.push({ request, answer });
  }
  async markRead() {}
  async workflows() {
    return [];
  }
  async readFile(_project: string, path: string) {
    this.reads.push(path);
    return "hello\n";
  }
  async findFiles() {
    return [];
  }

  private refuse = (name: string) => async () => {
    this.calls.push(name);
  };
  signIn = this.refuse("signIn");
  cancelSignIn = this.refuse("cancelSignIn");
  signOut = this.refuse("signOut");
  addProject = async (): Promise<never> => {
    throw { kind: "invalid", message: "not in the recording" };
  };
  removeProject = this.refuse("removeProject");
  startThread = async (): Promise<never> => {
    throw { kind: "invalid", message: "not in the recording" };
  };
  send = this.refuse("send");
  interrupt = this.refuse("interrupt");
  fork = async (): Promise<never> => {
    throw { kind: "invalid", message: "not in the recording" };
  };
  revert = this.refuse("revert");
  rename = this.refuse("rename");
  archive = this.refuse("archive");
}
