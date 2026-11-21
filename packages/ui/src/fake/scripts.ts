// Scripted turns the fake engine plays: what a real Codex turn would emit.
import type { Answer, ItemBody, ItemId, Message, PlanStep, Request, ThreadId } from "../engine/types";
import { alternativeDiff, basisDiff, quantitiesDiff, sowDiff } from "./fixtures";

/** What a script can do inside its turn. */
export interface Run {
  readonly message: Message;
  readonly asks: boolean;
  wait(ms: number): Promise<void>;
  item(body: ItemBody, done?: boolean): ItemId;
  update(id: ItemId, body: ItemBody, done?: boolean): void;
  /** Streams agent text or a reasoning summary into a new item. */
  stream(kind: "agentMessage" | "reasoning", text: string): Promise<ItemId>;
  /** Runs a command, streaming its output. */
  command(command: string, output: string, exitCode?: number): Promise<void>;
  /** Waits on the person; resolves with their answer. */
  ask(request: Request): Promise<Answer>;
  /** A named point a scene can stop at. */
  mark(label: string): Promise<void>;
  /** Starts a subagent named `task` on `script`. */
  delegate(task: string, script: Script): ThreadId;
  /** Waits until each subagent has finished its turn. */
  waitFor(subagents: ThreadId[]): Promise<void>;
}

export type Script = (run: Run) => Promise<void>;

const cwd = "/Users/person/work/harbour-bridge";

const plan = (done: number, texts: string[]): PlanStep[] =>
  texts.map((text, i) => ({ text, status: i < done ? "completed" : i === done ? "inProgress" : "pending" }));

const scopePlan = ["Read the brief and the pier drawings", "Draft SOW-01 for the foundations", "Correct the pile allowance in the basis"];

const allowed = (a: Answer) => a.kind === "allow" || a.kind === "allowForSession";

/** Reads the folder, plans, asks to write a scope of work, then edits the basis. */
export const scopeTurn: Script = async (run) => {
  await run.stream(
    "reasoning",
    "The person wants a scope for the foundations. I should read the brief and the pier drawings first, then check what the estimate assumes for piles.",
  );
  await run.command("ls brief drawings estimate", "brief:\nclient-brief.pdf  geotech-summary.md  site-survey.md\n\ndrawings:\nGA-001-general-arrangement.pdf  S-101-piers.pdf  S-102-deck.pdf\n\nestimate:\nbasis.md  quantities.csv  rates.csv  summary.xlsx\n");
  const planId = run.item({ kind: "plan", steps: plan(0, scopePlan) });
  await run.command(
    'rg -n "rock head|pile" brief/geotech-summary.md estimate/basis.md',
    "brief/geotech-summary.md:12:Rock head was found between 14 m and 19 m below bed level, 17 m on average.\nbrief/geotech-summary.md:18:Recommend bored piles socketed 2 m into rock.\nestimate/basis.md:6:Piles priced at an allowance of 40 m per pile.\n",
  );
  run.update(planId, { kind: "plan", steps: plan(1, scopePlan) });
  await run.stream(
    "agentMessage",
    "The geotechnical summary puts rock head at **17 m on average**, but the basis of estimate allows **40 m per pile**. I'll draft the scope from the survey and then correct the allowance.",
  );
  await run.mark("streamed");
  const sow = { path: "scope/SOW-01-foundations.md", diff: sowDiff };
  if (run.asks) {
    const answer = await run.ask({ kind: "fileChange", files: [sow], reason: "Write the scope of work for the foundations" });
    if (!allowed(answer)) {
      await run.stream("agentMessage", "I've left the scope unwritten. Tell me what to change and I'll draft it again.");
      return;
    }
  }
  run.item({ kind: "fileChange", files: [sow] }, true);
  run.update(planId, { kind: "plan", steps: plan(2, scopePlan) });
  await run.wait(400);
  run.item({ kind: "fileChange", files: [{ path: "estimate/basis.md", diff: basisDiff }] }, true);
  run.update(planId, { kind: "plan", steps: plan(3, scopePlan) }, true);
  await run.stream(
    "agentMessage",
    [
      "Done. **SOW-01 Foundations** is in `scope/SOW-01-foundations.md`, and the basis now prices piles from the survey.",
      "",
      "| | Before | Now |",
      "| --- | --- | --- |",
      "| Pile length | 40 m | 19 m |",
      "| Piles | 32 | 32 |",
      "| Total | 1,280 m | 608 m |",
      "",
      "The quantities in `estimate/quantities.csv` still use 1,280 m; I can update them next.",
    ].join("\n"),
  );
};

/** Asks which estimate class to follow, then runs the estimate with the answer. */
export const classTurn: Script = async (run) => {
  await run.stream("reasoning", "Updating the quantities changes the estimate. The class of estimate decides the contingency, so I should ask.");
  let estimateClass = "Class 4";
  if (run.asks) {
    const answer = await run.ask({
      kind: "question",
      questions: [
        {
          id: "class",
          header: "Estimate class",
          text: "Which class of estimate should this follow?",
          options: ["Class 5 (concept)", "Class 4 (feasibility)", "Class 3 (budget)"],
          freeText: true,
        },
      ],
    });
    if (answer.kind === "answers" && answer.answers[0]) estimateClass = answer.answers[0][1].replace(/ \(.*\)$/, "");
  }
  const command = `python3 tools/estimate.py --class "${estimateClass}"`;
  if (run.asks) {
    const answer = await run.ask({ kind: "command", command, cwd, reason: "Recalculate the estimate with the new quantities" });
    if (!allowed(answer)) {
      run.item({ kind: "fileChange", files: [{ path: "estimate/quantities.csv", diff: quantitiesDiff }] }, true);
      await run.stream("agentMessage", "I've updated the quantities but not rerun the estimate.");
      return;
    }
  }
  run.item({ kind: "fileChange", files: [{ path: "estimate/quantities.csv", diff: quantitiesDiff }] }, true);
  await run.command(command, `Reading estimate/quantities.csv\nApplying ${estimateClass} contingency\nFoundations   4,104,000\nSubstructure  6,880,000\nDeck         11,215,000\nTotal        22,199,000\n`);
  await run.stream(
    "agentMessage",
    `The estimate now follows **${estimateClass}**. Foundations fall from 8.6 M to **4.1 M**, and the total is **22.2 M**.\n\n1. Pile lengths come from the survey.\n2. Contingency follows ${estimateClass}.\n3. The schedule still assumes the longer piling period.`,
  );
};

/** Prices the alternative the person asked for. */
export const alternativeTurn: Script = async (run) => {
  await run.stream("reasoning", "An alternative pile length: change the basis and the quantities, and show the difference.");
  const sure = !run.asks || allowed(await run.ask({ kind: "fileChange", files: [{ path: "estimate/basis.md", diff: alternativeDiff }], reason: null }));
  if (!sure) return void (await run.stream("agentMessage", "Left the basis as it was."));
  run.item({ kind: "fileChange", files: [{ path: "estimate/basis.md", diff: alternativeDiff }] }, true);
  await run.stream(
    "agentMessage",
    "Priced at **22 m** per pile: 704 m in all, which adds about **0.6 M** to the foundations. The basis records it as an alternative, so the 19 m case stays in the parent thread.",
  );
};

/** A plain reply. */
export const replyTurn: Script = async (run) => {
  await run.stream("reasoning", "I can answer this from the decomposition and the drawings I've read.");
  await run.stream(
    "agentMessage",
    [
      "The decomposition has three packages:",
      "",
      "- **PKG-01 Foundations**: piles and pile caps for P1 to P4; depends on the geotechnical survey.",
      "- **PKG-02 Substructure**: pier columns and crossheads; depends on PKG-01's as-built toe levels.",
      "- **PKG-03 Deck**: precast beams and the deck slab; depends on PKG-02 and the marine plant window.",
    ].join("\n"),
  );
};

/** A subagent's task: check the pile lengths against the survey. */
export const pilesCheck: Script = async (run) => {
  await run.stream("reasoning", "Compare each pier's pile length in the basis with the rock head the survey found there.");
  await run.mark("checking");
  await run.command('rg -n "P[1-4]" brief/geotech-summary.md', "brief/geotech-summary.md:21:P1 rock head 14 m\nbrief/geotech-summary.md:22:P2 rock head 17 m\nbrief/geotech-summary.md:23:P3 rock head 19 m\nbrief/geotech-summary.md:24:P4 rock head 18 m\n");
  await run.stream("agentMessage", "P3 is the deepest at 19 m to rock, so 21 m piles there; 19 m covers the other three.");
};

/** A subagent's task: rerun the deck takeoff, which needs a command approved. */
export const deckCheck: Script = async (run) => {
  await run.stream("reasoning", "The deck quantities come from the takeoff script; run it on S-102.");
  const command = "python3 tools/takeoff.py drawings/S-102-deck.pdf";
  if (run.asks) {
    const answer = await run.ask({ kind: "command", command, cwd, reason: "Rerun the deck takeoff" });
    if (!allowed(answer)) return void (await run.stream("agentMessage", "Left the takeoff as it was."));
  }
  await run.command(command, "Precast beams  48 no.\nDeck slab      1,920 m2\n");
  await run.stream("agentMessage", "The deck takeoff matches the quantities: 48 beams and 1,920 m² of slab.");
};

/** Hands two checks to subagents and waits for both. */
export const delegateTurn: Script = async (run) => {
  await run.stream("reasoning", "Two independent checks: the pile lengths and the deck takeoff. A subagent each, then compare.");
  const piles = run.delegate("piles", pilesCheck);
  const deck = run.delegate("deck", deckCheck);
  await run.waitFor([piles, deck]);
  await run.stream("agentMessage", "Both checks are back: P3 needs **21 m** piles, the other piers 19 m, and the deck takeoff matches the quantities.");
};

/** The script for a message, by what it asks. */
export function scriptFor(message: Message): Script {
  if (/subagent|in parallel/i.test(message.text)) return delegateTurn;
  if (/instead/i.test(message.text)) return alternativeTurn;
  if (/estimate|quantit/i.test(message.text)) return classTurn;
  if (/scope/i.test(message.text)) return scopeTurn;
  return replyTurn;
}
