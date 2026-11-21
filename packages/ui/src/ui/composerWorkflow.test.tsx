// The composer's workflow, attachments and draft over what the real engine produced
// (the workflow_turn recording, with the workflows the engine found in its project).
import { act, fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import type { Message } from "../engine/types";
import { mount, settle } from "../testing/render";
import { engineData, ReplayTransport } from "../testing/replay";

const estimate = engineData("workflow_turn").workflows[0];
const composer = () => document.querySelector<HTMLTextAreaElement>(".composer-input")!;
const type = async (text: string) => {
  fireEvent.change(composer(), { target: { value: text, selectionStart: text.length } });
  await settle();
};
const enter = async () => {
  fireEvent.keyDown(composer(), { key: "Enter" });
  await settle();
};
const pick = async () => {
  await userEvent.click(screen.getByRole("button", { name: /Workflow/ }));
  await userEvent.click(screen.getByRole("option", { name: /estimate/ }));
};
const pin = () => screen.getByRole("button", { name: "Keep attached" });
const chip = () => screen.queryByLabelText(`Remove ${estimate.name}`);

function recording() {
  return new ReplayTransport(engineData("workflow_turn"), (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));
}

/** What each message took, in order. */
const sentWorkflows = (engine: ReplayTransport) => engine.sent.map((s) => (s.args.message as Message).workflow);

/** The recording's thread, played to its end and selected. */
async function inThread() {
  const engine = recording();
  const mounted = await mount(engine.client);
  await act(async () => void engine.play());
  const thread = mounted.store.get().model!.snapshot.threads[0].id;
  await act(() => mounted.store.selectThread(thread));
  await settle();
  return { engine, ...mounted, thread };
}

/** The recording's project, before its thread starts: a new thread's composer. */
async function inNewThread() {
  const engine = recording();
  const mounted = await mount(engine.client);
  await act(async () => void engine.play((e) => e.type === "projects"));
  const project = mounted.store.get().model!.snapshot.projects[0].id;
  await act(async () => mounted.store.selectProject(project));
  await settle();
  return { engine, ...mounted, project };
}

beforeEach(() => localStorage.clear());

describe("a workflow in the composer", () => {
  it("goes with one message", async () => {
    const { engine } = await inThread();
    await pick();
    await type("Estimate the deck.");
    await enter();
    expect(chip()).not.toBeInTheDocument();
    await type("Now the piles.");
    await enter();
    expect(sentWorkflows(engine)).toEqual([estimate.id, null]);
  });

  it("goes with every message while kept attached, and with the next only once unpinned", async () => {
    const { engine } = await inThread();
    await pick();
    await userEvent.click(pin());
    expect(pin()).toHaveAttribute("aria-pressed", "true");
    await type("Estimate the deck.");
    await enter();
    expect(pin()).toHaveAttribute("aria-pressed", "true");
    await type("Now the piles.");
    await enter();
    await userEvent.click(pin());
    expect(pin()).toHaveAttribute("aria-pressed", "false");
    await type("And the rails.");
    await enter();
    expect(chip()).not.toBeInTheDocument();
    expect(sentWorkflows(engine)).toEqual([estimate.id, estimate.id, estimate.id]);
  });

  it("is unpinned when removed", async () => {
    await inThread();
    await pick();
    await userEvent.click(pin());
    await userEvent.click(chip()!);
    await pick();
    expect(pin()).toHaveAttribute("aria-pressed", "false");
  });

  it("stays kept with its thread when the person goes elsewhere and back", async () => {
    const { engine, store, thread } = await inThread();
    await pick();
    await userEvent.click(pin());
    await act(async () => store.selectProject(store.get().model!.snapshot.projects[0].id));
    await settle();
    expect(chip()).not.toBeInTheDocument();
    await act(() => store.selectThread(thread));
    await settle();
    expect(pin()).toHaveAttribute("aria-pressed", "true");
    await type("Estimate the deck.");
    await enter();
    expect(sentWorkflows(engine)).toEqual([estimate.id]);
  });

  it("is kept with the message, text and attachments when the engine refuses the send", async () => {
    const { engine } = await inThread();
    engine.refuse.set("send", { kind: "codex", message: "Codex is busy." });
    await pick();
    await userEvent.click(screen.getByRole("button", { name: "Attach files" }));
    await type("Estimate the deck.");
    await enter();
    expect(screen.getByRole("alert")).toHaveTextContent("Codex is busy.");
    expect(composer().value).toBe("Estimate the deck.");
    expect(chip()).toBeInTheDocument();
    expect(screen.getByLabelText("Remove /Users/person/Desktop/site-photo.jpg")).toBeInTheDocument();
  });

  it("goes with a new thread's first message only, unless kept, when it goes on with the thread", async () => {
    const first = await inNewThread();
    await pick();
    await type("Estimate this.");
    await enter();
    expect(first.store.get().thread).not.toBeNull();
    expect(chip()).not.toBeInTheDocument();
    expect(sentWorkflows(first.engine)).toEqual([estimate.id]);
    first.unmount();

    const kept = await inNewThread();
    await pick();
    await userEvent.click(pin());
    await type("Estimate this.");
    await enter();
    expect(pin()).toHaveAttribute("aria-pressed", "true");
    await type("Now the piles.");
    await enter();
    expect(sentWorkflows(kept.engine)).toEqual([estimate.id, estimate.id]);
  });

  it("is kept when the engine refuses a new thread's first message", async () => {
    const { engine } = await inNewThread();
    engine.refuse.set("start_thread", { kind: "notFound", what: "workflow" });
    await pick();
    await type("Estimate this.");
    await enter();
    expect(composer().value).toBe("Estimate this.");
    expect(chip()).toBeInTheDocument();
  });

  it("is not pinned to a new thread when unpinned while its start was pending", async () => {
    const { engine, store } = await inNewThread();
    const start = engine.client.startThread;
    let release = () => {};
    engine.client.startThread = (...args) => new Promise((resolve) => (release = () => resolve(start(...args))));
    await pick();
    await userEvent.click(pin());
    await type("Estimate this.");
    await enter();
    await userEvent.click(pin());
    await act(async () => release());
    await settle();
    expect(store.get().thread).not.toBeNull();
    expect(chip()).not.toBeInTheDocument();
  });
});

describe("errors in the conversation", () => {
  it("keeps a refused revert's error shown", async () => {
    const { engine } = await inThread();
    engine.refuse.set("revert", { kind: "unsupported", message: "Codex can't remove these turns." });
    await userEvent.click(screen.getByRole("button", { name: /Revert/ }));
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await settle();
    expect(screen.getByRole("alert")).toHaveTextContent("Codex can't remove these turns.");
  });

  it("shows nothing when the read after a completed turn fails", async () => {
    const engine = recording();
    const { store } = await mount(engine.client);
    const started = await act(async () => engine.play((e) => e.type === "thread"));
    if (started?.type !== "thread") throw new Error("no thread");
    await act(() => store.selectThread(started.thread.id));
    await settle();
    engine.refuse.set("read_thread", { kind: "codex", message: "read failed" });
    await act(async () => void engine.play((e) => e.type === "turnEnded"));
    await settle();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByText("Following the workflow.")).toBeInTheDocument();
  });
});
