// Keeps the interface's copy of the engine's state current, one event at a time.
import type { Event, Item, ItemBody, Snapshot, ThreadId, ThreadView, Turn } from "../engine/types";

export interface Model {
  /** Its `seq` is the last event applied (when `applyEvent` is given the event's number). */
  snapshot: Snapshot;
  /** The threads read so far, kept current from events. */
  views: Record<ThreadId, ThreadView>;
}

const upsert = <T extends { id: unknown }>(list: T[], value: T) => {
  const at = list.findIndex((v) => v.id === value.id);
  return at < 0 ? [...list, value] : list.map((v, i) => (i === at ? value : v));
};

const withTurn = (view: ThreadView, id: string, change: (turn: Turn) => Turn): ThreadView => {
  const exists = view.turns.some((t) => t.id === id);
  const turns = exists ? view.turns : [...view.turns, { id, items: [], state: { state: "running" as const } }];
  return { ...view, turns: turns.map((t) => (t.id === id ? change(t) : t)) };
};

const appended = (body: ItemBody, text: string): ItemBody => {
  switch (body.kind) {
    case "agentMessage":
      return { ...body, text: body.text + text };
    case "reasoning":
      return { ...body, summary: body.summary + text };
    case "command":
      return { ...body, output: body.output + text };
    default:
      return body;
  }
};

/** A thread's view with one of that thread's events applied. */
export function applyToView(view: ThreadView, event: Event): ThreadView {
  switch (event.type) {
    case "thread":
      return { ...view, summary: event.thread };
    case "turnStarted":
      return withTurn(view, event.turn, (t) => t);
    case "turnEnded":
      return withTurn(view, event.turn, (t) => ({ ...t, state: event.state }));
    case "item":
      return withTurn(view, event.turn, (t) => ({ ...t, items: upsert<Item>(t.items, event.item) }));
    case "itemDelta":
      // A done item's text is whole.
      return withTurn(view, event.turn, (t) => ({
        ...t,
        items: t.items.map((i) => (i.id === event.item && !i.done ? { ...i, body: appended(i.body, event.text) } : i)),
      }));
    default:
      return view;
  }
}

export const threadOf = (event: Event): ThreadId | null => {
  switch (event.type) {
    case "thread":
      return event.thread.id;
    case "turnStarted":
    case "turnEnded":
    case "item":
    case "itemDelta":
      return event.thread;
    default:
      return null;
  }
};

/** The model with `event` applied. Given its number `seq`, a view already
 * current to it (read after it) is left as it is, and the model is numbered. */
export function applyEvent(model: Model, event: Event, seq?: number): Model {
  const applied = apply(model, event, seq);
  return seq === undefined ? applied : { ...applied, snapshot: { ...applied.snapshot, seq } };
}

function apply(model: Model, event: Event, seq: number | undefined): Model {
  const snapshot = applyToSnapshot(model.snapshot, event);
  let views = model.views;
  const thread = threadOf(event);
  const view = thread ? views[thread] : undefined;
  if (thread && view && (seq === undefined || seq > view.seq)) {
    views = { ...views, [thread]: { ...applyToView(view, event), ...(seq === undefined ? {} : { seq }) } };
  }
  if (event.type === "threadRemoved" && views[event.thread]) {
    views = { ...views };
    delete views[event.thread];
  }
  return snapshot === model.snapshot && views === model.views ? model : { snapshot, views };
}

function applyToSnapshot(s: Snapshot, event: Event): Snapshot {
  switch (event.type) {
    case "codex":
      return { ...s, codex: event.status };
    case "account":
      return { ...s, account: event.account };
    case "models":
      return { ...s, models: event.models };
    case "projects":
      return { ...s, projects: event.projects };
    case "thread": {
      const known = s.threads.some((t) => t.id === event.thread.id);
      const threads = known ? s.threads.map((t) => (t.id === event.thread.id ? event.thread : t)) : [event.thread, ...s.threads];
      return { ...s, threads };
    }
    case "threadRemoved":
      return {
        ...s,
        threads: s.threads.filter((t) => t.id !== event.thread),
        pending: s.pending.filter((p) => p.thread !== event.thread),
      };
    case "pending":
      return { ...s, pending: upsert(s.pending, event.request) };
    case "resolved":
      return { ...s, pending: s.pending.filter((p) => p.id !== event.request) };
    default:
      return s;
  }
}
