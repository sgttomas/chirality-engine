// A fake engine in TypeScript: the same client as the real one, playing
// scripted turns so the interface can be built and tested in a browser.
import type { EngineClient } from "../engine/client";
import type {
  Answer,
  EngineError,
  Event,
  Item,
  ItemBody,
  ItemId,
  Message,
  Numbered,
  PendingRequest,
  Project,
  ProjectId,
  Request,
  RequestId,
  Snapshot,
  ThreadId,
  ThreadStatus,
  ThreadSummary,
  ThreadView,
  Turn,
  TurnId,
  TurnSettings,
} from "../engine/types";
import { subtreeOf } from "../state/selectors";
import { files, libraryWorkflows, models, projectWorkflow } from "./fixtures";
import { scriptFor, type Run, type Script } from "./scripts";

/** Names Codex gives subagents. */
const nicknames = ["Noether", "Hopper", "Lovelace", "Turing"];

export interface FakeOptions {
  /** Every wait resolves at once: for seeding scenes and for tests. */
  instant?: boolean;
  /** Multiplies every delay. */
  pace?: number;
  /** Answers requests as they come, while seeding. */
  autoAnswer?: (request: Request) => Answer | null;
}

class Stopped extends Error {}

interface Running {
  turn: Turn;
  stopped: boolean;
  steers: Message[];
  done: Promise<void>;
  /** Lets a turn held at a mark go on. */
  release?: () => void;
}

/** A short title, as Codex gives one: the first clause, about forty characters. */
const titleOf = (text: string) => {
  const clause = text.split(/[.,;:\n]/)[0].trim();
  if (clause.length <= 44) return clause || "New thread";
  return clause.slice(0, 44).replace(/\s+\S*$/, "");
};
const now = () => Math.floor(Date.now() / 1000);
const fail = (error: EngineError) => Promise.reject(error);

export class FakeEngine implements EngineClient {
  private snap: Snapshot;
  private views = new Map<ThreadId, ThreadView>();
  private listeners = new Set<(event: Numbered) => void>();
  private seq = 0;
  private running = new Map<ThreadId, Running>();
  private waiting = new Map<RequestId, { thread: ThreadId; resolve: (a: Answer) => void }>();
  private marks = new Map<string, () => void>();
  /** Files the fake's turns wrote, by absolute path. */
  private written = new Map<string, string>();
  private stopAt: string | null = null;
  private next = 0;

  constructor(public options: FakeOptions = {}) {
    this.snap = {
      codex: { state: "ready", version: "0.171.0" },
      account: { state: "signedOut" },
      models,
      projects: [],
      threads: [],
      pending: [],
      seq: 0,
    };
  }

  // Seeding, for scenes and tests.

  seedAccount(account: Snapshot["account"]) {
    this.snap.account = account;
  }
  seedProject(project: Project) {
    this.snap.projects.push(project);
  }
  /** The next run to reach `label` is held there until released or stopped; resolves then. */
  stopAtMark(label: string): Promise<void> {
    this.stopAt = label;
    return new Promise((resolve) => this.marks.set(label, resolve));
  }
  /** Lets a held turn go on. */
  release(thread: ThreadId) {
    this.running.get(thread)?.release?.();
  }
  /** Resolves when the thread's running turn ends. */
  settled(thread: ThreadId): Promise<void> {
    return this.running.get(thread)?.done ?? Promise.resolve();
  }
  setUpdated(thread: ThreadId, secondsAgo: number, unread = false) {
    const summary = this.summary(thread);
    summary.updatedAt = now() - secondsAgo;
    summary.unread = unread;
  }

  // The client.

  async snapshot() {
    return structuredClone({ ...this.snap, seq: this.seq });
  }
  /** The fake loses no events, so it sends no marks. */
  async subscribe(onEvent: (event: Numbered) => void) {
    this.listeners.add(onEvent);
    return () => void this.listeners.delete(onEvent);
  }

  async signIn() {
    this.emit({ type: "account", account: (this.snap.account = { state: "signingIn", url: "https://auth.example.invalid/sign-in" }) });
    await this.wait(1200);
    if (this.snap.account.state !== "signingIn") return;
    this.emit({ type: "account", account: (this.snap.account = { state: "signedIn", email: "person@example.com", plan: "Pro" }) });
  }
  async cancelSignIn() {
    this.emit({ type: "account", account: (this.snap.account = { state: "signedOut" }) });
  }
  async signOut() {
    this.emit({ type: "account", account: (this.snap.account = { state: "signedOut" }) });
  }

  async addProject(path: string) {
    const name = path.replace(/\/+$/, "").split("/").pop() || path;
    const project: Project = { id: this.id("p"), name, path };
    this.snap.projects.push(project);
    this.emit({ type: "projects", projects: structuredClone(this.snap.projects) });
    return project;
  }
  async removeProject(project: ProjectId) {
    this.snap.projects = this.snap.projects.filter((p) => p.id !== project);
    this.emit({ type: "projects", projects: structuredClone(this.snap.projects) });
  }
  async workflows(project: ProjectId) {
    const found = this.snap.projects.find((p) => p.id === project);
    if (!found) return fail({ kind: "notFound", what: `project ${project}` });
    return [...libraryWorkflows, projectWorkflow(found)];
  }

  async readThread(thread: ThreadId) {
    const view = this.views.get(thread);
    if (!view) return fail({ kind: "notFound", what: `thread ${thread}` });
    return structuredClone({ ...view, seq: this.seq });
  }
  async startThread(project: ProjectId, message: Message, settings: TurnSettings) {
    if (!this.snap.projects.some((p) => p.id === project)) return fail({ kind: "notFound", what: `project ${project}` });
    const summary: ThreadSummary = {
      id: this.id("t"),
      project,
      title: null,
      status: { state: "idle" },
      unread: false,
      forkedFrom: null,
      subagent: null,
      assignedBy: null,
      updatedAt: now(),
      settings,
    };
    this.snap.threads.unshift(summary);
    this.views.set(summary.id, { summary, turns: [], seq: 0 });
    this.emitThread(summary.id);
    this.startTurn(summary.id, message, settings);
    return summary.id;
  }

  /** A subagent of `parent`, as Codex makes one; its turn is its task, which Codex keeps in
   * neither thread. */
  private spawn(parent: ThreadId, task: string) {
    const of = this.summary(parent);
    const path = `${of.subagent?.path ?? "/root"}/${task}`;
    const summary: ThreadSummary = {
      ...structuredClone(of),
      id: this.id("t"),
      title: null,
      status: { state: "idle" },
      unread: false,
      forkedFrom: null,
      subagent: { parent, path, nickname: nicknames[this.next % nicknames.length] },
      assignedBy: null,
      updatedAt: now(),
    };
    this.snap.threads.unshift(summary);
    this.views.set(summary.id, { summary, turns: [], seq: 0 });
    this.emitThread(summary.id);
    return { id: summary.id, path };
  }
  async send(thread: ThreadId, message: Message, settings: TurnSettings) {
    if (!this.views.has(thread)) return fail({ kind: "notFound", what: `thread ${thread}` });
    if (this.summary(thread).subagent) return fail({ kind: "invalid", message: "A subagent takes its messages from the agent that started it." });
    const running = this.running.get(thread);
    if (!running) return this.startTurn(thread, message, settings);
    running.steers.push(message);
    this.addItem(thread, running.turn, this.userItem(message));
  }
  async interrupt(thread: ThreadId) {
    const running = this.running.get(thread);
    if (!running) return;
    running.stopped = true;
    running.release?.();
    for (const [id, w] of this.waiting) if (w.thread === thread) this.resolve(id, { kind: "cancel" });
  }
  async fork(thread: ThreadId, through: TurnId) {
    const view = this.views.get(thread);
    const at = view?.turns.findIndex((t) => t.id === through) ?? -1;
    if (!view || at < 0) return fail({ kind: "notFound", what: `turn ${through}` });
    const summary: ThreadSummary = {
      ...structuredClone(view.summary),
      id: this.id("t"),
      status: { state: "idle" },
      unread: false,
      forkedFrom: thread,
      updatedAt: now(),
    };
    this.snap.threads.unshift(summary);
    this.views.set(summary.id, { summary, turns: structuredClone(view.turns.slice(0, at + 1)), seq: 0 });
    this.emitThread(summary.id);
    return summary.id;
  }
  async revert(thread: ThreadId, from: TurnId) {
    const view = this.views.get(thread);
    const at = view?.turns.findIndex((t) => t.id === from) ?? -1;
    if (!view || at < 0) return fail({ kind: "notFound", what: `turn ${from}` });
    if (this.running.has(thread)) return fail({ kind: "invalid", message: "Stop the turn first." });
    view.turns = view.turns.slice(0, at);
    this.summary(thread).updatedAt = now();
    this.emitThread(thread);
  }
  async rename(thread: ThreadId, title: string) {
    this.summary(thread).title = title;
    this.emitThread(thread);
  }
  /** Archives the thread and, as Codex does, its subagents, with what they asked. */
  async archive(thread: ThreadId) {
    const gone = [thread, ...subtreeOf(this.snap.threads, thread).map((r) => r.thread.id)];
    for (const request of this.snap.pending.filter((p) => gone.includes(p.thread))) {
      this.waiting.delete(request.id);
      this.snap.pending = this.snap.pending.filter((p) => p.id !== request.id);
      this.emit({ type: "resolved", request: request.id });
    }
    for (const id of gone) {
      this.snap.threads = this.snap.threads.filter((t) => t.id !== id);
      this.views.delete(id);
      this.emit({ type: "threadRemoved", thread: id });
    }
  }
  async markRead(thread: ThreadId) {
    const summary = this.snap.threads.find((t) => t.id === thread);
    if (!summary?.unread) return;
    summary.unread = false;
    this.emitThread(thread);
  }

  async answer(request: RequestId, answer: Answer) {
    if (!this.waiting.has(request)) return fail({ kind: "notFound", what: `request ${request}` });
    this.resolve(request, answer);
  }
  async readFile(project: ProjectId, path: string) {
    const root = this.snap.projects.find((p) => p.id === project)?.path;
    if (!root) return fail({ kind: "notFound", what: `project ${project}` });
    const full = path.startsWith("/") ? path : `${root}/${path}`;
    const written = this.written.get(full);
    if (written !== undefined) return written;
    if (!files.some((f) => `${root}/${f}` === full)) return fail({ kind: "notFound", what: path });
    return `# ${full.split("/").pop()}\n`;
  }

  // Turns.

  /** A turn from the person's `message`, or a subagent's from its task (`script`). */
  private startTurn(thread: ThreadId, message: Message | null, settings: TurnSettings, given?: Script) {
    const view = this.views.get(thread)!;
    const turn: Turn = { id: this.id("turn"), items: [], state: { state: "running" } };
    const script = given ?? scriptFor(message!);
    view.turns.push(turn);
    const summary = view.summary;
    summary.settings = settings;
    if (!summary.title && message) summary.title = titleOf(message.text);
    const running: Running = { turn, stopped: false, steers: [], done: Promise.resolve() };
    this.running.set(thread, running);
    this.emit({ type: "turnStarted", thread, turn: turn.id });
    if (message) this.addItem(thread, turn, this.userItem(message));
    this.refresh(thread);
    const run = this.run(thread, running, message ?? { text: "", attachments: [], workflow: null, threads: [] }, settings);
    running.done = script(run)
      .then(() => this.answerSteers(run, running))
      .then(
        () => this.endTurn(thread, running, { state: "completed" }),
        (e) => this.endTurn(thread, running, e instanceof Stopped ? { state: "interrupted" } : { state: "failed", message: String(e) }),
      );
  }

  private async answerSteers(run: Run, running: Running) {
    for (const steer of running.steers.splice(0)) await run.stream("agentMessage", `Noted: ${steer.text}`);
  }

  private endTurn(thread: ThreadId, running: Running, state: Turn["state"]) {
    running.turn.state = state;
    this.running.delete(thread);
    this.emit({ type: "turnEnded", thread, turn: running.turn.id, state });
    const summary = this.summary(thread);
    summary.unread = !summary.subagent;
    summary.updatedAt = now();
    this.refresh(thread);
  }

  private run(thread: ThreadId, running: Running, message: Message, settings: TurnSettings): Run {
    const asks = settings.access !== "fullAccess";
    const check = () => {
      if (running.stopped) throw new Stopped();
    };
    const turn = running.turn;
    const run: Run = {
      message,
      asks,
      wait: async (ms) => {
        check();
        await this.wait(ms);
        check();
      },
      item: (body, done = false) => {
        check();
        const item: Item = { id: this.id("item"), body: this.absolute(thread, body), done };
        if (body.kind === "fileChange" && done) for (const f of (item.body as typeof body).files) this.write(f.path, f.diff);
        this.addItem(thread, turn, item);
        return item.id;
      },
      update: (id, body, done = false) => this.addItem(thread, turn, { id, body, done }),
      stream: async (kind, text) => {
        const id = run.item(kind === "agentMessage" ? { kind, text: "" } : { kind, summary: "" });
        for (const chunk of text.match(/(\S+\s*){1,3}/g) ?? []) {
          await run.wait(30);
          this.appendText(thread, turn, id, chunk);
        }
        this.addItem(thread, turn, { id, body: this.itemIn(turn, id).body, done: true });
        return id;
      },
      command: async (command, output, exitCode = 0) => {
        const id = run.item({ kind: "command", command, cwd: this.projectOf(thread).path, output: "", exitCode: null });
        await run.wait(350);
        for (const chunk of output.match(/([^\n]*\n){1,2}|[^\n]+$/g) ?? []) {
          await run.wait(60);
          this.appendText(thread, turn, id, chunk);
        }
        const body = this.itemIn(turn, id).body as Extract<ItemBody, { kind: "command" }>;
        this.addItem(thread, turn, { id, body: { ...body, exitCode }, done: true });
      },
      ask: async (request) => {
        check();
        if (request.kind === "fileChange") request = { ...request, files: request.files.map((f) => ({ ...f, path: this.full(thread, f.path) })) };
        const pending: PendingRequest = { id: this.id("req"), thread, request };
        const answer = new Promise<Answer>((resolve) => this.waiting.set(pending.id, { thread, resolve }));
        this.snap.pending.push(pending);
        this.emit({ type: "pending", request: structuredClone(pending) });
        // A subagent's request is the person's to see from where it shows; its parent tells what it did.
        if (!this.summary(thread).subagent) this.summary(thread).unread = true;
        this.refresh(thread);
        const auto = this.options.autoAnswer?.(request);
        if (auto) this.resolve(pending.id, auto);
        const given = await answer;
        if (given.kind === "cancel") running.stopped = true;
        check();
        return given;
      },
      delegate: (task, script) => {
        check();
        const child = this.spawn(thread, task);
        run.item({ kind: "subagent", thread: child.id, path: child.path, activity: "started" }, true);
        this.startTurn(child.id, null, settings, script);
        return child.id;
      },
      waitFor: async (children) => {
        const id = run.item({ kind: "subagentTool", tool: "wait", status: "running", threads: [], prompt: null });
        for (const child of children) {
          await this.settled(child);
          check();
          // One the person stopped is not reported to its parent, as with Codex.
          const ended = this.views.get(child)?.turns.at(-1)?.state.state;
          if (ended === "completed") run.item({ kind: "subagent", thread: child, path: this.summary(child).subagent!.path, activity: "finished" }, true);
        }
        run.update(id, { kind: "subagentTool", tool: "wait", status: "completed", threads: [], prompt: null }, true);
      },
      mark: async (label) => {
        if (label !== this.stopAt) return;
        this.stopAt = null;
        this.marks.get(label)?.();
        await new Promise<void>((resolve) => (running.release = resolve));
        check();
      },
    };
    return run;
  }

  private resolve(request: RequestId, answer: Answer) {
    const waiting = this.waiting.get(request);
    if (!waiting) return;
    this.waiting.delete(request);
    this.snap.pending = this.snap.pending.filter((p) => p.id !== request);
    this.emit({ type: "resolved", request });
    this.refresh(waiting.thread);
    waiting.resolve(answer);
  }

  // State.

  /** Paths as the engine gives them: absolute. */
  private full(thread: ThreadId, path: string) {
    return path.startsWith("/") ? path : `${this.projectOf(thread).path}/${path}`;
  }
  private absolute(thread: ThreadId, body: ItemBody): ItemBody {
    return body.kind === "fileChange" ? { ...body, files: body.files.map((f) => ({ ...f, path: this.full(thread, f.path) })) } : body;
  }
  /** Keeps what a change leaves: a new file's content, or the added lines of an edit. */
  private write(path: string, diff: string) {
    const isEdit = diff.split("\n").some((l) => l.startsWith("@@"));
    this.written.set(path, isEdit ? diff.split("\n").filter((l) => !l.startsWith("-") && !l.startsWith("@@")).map((l) => l.slice(1)).join("\n") : diff);
  }

  private userItem(message: Message): Item {
    return { id: this.id("item"), body: { kind: "userMessage", ...structuredClone(message), from: null }, done: true };
  }
  private addItem(thread: ThreadId, turn: Turn, item: Item) {
    const at = turn.items.findIndex((i) => i.id === item.id);
    if (at < 0) turn.items.push(item);
    else turn.items[at] = item;
    this.emit({ type: "item", thread, turn: turn.id, item: structuredClone(item) });
  }
  private appendText(thread: ThreadId, turn: Turn, id: ItemId, text: string) {
    const body = this.itemIn(turn, id).body;
    if (body.kind === "agentMessage") body.text += text;
    else if (body.kind === "reasoning") body.summary += text;
    else if (body.kind === "command") body.output += text;
    else return;
    this.emit({ type: "itemDelta", thread, turn: turn.id, item: id, kind: body.kind, text });
  }
  private itemIn(turn: Turn, id: ItemId) {
    return turn.items.find((i) => i.id === id)!;
  }
  private status(thread: ThreadId): ThreadStatus {
    const pending = this.snap.pending.filter((p) => p.thread === thread);
    if (pending.some((p) => p.request.kind !== "question")) return { state: "needsApproval" };
    if (pending.length) return { state: "needsAnswer" };
    return this.running.has(thread) ? { state: "working" } : { state: "idle" };
  }
  private refresh(thread: ThreadId) {
    this.summary(thread).status = this.status(thread);
    this.emitThread(thread);
  }
  private summary(thread: ThreadId) {
    return this.snap.threads.find((t) => t.id === thread)!;
  }
  private projectOf(thread: ThreadId) {
    return this.snap.projects.find((p) => p.id === this.summary(thread).project)!;
  }
  private emitThread(thread: ThreadId) {
    const summary = this.summary(thread);
    this.views.get(thread)!.summary = summary;
    this.emit({ type: "thread", thread: structuredClone(summary) });
  }
  private emit(event: Event) {
    const numbered = { seq: ++this.seq, event };
    for (const listener of this.listeners) listener(numbered);
  }
  private wait(ms: number) {
    if (this.options.instant) return Promise.resolve();
    return new Promise<void>((resolve) => setTimeout(resolve, ms * (this.options.pace ?? 1)));
  }
  private id(prefix: string) {
    return `${prefix}-${++this.next}`;
  }
}
