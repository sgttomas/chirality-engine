import type { ThreadRef } from "../../engine/types";
import { nameOf } from "../../state/selectors";
import { useStore, useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";

/** The threads a message mentions, each by its title now (or the one it had when mentioned), opening it. */
export function ThreadChips({ threads }: { threads: ThreadRef[] }) {
  return (
    <>
      {threads.map((r) => (
        <ThreadChip key={r.thread} reference={r} />
      ))}
    </>
  );
}

function ThreadChip({ reference }: { reference: ThreadRef }) {
  const store = useStoreHandle();
  const thread = useStore((s) => s.model!.snapshot.threads.find((t) => t.id === reference.thread));
  const name = thread ? nameOf(thread) : (reference.title ?? "A thread");
  return (
    <button className="chip chip--thread" disabled={!thread} onClick={() => thread && store.selectThread(thread.id)}>
      <Icon name="at" /> {name}
    </button>
  );
}
