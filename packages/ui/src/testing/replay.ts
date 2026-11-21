// Data the real engine produced from Codex's recorded transcripts
// (crates/engine/src/testkit/export.rs), and a transport that plays it back
// to the real client.
import { createEngineClient, type EngineClient, type Transport } from "../engine/client";
import { engineEvent, type EngineCommand } from "../engine/commands";
import type { Answer, Event, Numbered, Snapshot, ThreadView } from "../engine/types";

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

const notRecorded = { kind: "invalid", message: "not in the recording" };

/** Plays recorded events on demand; records what the interface asks of it. */
export class ReplayTransport implements Transport {
  private listeners = new Set<(event: Numbered) => void>();
  private at = 0;
  readonly answers: { request: string; answer: Answer }[] = [];
  /** Commands the recording has no answer for, by name. */
  readonly calls: EngineCommand[] = [];
  readonly reads: string[] = [];
  /** The engine client over this transport. */
  readonly client: EngineClient = createEngineClient(this);

  constructor(
    readonly data: EngineData,
    private patch: (s: Snapshot) => Snapshot = (s) => s,
  ) {}

  /** Emits events, numbered as the engine numbered them, until `stop` accepts one (it is emitted too), or to the end. */
  play(stop: (event: Event) => boolean = () => false) {
    while (this.at < this.data.events.length) {
      const event = this.data.events[this.at++];
      const numbered = { seq: this.data.before.seq + this.at, event: structuredClone(event) };
      for (const listener of this.listeners) listener(numbered);
      if (stop(event)) return event;
    }
    return undefined;
  }
  get done() {
    return this.at >= this.data.events.length;
  }

  async listen<T>(event: string, handler: (payload: T) => void) {
    if (event !== engineEvent) return () => {};
    const listener = handler as unknown as (event: Numbered) => void;
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  }

  async invoke<T>(command: EngineCommand, args: Record<string, unknown> = {}): Promise<T> {
    return (await this.answer(command, args)) as T;
  }

  private async answer(command: EngineCommand, args: Record<string, unknown>): Promise<unknown> {
    switch (command) {
      case "snapshot":
        return this.patch(structuredClone(this.data.before));
      case "read_thread":
        return this.readThread(args.thread as string);
      case "answer":
        this.answers.push({ request: args.request as string, answer: args.answer as Answer });
        return null;
      case "mark_read":
        return null;
      case "workflows":
      case "find_files":
        return [];
      case "read_file":
        this.reads.push(args.path as string);
        return "hello\n";
      case "add_project":
      case "start_thread":
      case "fork":
        throw notRecorded;
      default:
        this.calls.push(command);
        return null;
    }
  }

  /** The engine's own read when there is one by now; a thread not yet read has no turns. */
  private readThread(thread: string): ThreadView {
    const read = this.data.reads.filter((r) => r.view.summary.id === thread && r.afterEvents <= this.at).at(-1);
    if (read) return structuredClone(read.view);
    const summary = [...this.data.events.slice(0, this.at)].reverse().find((e) => e.type === "thread" && e.thread.id === thread);
    if (summary?.type !== "thread") throw { kind: "notFound", what: thread };
    return { summary: structuredClone(summary.thread), turns: [], seq: this.data.before.seq + this.at };
  }
}
