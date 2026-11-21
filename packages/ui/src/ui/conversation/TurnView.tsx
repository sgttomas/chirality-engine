import { useState } from "react";

import type { ThreadId, Turn, Workflow } from "../../engine/types";
import { useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";
import { blocksOf } from "./group";
import { AgentMessage, ErrorItem, FileChange, Plan, UserMessage, Work } from "./Items";

export function TurnView(props: { thread: ThreadId; turn: Turn; workflows: Workflow[]; canAct: boolean }) {
  const { thread, turn, workflows, canAct } = props;
  const store = useStoreHandle();
  const [confirmRevert, setConfirmRevert] = useState(false);
  const running = turn.state.state === "running";
  const blocks = blocksOf(turn.items);

  const fork = async () => {
    const id = await store.attempt(() => store.engine.fork(thread, turn.id));
    if (id) await store.selectThread(id);
  };
  const revert = async () => {
    setConfirmRevert(false);
    const reverted = await store.attempt(async () => {
      await store.engine.revert(thread, turn.id);
      return true;
    });
    if (reverted) await store.reload(thread);
  };

  return (
    <section className="turn" data-state={turn.state.state}>
      {blocks.map((block, i) => {
        if (block.kind === "work") return <Work key={block.items[0].id} items={block.items} live={running && i === blocks.length - 1} />;
        const { item } = block;
        switch (item.body.kind) {
          case "userMessage":
            return <UserMessage key={item.id} item={item} workflows={workflows} />;
          case "agentMessage":
            return <AgentMessage key={item.id} text={item.body.text} />;
          case "plan":
            return <Plan key={item.id} steps={item.body.steps} />;
          case "fileChange":
            return <FileChange key={item.id} item={item} />;
          case "error":
            return <ErrorItem key={item.id} message={item.body.message} />;
          default:
            return null;
        }
      })}
      {running && blocks.at(-1)?.kind !== "work" && turn.items.at(-1)?.done !== false && (
        <div className="working">
          <span className="mark mark--running" /> Working
        </div>
      )}
      {turn.state.state === "interrupted" && <div className="turn-note">Stopped</div>}
      {turn.state.state === "failed" && <ErrorItem message={turn.state.message} />}
      {!running && canAct && (
        <div className="turn-actions">
          {confirmRevert ? (
            <>
              <span className="turn-confirm">Remove this turn and the ones after it? Files stay as they are.</span>
              <button className="quiet" onClick={revert}>
                Remove
              </button>
              <button className="quiet" onClick={() => setConfirmRevert(false)}>
                Keep
              </button>
            </>
          ) : (
            <>
              <button className="quiet" onClick={fork} title="A new thread from here">
                <Icon name="fork" /> Fork
              </button>
              <button className="quiet" onClick={() => setConfirmRevert(true)} title="Remove this turn and the ones after it">
                <Icon name="undo" /> Revert
              </button>
            </>
          )}
        </div>
      )}
    </section>
  );
}
