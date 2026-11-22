// `@` mentions threads, over what the real engine emitted when threads in two projects were started
// and renamed, and a message mentioning one in each was sent: the list (grouped, ordered, filtered
// across spaces), picking one, the chips, and the message in history whose chips open the threads.
// Files are no longer referenced with `@`.
import { act, fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { engineCommands } from "../engine/commands";
import type { Snapshot, ThreadSummary } from "../engine/types";
import { mount, settle } from "../testing/render";
import { engineData, ReplayTransport } from "../testing/replay";
import { mentionAt, mentionGroups } from "./composer/Mentions";

const signedIn = (s: Snapshot): Snapshot => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } });
const main = () => screen.getByRole("main");
const composer = () => document.querySelector<HTMLTextAreaElement>(".composer-input")!;
const type = async (text: string, caret = text.length) => {
  fireEvent.change(composer(), { target: { value: text, selectionStart: caret } });
  await settle();
};
const options = () => within(screen.getByRole("listbox", { name: "Threads" })).getAllByRole("option").map((o) => o.querySelector(".mention-name")!.textContent);

/** The engine's threads by title. */
function byTitle(snapshot: Snapshot) {
  const find = (title: string) => snapshot.threads.find((t) => t.title === title)!;
  return { piles: find("Pile plan"), steel: find("Steel estimate"), deck: find("Deck survey"), compare: find("STUB[compare] Check @Pile plan") };
}

/** The recording played to its end, with the thread that sent the mentions open. */
async function opened() {
  const engine = new ReplayTransport(engineData("thread_refs"), signedIn);
  const { store } = await mount(engine.client);
  await act(async () => void engine.play());
  await settle();
  const threads = byTitle(store.get().model!.snapshot);
  await act(() => store.selectThread(threads.deck.id));
  await settle();
  return { engine, store, threads };
}

beforeEach(() => localStorage.clear());

describe("the threads @ offers", () => {
  const after = () => engineData("thread_refs").after;
  const here = (s: Snapshot) => s.projects.find((p) => p.name === "project")!;
  const set = (s: Snapshot, id: string, change: Partial<ThreadSummary>) => ({ ...s, threads: s.threads.map((t) => (t.id === id ? { ...t, ...change } : t)) });

  it("are grouped by project, the current first, then by status and how recent, without the current thread or subagents", () => {
    let s = after();
    const t = byTitle(s);
    s = set(s, t.compare.id, { status: { state: "working" }, updatedAt: 10 });
    s = set(s, t.piles.id, { unread: false, updatedAt: 30 });
    s = set(s, t.deck.id, { unread: true, updatedAt: 20 });
    s = { ...s, threads: [...s.threads, { ...t.piles, id: "sub", title: "Pile worker", subagent: { parent: t.piles.id, path: "/root/worker", nickname: null } }] };
    const groups = mentionGroups(s, here(s).id, null, "");
    expect(groups.map((g) => g.project.name)).toEqual(["project", "other"]);
    // Running first, then unread, then the rest; most recent first within each.
    expect(groups[0].threads.map((x) => x.title)).toEqual(["STUB[compare] Check @Pile plan", "Deck survey", "Pile plan"]);
    expect(mentionGroups(s, here(s).id, t.deck.id, "")[0].threads.map((x) => x.title)).not.toContain("Deck survey");
    // Another project's threads come after, the most recently active project first.
    const other = s.projects.find((p) => p.name === "other")!;
    expect(mentionGroups(s, other.id, null, "").map((g) => g.project.name)).toEqual(["other", "project"]);
  });

  it("are at most a few per project until a title is typed, which finds them in every project", () => {
    const s = after();
    const t = byTitle(s);
    const many = { ...s, threads: [...s.threads, ...Array.from({ length: 8 }, (_, i) => ({ ...t.steel, id: `steel-${i}`, title: `Steel batch ${i}` }))] };
    const other = mentionGroups(many, here(s).id, null, "").find((g) => g.project.name === "other")!;
    expect(other.threads).toHaveLength(5);
    expect(mentionGroups(many, here(s).id, null, "steel BATCH").flatMap((g) => g.threads)).toHaveLength(8);
    expect(mentionGroups(s, here(s).id, null, "steel est").flatMap((g) => g.threads.map((x) => x.title))).toEqual(["Steel estimate"]);
    expect(mentionGroups(s, here(s).id, null, "steel estimate for nothing")).toEqual([]);
  });

  it("are asked for from an @ at the start or after a space, across the spaces of a title", () => {
    expect(mentionAt("Ask @Pile pl", 12)).toEqual({ start: 4, end: 12, query: "Pile pl" });
    expect(mentionAt("@", 1)).toEqual({ start: 0, end: 1, query: "" });
    expect(mentionAt("mail@example", 12)).toBeNull();
    expect(mentionAt("Ask @ Pile", 10)).toBeNull();
    expect(mentionAt("Ask @Pile\nplan", 14)).toBeNull();
  });
});

describe("mentioning a thread", () => {
  it("lists the threads at the caret from the @ button, picks one into the text and a chip, and sends it with the message", async () => {
    const { engine, threads } = await opened();
    await type("Compare with", 7);
    composer().setSelectionRange(7, 7);
    await userEvent.click(screen.getByRole("button", { name: "Mention a thread" }));
    await settle();
    expect(composer().value).toBe("Compare @ with");
    expect(document.activeElement).toBe(composer());
    expect(composer().selectionStart).toBe(9);
    const list = screen.getByRole("listbox", { name: "Threads" });
    expect(within(list).getAllByRole("group").map((g) => g.getAttribute("aria-label"))).toEqual(["project", "other"]);
    expect(options()).not.toContain("Deck survey");

    await type("Compare @steel est with", 18);
    expect(options()).toEqual(["Steel estimate"]);
    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    expect(composer().value).toBe("Compare @Steel estimate with");
    expect(screen.queryByRole("listbox", { name: "Threads" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove Steel estimate" })).toBeInTheDocument();

    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    const sent = engine.sent.at(-1)!;
    expect(sent.args.message).toMatchObject({ text: "Compare @Steel estimate with", threads: [{ thread: threads.steel.id, title: "Steel estimate" }] });
  });

  it("closes the list when no title matches, and a chip can be removed", async () => {
    await opened();
    await type("Ask @deck");
    expect(screen.queryByRole("listbox", { name: "Threads" })).not.toBeInTheDocument();
    await type("Ask @pile plan");
    expect(options()).toEqual(["Pile plan", "STUB[compare] Check @Pile plan"]);
    await type("Ask @pile plan and the rest");
    expect(screen.queryByRole("listbox", { name: "Threads" })).not.toBeInTheDocument();
    await type("Ask @pile plan");
    fireEvent.keyDown(composer(), { key: "Tab" });
    await settle();
    await userEvent.click(screen.getByRole("button", { name: "Remove Pile plan" }));
    expect(screen.queryByRole("button", { name: "Remove Pile plan" })).not.toBeInTheDocument();
  });

  it("shows each thread a sent message mentions as a chip that opens it", async () => {
    const { store, threads } = await opened();
    const message = within(main()).getByText("STUB[refs] Compare @Pile plan with @Steel estimate").closest(".msg-user") as HTMLElement;
    expect(within(message).getAllByRole("button").map((b) => b.textContent?.trim())).toEqual(["Pile plan", "Steel estimate"]);
    await userEvent.click(within(message).getByRole("button", { name: "Steel estimate" }));
    await settle();
    expect(store.get().thread).toBe(threads.steel.id);
  });

  it("names a mentioned thread by its title now, or the one it had when mentioned", async () => {
    const data = engineData("thread_refs");
    const steel = byTitle(data.after).steel;
    // Renamed since it was mentioned, then gone.
    data.events.push({ type: "thread", thread: { ...steel, title: "Steel takeoff" } });
    const engine = new ReplayTransport(data, signedIn);
    const { store } = await mount(engine.client);
    await act(async () => void engine.play());
    await settle();
    await act(() => store.selectThread(byTitle(data.after).deck.id));
    await settle();
    expect(within(main()).getByRole("button", { name: "Steel takeoff" })).toBeEnabled();
    data.events.push({ type: "threadRemoved", thread: steel.id });
    await act(async () => void engine.play());
    await settle();
    expect(within(main()).getByRole("button", { name: "Steel estimate" })).toBeDisabled();
  });
});

describe("files", () => {
  it("are not referenced with @: the list holds threads only and the engine is asked for no files", async () => {
    const { engine } = await opened();
    await type("Check @not");
    expect(screen.queryByRole("listbox", { name: "Files" })).not.toBeInTheDocument();
    await type("Check @");
    expect(screen.getAllByRole("option").every((o) => o.querySelector(".mention-name"))).toBe(true);
    expect(engine.calls).not.toContain("find_files" as never);
    expect(engineCommands).not.toContain("find_files" as never);
    expect("findFiles" in engine.client).toBe(false);
    expect(screen.getByRole("button", { name: "Attach files" })).toBeInTheDocument();
  });
});
