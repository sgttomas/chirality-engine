// Starting points for the fake engine: first use, a project with history, a
// turn stopped at a given moment. `?scene=<name>` picks one in the browser.
import type { Answer, Message, ProjectId, Request, ThreadId, TurnSettings } from "../engine/types";
import type { PanelTab } from "../ui/panel";
import { FakeEngine } from "./engine";
import { bridge, clinic } from "./fixtures";


export interface Scene {
  engine: FakeEngine;
  project?: ProjectId;
  thread?: ThreadId;
  panel?: PanelTab;
}

export const sceneNames = ["firstUse", "empty", "running", "approval", "question", "diff", "fork", "subagents", "home"] as const;
export type SceneName = (typeof sceneNames)[number];

const settings: TurnSettings = { model: "gpt-5.5", effort: "high", access: "ask" };
const msg = (text: string, workflow: string | null = null): Message => ({ text, attachments: [], workflow });

const allowAll = (request: Request): Answer =>
  request.kind === "question" ? { kind: "answers", answers: [[request.questions[0].id, request.questions[0].options[1]]] } : { kind: "allow" };

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

/** Plays whole turns at once, answering as the person would. */
async function play(engine: FakeEngine, project: ProjectId, texts: Message[], thread?: ThreadId) {
  for (const message of texts) {
    if (thread) await engine.send(thread, message, settings);
    else thread = await engine.startThread(project, message, settings);
    await engine.settled(thread);
  }
  return thread!;
}

const scopeAsk = msg("Draft the scope of work for the foundations, from the brief and the pier drawings.", "wf-scope");
const classAsk = msg("Update the quantities to match and rerun the estimate.");

async function history(engine: FakeEngine) {
  const older = await play(engine, clinic.id, [msg("Summarise what the brief asks for in the clinic fit-out.")]);
  engine.setUpdated(older, 86400 * 3);
  const main = await play(engine, bridge.id, [msg("List the deliverables in the bridge decomposition and what each depends on.", "wf-decompose")]);
  engine.setUpdated(main, 3600 * 5);
  return main;
}

export async function buildScene(name: SceneName): Promise<Scene> {
  const engine = new FakeEngine({ instant: true, autoAnswer: allowAll });
  const live = () => {
    engine.options = {};
  };
  if (name === "firstUse") {
    live();
    return { engine };
  }
  engine.seedAccount({ state: "signedIn", email: "person@example.com", plan: "Pro" });
  engine.seedProject(bridge);
  engine.seedProject(clinic);
  await history(engine);

  switch (name) {
    case "home":
      live();
      return { engine, project: bridge.id };
    case "empty": {
      await engine.archive((await engine.snapshot()).threads.find((t) => t.project === clinic.id)!.id);
      live();
      return { engine, project: clinic.id };
    }
    case "running": {
      const stopped = engine.stopAtMark("streamed");
      const thread = await engine.startThread(bridge.id, scopeAsk, settings);
      await stopped;
      live();
      return { engine, thread };
    }
    case "diff": {
      const thread = await play(engine, bridge.id, [scopeAsk]);
      await engine.markRead(thread);
      live();
      return { engine, thread, panel: "changes" };
    }
    case "approval":
    case "question": {
      const thread = await play(engine, bridge.id, [scopeAsk]);
      engine.options.autoAnswer = (request) => (name === "approval" && request.kind === "question" ? allowAll(request) : null);
      await engine.send(thread, classAsk, settings);
      await flush();
      await engine.markRead(thread);
      live();
      return { engine, thread };
    }
    case "subagents": {
      // One subagent has finished; the other waits on the person, and its parent waits on it.
      engine.options.autoAnswer = () => null;
      const thread = await engine.startThread(bridge.id, msg("Check the pile lengths and the deck takeoff in parallel, with a subagent each."), settings);
      for (let i = 0; i < 20; i++) await flush();
      live();
      return { engine, thread };
    }
    case "fork": {
      const thread = await play(engine, bridge.id, [scopeAsk, classAsk]);
      await engine.markRead(thread);
      const view = await engine.readThread(thread);
      const fork = await engine.fork(thread, view.turns[0].id);
      await play(engine, bridge.id, [msg("Instead, price the piles at 22 m in case the rock head is deeper than the survey found.")], fork);
      await engine.markRead(fork);
      live();
      return { engine, thread: fork, panel: "changes" };
    }
  }
}
