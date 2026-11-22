// Subagents in the interface: over what the real engine emitted when the agent started one (which
// asked the person, and started one of its own), and on the fake engine for stopping one.
import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import type { TurnSettings } from "../engine/types";
import { bridge } from "../fake/fixtures";
import { fakeEngine, mount, settle } from "../testing/render";
import { engineData, ReplayTransport } from "../testing/replay";

const nav = () => screen.getByRole("navigation", { name: "Projects" });

beforeEach(() => localStorage.clear());

describe("subagents", () => {
  it("are under their parent, ask from both threads, and open from where they were started", async () => {
    const engine = new ReplayTransport(engineData("subagent"), (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));
    const { store } = await mount(engine.client);
    const first = await act(async () => engine.play((e) => e.type === "thread"));
    if (first?.type !== "thread") throw new Error("no thread");
    const lead = first.thread.id;
    await act(() => store.selectThread(lead));
    await act(async () => void engine.play((e) => e.type === "thread" && !!e.thread.subagent && e.thread.status.state === "working"));
    await settle();
    // Folded by hand while it runs; shown again once it waits on the person.
    await userEvent.click(within(nav()).getByRole("button", { name: "1 subagent" }));
    expect(within(nav()).queryByRole("button", { name: /^worker/ })).not.toBeInTheDocument();
    await act(async () => void engine.play((e) => e.type === "thread" && e.thread.status.state === "needsApproval"));
    await settle();

    // The subagent that waits on the person is shown under its parent, with its mark, which its parent's row shows too.
    const worker = within(nav()).getByRole("button", { name: /^worker/ });
    expect(within(worker).getByLabelText("Needs you")).toBeInTheDocument();
    const leadRow = () => within(nav()).getAllByRole("button").find((b) => b.getAttribute("aria-current") === "page")!;
    expect(within(leadRow()).getByLabelText("Needs you")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem").length).toBeGreaterThan(2);
    // Its parent's conversation shows where it started, and its approval, from it.
    expect(screen.getByRole("button", { name: /Started worker/ })).toBeInTheDocument();
    const ask = () => screen.getByRole("dialog", { name: "Run this command?" });
    // Enter on the subagent's name opens it and answers nothing.
    within(ask()).getByRole("button", { name: "worker" }).focus();
    await userEvent.keyboard("{Enter}");
    await settle();
    expect(screen.getByRole("heading", { name: "worker" })).toBeInTheDocument();
    expect(engine.answers).toEqual([]);
    await act(() => store.selectThread(lead));
    await settle();
    await userEvent.click(within(ask()).getByRole("button", { name: "Allow" }));
    expect(engine.answers).toEqual([{ request: "1:0", answer: { kind: "allow" } }]);

    await act(async () => void engine.play((e) => e.type === "turnEnded" && e.thread === lead));
    await settle();
    expect(screen.getByRole("button", { name: "worker finished" })).toBeInTheDocument();
    expect(screen.getByText("Lead done.")).toBeInTheDocument();
    // Once none runs or needs the person, they fold under their parent.
    expect(within(nav()).getByRole("button", { name: "2 subagents" })).toHaveAttribute("aria-expanded", "false");
    expect(within(nav()).queryByRole("button", { name: /^worker/ })).not.toBeInTheDocument();
    await userEvent.click(within(nav()).getByRole("button", { name: "2 subagents" }));
    await userEvent.click(within(nav()).getByRole("button", { name: "2 subagents" }));
    expect(within(nav()).queryByRole("button", { name: /^worker/ })).not.toBeInTheDocument();

    // Opened from where it started, though folded by hand: its own work, no composer, and the way back.
    await userEvent.click(screen.getByRole("button", { name: /Started worker/ }));
    await settle();
    expect(screen.getByRole("heading", { name: "worker" })).toBeInTheDocument();
    expect(screen.getByText("Worker done.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "inner finished" })).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Fork/ })).not.toBeInTheDocument();
    expect(within(nav()).getByRole("button", { name: /^worker/ })).toHaveAttribute("aria-current", "page");
    await userEvent.click(within(screen.getByRole("main")).getByRole("button", { name: "New thread" }));
    await settle();
    expect(store.get().thread).toBe(lead);

    // Searched for, a folded subagent is found under its parent.
    expect(within(nav()).queryByRole("button", { name: /^inner/ })).not.toBeInTheDocument();
    await userEvent.type(within(nav()).getByRole("searchbox"), "inner");
    expect(within(nav()).getByRole("button", { name: /^inner/ })).toBeInTheDocument();
  });

  it("stops a running subagent from its own conversation", async () => {
    const engine = fakeEngine({ autoAnswer: (r) => (r.kind === "question" ? null : { kind: "allow" }) });
    const held = engine.stopAtMark("checking");
    const { store } = await mount(engine);
    const settings: TurnSettings = { model: "gpt-5.5", effort: "high", access: "ask" };
    const lead = await act(() => engine.startThread(bridge.id, { text: "Check the piles and the deck with a subagent each.", attachments: [], workflow: null, threads: [] }, settings));
    await act(() => held);
    await settle();
    await act(() => store.selectThread(lead));
    await userEvent.click(within(nav()).getByRole("button", { name: /^piles/ }));
    await settle();
    expect(screen.getByRole("heading", { name: "piles" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    await act(() => engine.settled(lead));
    await settle();
    expect(screen.getByText("Stopped")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
    await act(() => store.selectThread(lead));
    await settle();
    expect(screen.getByRole("button", { name: "deck finished" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "piles finished" })).not.toBeInTheDocument();
  });
});
