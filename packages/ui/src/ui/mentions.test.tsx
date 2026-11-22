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
import { mentionFrom, mentionGroups, typedMention } from "./composer/Mentions";

const signedIn = (s: Snapshot): Snapshot => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } });
const main = () => screen.getByRole("main");
const composer = () => document.querySelector<HTMLTextAreaElement>(".composer-input")!;
const type = async (text: string, caret = text.length) => {
  fireEvent.change(composer(), { target: { value: text, selectionStart: caret } });
  await settle();
};
/** Types `before`, then an `@` after it, as the person does. */
const typeAt = async (before: string) => {
  await type(before);
  await type(`${before}@`);
};
const list = () => screen.queryByRole("listbox", { name: "Threads" });
const options = () => within(list()!).getAllByRole("option").map((o) => o.querySelector(".mention-name")!.textContent);

/** The engine's threads by title. */
function byTitle(snapshot: Snapshot) {
  const find = (title: string) => snapshot.threads.find((t) => t.title === title)!;
  return { piles: find("Pile plan"), steel: find("Steel estimate"), deck: find("Deck survey"), bearing: find("Bearing check"), yard: find("Yard layout"), compare: find("STUB[compare] Check @Pile plan") };
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
  const project = (s: Snapshot, name: string) => s.projects.find((p) => p.name === name)!.id;
  const set = (s: Snapshot, changes: [ThreadSummary, Partial<ThreadSummary>][]) => ({
    ...s,
    threads: s.threads.map((t) => ({ ...t, ...(changes.find(([c]) => c.id === t.id)?.[1] ?? {}) })),
  });
  const titles = (s: Snapshot, current: string | null = null) => mentionGroups(s, project(s, "project"), current, "")[0].threads.map((x) => x.title);

  it("come by project: the current one first, then the others by their latest activity", () => {
    // The current project's threads are older than the others'.
    const s = { ...after(), threads: after().threads.map((t) => ({ ...t, updatedAt: 1 })) };
    const t = byTitle(s);
    const names = (s: Snapshot, from: string) => mentionGroups(s, project(s, from), null, "").map((g) => g.project.name);
    expect(names(set(s, [[t.steel, { updatedAt: 50 }], [t.yard, { updatedAt: 70 }]]), "project")).toEqual(["project", "yard", "other"]);
    expect(names(set(s, [[t.steel, { updatedAt: 90 }], [t.yard, { updatedAt: 70 }]]), "project")).toEqual(["project", "other", "yard"]);
    expect(names(set(s, [[t.steel, { updatedAt: 90 }], [t.yard, { updatedAt: 70 }]]), "yard")).toEqual(["yard", "other", "project"]);
  });

  it("in a project: needing the person or running first, then unread (failed too), then the rest, each most recent first", () => {
    const s = after();
    const t = byTitle(s);
    const failed = { status: { state: "failed" as const, message: "boom" }, unread: true, updatedAt: 30 };
    const read = { status: { state: "idle" as const }, unread: false, updatedAt: 100 };
    const answer = set(s, [[t.deck, { status: { state: "needsAnswer" }, updatedAt: 20 }], [t.compare, { status: { state: "working" }, updatedAt: 10 }], [t.piles, failed], [t.bearing, read]]);
    expect(titles(answer)).toEqual(["Deck survey", "STUB[compare] Check @Pile plan", "Pile plan", "Bearing check"]);
    const approval = set(s, [[t.deck, { status: { state: "working" }, updatedAt: 5 }], [t.compare, { status: { state: "needsApproval" }, updatedAt: 10 }], [t.piles, failed], [t.bearing, read]]);
    expect(titles(approval)).toEqual(["STUB[compare] Check @Pile plan", "Deck survey", "Pile plan", "Bearing check"]);
  });

  it("leave out the current thread and subagents", () => {
    const s = after();
    const t = byTitle(s);
    const withSubagent = { ...s, threads: [...s.threads, { ...t.piles, id: "sub", title: "Pile worker", subagent: { parent: t.piles.id, path: "/root/worker", nickname: null } }] };
    const shown = mentionGroups(withSubagent, project(s, "project"), t.deck.id, "pile").flatMap((g) => g.threads.map((x) => x.title));
    expect(shown).toEqual(expect.arrayContaining(["Pile plan"]));
    expect(shown).not.toContain("Pile worker");
    expect(titles(s, t.deck.id)).not.toContain("Deck survey");
  });

  it("are at most a few per project until a title is typed, which finds them in every project", () => {
    const s = after();
    const t = byTitle(s);
    const many = { ...s, threads: [...s.threads, ...Array.from({ length: 8 }, (_, i) => ({ ...t.steel, id: `steel-${i}`, title: `Steel batch ${i}` }))] };
    const other = mentionGroups(many, project(s, "project"), null, "").find((g) => g.project.name === "other")!;
    expect(other.threads).toHaveLength(5);
    expect(mentionGroups(many, project(s, "project"), null, "steel BATCH").flatMap((g) => g.threads)).toHaveLength(8);
    expect(mentionGroups(s, project(s, "project"), null, "steel est").flatMap((g) => g.threads.map((x) => x.title))).toEqual(["Steel estimate"]);
    expect(mentionGroups(s, project(s, "project"), null, "steel estimate for nothing")).toEqual([]);
  });

  it("are asked for by an @ typed fresh, at the start or after a space, and followed across the spaces of a title", () => {
    expect(typedMention("Ask ", "Ask @", 5)).toEqual({ start: 4, end: 5, query: "" });
    expect(typedMention("", "@", 1)).toEqual({ start: 0, end: 1, query: "" });
    expect(typedMention("mail", "mail@", 5)).toBeNull();
    expect(typedMention("Ask @Pile plan", "Ask @Pile plan ", 15)).toBeNull();
    expect(typedMention("", "Ask @", 5)).toBeNull();
    // Typed over a selection, it opens; an `@` a deletion leaves at the caret does not.
    expect(typedMention("Ask the deck", "Ask @ deck", 5)).toEqual({ start: 4, end: 5, query: "" });
    expect(typedMention("Ask @x", "Ask @", 5)).toBeNull();
    expect(typedMention("Ask x", "Ask @x", 5)).toEqual({ start: 4, end: 5, query: "" });
    expect(mentionFrom("Ask @Pile pl", 4, 12)).toEqual({ start: 4, end: 12, query: "Pile pl" });
    expect(mentionFrom("Ask @ Pile", 4, 10)).toBeNull();
    expect(mentionFrom("Ask @Pile\nplan", 4, 14)).toBeNull();
    expect(mentionFrom("Ask Pile", 4, 8)).toBeNull();
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
    const groups = within(list()!).getAllByRole("group").map((g) => g.getAttribute("aria-label"));
    expect(groups[0]).toBe("project");
    expect([...groups].sort()).toEqual(["other", "project", "yard"]);
    expect(options()).not.toContain("Deck survey");

    await type("Compare @steel est with", 18);
    expect(options()).toEqual(["Steel estimate"]);
    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    expect(composer().value).toBe("Compare @Steel estimate with");
    expect(list()).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove Steel estimate" })).toBeInTheDocument();

    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    const sent = engine.sent.at(-1)!;
    expect(sent.args.message).toMatchObject({ text: "Compare @Steel estimate with", threads: [{ thread: threads.steel.id, title: "Steel estimate" }] });
  });

  it("stays closed once a thread is picked, whatever is typed after it, so Enter sends", async () => {
    const { engine } = await opened();
    await typeAt("Ask ");
    await type("Ask @pile pl");
    expect(options()).toEqual(["Pile plan", "STUB[compare] Check @Pile plan"]);
    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    expect(composer().value).toBe("Ask @Pile plan ");
    // A second space, or prose a longer title holds, opens nothing.
    await type("Ask @Pile plan  ");
    expect(list()).not.toBeInTheDocument();
    await type("Ask @Pile plan check");
    expect(list()).not.toBeInTheDocument();
    fireEvent.keyDown(composer(), { key: "Enter" });
    await settle();
    expect(engine.sent.at(-1)!.args.message).toMatchObject({ text: "Ask @Pile plan check", threads: [{ title: "Pile plan" }] });
  });

  it("opens only for an @ typed fresh, and closes for good when no title matches", async () => {
    await opened();
    await type("Ask @pile");
    expect(list(), "text arriving whole is not an @ typed").not.toBeInTheDocument();
    await type("");
    await typeAt("Ask ");
    await type("Ask @deck");
    expect(list(), "the current thread is not offered").not.toBeInTheDocument();
    await type("Ask @");
    expect(list(), "closed for good").not.toBeInTheDocument();
    await type("Ask @ and ");
    await typeAt("Ask @ and ");
    await type("Ask @ and pile plan and the rest");
    expect(list()).not.toBeInTheDocument();
  });

  it("keeps a line break after the name it replaces, and a chip can be removed", async () => {
    await opened();
    await typeAt("Ask ");
    await type("Ask @pile pl\nnext line", 12);
    fireEvent.keyDown(composer(), { key: "Tab" });
    await settle();
    expect(composer().value).toBe("Ask @Pile plan \nnext line");
    await userEvent.click(screen.getByRole("button", { name: "Remove Pile plan" }));
    expect(screen.queryByRole("button", { name: "Remove Pile plan" })).not.toBeInTheDocument();
  });

  it("takes its keys from the composer only, and closes when the composer loses focus or the caret moves", async () => {
    await opened();
    await typeAt("Ask ");
    const search = screen.getByPlaceholderText("Search");
    for (const key of ["ArrowDown", "Enter", "Tab", "Escape"]) fireEvent.keyDown(search, { key });
    await settle();
    expect(composer().value, "another field's keys are its own").toBe("Ask @");
    expect(list()).toBeInTheDocument();
    // While an input method composes, every key is the input method's.
    const chosen = () => within(list()!).getAllByRole("option").findIndex((o) => o.getAttribute("aria-selected") === "true");
    for (const key of ["ArrowDown", "ArrowUp", "Enter", "Tab", "Escape"]) fireEvent.keyDown(composer(), { key, isComposing: true });
    await settle();
    expect(composer().value).toBe("Ask @");
    expect([list() !== null, chosen()]).toEqual([true, 0]);
    composer().setSelectionRange(1, 1);
    fireEvent.select(composer());
    await settle();
    expect(list()).not.toBeInTheDocument();
    await type("Ask ");
    await typeAt("Ask ");
    expect(list()).toBeInTheDocument();
    fireEvent.blur(composer());
    await settle();
    expect(list()).not.toBeInTheDocument();
    await type("Ask ");
    await typeAt("Ask ");
    expect(list()).toBeInTheDocument();
    fireEvent.keyDown(composer(), { key: "Escape" });
    await settle();
    expect([list(), composer().value]).toEqual([null, "Ask @"]);
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
    await typeAt("Check ");
    await type("Check @not");
    expect(screen.queryByRole("listbox", { name: "Files" })).not.toBeInTheDocument();
    await type("Check ");
    await typeAt("Check ");
    expect(screen.getAllByRole("option").every((o) => o.querySelector(".mention-name"))).toBe(true);
    expect(engine.calls).not.toContain("find_files" as never);
    expect(engineCommands).not.toContain("find_files" as never);
    expect("findFiles" in engine.client).toBe(false);
    expect(screen.getByRole("button", { name: "Attach files" })).toBeInTheDocument();
  });
});
