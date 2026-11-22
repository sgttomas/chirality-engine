# Chirality engine: specification

## Purpose

Chirality is an engine that gives an application a built-in agent working on
workflows. It runs the Codex app-server and exposes conversations, workflows
and the person's part in them through one typed Rust API. With it come the
parts an application builds its interface from: a Tauri adapter and an
interface package. Each application composes the parts it wants.

The plain application is the Chirality App (`sgttomas/chirality-app`): the
standard layout, for using any Library workflow with the agent. The next is a
project-estimating application (`sgttomas/epc-project-init`: decomposition,
scopes, dependencies, basis of estimate, cost estimate, schedule and resource
loading); its integration is out of scope here, but every engine decision
should serve an application like it.

## Goals

1. A person can do real work with an agent on a project folder, with the
   experience of a current agent interface, not a governance tool.
2. A workflow is a first-class thing to pick and use in a conversation.
3. The engine is embeddable: no dependency on an application or its framework,
   one typed API, and the parts use nothing the engine does not expose.
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

- **Codex process:** start, health, restart, stop; one process for all projects. After a restart, every project's threads are listed again.
- **Account:** read, sign in with ChatGPT through Codex, sign out.
- **Models:** the models Codex lists, each with its reasoning levels.
- **Projects:** folders the person adds; each thread belongs to one.
- **Threads:** list (from Codex, which keeps them), start, resume, fork, archive, rename; title as Codex gives it.
- **Subagents:** a thread an agent starts (Codex's multi-agent tools) is a thread of the snapshot with its parent (the thread that started it, never one it messaged), path (`/root/worker`) and, once Codex lists it, nickname; it is listed under that parent, never at the top. Neither its end nor its approval marks it unread: its parent tells what it did. It is read and followed live like any thread, and Stop works on it; a message to it is refused, as Codex refuses one. Archiving a thread archives its subagents, as Codex does.
- **Turns:** send a message with text, attachments (images, files) and file references; steer or interrupt a running turn; model, reasoning level and access level per turn. A thread takes one send or Stop at a time, each carried through by the engine even if its caller gives up: a send while a turn runs (or starts) steers it, Stop during a start stops that turn, and after a Stop the thread waits for Codex to answer and end the turn, so the next send starts a new one. If Codex does neither within a while, the Stop is reported as not taken and the thread is let go; the next send goes to Codex as a start, which Codex takes as a steer if the turn still runs.
- **Events:** one typed stream of everything a thread does: agent text as it streams, reasoning summaries, plans, commands with output, file changes with diffs, tool calls, a subagent started, messaged, stopped or finished (naming it), waiting for subagents, turn start and end, errors. Each event is numbered; a gap means some were missed.
- **The person's part:** approvals (commands, file changes, permissions) and questions, each answered through the API; nothing is answered on the person's behalf. A subagent's approval names the subagent, which the snapshot holds under its parent.
- **Application tools:** the application gives the agent its own tools (in its own namespace: name, description, JSON Schema of the arguments) and its handler answers every call; Codex keeps a thread's tools with it through resume and fork. A subagent has its parent's tools; its call names the subagent's own thread and turn, and shows there.
- **Assignment between threads:** the engine's own tools (namespace `chirality`) on every thread it starts, with which one thread assigns work to another, ordinary thread and threads message each other; see below.
- **Access:** Codex's own access levels (read only, ask, full access), nothing added.
- **Workflows:** discovered in the folders the application names (the Chirality Library, the project's own); listed with name, summary and source; picked into a message so the agent follows it in that conversation.
- **State:** a snapshot of everything the application shows (projects, threads, their status, what waits on the person), plus the event stream to keep it current. The snapshot, and each thread read, carries the number of the last event it holds. A read is Codex's history, numbered from before the read: every event after that number applies to it (an item it has as done stays done; text for an item it lacks begins that item), and when a turn of the thread in view completes, the thread is read again (keeping that turn's plan, which Codex leaves out of history). The engine keeps no copy of the conversation: text Codex did not keep (of a reply a Stop cut short) is in the live view, and not in the thread read again.

The API is Rust types. Bindings for an application's interface are generated
from them (TypeScript, in the interface package); nothing crosses as
hand-built JSON.

## The parts an application builds on

Versioned with the engine, so its types, the adapter and the interface move
together. An application pins one engine revision for all of them.

- **Tauri adapter** (`crates/tauri`, crate `chirality-engine-tauri`): one
  command per engine method (named `engine:<method>`), every engine event
  forwarded to the webview with a mark after each burst, the engine made when
  the app starts and shut down (Codex, started or starting, ended and reaped)
  before the app exits. An application registers it on its
  Tauri builder (`plugin(config)` and `commands(its own handler)`), gives its
  own tools in the engine's setup, and reaches the engine from its own
  commands. Tested headless against the pinned Codex.
- **Interface package** (`packages/ui`, package `@chirality/engine-ui`): the
  engine client over a host transport (invoke and listen; Tauri is one, recorded
  engine data another), the generated bindings, the engine half of the state,
  and the interface as parts. Each part works inside an application's own
  layout; the standard layout puts them together:
  - **Left:** projects, each with its threads; new thread; search; a mark on a thread that is running, finished, or waiting on the person.
  - **Centre:** the conversation. The person's messages, the agent's text rendered as Markdown, reasoning and tool activity folded, a plan when there is one, file changes as diffs. A composer with attachments, file references (`@`), the workflow picker, and one line of controls: model, reasoning level, access level. Send becomes Stop while a turn runs.
  - **Approvals and questions:** shown where the person will see them, answered in one or two presses, then out of the way.
  - **Right panel:** the diff of the thread's changes, the workflow in use, and a preview of a file the agent produced; an application adds its own tabs.
  - **Forks:** fork a thread at a message to try another path; the fork is a thread of its own, shown under its parent.
  - **Assignment:** an assigned thread is an ordinary thread at the top of the list; it shows the thread that assigned it. A message from another thread (a brief, a message, a report) shows as from that thread, by its title, which opens it; in the sender's conversation each `assign`, `message` and `close` names the other thread and opens it.
  - **Subagents:** listed under the thread that started them, folded unless one runs or waits on the person (one waiting, or open, is always shown), and their parent's mark counts theirs; each opens as a conversation, with a way back to its parent and Stop in place of the composer. In the parent, each start, message, stop and finish names the subagent and opens it, and an approval a subagent waits on is answered there too.
  - **Background:** several threads may run at once; a notice when one finishes or needs the person.
  - **First use:** sign in, add a project folder, start. Nothing else.

An application consumes both from this repository at a pinned revision (a git
dependency in Cargo, and `#<revision>::path:packages/ui` in npm), fetched with
the developer's own access; no registry.

## Assignment between threads

Designed with the owner (2026-11-21); built at Codex 0.171.0.

Codex's subagents are the agent's own helpers: they run inside its turn, and
the person can watch and stop them, but Codex refuses a message from the
person to one. Assignment is the other relation T3 Code has: one thread gives
work to another, ordinary thread; the two exchange messages; the person can
open, talk to and direct either. Codex does not offer it, so the engine
provides it, with tools of its own given to the agent as an application's are.

**What the agent can do.** Engine tools in the namespace `chirality`, on every
thread the engine starts, beside the application's tools:

- `assign`: start a new thread, in this thread's project or another the
  person has added, with a title and a brief, optionally a workflow picked
  into the brief, and a model, reasoning level and access level. Each is the
  assigner's own unless the call names another. Model and reasoning may be
  higher or lower, as the person directed. Access may be lower; a higher one is
  not given by the call: the thread starts at the assigner's level and its
  agent asks the person through Codex's own permission request. Returns the
  new thread's id.
- `message`: send text to any thread, by id.
- `threads`: the threads the agent can address: the person's projects' threads,
  each with its title, project, whether it is running, idle or waiting on the
  person, and its relation to this one (assigned by it, assigned it, or none);
  and the projects, by name and path.
- `close`: end an assignment this thread made; no more reports come from it.

A call is answered once its work is done (a thread started, a turn started
or a message queued), never after the turn it starts. A subagent has these
tools with its parent's, and its call is refused: Codex takes no message to a
subagent, so it could hear no report.

**What happens.**

- The brief is the assigned thread's first message, marked as from the
  assigning thread. A `message` is the same: a message in the receiving
  thread, marked with the sender's id and title, which the interface shows as
  from that thread, not from the person.
- When a turn of the assigned thread that its assigner started (by the brief
  or a message) ends, its last reply is delivered to the assigner as a
  message, with how the turn ended (completed, stopped, failed). Turns the
  person starts in the assigned thread report nothing unless its agent sends a
  `message`.
- A message to an idle thread starts a turn in it; to a running one it waits
  until the engine has seen that turn end (it does not steer, nor start on a
  turn a Stop could not settle). Messages to a thread are delivered in order,
  each starting a turn of its own; reports in the order their turns ended.
- A delivered turn runs with the receiving thread's settings as they are when
  it starts (a report's, the assigner's). The engine knows a thread's access
  once it has started a turn there; until then (after a restart, a listing)
  the turn has no more than Ask, the sender's, or the access Codex resumed or
  forked the thread with, and is read only where Codex did not say. A sender
  or reporter whose access the engine does not know counts as read only. The
  person's own sends are unchanged.
- A delivery is made only while its sender is there and, for a report, the
  assignment is open: archiving either thread drops what waits. A delivery
  Codex refuses after it waited is told on the receiving thread; one whose
  `turn/start` was sent and not answered before Codex stopped is looked for
  among that thread's turns after the latest it had then, before it is tried
  again.
- Approvals and questions in an assigned thread go to the person on that
  thread, as on any thread. Its assigner is not told; it hears when the turn
  ends.
- Archiving either thread ends the assignment. A subagent cannot assign or
  message (its call is refused, and it and its parent go on).

**What the person sees.** Assigned threads are ordinary threads, at the same
level in the list. Each shows which thread assigned it; the assigner's
conversation shows each `assign` and `message` as an item that opens the other
thread. The person can open, send to and stop either, at any time.

**The mark.** A brief, message or report is a message whose first part is
`<thread-message from="<thread id>" title="<title>"/>` (with
`report="completed|stopped|failed"` on a report), then the text. The engine
reads it from history into the message's sender (`from`), and from the
listing's preview into the assigned thread's summary (`assignedBy`); the
reverse relation is the threads that name a thread there. A fork has its
source's first message, and is not an assignment; the listing does not say
whether a thread is a fork (`forkedFromId` is null there), so a listed thread
with a mark is read (`thread/read` says) before it is taken as assigned. The
first text after the mark is the message's text whatever it says.

**What the engine keeps.** Nothing Codex does not. The relation is read from
the assigned thread's first message, which names its assigner (Codex's thread
listing carries it as the preview). Reports and messages are messages in
Codex's history. Deliveries waiting for a running turn are held in memory: a delivery waiting when the app quits is
not made, and the person finds the result in the assigned thread.

**Established at the pin (0.171.0).** The listing's preview is the first
message's parts joined without a separator and kept whole (a 30,000-character
first message, also after Codex restarts), so the mark comes first and is read
from there. The engine starts a thread and its turn while it answers a tool
call: the calling turn went on and ended first. A
thread started without these tools cannot use them: Codex answers the model
`unsupported call` and never asks the engine, and offers no way to add tools
to a thread (its own `replace_dynamic_tools` is not in the app-server API); a
fork keeps them. `features.multi_agent_v2_dynamic_tools=true` gives a new subagent all
of its parent's tools, the engine's and the application's (Codex gives one all
or none, and none without the setting); a subagent's call comes to the engine
on its own thread and turn. Codex announces a turn's end before
the next `turn/start` is taken as a new turn: a start sent once the end is
heard begins a turn, never steers; and a turn's first message and last reply
are in its history (`itemsView: summary`) once its end is heard.
`thread/resume` and `thread/fork` answer with the sandbox Codex loaded the
thread with: a read only or full access thread's own, and read only for one
the engine ran at Ask in a folder Codex does not trust, so it bounds an access
but does not give it. The engine keeps it for as long as Codex has the thread
loaded.

**Decided by the owner (2026-11-21).** Turns the person starts in an assigned
thread report nothing back. The assigning agent gives the assigned thread its
own model and reasoning level unless directed otherwise, and may give higher
or lower ones; higher access is asked of the person, as above. Any thread may
message any thread, in any project; in practice agents do so when directed, or
with the threads they assigned or were assigned by. No bound on turns started
by deliveries: the person sees every running thread and can stop any; one is
added only if exchanges that do not end are seen.

**Decided by the owner (2026-11-22).** Subagents get the application's tools,
never the engine's: the engine refuses those to them.

## Known limits

- Assignment: a thread started before the engine had its tools (or by another client) cannot assign or message; Codex tells its agent the call is unsupported. A delivery waiting when the engine stops is not made, and a turn that ends because Codex stopped reports nothing. The sender's title in a mark is the one the engine knew when it sent it (none for a thread Codex has not yet titled); the interface shows the sender's current title. A message the person writes that begins with a mark is read as from that thread. After a restart, a close is found by reading the assigner's history, once per report.
- A legacy-history exchange can stay doubled in a view the person switches away from while its read is pending, and can show doubled briefly before the completion read returns.
- Text or a plan that streamed before a successful read whose history does not yet hold it (a thread opened mid-reply, a follow-up sent before a completion read returns) is missing from the view until its item completes, and stays missing if the person stops the turn first.
- A Stop sent after a Stop that timed out and a send that went to Codex as `turn/start` can name the old turn.
- Threads in Codex's older (legacy) history format are not supported.
- Codex's refusals (of a thread it no longer has, or a turn it does not know) reach the person in Codex's words.
- A draft edited in a just-created thread while its first send is still being accepted can be replaced by the moved draft, and an attachment picked across that moment can land in the project's next new thread.
- A project's thread listing in flight when a thread is archived can list it again until the next listing.
- Subagents (Codex 0.171.0, multi-agent v2): the person cannot message one (Codex refuses), and one cannot ask the person a question (Codex refuses it to the agent). The task and messages an agent gives a subagent are in neither thread's history. Stopping a subagent does not tell its parent, and stopping the parent does not stop its subagents. The nickname is known once Codex lists the subagent. A subagent's settings shown are those of the turn that started it. A fork has no subagents, and a revert leaves the reverted turn's subagents listed. After Codex restarts, a subagent is read but runs again only once its parent is resumed. Live, an agent's wait for a subagent comes before the subagent's finish; read again, after it.

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
- The parts use the engine only through its API, and every application uses the engine and the parts the same way.
- Engine tests run the pinned Codex against a local stub model, with no network and no account; interface tests use data the engine itself produced.
