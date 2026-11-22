// Assignment between threads in the interface, over what the real engine emitted when an agent
// listed the threads, assigned one with a workflow, heard its report, closed the assignment and
// sent one more message.
import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { buildScene } from "../fake/scenes";
import { mount, settle } from "../testing/render";
import { engineData, ReplayTransport } from "../testing/replay";
import { nameOf } from "../state/selectors";

const nav = () => screen.getByRole("navigation", { name: "Projects" });
const main = () => screen.getByRole("main");

beforeEach(() => localStorage.clear());

describe("assignment", () => {
  it("shows each thread's messages as from the other, opens either from the other, and nests neither", async () => {
    const engine = new ReplayTransport(engineData("assignment"), (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));
    const { store } = await mount(engine.client);
    await act(async () => void engine.play());
    await settle();
    const threads = store.get().model!.snapshot.threads;
    const worker = threads.find((t) => t.assignedBy)!;
    const lead = threads.find((t) => t.id === worker.assignedBy)!;
    await act(() => store.selectThread(lead.id));
    await settle();

    // The lead's conversation: where it assigned, what came back from the thread, and what it did next.
    expect(within(main()).getByRole("button", { name: "Assigned Piles" })).toBeInTheDocument();
    expect(within(main()).getByRole("button", { name: "Piles" })).toBeInTheDocument();
    expect(within(main()).getByText("The piles are within tolerance. STUB[heard]")).toBeInTheDocument();
    expect(within(main()).getByRole("button", { name: "Closed Piles" })).toBeInTheDocument();
    expect(within(main()).getByRole("button", { name: "Messaged Piles" })).toBeInTheDocument();
    expect(within(main()).getByText("Listed the threads").tagName).toBe("LI");

    // Both are ordinary threads, at the same level in the list.
    const rows = within(nav()).getAllByRole("button").filter((b) => b.classList.contains("nav-thread"));
    expect(rows.map((r) => r.style.paddingLeft)).toEqual(["26px", "26px"]);
    expect(within(nav()).queryByRole("button", { name: /subagent/ })).not.toBeInTheDocument();

    // Opened from the lead: its brief and the later message are from the lead, not the person, and it names the lead.
    await userEvent.click(within(main()).getByRole("button", { name: "Assigned Piles" }));
    await settle();
    expect(store.get().thread).toBe(worker.id);
    expect(screen.getByRole("heading", { name: "Piles" })).toBeInTheDocument();
    expect(within(main()).getByText("STUB[worker] Check the pile lengths.")).toBeInTheDocument();
    expect(within(main()).getByText("survey").closest(".msg-from")).not.toBeNull();
    expect(within(main()).getAllByRole("button", { name: nameOf(lead) })).toHaveLength(3);
    expect(main().querySelector(".msg-user")).toBeNull();

    // And back to the lead, from the message it sent.
    await userEvent.click(within(main()).getAllByRole("button", { name: nameOf(lead) })[1]);
    await settle();
    expect(store.get().thread).toBe(lead.id);
  });

  it("is a scene from the engine's own data", async () => {
    const scene = await buildScene("assignment");
    const { store } = await mount(scene.engine);
    await act(() => store.selectThread(scene.thread!));
    await settle();
    expect(within(main()).getByRole("button", { name: "Assigned Piles" })).toBeInTheDocument();
    expect(within(main()).getByText("Closed.")).toBeInTheDocument();
  });
});
