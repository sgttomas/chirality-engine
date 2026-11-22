// The workflow picker's featured list, over the workflow the real engine found (the
// workflow_turn recording), with Library workflows made from it the way the engine lists a
// workflow root's.
import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import type { Workflow } from "../engine/types";
import { mount, settle } from "../testing/render";
import { engineData, ReplayTransport } from "../testing/replay";
import { listedWorkflows } from "./composer/WorkflowPicker";
import { StandardLayout } from "./StandardLayout";

const recorded = engineData("workflow_turn");
const own = recorded.workflows[0];
const library = "/library/library-v3.4.0/workflows";
const fromLibrary = (name: string, summary = `${name} work`): Workflow => ({ ...own, id: `${library}/${name}`, name, summary, source: library, path: `${library}/${name}/WORKFLOW.md` });
const workflows = [own, fromLibrary("project-setup"), fromLibrary("scope-change"), fromLibrary("pdf2md", "Convert a PDF to Markdown")];

describe("the workflows the picker lists", () => {
  it("are all of them when nothing is featured", () => {
    expect(listedWorkflows(workflows, "", undefined)).toEqual(workflows);
  });

  it("are the featured ones and the project's own before anything is typed", () => {
    expect(listedWorkflows(workflows, "", ["project-setup", "scope-change"]).map((w) => w.name)).toEqual([own.name, "project-setup", "scope-change"]);
  });

  it("are every match once something is typed, featured or not", () => {
    expect(listedWorkflows(workflows, "pdf", ["project-setup"]).map((w) => w.name)).toEqual(["pdf2md"]);
    expect(listedWorkflows(workflows, "markdown", ["project-setup"]).map((w) => w.name)).toEqual(["pdf2md"]);
  });
});

describe("the picker in the standard layout", () => {
  it("lists the featured workflows and finds the rest by search", async () => {
    const engine = new ReplayTransport({ ...recorded, workflows }, (s) => ({ ...s, account: { state: "signedIn", email: "person@example.com", plan: null } }));
    const mounted = await mount(engine.client, <StandardLayout featuredWorkflows={["project-setup"]} />);
    await act(async () => void engine.play());
    await act(() => mounted.store.selectThread(mounted.store.get().model!.snapshot.threads[0].id));
    await settle();

    await userEvent.click(screen.getByRole("button", { name: /Workflow/ }));
    const names = () => screen.getAllByRole("option").map((o) => o.textContent ?? "");
    expect(names().some((n) => n.includes("project-setup"))).toBe(true);
    expect(names().some((n) => n.includes(own.name))).toBe(true);
    expect(names().some((n) => n.includes("pdf2md"))).toBe(false);

    await userEvent.type(screen.getByPlaceholderText("Find a workflow"), "pdf");
    expect(names().some((n) => n.includes("pdf2md"))).toBe(true);
  });
});
