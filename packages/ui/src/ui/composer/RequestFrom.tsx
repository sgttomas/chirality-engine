import type { PendingRequest } from "../../engine/types";
import { nameOf } from "../../state/selectors";
import { useStore, useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";

/** Which subagent a request in its parent's conversation comes from; opens it. */
export function RequestFrom({ pending }: { pending: PendingRequest }) {
  const store = useStoreHandle();
  const here = useStore((s) => s.thread);
  const from = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === pending.thread));
  if (!from || pending.thread === here) return null;
  return (
    <button className="ask-from" onClick={() => store.selectThread(from.id)}>
      <Icon name="subagent" /> {nameOf(from)}
    </button>
  );
}
