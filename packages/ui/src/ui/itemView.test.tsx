// An application's own view of items, over what the real engine emitted: a command it ran with
// approval, a turn whose work waited for a subagent and read two threads, and a question.
import { act, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Event, Item, ThreadId, TurnId } from "../engine/types";
import { nameOf } from "../state/selectors";
import { EngineProvider } from "../state/store";
import { mount, settle } from "../testing/render";
import { engineData, ReplayTransport, type EngineData } from "../testing/replay";
import { ItemView, type ItemViewer } from "./conversation/itemView";
import { StandardLayout } from "./StandardLayout";

const signedIn = (data: EngineData) => new ReplayTransport(data, (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));

/** Mounts `ui` over a recording, follows its first thread from its start, and plays `events` of it. */
async function open(data: EngineData, ui: ReactNode, events = 0) {
  const engine = signedIn(data);
  const mounted = await mount(engine.client, ui);
  const first = await act(async () => engine.play((e) => e.type === "thread"));
  if (first?.type !== "thread") throw new Error("no thread");
  await act(() => mounted.store.selectThread(first.thread.id));
  await play(engine, events - 2);
  return { engine, ...mounted };
}

/** Plays the next `count` events. */
async function play(engine: ReplayTransport, count = 1) {
  if (count <= 0) return;
  let played = 0;
  await act(async () => void engine.play(() => ++played === count));
  await settle();
}

const turns = (container: HTMLElement) => [...container.querySelectorAll("section.turn")].map((t) => t.outerHTML);
const folds = (container: HTMLElement) => [...container.querySelectorAll<HTMLDetailsElement>("section.turn details.work")];
const working = (container: HTMLElement) => container.querySelector(".working");

/** The recording with an item no view shows (a kind this interface does not know) after event `after`, in that event's turn. */
function withInvisible(data: EngineData, after: number, done: boolean): EngineData {
  const at = data.events[after - 1];
  if (at.type !== "item" && at.type !== "turnStarted") throw new Error("not in a turn");
  const thread: ThreadId = at.thread;
  const turn: TurnId = at.turn;
  const item = { id: `unseen-${after}`, body: { kind: "sleep" }, done } as unknown as Item;
  const extra: Event = { type: "item", thread, turn, item };
  const events = [...data.events.slice(0, after), extra, ...data.events.slice(after)];
  return { ...data, events, reads: data.reads.map((r) => (r.afterEvents >= after ? { ...r, afterEvents: r.afterEvents + 1 } : r)) };
}

const commandDone = 11; // command_approved: the command has run, the answer not yet begun
const answerDone = 14; // command_approved: the answer is in, the turn not yet ended

beforeEach(() => localStorage.clear());

describe("an application's view of items", () => {
  it("leaves the conversation as it was with no view, or one that gives no node, an unseen item after the work included", async () => {
    const data = engineData("command_approved");
    const plain = await open(data, <StandardLayout />, 2);
    const none = await open(data, <StandardLayout itemView={() => null} />, 2);
    const unset = await open(data, <StandardLayout itemView={() => undefined} />, 2);
    const unseen = await open(withInvisible(withInvisible(data, answerDone, false), commandDone, true), <StandardLayout />, 2);
    for (let event = 3; event <= data.events.length; event++) {
      for (const mounted of [plain, none, unset, unseen]) await play(mounted.engine);
      if (event === commandDone || event === answerDone) await play(unseen.engine);
      expect(turns(none.container)).toEqual(turns(plain.container));
      expect(turns(unset.container)).toEqual(turns(plain.container));
      expect(turns(unseen.container)).toEqual(turns(plain.container));
      if (event === commandDone) {
        expect(folds(plain.container)).toHaveLength(1);
        expect(folds(plain.container)[0]).toHaveClass("work--live");
        expect(working(plain.container)).toBeNull();
      }
      if (event === answerDone) expect(working(plain.container)).not.toBeNull();
    }
    expect(plain.engine.done && unseen.engine.done).toBe(true);
    expect(turns(plain.container)).toHaveLength(1);
  });

  it("keeps work in one fold across an unseen item between", async () => {
    // assignment_read: the reader's thread first shows at event 23; its first read is done by 44, its second by 46.
    const follow = async (data: EngineData) => {
      const engine = signedIn(data);
      const mounted = await mount(engine.client, <StandardLayout />);
      await play(engine, 23);
      await act(() => mounted.store.selectThread("00000000-0000-0000-0000-000000000009"));
      await settle();
      return { engine, ...mounted };
    };
    const data = engineData("assignment_read");
    const plain = await follow(data);
    const unseen = await follow(withInvisible(data, 44, true));
    await play(plain.engine, 46 - 23);
    await play(unseen.engine, 47 - 23);
    const reads = folds(unseen.container).at(-1)!;
    expect([...reads.querySelectorAll("li")].map((li) => li.textContent)).toEqual([expect.stringMatching(/^Read /), expect.stringMatching(/^Read /)]);
    expect(reads).toHaveClass("work--live");
    expect(turns(unseen.container)).toEqual(turns(plain.container));
  });

  it("gives an item with a node a block of its own, the work on either side folded, and joins them again", async () => {
    const engine = signedIn(engineData("assignment_read"));
    let shown = false;
    const view: () => ItemViewer = () => (item) => (shown && item.id === "call_read_peer" ? <p className="mine">The peer, read</p> : null);
    const { store, container, rerender } = await mount(engine.client, <StandardLayout itemView={view()} />);
    await act(async () => void engine.play());
    await settle();
    const threads = store.get().model!.snapshot.threads;
    const reader = threads.find((t) => t.title === "STUB[reader] Read the peer and the worker.")!;
    const worker = threads.find((t) => t.subagent)!;
    await act(() => store.selectThread(reader.id));
    await settle();
    const show = async (on: boolean) => {
      shown = on;
      rerender(
        <EngineProvider store={store}>
          <StandardLayout itemView={view()} />
        </EngineProvider>,
      );
      await settle();
    };
    const lines = (fold: HTMLElement) => [...fold.querySelectorAll("li")].map((li) => li.textContent);
    const answer = container.querySelector(".msg-agent");

    const [fold] = folds(container);
    expect(lines(fold)).toEqual(["Waited for subagents", "Read STUB[peer] Survey.", `Read ${nameOf(worker)}`]);
    fold.open = true;

    await show(true);
    const mine = container.querySelector(".mine")!;
    const [before, after] = folds(container);
    expect(folds(container)).toHaveLength(2);
    expect(before).toBe(fold);
    expect(before.open).toBe(true);
    expect(lines(before)).toEqual(["Waited for subagents"]);
    expect(lines(after)).toEqual([`Read ${nameOf(worker)}`]);
    expect(before.compareDocumentPosition(mine) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(mine.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    await show(true);
    expect(container.querySelector(".mine")).toBe(mine);
    expect(folds(container)).toEqual([before, after]);

    await show(false);
    expect(container.querySelector(".mine")).toBeNull();
    expect(folds(container)).toEqual([fold]);
    expect(fold.open).toBe(true);
    expect(lines(fold)).toEqual(["Waited for subagents", "Read STUB[peer] Survey.", `Read ${nameOf(worker)}`]);
    expect(container.querySelector(".msg-agent")).toBe(answer);
  });

  it("shows Working after the last block shown, a node of the application's own included", async () => {
    const data = withInvisible(engineData("command_approved"), commandDone, false);
    const view: ItemViewer = (item) => (item.body.kind === "command" ? <p className="mine">{item.done ? "Ran" : "Running"}</p> : null);
    const { engine, container } = await open(data, <StandardLayout itemView={view} />, 6);
    expect(container.querySelector(".mine")).toHaveTextContent("Running");
    expect(folds(container)).toHaveLength(0);
    expect(working(container)).toBeNull();
    await play(engine, commandDone - 6);
    expect(container.querySelector(".mine")).toHaveTextContent("Ran");
    expect(working(container)).not.toBeNull();
    expect(container.querySelector(".mine")!.compareDocumentPosition(working(container)!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    await play(engine); // the unseen item, still running
    expect(working(container)).not.toBeNull();
    await play(engine); // the answer begins
    expect(working(container)).toBeNull();
  });

  it("takes the layout's view over one around it, and the one around it when the layout has none", async () => {
    const data = engineData("command_approved");
    const outer: ItemViewer = (item) => (item.body.kind === "agentMessage" ? <p className="outer">{item.body.text}</p> : null);
    const inner: ItemViewer = (item) => (item.body.kind === "agentMessage" ? <p className="inner">{item.body.text}</p> : null);
    const around = (ui: ReactNode) => <ItemView.Provider value={outer}>{ui}</ItemView.Provider>;

    const kept = await open(data, around(<StandardLayout />), data.events.length);
    expect(kept.container.querySelector(".outer")).toHaveTextContent("Done.");
    expect(kept.container.querySelector(".msg-agent")).toBeNull();
    kept.unmount();

    const own = await open(data, around(<StandardLayout itemView={inner} />), data.events.length);
    expect(own.container.querySelector(".inner")).toHaveTextContent("Done.");
    expect(own.container.querySelector(".outer")).toBeNull();
  });

  it("asks the view once for each item each time a turn is drawn", async () => {
    const data = engineData("command_approved");
    const view = vi.fn<ItemViewer>((item) => (item.body.kind === "command" ? <p className="mine">Ran</p> : null));
    const { store, container } = await open(data, <StandardLayout itemView={view} />, data.events.length);
    const { model, thread } = store.get();
    const ids = model!.views[thread!].turns[0].items.map((i) => i.id);
    expect(ids).toHaveLength(3);
    expect(container.querySelector(".mine")).toHaveTextContent("Ran");
    const turn = container.querySelector<HTMLElement>("section.turn")!;
    for (const press of ["Revert", "Keep"]) {
      view.mockClear();
      await userEvent.click(within(turn).getByRole("button", { name: press }));
      expect(view.mock.calls.map(([item]) => item.id)).toEqual(ids);
    }
    expect(within(turn).getByRole("button", { name: "Revert" })).toBeInTheDocument();
  });

  it("keeps the conversation as it is when the layout's view comes and goes", async () => {
    const data = engineData("question");
    const view: ItemViewer = (item) => (item.body.kind === "userMessage" ? <p className="mine">{item.body.text}</p> : null);
    const { engine, store, container, rerender } = await open(data, <StandardLayout />, 6);
    const ask = within(container).getByRole("dialog", { name: "Colour" });
    const answer = within(ask).getByRole("textbox", { name: "Your answer" });
    await userEvent.type(answer, "Green");
    const composer = container.querySelector<HTMLTextAreaElement>("textarea.composer-input")!;
    await userEvent.type(composer, "After this");
    for (const itemView of [view, undefined, view]) {
      rerender(
        <EngineProvider store={store}>
          <StandardLayout itemView={itemView} />
        </EngineProvider>,
      );
      await settle();
      expect(container.querySelector(".mine") !== null).toBe(itemView !== undefined);
      expect(within(container).getByRole("textbox", { name: "Your answer" })).toBe(answer);
      expect(answer).toHaveValue("Green");
      expect(container.querySelector("textarea.composer-input")).toBe(composer);
      expect(composer).toHaveValue("After this");
    }
    expect(engine.answers).toEqual([]);
  });

  it("takes no view that answers `false`", () => {
    // @ts-expect-error `false` would hide every item it answers for
    const view: ItemViewer = (item) => item.body.kind === "command" && <p>Ran</p>;
    expect(view).toBeTypeOf("function");
  });
});
