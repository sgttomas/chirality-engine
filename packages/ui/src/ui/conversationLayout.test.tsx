// The conversation says itself whether it holds a project's new thread, so the layout follows the
// selection without :has(), which WebKit can leave applied after the class it matched has gone.
import { act, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { buildScene } from "../fake/scenes";
import { mount } from "../testing/render";

describe("the conversation's layout", () => {
  it("is the new thread's while no thread is selected, and a thread's once one is", async () => {
    const scene = await buildScene("running");
    const { store } = await mount(scene.engine);
    const conv = () => screen.getByRole("main");
    const project = store.get().model!.snapshot.threads.find((t) => t.id === scene.thread)!.project;
    act(() => store.selectProject(project));
    expect(conv()).toHaveClass("conv--new");
    await act(() => store.selectThread(scene.thread!));
    expect(conv()).not.toHaveClass("conv--new");
  });
});
