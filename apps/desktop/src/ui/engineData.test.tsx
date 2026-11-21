// The interface over what the real engine emitted from recorded Codex sessions.
import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { mount, settle } from "../test/render";
import { engineData, ReplayEngine } from "../test/replay";

const signedIn = (name: string) => new ReplayEngine(engineData(name), (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));

/** Mounts over a recording and follows its first thread from its start, as after starting it. */
async function open(name: string) {
  const engine = signedIn(name);
  const mounted = await mount(engine);
  const first = await act(async () => engine.play((e) => e.type === "thread"));
  if (first?.type !== "thread") throw new Error("no thread");
  await act(() => mounted.store.selectThread(first.thread.id));
  await settle();
  return { engine, ...mounted, thread: first.thread.id };
}

const playTo = async (engine: ReplayEngine, type: string) => {
  await act(async () => void engine.play((e) => e.type === type));
  await settle();
};

beforeEach(() => localStorage.clear());

describe("over the real engine's data", () => {
  it("shows first use while signed out", async () => {
    await mount(new ReplayEngine(engineData("text_turn")));
    expect(screen.getByRole("button", { name: "Sign in" })).toBeInTheDocument();
  });

  it("streams a turn, then shows the title the engine gives", async () => {
    const { engine } = await open("text_turn");
    await playTo(engine, "turnEnded");
    expect(screen.getByText("Hello from the stub.")).toBeInTheDocument();
    await act(async () => void engine.play());
    await settle();
    expect(screen.getByRole("heading", { name: "Greeting" })).toBeInTheDocument();
  });

  it("asks a command approval and sends Allow in one press", async () => {
    const { engine } = await open("command_approved");
    await playTo(engine, "pending");
    await playTo(engine, "thread");
    const ask = screen.getByRole("dialog", { name: "Run this command?" });
    expect(within(ask).getByText(/printf ok > approve\.txt/)).toBeInTheDocument();
    expect(within(ask).getByText("Run the invented fixture command?")).toBeInTheDocument();
    expect(screen.getByLabelText("Needs you")).toBeInTheDocument();
    await userEvent.click(within(ask).getByRole("button", { name: "Allow" }));
    expect(engine.answers).toEqual([{ request: "1:0", answer: { kind: "allow" } }]);
    await act(async () => void engine.play());
    await settle();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByText("Done.")).toBeInTheDocument();
  });

  it("shows a new file's change relative to the project, in the conversation, the approval and the side panel", async () => {
    const { engine, store } = await open("file_change");
    await playTo(engine, "pending");
    const ask = screen.getByRole("dialog", { name: "Make these changes?" });
    expect(within(ask).getByText("hello.txt")).toBeInTheDocument();
    await userEvent.click(within(ask).getByRole("button", { name: "Allow" }));
    await act(async () => void engine.play());
    await settle();
    await userEvent.click(screen.getByRole("button", { name: /hello\.txt/ }));
    const panel = screen.getByRole("complementary", { name: "Side panel" });
    expect(within(panel).getByText("+1")).toBeInTheDocument();
    await userEvent.click(within(panel).getByRole("tab", { name: "Preview" }));
    await settle();
    expect(engine.reads).toEqual(["/work/run/project/hello.txt"]);
    expect(within(panel).getByText("hello")).toBeInTheDocument();
    expect({ ...store.get().model!.snapshot, account: null }).toEqual({ ...engine.data.after, account: null });
  });

  it("answers the agent's question with one press", async () => {
    const { engine } = await open("question");
    await playTo(engine, "pending");
    const ask = screen.getByRole("dialog", { name: "Colour" });
    expect(within(ask).getByText("Which colour?")).toBeInTheDocument();
    await userEvent.click(within(ask).getByRole("button", { name: /Blue/ }));
    expect(engine.answers).toEqual([{ request: "1:0", answer: { kind: "answers", answers: [["colour", "Blue"]] } }]);
  });

  it("shows Stop while the turn runs, then the turn stopped", async () => {
    const { engine } = await open("interrupt");
    await playTo(engine, "turnStarted");
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    await playTo(engine, "turnEnded");
    expect(screen.getByText("Stopped")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Send" })).toBeInTheDocument();
  });

  it("shows the workflow and attachments a message carried", async () => {
    const { engine } = await open("workflow_turn");
    await act(async () => void engine.play());
    await settle();
    expect(screen.getByText("estimate")).toBeInTheDocument();
    expect(screen.getByText("sketch.png")).toBeInTheDocument();
    expect(screen.getByTitle("scope.md")).toBeInTheDocument();
  });

  it("shows the application's tool calls with their answers, and the failed one as failed", async () => {
    const { engine } = await open("app_tool");
    await act(async () => void engine.play());
    await settle();
    expect(screen.getAllByText("estimate · check_estimate")).toHaveLength(2);
    expect(screen.getByText("The estimate of 120 checks out.")).toBeInTheDocument();
    const failed = screen.getByText("The estimate has no total.", { selector: "pre" }).closest("li")!;
    expect(within(failed).getByText("failed")).toBeInTheDocument();
    expect(screen.getAllByText("failed")).toHaveLength(1);
  });

  it("shows a fork under its parent with only the history it was forked with, and removes it when archived", async () => {
    const { engine, store } = await open("fork");
    const fork = await act(async () => engine.play((e) => e.type === "thread" && e.thread.forkedFrom !== null));
    if (fork?.type !== "thread") throw new Error("no fork");
    for (let i = 0; i < 4; i++) await playTo(engine, "thread");
    await act(() => store.selectThread(fork.thread.id));
    await settle();
    expect(screen.getAllByText("One.")).toHaveLength(1);
    expect(screen.queryByText("Two.")).not.toBeInTheDocument();
    const rows = screen.getAllByRole("button", { name: /STUB\[one\] go/ }).filter((b) => b.classList.contains("nav-thread"));
    expect(rows).toHaveLength(2);
    expect(rows[1].querySelector(".nav-fork")).not.toBeNull();
    await act(async () => void engine.play());
    await settle();
    expect(store.get().thread).toBeNull();
    expect(screen.getAllByRole("button", { name: /STUB\[one\] go/ }).filter((b) => b.classList.contains("nav-thread"))).toHaveLength(1);
  });
});
