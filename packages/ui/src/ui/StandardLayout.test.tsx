import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { fakeEngine, mount, notices, settle } from "../testing/render";

const composer = () => document.querySelector<HTMLTextAreaElement>(".composer-input")!;
const type = async (text: string) => {
  fireEvent.change(composer(), { target: { value: text, selectionStart: text.length } });
  await settle();
};
const enter = async () => {
  fireEvent.keyDown(composer(), { key: "Enter" });
  await settle();
};

beforeEach(() => {
  localStorage.clear();
  notices.length = 0;
});

describe("first use", () => {
  it("goes from sign-in to a project to the composer", async () => {
    await mount(fakeEngine({ bare: true }));
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    await settle();
    await user.click(screen.getByRole("button", { name: "Choose folder" }));
    await settle();
    expect(screen.getByRole("heading", { name: "estimating-demo" })).toBeInTheDocument();
    expect(composer()).toHaveAttribute("placeholder", "What should we work on?");
  });
});

describe("a conversation", () => {
  it("asks for a file change in the composer area and is allowed in one press", async () => {
    await mount(fakeEngine());
    await type("Draft the scope of work for the foundations.");
    await enter();

    const ask = screen.getByRole("dialog", { name: "Make these changes?" });
    expect(within(ask).getByText("scope/SOW-01-foundations.md")).toBeInTheDocument();
    expect(screen.getByLabelText("Needs you")).toBeInTheDocument();

    await userEvent.click(within(ask).getByRole("button", { name: "Allow" }));
    await settle();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByText(/is in/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /SOW-01-foundations\.md/ })).toBeInTheDocument();
  });

  it("answers a question with one press and goes on with the answer", async () => {
    await mount(fakeEngine({ autoAnswer: (r) => (r.kind === "question" ? null : { kind: "allow" }) }));
    await type("Rerun the estimate.");
    await enter();
    const ask = screen.getByRole("dialog", { name: "Estimate class" });
    await userEvent.click(within(ask).getByRole("button", { name: /Class 3 \(budget\)/ }));
    await settle();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByText(/now follows/)).toHaveTextContent("Class 3");
  });

  it("turns Send into Stop while a turn runs, steers with Enter, and stops", async () => {
    const engine = fakeEngine();
    const held = engine.stopAtMark("streamed");
    await mount(engine);
    await type("Draft the scope of work for the foundations.");
    await enter();
    await act(() => held);
    await settle();

    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    await type("Use metric units throughout.");
    expect(screen.getByRole("button", { name: "Steer" })).toBeInTheDocument();
    await enter();
    expect(screen.getByText("Use metric units throughout.")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await settle();
    expect(screen.getByText("Stopped")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
  });

  it("shows the thread's changes in the right panel from a file change", async () => {
    await mount(fakeEngine({ autoAnswer: () => ({ kind: "allow" }) }));
    await type("Draft the scope of work for the foundations.");
    await enter();
    await userEvent.click(screen.getByRole("button", { name: /estimate\/basis\.md/ }));
    const panel = screen.getByRole("complementary", { name: "Side panel" });
    expect(within(panel).getByRole("tab", { name: /Changes/ })).toHaveAttribute("aria-selected", "true");
    expect(within(panel).getAllByText("estimate/basis.md").length).toBeGreaterThan(0);
    await userEvent.click(within(panel).getByRole("tab", { name: "Workflow" }));
    expect(within(panel).getByText("No workflow in this thread")).toBeInTheDocument();
  });

  it("forks from a turn into a thread of its own, under its parent", async () => {
    const { store } = await mount(fakeEngine({ autoAnswer: () => ({ kind: "allow" }) }));
    await type("Draft the scope of work for the foundations.");
    await enter();
    const parent = store.get().thread;
    await userEvent.click(screen.getByRole("button", { name: "Fork" }));
    await settle();
    expect(store.get().thread).not.toBe(parent);
    expect(screen.getByRole("button", { name: /Draft the scope of work for the foundations$/ })).toBeInTheDocument();
    const rows = screen.getAllByRole("button", { name: /Draft the scope/ }).filter((b) => b.classList.contains("nav-thread"));
    expect(rows).toHaveLength(2);
    expect(rows[1].querySelector(".nav-fork")).not.toBeNull();
  });

  it("references a file with @ and attaches it", async () => {
    await mount(fakeEngine());
    await type("Check @quant");
    const menu = await screen.findByRole("listbox", { name: "Files" });
    expect(within(menu).getByText("quantities.csv")).toBeInTheDocument();
    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    expect(composer().value).toBe("Check @estimate/quantities.csv ");
    expect(screen.getByLabelText("Remove estimate/quantities.csv")).toBeInTheDocument();
  });

  it("picks a workflow into the message", async () => {
    const { store } = await mount(fakeEngine({ autoAnswer: () => ({ kind: "allow" }) }));
    await userEvent.click(screen.getByRole("button", { name: /Workflow/ }));
    await userEvent.click(screen.getByRole("option", { name: /Basis of estimate/ }));
    await type("Write the basis for the deck.");
    await enter();
    const view = store.get().model!.views[store.get().thread!];
    expect(view.turns[0].items[0].body).toMatchObject({ kind: "userMessage", workflow: "wf-basis" });
    await userEvent.click(screen.getByRole("button", { name: "Side panel" }));
    await userEvent.click(screen.getByRole("tab", { name: "Workflow" }));
    expect(screen.getByRole("heading", { name: "Basis of estimate" })).toBeInTheDocument();
  });

  it("remembers the last model, reasoning level and access", async () => {
    const first = await mount(fakeEngine());
    await userEvent.click(screen.getByRole("button", { name: "GPT-5.5" }));
    await userEvent.click(screen.getByRole("option", { name: "GPT-5.5 mini" }));
    await userEvent.click(screen.getByRole("button", { name: "Medium" }));
    await userEvent.click(screen.getByRole("option", { name: "High" }));
    await userEvent.click(screen.getByRole("button", { name: "Ask" }));
    await userEvent.click(screen.getByRole("option", { name: /Full access/ }));
    first.unmount();

    await mount(fakeEngine());
    expect(screen.getByRole("button", { name: "GPT-5.5 mini" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "High" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Full access" })).toBeInTheDocument();
  });
});

describe("background threads", () => {
  it("notifies when a thread the person is not looking at needs them or finishes", async () => {
    const engine = fakeEngine({ autoAnswer: (r) => (r.kind === "question" ? null : { kind: "allow" }) });
    const { store } = await mount(engine);
    await type("Rerun the estimate.");
    await enter();
    const thread = store.get().thread!;
    act(() => store.selectProject(store.get().project!));
    await settle();

    await act(() => engine.answer(store.get().model!.snapshot.pending[0].id, { kind: "answers", answers: [["class", "Class 4"]] }));
    await act(() => engine.settled(thread));
    await settle();
    expect(notices).toContainEqual({ title: "Rerun the estimate", body: "Finished" });
    expect(screen.getByLabelText("Finished")).toBeInTheDocument();
  });
});

