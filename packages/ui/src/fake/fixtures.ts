// The fake engine's world: models, projects, workflows and files.
import type { Model, Project, Workflow } from "../engine/types";

export const models: Model[] = [
  {
    id: "gpt-5.5",
    name: "GPT-5.5",
    efforts: ["low", "medium", "high", "xhigh"],
    defaultEffort: "medium",
    isDefault: true,
  },
  { id: "gpt-5.5-mini", name: "GPT-5.5 mini", efforts: ["low", "medium", "high"], defaultEffort: "low", isDefault: false },
  { id: "gpt-5.3-codex", name: "GPT-5.3 Codex", efforts: ["low", "medium", "high"], defaultEffort: "medium", isDefault: false },
];

export const bridge: Project = { id: "p-bridge", name: "harbour-bridge", path: "/Users/person/work/harbour-bridge" };
export const clinic: Project = { id: "p-clinic", name: "clinic-fitout", path: "/Users/person/work/clinic-fitout" };

const library = "/Users/person/Chirality Library/workflows";

export const libraryWorkflows: Workflow[] = [
  {
    id: "wf-decompose",
    name: "Decompose a project",
    summary: "Break the work into packages and deliverables, each with a scope and its dependencies.",
    source: library,
    path: `${library}/decompose/WORKFLOW.md`,
  },
  {
    id: "wf-scope",
    name: "Write a scope of work",
    summary: "State what a deliverable must achieve, under what conditions, and what shows it is adequate.",
    source: library,
    path: `${library}/scope-of-work/WORKFLOW.md`,
  },
  {
    id: "wf-basis",
    name: "Basis of estimate",
    summary: "Record the quantities, rates and assumptions behind each line of the estimate.",
    source: library,
    path: `${library}/basis-of-estimate/WORKFLOW.md`,
  },
];

export const projectWorkflow = (project: Project): Workflow => ({
  id: `wf-${project.id}-review`,
  name: "Weekly cost review",
  summary: "Compare the forecast with the last review and explain each movement over 2%.",
  source: "project",
  path: `${project.path}/.chirality/workflows/cost-review/WORKFLOW.md`,
});

export const files = [
  "README.md",
  "AGENTS.md",
  "brief/client-brief.pdf",
  "brief/site-survey.md",
  "brief/geotech-summary.md",
  "drawings/GA-001-general-arrangement.pdf",
  "drawings/S-101-piers.pdf",
  "drawings/S-102-deck.pdf",
  "estimate/basis.md",
  "estimate/rates.csv",
  "estimate/quantities.csv",
  "estimate/summary.xlsx",
  "scope/SOW-01-foundations.md",
  "scope/SOW-02-substructure.md",
  "scope/SOW-03-deck.md",
  "schedule/programme.md",
  "schedule/resources.csv",
  "tools/estimate.py",
  "tools/takeoff.py",
];

/** A new file: Codex gives its content as the diff. */
export const sowDiff = `# SOW-01 Foundations

## What it must achieve

Piled foundations for piers P1 to P4, carrying the deck loads in
\`drawings/S-101-piers.pdf\` to the rock head found in the geotechnical survey.

## Conditions

- Marine works inside the tidal window, April to September.
- Rock head between 14 m and 19 m below bed level.

## What shows it is adequate

- Pile load tests on two working piles per pier.
- As-built toe levels within 0.5 m of design.
`;

export const basisDiff = `@@ -4,7 +4,10 @@
 ## Foundations

-Piles priced at an allowance of 40 m per pile.
+Piles priced from the geotechnical summary: 17 m average to rock head,
+plus 2 m socket, so 19 m per pile.
+
+Rate: bored pile 1.2 m diameter, marine plant, from \`estimate/rates.csv\` row 14.

 ## Substructure

`;

export const quantitiesDiff = `@@ -1,5 +1,5 @@
 item,unit,quantity
-bored pile 1.2m,m,1280
+bored pile 1.2m,m,608
 pile cap,m3,420
 pier column,m3,312
 crosshead,m3,188
`;

export const alternativeDiff = `@@ -4,8 +4,12 @@
 ## Foundations
 
 Piles priced from the geotechnical summary: 17 m average to rock head,
 plus 2 m socket, so 19 m per pile.
 
+Alternative: 22 m per pile, allowing for rock head 3 m deeper than the
+survey found. 32 piles at 22 m is 704 m, about 0.6 M more.
+
 Rate: bored pile 1.2 m diameter, marine plant, from \`estimate/rates.csv\` row 14.
 
 ## Substructure
`;
