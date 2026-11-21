// Assignment between threads: a message another thread sent, shown as from that thread; the
// agent's assign, message and close, each opening the other thread; and, on an assigned thread,
// the thread that assigned it.
import type { Item, ThreadId, ThreadSummary } from "../engine/types";
import { nameOf } from "../state/selectors";
import { useStore, useStoreHandle } from "../state/store";
import { Icon } from "./Icon";

/** The other thread, when the snapshot has it. */
function useThread(id: ThreadId | null | undefined): ThreadSummary | undefined {
  return useStore((s) => (id ? s.model!.snapshot.threads.find((t) => t.id === id) : undefined));
}

const ended = { completed: null, stopped: "Stopped", failed: "Failed" } as const;

/** A message from another thread: its title, which opens it, then the text. */
export function FromThread({ item }: { item: Item }) {
  const store = useStoreHandle();
  const from = item.body.kind === "userMessage" ? item.body.from : null;
  const sender = useThread(from?.thread);
  if (item.body.kind !== "userMessage" || !from) return null;
  const name = sender ? nameOf(sender) : (from.title ?? "Another thread");
  const end = from.report ? ended[from.report] : null;
  return (
    <div className="msg-from">
      <button className="msg-from-head" disabled={!sender} onClick={() => sender && store.selectThread(sender.id)}>
        <Icon name="at" /> {name}
        {end && <span className="msg-from-end">{end}</span>}
      </button>
      {item.body.text && <div className="msg-from-text">{item.body.text}</div>}
    </div>
  );
}

/** The agent assigned, messaged or closed another thread; opens it. */
export function ThreadToolItem({ item }: { item: Item }) {
  const store = useStoreHandle();
  const body = item.body.kind === "threadTool" ? item.body : null;
  const other = useThread(body?.thread);
  if (!body || body.tool === "threads") return null;
  const name = other ? nameOf(other) : "a thread";
  const failed = body.status === "failed";
  const said = {
    assign: failed ? "Could not assign a thread" : body.status === "running" ? "Assigning a thread" : `Assigned ${name}`,
    message: failed ? `Could not message ${name}` : `Messaged ${name}`,
    close: failed ? `Could not close ${name}` : `Closed ${name}`,
  }[body.tool];
  return (
    <button className="delegation" disabled={!other} title={body.text ?? undefined} onClick={() => other && store.selectThread(other.id)}>
      <Icon name="at" />
      <span className="delegation-text">{said}</span>
    </button>
  );
}

/** On an assigned thread, the thread that assigned it, which opens it. */
export function AssignedBy({ thread }: { thread: ThreadSummary }) {
  const store = useStoreHandle();
  const assigner = useThread(thread.assignedBy);
  if (!assigner) return null;
  return (
    <button className="conv-parent" onClick={() => store.selectThread(assigner.id)}>
      <Icon name="at" /> {nameOf(assigner)}
    </button>
  );
}
