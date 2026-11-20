# Chirality engine: specification

## Purpose

Chirality is an engine that gives an application a built-in agent working on
workflows. It runs the Codex app-server and exposes conversations, workflows
and the person's part in them through one typed Rust API. Each application
puts a thin adapter and its own interface over the engine.

The first application is a plain desktop wrapper for using and trying the
engine on its own. The next is a project-estimating application (decomposition,
scopes, dependencies, basis of estimate, cost estimate, schedule and resource
loading); its integration is out of scope here, but every engine decision
should serve an application like it.

## Goals

1. A person can do real work with an agent on a project folder, with the
   experience of a current agent interface, not a governance tool.
2. A workflow is a first-class thing to pick and use in a conversation.
3. The engine is embeddable: no dependency on the wrapper or its framework,
   one typed API, and the wrapper uses nothing the engine does not expose.
4. The code is exemplary: small modules, typed boundaries, tests on what Codex
   really sends, and nothing in it that does not serve a goal above.

## References

Learn the implementation from these. Read them before designing a part.

| Reference | Use it for | Where |
| --- | --- | --- |
| **T3 Code** | The pattern to follow: how a harness is wrapped, how Codex's app-server is driven, threads, forks, approvals and questions, diffs, the side panel, background work | `github.com/pingdotgg/t3code` (MIT) |
| **Chirality 3.0.1** | Design language only: layout, type, colour, spacing, the feel of the navigator and conversation | Chirality repository history, `projects/chirality-app-dev/frontend` at `cf4653526d2aa24976069275878f4fd4bedd0b9c` |
| **Chirality App v4** | Mostly what not to do (see Non-goals). Positive: what was learned about Codex (below) | `sgttomas/chirality-app`, tags `app-v4/reference-main`, `app-v4/reference-interface` |
| **Codex app-server protocol** | The supplier contract | Generated from the pinned binary: `codex app-server generate-json-schema` and `generate-ts` |

Facts v4 established about the Codex app-server (re-check at the pin):
- Each conversation honours its own working folder, so one Codex process serves every project.
- The agent can ask the person a question (`item/tool/requestUserInput`), enabled in Default mode by a session setting; a question still outstanding when Codex ends is lost.
- A project marked trusted loads its own `AGENTS.md` and `.codex` settings, including MCP servers and shell start-up files.

## Supplier

Codex `0.171.0` (`rust-v0.171.0`), the macOS package
`codex-package-aarch64-apple-darwin.tar.gz`, sha256
`58c88032ec446e72922d911d8f03773102da20c16a6059cf21f6744bce57f210`. The engine
starts `codex app-server` as a child process and speaks its JSON-RPC protocol
over stdio. Moving the pin is a deliberate change with the protocol types
regenerated.

The engine uses the person's own Codex home by default, as T3 Code does, so
sign-in, models and settings are shared with their Codex. An application may
give it another home.

## The engine

- **Codex process:** start, health, restart, stop; one process for all projects.
- **Account:** read, sign in with ChatGPT through Codex, sign out.
- **Models:** the models Codex lists, each with its reasoning levels.
- **Projects:** folders the person adds; each thread belongs to one.
- **Threads:** list (from Codex, which keeps them), start, resume, fork, archive, rename; title as Codex gives it.
- **Turns:** send a message with text, attachments (images, files) and file references; steer or interrupt a running turn; model, reasoning level and access level per turn.
- **Events:** one typed stream of everything a thread does: agent text as it streams, reasoning summaries, plans, commands with output, file changes with diffs, tool calls, turn start and end, errors.
- **The person's part:** approvals (commands, file changes, permissions) and questions, each answered through the API; nothing is answered on the person's behalf.
- **Application tools:** the application gives the agent its own tools (name, description, JSON Schema of the arguments, an optional namespace) and its handler answers every call; Codex keeps a thread's tools with it through resume and fork.
- **Access:** Codex's own access levels (read only, ask, full access), nothing added.
- **Workflows:** discovered in the folders the application names (the Chirality Library, the project's own); listed with name, summary and source; picked into a message so the agent follows it in that conversation.
- **State:** a snapshot of everything the application shows (projects, threads, their status, what waits on the person), plus the event stream to keep it current.

The API is Rust types. Bindings for an application's interface are generated
from them (TypeScript for the wrapper); nothing crosses as hand-built JSON.

## The wrapper

A desktop app (Tauri, React) that is a thin adapter over the engine.

- **Left:** projects, each with its threads; new thread; search; a mark on a thread that is running, finished, or waiting on the person.
- **Centre:** the conversation. The person's messages, the agent's text rendered as Markdown, reasoning and tool activity folded, a plan when there is one, file changes as diffs. A composer with attachments, file references (`@`), the workflow picker, and one line of controls: model, reasoning level, access level. Send becomes Stop while a turn runs.
- **Approvals and questions:** shown where the person will see them, answered in one or two presses, then out of the way.
- **Right panel:** the diff of the thread's changes, the workflow in use, and a preview of a file the agent produced.
- **Forks:** fork a thread at a message to try another path; the fork is a thread of its own, shown under its parent.
- **Background:** several threads may run at once; a notice when one finishes or needs the person.
- **First use:** sign in, add a project folder, start. Nothing else.

## Non-goals

These are what made v4 unusable. None of them is built.

- Explanatory text, warnings and disclosures the person did not ask for.
- A records store, ledger, or second source of truth for what Codex already keeps.
- A permission or trust model of our own on top of Codex's access levels.
- State words and counters the person has to interpret.
- Requirements, tests or reviews about the wording of the interface.
- Hand-shaped data between engine and interface.
- Files over about 600 lines; one module doing several jobs.
- Stages, staleness tracking, or a declared workflow position. These may come later, in an application that shows it needs them.

## Done means

- A person signs in, adds a project, picks a model and reasoning level, picks a workflow, works through it with the agent, approves what it asks, answers its questions, reviews its changes as diffs, forks to try an alternative, and comes back after quitting to find every thread where it was.
- The engine is used by the wrapper only through its API, and could be used by another application the same way.
- Engine tests run the pinned Codex against a local stub model, with no network and no account; interface tests use data the engine itself produced.
