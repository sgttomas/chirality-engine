// The parts composed as an application would, over what the real engine emitted.
import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { fakeEngine, mount, settle } from "../testing/render";
import { applyTheme } from "./theme";
import { engineData, ReplayTransport } from "../testing/replay";
import { Composer } from "./composer/Composer";
import { Conversation, Turns } from "./Conversation";
import { ThreadList } from "./Navigator";
import { standardTabs } from "./RightPanel";
import { StandardLayout } from "./StandardLayout";

const signedIn = (name: string) => new ReplayTransport(engineData(name), (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));

beforeEach(() => localStorage.clear());

describe("parts in an application's own layout", () => {
  it("show the theme on their own root, not the page's", async () => {
    const { container } = await mount(fakeEngine(), <Conversation />);
    act(() => applyTheme("dark"));
    expect(container.querySelector(".chirality")).toHaveAttribute("data-theme", "dark");
    expect(document.documentElement).not.toHaveAttribute("data-theme");
    act(() => applyTheme("light"));
    expect(container.querySelector(".chirality")).toHaveAttribute("data-theme", "light");
  });

  it("runs a conversation with no side panel: the change shows, the approval is answered", async () => {
    const engine = signedIn("file_change");
    const { store } = await mount(
      engine.client,
      <div className="my-app">
        <aside>My outline</aside>
        <Conversation />
      </div>,
    );
    const first = await act(async () => engine.play((e) => e.type === "thread"));
    if (first?.type !== "thread") throw new Error("no thread");
    await act(() => store.selectThread(first.thread.id));
    await act(async () => void engine.play((e) => e.type === "pending"));
    await settle();
    const ask = screen.getByRole("dialog", { name: "Make these changes?" });
    await userEvent.click(within(ask).getByRole("button", { name: "Allow" }));
    expect(engine.answers).toEqual([{ request: "1:0", answer: { kind: "allow" } }]);
    await act(async () => void engine.play());
    await settle();
    expect(screen.getByText("My outline")).toBeInTheDocument();
    expect(screen.getAllByText("hello.txt").length).toBeGreaterThan(0);
    expect(screen.queryByRole("button", { name: /hello\.txt/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Side panel" })).not.toBeInTheDocument();
  });

  it("shows a project's threads, the turns and the composer as separate parts", async () => {
    const engine = signedIn("text_turn");
    const { store } = await mount(engine.client, <PartsFor />);
    await act(async () => void engine.play());
    await settle();
    const thread = store.get().model!.snapshot.threads[0];
    await act(() => store.selectThread(thread.id));
    await settle();
    expect(screen.getByRole("button", { name: /Greeting/ })).toHaveAttribute("aria-current", "page");
    expect(screen.getByText("Hello from the stub.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeInTheDocument();
  });
});

function PartsFor() {
  return (
    <>
      <ThreadList project="project-1" />
      <Turns />
      <Composer />
    </>
  );
}

describe("the standard layout with an application's own panel", () => {
  it("adds a tab beside the standard ones", async () => {
    const engine = signedIn("text_turn");
    const tabs = [...standardTabs, { tab: "notes", label: "Notes", render: () => <p>The application's notes</p> }];
    await mount(engine.client, <StandardLayout brand="Estimating" panelTabs={tabs} panel="notes" />);
    await act(async () => void engine.play());
    await settle();
    const panel = screen.getByRole("complementary", { name: "Side panel" });
    expect(within(panel).getAllByRole("tab").map((t) => t.textContent)).toEqual(["Changes", "Workflow", "Preview", "Notes"]);
    expect(within(panel).getByText("The application's notes")).toBeInTheDocument();
    await userEvent.click(within(panel).getByRole("tab", { name: "Workflow" }));
    expect(within(panel).getByText("No workflow in this thread")).toBeInTheDocument();
    expect(screen.getByText("Estimating")).toBeInTheDocument();
  });
});
