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
give it another home, or run Codex itself and give the engine its transport
(an embedded engine, below).

## The engine

- **Codex process:** start, health, restart, stop; one process for all projects. After a restart, every project's threads are listed again.
- **Account:** read, sign in with ChatGPT through Codex, sign out.
- **Models:** the models Codex lists, each with its reasoning levels.
- **Projects:** folders the person adds; each thread belongs to one.
- **Threads:** list (from Codex, which keeps them), start, resume, fork, archive, rename; title as Codex gives it.
- **Subagents:** a thread an agent starts (Codex's multi-agent tools) is a thread of the snapshot with its parent (the thread that started it, never one it messaged), path (`/root/worker`) and, once Codex lists it, nickname; it is listed under that parent, never at the top. Neither its end nor its approval marks it unread: its parent tells what it did. It is read and followed live like any thread, and Stop works on it; a message to it is refused, as Codex refuses one. Archiving a thread archives its subagents, as Codex does.
- **Turns:** send a message with text, attachments (images, files) and the threads it mentions; steer or interrupt a running turn; model, reasoning level and access level per turn. A thread takes one send or Stop at a time, each carried through by the engine even if its caller gives up: a send while a turn runs (or starts) steers it, Stop during a start stops that turn, and after a Stop the thread waits for Codex to answer and end the turn, so the next send starts a new one. If Codex does neither within a while, the Stop is reported as not taken and the thread is let go; the next send goes to Codex as a start, which Codex takes as a steer if the turn still runs.
- **Events:** one typed stream of everything a thread does: agent text as it streams, reasoning summaries, plans, commands with output, file changes with diffs, tool calls, a subagent started, messaged, stopped or finished (naming it), waiting for subagents, turn start and end, errors. Each event is numbered; a gap means some were missed.
- **The person's part:** approvals (commands, file changes, permissions) and questions, each answered through the API; nothing is answered on the person's behalf. A subagent's approval names the subagent, which the snapshot holds under its parent.
- **Application tools:** the application gives the agent its own tools (in its own namespace: name, description, JSON Schema of the arguments) and its handler answers every call; Codex keeps a thread's tools with it through resume and fork. A subagent has its parent's tools; its call names the subagent's own thread and turn, and shows there.
- **Assignment between threads:** the engine's own tools (namespace `chirality`) on every thread it starts, with which one thread assigns work to another, ordinary thread, threads message each other, and one reads another; see below.
- **Access:** Codex's own access levels (read only, ask, full access), nothing added.
- **Workflows:** discovered in the folders the application names (the Chirality Library, the project's own); listed with name, summary and source; picked into a message so the agent follows it in that conversation.
- **Embedding:** an application that runs Codex itself gives the engine its transport: an embedded engine, with one thread, its own tools only, one rule for what Codex asks, and bounded memory; see below.
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
  - **Centre:** the conversation. The person's messages, the agent's text rendered as Markdown, reasoning and tool activity folded, a plan when there is one, file changes as diffs. A composer with attachments, thread mentions (`@`), the workflow picker (an application may name the workflows it lists before anything is typed, besides the project's own; a search finds every workflow), and one line of controls: model, reasoning level, access level. Send becomes Stop while a turn runs.
  - **Mentions:** an `@` typed (at the start or after a space, over a selection too), or the `@` button (at the caret), lists the person's threads by project, the current project first, then the others by latest activity; in each, those running or needing the person first, then unread (finished or failed), then the rest, most recent first; a few per project until a title is typed, which filters every project's threads by title (spaces and all), and closes the list for good when none matches. Not the current thread, nor subagents. The list takes the composer's arrows, Enter, Tab and Escape (none while an input method composes), and closes when the composer loses focus or the caret moves. A picked thread is `@<title>` in the text and a removable chip, and its `@` opens nothing again; in the sent message each is a chip that opens it, by its current title (the one it was mentioned with when it is gone). Files are attached, not mentioned.
  - **Approvals and questions:** shown where the person will see them, answered in one or two presses, then out of the way.
  - **Right panel:** the diff of the thread's changes, the workflow in use, and a preview of a file the agent produced; an application adds its own tabs.
  - **Forks:** fork a thread at a message to try another path; the fork is a thread of its own, shown under its parent.
  - **Assignment:** an assigned thread is an ordinary thread at the top of the list; it shows the thread that assigned it. A message from another thread (a brief, a message, a report) shows as from that thread, by its title, which opens it; in the sender's conversation each `assign`, `message` and `close` names the other thread and opens it, and a `read` folds into the work as "Read <title>".
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
- `message`: send text to any thread, by id, to start or continue an exchange.
- `threads`: the threads the agent can address: the person's projects' threads,
  most recently active first, each with its title, project, whether it is
  running, idle or waiting on the person, and its relation to this one
  (assigned by it, assigned it, or none); and the projects, by name and path.
- `read`: another thread (any the person has, in any project, a subagent's
  too; not the caller's own), without waking it or queuing anything there:
  its title, project, status (running, waiting on the person, idle, failed)
  and last error (the one the engine saw, or else the latest turn's in
  history, if it failed), then its items newest first; a thread Codex has not
  kept yet has its header and no items. `view: messages`
  (the default) gives the person's messages, messages from other threads with
  their sender, a completed turn's final reply (its last message that is not
  commentary), and, for a turn with none (running, stopped or failed), its
  latest message marked as an update with how that turn stands; `view:
  activity` adds the work and the agent's other messages: commands, file
  changes and tool calls, each named, with its status (in progress,
  completed, failed, declined) and output cut short. Every text in the answer,
  its header too, is cut to a size, a file change lists a few of its files, and
  the whole answer is kept to about 16 KB: the first item of a page is cut
  further until it fits (or, if it cannot, is said to be too large), so every
  page shows something or ends; no other item is added that does not fit. When
  there are more, the answer gives `before`, a cursor naming the turn and item
  it stopped at, which goes on from there however many turns have come since
  (pages are read until that turn is found, so an answer can have no items
  and still have more); a `before` that is not one a read gave, or is too
  long to be, is refused; `limit` bounds the items. Read
  from Codex a few turns at a time, newest first (`thread/turns/list`), each
  item built as it is added, until the answer is full, keeping nothing. Its description: read to see where a thread stands or what it did;
  message it to ask for its judgment or have it act.
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
- When a turn the engine started with another thread's message (a brief, or
  any `message`, whether or not that thread assigned this one) ends, its last
  reply is delivered back to that thread as a message, with how the turn ended
  (completed, stopped, failed): a report. An assigned thread's turn that its
  assigner's mark opens reports too when the engine did not see it start
  (after a restart). A report's own turn reports nothing,
  so exchanges cannot go on by themselves. A `close` ends reports from a
  thread the closer assigned. Turns the person starts report nothing unless
  the agent sends a `message`. A thread without the engine's tools (started
  before them) is messaged like any, and its reply comes back without it
  calling anything.
- A message may mention threads: after its text, one part per thread,
  `<thread-ref id="<thread id>" title="<title>" project="<project>"/>` (escaped
  as the mark is), with the title and project as the engine knows them when it
  sends; read back from history into the message's `threads`, and left out of
  the title Codex's preview gives a new thread (only parts that are whole
  references; text that merely holds one stays). The engine keeps nothing of it.
- A message to an idle thread starts a turn in it; to a running one it waits
  until the engine has seen that turn end (it does not steer, nor start on a
  turn a Stop could not settle). Messages to a thread are delivered in order,
  each starting a turn of its own.
- A report is read from the ended turn in Codex's history, found by its id,
  whatever has started on the thread since. Who it goes to is known when the
  turn ends (the thread whose message the engine started it with, or else the
  assigner), and a thread's reports, from every thread, come in the order
  their turns ended; no thread's reports wait on another's. Only the reads of
  history a report needs have a deadline, 30 seconds; a report found is
  delivered as any message is. A report owed (the engine started the turn
  with the recipient's message) that cannot be made, because a read failed or
  took longer or the turn is not among the thread's latest 100, is said in that
  turn as an error item; neither the turn nor the thread changes. A report not
  known to be owed (an assigned thread's turn the engine did not start) says
  nothing. A report whose thread or recipient went from the engine (archived,
  its project removed) after its turn ended is not made, even if it is back.
- A delivered turn runs with the receiving thread's settings as they are when
  it starts (a report's, the assigner's). The engine knows a thread's access
  once it has started a turn there; until then (after a restart, a listing)
  the turn has no more than Ask, the sender's, or the access Codex resumed or
  forked the thread with, and is read only where Codex did not say. A sender
  or reporter whose access the engine does not know counts as read only. The
  person's own sends are unchanged.
- A delivery is made only while its sender is there and, for a report from a
  thread the receiver assigned, the assignment is open: archiving either
  thread drops what waits. A delivery
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
on its own thread and turn. Multi-agent v2 has no depth limit (`agent_max_depth` is v1's): a subagent has the tools to start subagents whenever its model declares v2, as every current model does. The engine passes `features.multi_agent_v2.max_concurrent_threads_per_session=8` (eight threads at once to a session, the parent's included; Codex's default is four) and a subagent usage hint, in place of the model's own, telling a subagent to do its task, report to its parent and start no subagents of its own; an application's own settings come after these and win. Codex announces a turn's end before
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
never the engine's: the engine refuses those to them. A turn another thread's
message started reports back to that thread whether or not it assigned this
one (replacing, for those turns, the 2026-11-21 rule that only the assigner's
turns report); turns the person starts still report nothing. The composer's
`@` mentions threads only (files are attached), and a message carries the
threads it mentions to the agent. `threads` lists by most recent activity, as
`@` does. `read` is included, for effective orchestration: an orchestrating
thread sees where a worker or subagent stands without waking it; `message` is
still how threads ask for judgment or action.

## Embedding under containment

Designed 2026-11-22 for SWBPIPE, revised after three rounds of review; not yet
built.

SWBPIPE runs one Codex per session, with one ephemeral thread, no built-in
capability, and remote control off; any built-in activity ends the session.
Its host spawns, frames, observes and cleans up Codex, and the engine speaks
JSON-RPC over the host's transport. All of this is opt-in: an ordinary engine
behaves as now for the person (it reports more, shown only where asked).

**The surface** (`api/embedded.rs`).

```rust
pub fn embedded(config: EmbeddedConfig) -> Result<(Engine, EmbeddedEvents)>;
/// What the host reserves before `connect`; see Memory.
pub const fn embedded_max_bytes(limits: &EmbeddedLimits) -> u64;

pub struct EmbeddedConfig {
    pub transport: HostTransport,
    pub work_dir: PathBuf, // the thread's `cwd`
    pub tools: AppTools,   // the agent's only tools (SWBPIPE's: namespace `swbpipe`)
    pub limits: EmbeddedLimits,
}
/// One JSON-RPC message per frame, without its newline.
pub struct HostTransport {
    pub from_codex: tokio::sync::mpsc::Receiver<String>,
    pub to_codex: tokio::sync::mpsc::Sender<String>,
}
/// `embedded` refuses a value outside its range.
pub struct EmbeddedLimits {
    pub pending_calls: u32,  // 1..=16 application calls outstanding
    pub calls_per_turn: u32, // 1..=1,024 requests from Codex per turn, refused ones included
    pub argument_bytes: u32, // 1..=65,536 bytes of a call's arguments
    pub event_bytes: u32,    // 256 KiB..=16 MiB of events not yet received
}
impl EmbeddedEvents {
    pub async fn recv(&mut self) -> Option<Event>; // None after the terminal event
    pub fn try_recv(&mut self) -> Result<Event, TryRecvError>;
}
```

The engine it returns has only `connect` (`initialize`, with no account or
model reads), `open_thread` once, `send` (text only), `interrupt`, `close()`
(also on dropping the last handle), `finished()`, `shutdown()` (close, then
`finished()`), `snapshot()` (holding only `remoteControl`), and the event
stream. Everything else is `Invalid`, answers included. It never spawns,
signals or reaps, touches no file, and has no `chirality` tools, assignment,
deliveries or reports.

**Frames.** The host owns line framing, duplicate keys and numeric fidelity;
its 128 KiB includes the newline both ways, so a frame's text is at most
131,071 bytes. A valid frame is one JSON object that is exactly one of these
(other members, such as Codex's `emittedAtMs`, are ignored):
- a request: an integer `id` or a string one (non-empty, no control
  characters, at most 256 bytes), a string `method`, optional object
  `params`, no `result` or `error`;
- a notification: a string `method`, optional object `params`, no `id`,
  `result` or `error`;
- a response: an integer `id` in flight, no `method` or `params`, and
  exactly one of `result` or an `error` object with an integer `code` and a
  string `message`.

Anything else faults the transport. A frame the engine writes is serialized
into a writer that faults at 131,071 bytes, before allocating past them.

**One consumer, in wire order.** There is no reader task or engine queue:
one consumer handles each frame from `from_codex` before the next, never
awaiting an RPC or a lock held across one (the thread's send lock).
- A reply's effect on admission (the thread `thread/start` returned, the
  turn `turn/start` returned) is applied before its caller completes. At the
  pin each reply came just before its `thread/started` or `turn/started`.
- A turn is admitted only from its reply, and not if it has completed, or
  the thread closed, by then. A turn notification on the held thread naming
  a turn other than the admitted one, the last ended one, or (with a
  `turn/start` in flight) the first new one faults, as does a reply naming
  another than that first new one.
- A caller that gives up leaves its id pending until the reply, then
  discarded; at most one per operation.
- `thread/closed` revokes admission for the rest of the session.

**Close.** On the wire, close is the end of `to_codex`: the engine drops its
only sender, and Codex exits at the end of its input (at the pin, exit 0
within 10 ms, mid-turn). No RPC comes first: `thread/archive` refuses an
ephemeral thread ("no rollout found"), and `thread/unsubscribe` leaves it
loaded, still taking turns. The host waits for Codex to exit, as now.

Closing is synchronous and awaits nothing, on either runtime flavour. Under
the publication lock it marks the engine closed, so nothing more is
published or written; it takes `to_codex` from its slot (before `connect`,
the whole `HostTransport`), fails every waiter (a waiting `connect` returns
`Closed`), aborts the consumer and the handlers, and puts the terminal event
in its own slot: `Codex { status: Stopped }`, or `Failed { message }`. The
faults: an invalid frame, a charge past the budget, the end of `from_codex`,
and a full or closed `to_codex` at a `try_send` (every send is one), so an
idle close is noticed at the next send. Since aborting only schedules
cancellation, `finished()` resolves once every task has ended and every
charge is released, unread events included (released as the host takes
them or drops `EmbeddedEvents`); the host keeps its reservation until then.

**Memory.** Every byte the engine holds is charged against one budget before
it is allocated, at SWBPIPE's prices: each allocation at its class (the next
power of two, at least 16 bytes), old plus new while one grows. A charge
past the budget faults the transport instead. The budget is
`embedded_max_bytes(&limits)`:

| Charge | Bound | SWBPIPE |
| --- | --- | --- |
| The frame in hand (a capacity over 128 KiB faults) and, before decoding, 32 × it for what it decodes to | 33 × 128 KiB | 4,224 KiB |
| Admitted calls' arguments, as raw text | `pending_calls` × class(`argument_bytes`) | 512 KiB |
| The turn's RPC ids and `callId`s | `calls_per_turn` × 576 B | 576 KiB |
| Events not yet received | `event_bytes` | 1,024 KiB (theirs to set) |
| The frame being written, and the send in flight | 2 × 128 KiB | 256 KiB |
| Fixed: tasks, channels, the terminal slot, the thread, the turn | 1 MiB | 1,024 KiB |
| **Sum** | | **7,616 KiB** (7,798,784 bytes) |

- What a frame produces and the engine keeps (a call's arguments, an event,
  a reply awaiting its caller, an identity) moves to its own row out of the
  frame's charge; the rest is released when decoding ends.
- Decoding builds no `serde_json::Value` (measured up to 109× its text) and
  no serde `Content`, so no internally tagged enum: items, statuses and
  inputs dispatch on `type` by peeking. Opaque parts (arguments, `source`,
  `sandbox`, request ids, unmapped items) stay raw (`serde_json`'s
  `raw_value`), and unknown fields are skipped unbuffered.
- **Why 32×.** A decoded vector's element takes at most 8 bytes per byte of
  its shortest text (a 24-byte `String` for `""` and its comma), since every
  struct field kept is required. Doubling, class and old-plus-new add 2 ×
  4/3 × 1.5: 32 in all. Step 1's counting-allocator test checks every decode
  path on adversarial frames.
- Not charged: the host's own allocations, its handler's included.
- Gone: the broadcast ring (`subscribe` gives a closed receiver), history,
  listings, account, models, files, and `Lagged` (a full stream faults).

**Stop.** `interrupt` waits for the turn to end on a `watch` of the store's
sequence number (bumped on every change, closed on teardown), on both kinds
of engine, instead of a subscription. A close ends the wait with `Closed`.
The Stop-then-send test runs on both.

**The one rule.** Every request from Codex counts toward `calls_per_turn`,
and its RPC id and `callId` go in the turn's record before dispatch (cleared
when the next send starts a turn); a repeat, or a request past the count,
faults. The application gets an `item/tool/call` only if it has
`arguments`, names one of its tools, is in the admitted turn on the held
thread, has arguments within `argument_bytes`, and fewer than
`pending_calls` are outstanding. Any other request is refused: the engine
publishes

```rust
Refused { thread: Option<String>, method: String, json: String }, // json: the request's params
```

then writes `{"id": …, "error": {"code": -32601, "message": "not supported by
this client"}}`. Only the application's call gets a `result`
(`Taken::Refused` becomes this refusal); schema checks stay in the handler.
The test sends each kind of request and checks that `Refused` is published
before the answer, which has `error.code == -32601` and no `result`.
`thread`, here and below, is from `threadId`, `thread.id` or
`conversationId`.

Assignment is off at each place it is coupled in: `tools::specs` and
`tools::on_tool_call`; `mapping::{item, turn, user_message}`,
`title_from_preview` and `threads::{list_turns, list_pages}`; in `events.rs`,
the `item/started` mark, the `turn/completed` reports, `take_thread`,
`adopt_subagent` and `thread_live` (which would clear a closing); and in
`turns.rs`, marks, assignments and deliveries.

**The thread.**

```rust
pub async fn open_thread(&self, options: ThreadOptions) -> Result<OpenedThread>;

pub struct ThreadOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub base_instructions: Option<String>,
    pub policy: ThreadPolicy,
    pub raw_response_items: bool, // `experimentalRawEvents`, published as `RawResponseItem`
}
pub struct ThreadPolicy { pub sandbox: SandboxMode, pub approval: ApprovalPolicy }
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }
pub enum ApprovalPolicy { Untrusted, OnRequest, Never }

pub struct OpenedThread {
    pub thread: ThreadId,
    pub approval_policy: Option<ApprovalPolicy>, // None: Codex's granular policy
    pub sandbox: ReportedSandbox,
    pub ephemeral: bool,
    pub environments: Option<Vec<ThreadEnvironment>>, // None: Codex does not say
}
/// `network_access`: `Some` only when Codex says.
pub struct ReportedSandbox { pub kind: String, pub network_access: Option<bool> }
pub struct ThreadEnvironment { pub environment_id: String, pub cwd: PathBuf, pub runtime_workspace_roots: Vec<PathBuf> }
```

- `thread/start` carries `cwd`, `ephemeral: true`, `environments: []`,
  `dynamicTools`, the policy and the options. A second `open_thread` is
  `Invalid`; a cancelled one still uses up the thread. After
  `thread/closed`, `send` is `Invalid`.
- The first turn is the application's `send`, once it has checked the answer
  and seen remote control disabled. Turns name no policy, so Codex keeps the
  thread's; their access is never recorded (`settings` `None`, no
  `starting`, `access_known` false).

**Remote control.**

```rust
#[serde(rename_all = "camelCase")]
pub enum RemoteControl { Disabled, Connecting, Connected, Errored, Other } // Other: unknown or malformed
// Snapshot, now #[serde(rename_all = "camelCase")]
#[serde(default, skip_serializing_if = "Option::is_none")] #[ts(optional)]
pub remote_control: Option<RemoteControl>,
// Event; None when Codex (re)starts. `reduce.ts` removes `remoteControl` on null.
RemoteControl { status: Option<RemoteControl> },
```

The host sends nothing until it has seen `Disabled`. An ordinary engine's
`connect` handles what is queued once its last start reply is in, so its
snapshot holds the status too; the recordings are regenerated once.

**What each frame becomes**: the first of these that applies, and only it.
1. A fault: an invalid frame, a repeated id or `callId`, a request past the count.
2. A response: its caller.
3. An admitted `item/tool/call`: the handler.
4. Any other request: `Refused`.
5. A notification naming a thread not held: `Unheld { thread, method, json }`.
6. With `raw_response_items`, `rawResponseItem/completed`: `RawResponseItem { thread: ThreadId, turn: TurnId, json }`.
7. A notification the engine maps on the held thread, or `remoteControl/status/changed`: its events, at least one.
8. `Notice { thread: Option<ThreadId>, notice }`, below.
9. Anything else, including a notification that does not decode, an item with no `id`, and a mapping that yields no event: `Unmapped { method, thread: Option<String>, json }`.

```rust
pub enum Notice {
    ConfigWarning { summary: String, details: Option<String> }, // configWarning
    Warning { message: String },                                 // warning
    Retrying { message: String },                                // error with willRetry
    Status { status: String },                                   // thread/status/changed: its type
    TokenUsage { json: String },                                 // thread/tokenUsage/updated
    RateLimits { json: String },                                 // account/rateLimits/updated
}
```

An embedded engine maps messages, reasoning, the plan and the application's
calls; any other item, and on either engine one that does not decode, is
`ItemBody::Other { codex_type, json }`, kept raw. Items carry `codex_type:
Option<String>` (`None` for items the engine makes) and `AgentMessage`
`delivery` (`"async"`), both optional and omitted when absent.

**SWBPIPE's rule**, which the host applies: `Notice::Status`, `TokenUsage`
and `RateLimits` don't end the session; a warning ends it unless its text is
in the closed set recorded with the fixture (empty under the recorded
launch), and the code-mode-host warning always does; everything else ends
it. A test runs a whole turn recorded under that launch (startup, an
application call, a text answer, usage, limits, statuses) through the rule,
expects no end, then checks each ending case.

**The application's views of items** (`packages/ui`).

```tsx
export const ItemView = createContext<((item: Item) => ReactNode | null) | undefined>(undefined); // from the package root
itemView?: (item: Item) => ReactNode | null; // StandardLayoutProps
```

- `TurnView` calls the view once per item per render; an answer `!= null`
  is a node. Items that show nothing (an `Other` with no node; an item whose
  view gives none and which has no standard view) are dropped before
  grouping, and the live summary and Working follow the last visible block.
- An item with a node is its own block, keyed `item:${id}`, with the work on
  either side still folded (keyed `work:${id}`); the split follows the
  view's answer as it changes.
- Tests: a middle call changing from `null` to a node; no provider; a
  trailing invisible item.

The engine does not check Codex's answers or the home's settings, end a
session, hold the first turn, or add a permission model.

**Established at the pin (0.171.0).** Scratch homes, plugins off, a loopback
stub provider, `timeout`, no credential (the relay variable held a dummy),
nothing left running.

- **The exact launch** is SWBPIPE's, rebuilt from its source
  (`embedded_agent/{macos,sandbox}.rs`) with `--strict-config`: its root
  layout and `<root>/codex`, its Seatbelt recipe (`deny default`, `deny
  process-fork`, TCP only to the stub), its ten variables and its file.
  Codex ran turns and exited 0. Seatbelt logged no denied fork or exec, which
  shows none was attempted, not that one would be harmless.
- **The recorded configuration** is SWBPIPE's file with `model_catalog_json`
  (a catalogue of one entry, replacing the bundled list), `[agents] enabled =
  false` in place of `multi_agent = false`, and `[skills] include_instructions
  = false` and `[skills.bundled] enabled = false`. An entry without any of
  `slug`, `display_name`, `supported_reasoning_levels`, `shell_type`,
  `visibility`, `supported_in_api`, `priority`, `support_verbosity`,
  `truncation_policy`, `experimental_supported_tools` or `base_instructions`
  stops Codex before it initialises. The recorded entry adds `tool_mode:
  "direct"` and `apply_patch_tool_type: null`.
- **It offers the namespace only.** With SWBPIPE's seven declarations, for
  `gpt-6.2-sol` (bundled) and `swbpipe-relay-model` (not), every request's
  only tool was `swbpipe` with the seven; Codex sent no warning and listed no
  skills. `apply_patch_tool_type: "freeform"` adds `apply_patch`, and
  `tool_mode: "code_mode"` adds the code-mode-host warning. SWBPIPE's file as
  it is warns that `[features].multi_agent` is deprecated and lists skills.
  Recorded, with the file and the entry, in
  `crates/engine/fixtures/codex/swbpipe-namespace-offer.json` (sha256
  `d35430e127d172f9daf2f0b600d1930bccaac39b65fb2f9e5b90c4d896b32868`), with
  `builtIns: []` and `warnings: []`: the relay compares a request's other
  tools with `builtIns`, and the namespace's names and descriptions with its
  catalogue. Code-mode results are in this PR's history.
- **Codex reduces the schemas** it sends: it drops length, range, pattern
  and item-count bounds, turns `const` into `enum`, and replaces some nested
  schemas with `{}` (`swbpipe_propose_operations`: 6,904 bytes declared,
  1,854 sent). The host's own check of the arguments is what holds them.
- **A turn** also sends two status changes, token usage and rate limits.
- **Remote control: the variable's effect is unverified.** Without ChatGPT
  sign-in, Codex sends `remoteControl/status/changed` `"disabled"` just after
  its `initialize` reply, with the variable and without it.
- `--strict-config` exits 1 on an unknown key or malformed TOML (without
  it, Codex starts on its defaults). A thread's policy held over the home's
  for turns naming none.

**Not observed:** the variable's effect (it needs a sign-in); a
`thread/closed` from Codex; the `imageView` and `imageGeneration` items.

## Known limits

- Assignment: a thread started before the engine had its tools (or by another client) cannot assign, message or read; Codex tells its agent the call is unsupported. A turn whose start the engine did not see (another client's) reports only if its thread is assigned. A subagent's task is not in its history, so a read of one starts at its work. A `read` cursor whose turn is gone (the thread reverted under it) is refused. A report not made is said in the turn only until the thread is read again (Codex does not keep it). A project removed and added back while a report's recipient is being resumed for it can still receive that report. A mentioned thread's title and project are those the engine knew when the message was sent. A delivery waiting when the engine stops is not made, and a turn that ends because Codex stopped reports nothing. The sender's title in a mark is the one the engine knew when it sent it (none for a thread Codex has not yet titled); the interface shows the sender's current title. A message the person writes that begins with a mark is shown as from that thread; its turn reports only if that thread assigned this one. After a restart, a close is found by reading the assigner's history, once per report.
- A legacy-history exchange can stay doubled in a view the person switches away from while its read is pending, and can show doubled briefly before the completion read returns.
- Text or a plan that streamed before a successful read whose history does not yet hold it (a thread opened mid-reply, a follow-up sent before a completion read returns) is missing from the view until its item completes, and stays missing if the person stops the turn first.
- A Stop sent after a Stop that timed out and a send that went to Codex as `turn/start` can name the old turn.
- Threads in Codex's older (legacy) history format are not supported.
- The engine ends only the Codex process it started: what Codex starts (a plugins fetch; `codex-code-mode-host`, which Codex puts in a process group of its own) can outlive it. A later change starts Codex in a group of its own and kills the group while Codex is still unreaped (so its id cannot have been reused), on shutdown and when Codex dies; a child in a group of its own still outlives it.
- Codex's refusals (of a thread it no longer has, or a turn it does not know) reach the person in Codex's words.
- A draft edited in a just-created thread while its first send is still being accepted can be replaced by the moved draft, and an attachment picked across that moment can land in the project's next new thread.
- A project's thread listing in flight when a thread is archived can list it again until the next listing.
- Subagents (Codex 0.171.0, multi-agent v2): the person cannot message one (Codex refuses), and one cannot ask the person a question (Codex refuses it to the agent). The task and messages an agent gives a subagent are in neither thread's history. Stopping a subagent does not tell its parent, and stopping the parent does not stop its subagents. The nickname is known once Codex lists the subagent. A subagent's settings shown are those of the turn that started it. A subagent is told not to start subagents of its own, but nothing stops one that does. A fork has no subagents, and a revert leaves the reverted turn's subagents listed. After Codex restarts, a subagent is read but runs again only once its parent is resumed. Live, an agent's wait for a subagent comes before the subagent's finish; read again, after it.

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
