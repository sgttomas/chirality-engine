// The fake engine's subagents scene starts where it says: one subagent finished, the other
// waiting on the person, and their parent waiting on them.
import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { buildScene } from "../fake/scenes";
import { mount } from "../testing/render";

describe("the subagents scene", () => {
  it("shows both subagents under their running parent, and the approval from the one waiting", async () => {
    const scene = await buildScene("subagents");
    const { store } = await mount(scene.engine);
    await store.selectThread(scene.thread!);
    const nav = screen.getByRole("navigation", { name: "Projects" });
    expect(within(within(nav).getByRole("button", { name: /^deck/ })).getByLabelText("Needs you")).toBeInTheDocument();
    expect(within(nav).getByRole("button", { name: /^piles/ })).toBeInTheDocument();
    const ask = await screen.findByRole("dialog", { name: "Run this command?" });
    expect(within(ask).getByRole("button", { name: "deck" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "piles finished" })).toBeInTheDocument();
  });
});
