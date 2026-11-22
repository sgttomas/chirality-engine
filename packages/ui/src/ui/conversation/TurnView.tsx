import { Fragment, useContext, useState, type ReactNode } from "react";

import type { Item, ThreadId, Turn, Workflow } from "../../engine/types";
import { useStoreHandle } from "../../state/store";
import { Icon } from "../Icon";
import { blocksOf } from "./group";
import { ItemView } from "./itemView";
import { FromThread, ThreadToolItem } from "../Assignments";
import { Delegation } from "../Subagents";
import { AgentMessage, ErrorItem, FileChange, Plan, UserMessage, Work } from "./Items";

export function TurnView(props: { thread: ThreadId; turn: Turn; workflows: Workflow[]; canAct: boolean }) {
  const { thread, turn, workflows, canAct } = props;
  const store = useStoreHandle();
  const [confirmRevert, setConfirmRevert] = useState(false);
  const view = useContext(ItemView);
  const running = turn.state.state === "running";
  const blocks = blocksOf(turn.items, view ? turn.items.map((item) => view(item)) : []);
  const last = blocks.at(-1);
  const working = running && last?.kind !== "work" && last?.item.done !== false;

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
      {blocks.map((block, i) =>
        block.kind === "work" ? (
          <Work key={`work:${block.items[0].id}`} items={block.items} live={running && i === blocks.length - 1} />
        ) : (
          <Fragment key={`item:${block.item.id}`}>{block.kind === "view" ? block.node : standardView(block.item, workflows)}</Fragment>
        ),
      )}
      {working && (
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

/** An item as the standard layout shows it. */
function standardView(item: Item, workflows: Workflow[]): ReactNode {
  switch (item.body.kind) {
    case "userMessage":
      return item.body.from ? <FromThread item={item} workflows={workflows} /> : <UserMessage item={item} workflows={workflows} />;
    case "agentMessage":
      return <AgentMessage text={item.body.text} />;
    case "plan":
      return <Plan steps={item.body.steps} />;
    case "fileChange":
      return <FileChange item={item} />;
    case "subagent":
      return <Delegation item={item} />;
    case "threadTool":
      return <ThreadToolItem item={item} />;
    case "error":
      return <ErrorItem message={item.body.message} />;
    default:
      return null;
  }
}
