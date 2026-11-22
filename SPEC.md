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
give it another home, or launch Codex itself (an embedded engine, below).

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
- **Embedding:** an application that contains the agent itself starts an embedded engine: Codex launched exactly as it says, its own tools only, one session; see below.
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

Designed 2026-11-22 for SWBPIPE; not yet built.

SWBPIPE is a desktop application that runs the agent under strict
containment: one Codex process per agent session, with one ephemeral thread,
in a private home whose provider the person configures, with no shell, file,
patch or web capability, remote control off, and any built-in tool activity
ending the session. It keeps its own sandbox, process scope, supplier pin
check, provider store, relay, proposal review and receipts, and uses the
engine's core crate directly, without the Tauri adapter. The engine gives it
what follows. It is opt-in: an engine made with `Engine::new` and used as
before behaves as it does now for the person. It reports more (remote
control, requests it refused, items it does not map), which the interface
shows only where an application asks; and ending it ends what Codex started
too (below).

**An embedded engine.** `Engine::embedded(config)` makes an engine, before
Codex starts, as `Engine::new` does; `connect` starts it.

```rust
pub struct EmbeddedConfig {
    pub launch: LaunchSpec,
    /// Where the engine keeps its projects list.
    pub state_dir: PathBuf,
    /// The only tools the agent has, besides what Codex's own settings leave it.
    pub tools: Option<AppTools>,
}

/// The whole command that runs `codex app-server` over stdio.
pub struct LaunchSpec {
    /// The program run, such as `/usr/bin/sandbox-exec`.
    pub program: PathBuf,
    /// Every argument, in order.
    pub args: Vec<OsString>,
    /// The whole environment; nothing is inherited.
    pub env: Vec<(OsString, OsString)>,
    /// The working directory.
    pub cwd: PathBuf,
}
```

- Codex is started from the launch spec and nothing else: the program, its
  arguments as given (SWBPIPE's are `-f <root>/sandbox.sb <root>/codex
  app-server --listen stdio://`), the environment cleared and set to exactly
  the one given, and the working directory. The engine adds no `-c` setting,
  no argument, no `CODEX_HOME` and no `PATH` entry. It still speaks over the
  process's stdin and stdout, and drains stderr.
- `codex_bin`, `codex_home` and `codex_settings` have no part in it: the
  program and arguments replace the binary and the `-c` settings, the
  environment names the home, and Codex's settings are in that home's
  `config.toml`. Checking the binary is the application's.
- The threads carry only the application's tools: no `chirality` tools, so
  no assignment, messages, reports or reads between threads; the engine
  starts no turn of its own and reads no history for a report. With no tools,
  `thread/start` names none.
- No subagent settings: neither `features.multi_agent_v2_dynamic_tools`, the
  subagent limits and hint, nor `features.default_mode_request_user_input`.
  Whether the agent has subagents or questions is the home's settings'.
- One Codex: when it ends, the engine does not start another. Its status is
  failed and stays so, and every call after says Codex is not running.
- Sign-in through Codex is refused (`EngineError::Invalid`); the provider is
  the application's.
- What the engine sends Codex by itself is its start (`initialize`,
  `initialized`, `account/read`, `model/list`, `thread/list` for each
  project) and what the application's calls ask; never a `remoteControl/*`
  request.

The two belong together: the engine's own tools need its subagent settings,
which a launch spec does not carry, and an application that launches Codex
itself is one that decides everything the agent can do. They can be parted
later if an application needs one without the other.

**A thread with options, and what Codex says of it.** `Engine::open_thread`
starts a thread in a project without a first message, with what `thread/start`
takes beside the folder, and gives Codex's answer. The first turn is the
application's `send`, made when it is ready (after it has checked the answer,
and seen remote control disabled). It is on every engine.

```rust
pub async fn open_thread(&self, project: &ProjectId, options: ThreadOptions) -> Result<OpenedThread>;

#[derive(Clone, Debug, Default)]
pub struct ThreadOptions {
    pub model: Option<String>,
    /// A provider the Codex home names (`model_providers`).
    pub provider: Option<String>,
    /// In place of Codex's own base instructions.
    pub base_instructions: Option<String>,
    /// Codex keeps nothing of the thread once it ends.
    pub ephemeral: bool,
    /// Codex's sandbox and approval policy for the thread; its turns then send none.
    pub policy: Option<ThreadPolicy>,
    /// `Some(vec![])` gives the thread no environment; `None` leaves Codex's default.
    pub environments: Option<Vec<ThreadEnvironment>>,
}

pub struct ThreadPolicy { pub sandbox: SandboxMode, pub approval: ApprovalPolicy }
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }
pub enum ApprovalPolicy { Untrusted, OnFailure, OnRequest, Never }
pub struct ThreadEnvironment { pub id: String, pub cwd: PathBuf }

/// Codex's answer to `thread/start`.
pub struct OpenedThread {
    pub thread: ThreadId,
    /// `None` for a policy that is none of the four (Codex's granular one).
    pub approval_policy: Option<ApprovalPolicy>,
    pub sandbox: ReportedSandbox,
    pub ephemeral: bool,
    /// `None` when Codex did not say.
    pub environments: Option<Vec<ThreadEnvironment>>,
}

/// The sandbox Codex reports: its type as Codex names it (`readOnly`,
/// `workspaceWrite`, `externalSandbox`, `dangerFullAccess`) and whether it
/// lets the network through.
pub struct ReportedSandbox { pub kind: String, pub network_access: bool }
```

- The thread is in the snapshot from its start, idle, and its first `send`
  starts its first turn. A thread whose first turn never comes is not kept
  by Codex, as now.
- A thread opened with a policy has Codex's policy, not the engine's access
  levels: its turns send no approval policy, sandbox or reviewer (Codex keeps
  the thread's for every turn that names none), and `TurnSettings::access` is
  ignored there. Approvals and questions are as Codex's policy makes them
  (with `never`, none come). The model and reasoning level still go with each
  turn. The engine knows a thread has its own policy for as long as it
  runs; after an ordinary engine restarts, the thread's turns send an access
  level again.
- An ephemeral thread has no history in Codex: it is read as a thread Codex
  has not yet kept (no turns, numbered from before it opened) until its first
  turn ends, and after that a read is refused; a view keeps what the events
  made. Codex's listing leaves it out.
- `start_thread` is unchanged: a new thread with the access of its first
  turn and the engine's tools.

**Remote control.** Codex says whether remote control is on.

```rust
pub enum RemoteControl { Disabled, Connecting, Connected, Errored, Other }

// in Snapshot: None until Codex says, again from each start of Codex
#[ts(optional)]
pub remote_control: Option<RemoteControl>,

// in Event
RemoteControl { status: RemoteControl },
```

The hold is the application's: it sends nothing until it has seen
`Disabled`, from the snapshot or the event. Codex says it once, as it answers
`initialize`, so an application that subscribes before `connect` sees it, and
the snapshot has it once the engine has read it. The engine does nothing more:
an embedded engine starts no turn of its own, so nothing goes to the model
before the application's first send.

`remoteControl/enable` and `remoteControl/pairing/start` are requests a client
sends Codex, not ones Codex sends a client (at 0.171.0 they are in the
client's requests, and Codex's own requests are approvals, questions, tool
calls, an MCP elicitation, a ChatGPT token refresh and an attestation). The
engine never sends them. What it can expose is any request from Codex it does
not take, which it already answers as not supported:

```rust
// in Event
Refused { thread: Option<ThreadId>, method: String },
```

so an application can count any such request as a failure.

**Built-in activity, shown.** Every item Codex reports is an item of the
thread. One the engine does not map (its kind unknown to the engine, its
shape not what the engine reads, or an activity or subagent tool it does not
name) is

```rust
// in ItemBody
/// An item the engine does not map: Codex's kind and the item as Codex sent it, as JSON.
Other { kind: String, json: String },
```

emitted as any item is, live and in a read. The interface shows nothing for
it unless the application's item view does (below). SWBPIPE's tripwire kinds
arrive as:

| Codex's kind | Item body |
| --- | --- |
| `commandExecution` | `Command` |
| `fileChange` | `FileChange` |
| `mcpToolCall` | `ToolCall` with `server` set |
| `collabAgentToolCall` | `SubagentTool`; one the engine does not name, `Other` |
| `subAgentActivity` | `Subagent`; one the engine does not name, `Other` |
| `webSearch` | `WebSearch` |
| `imageView`, `sleep`, `imageGeneration` | `Other` |

An application's own tools are `ToolCall` with its namespace as `server`; the
engine's are `ThreadTool` (none in an embedded engine). Items of a thread the
engine does not hold (not in a project, not a subagent of one) are not
emitted, as now.

**One engine per session, and its end.** An embedded engine is one session:
the application makes one, connects it, and drops it or shuts it down when
the session ends.

- Today `shutdown` closes Codex's input, gives it two seconds, then kills it,
  and returns once it is reaped, waiting at most five seconds for that. Only
  the process the engine started is ended: one Codex started (such as a git
  fetch of plugins) goes on after Codex has gone. Dropping every `Engine`
  ends nothing: the engine's own tasks hold it, and Codex runs until the
  runtime ends.
- Codex is started in a process group of its own. Shutdown, once Codex has
  ended (or been killed), kills what is left of the group, and returns once
  Codex is reaped. A process that left the group is not followed; under a
  launch spec, the application's sandbox bounds it.
- Dropping the application's last `Engine` handle does the same at once:
  every process in the group is killed, and drop returns once Codex is
  reaped, which after the kill takes milliseconds. The engine's tasks hold
  their part of it, not a handle, so they keep nothing alive.
- These hold for every engine. An application that shuts the engine down
  before it exits, as both current ones do, sees no difference.

**The application's views of items** (`packages/ui`). An application can
show an item its own way:

```tsx
export const ItemView = createContext<((item: Item) => ReactNode | null) | undefined>(undefined);
// StandardLayoutProps
itemView?: (item: Item) => ReactNode | null;
```

`Turns` asks the view for each item; what it gives stands in the turn in the
item's place, outside the folded work; on `null`, the item shows as it does
now (an `Other` item, not at all). The view is called as a function; the
components it returns may use hooks. `StandardLayout` provides it from
`itemView`; an application composing `Turns` itself provides `ItemView`, as
it does `FeaturedWorkflows`.

**What the engine does not do.** It does not check the launch spec, the
binary, the home's settings or Codex's answer to `thread/start`: SWBPIPE
checks those against its own policy, and the engine passes on what Codex says
without judging it. It does not end a session on built-in activity: it shows
the activity, and the application ends the session by dropping the engine.
It does not hold the first turn for remote control: the application does not
send. It adds no permission model: a thread opened with a policy has Codex's.

**Established at the pin (0.171.0).** Observed with the pinned binary over
stdio, launched through `/usr/bin/sandbox-exec -f <profile>` with an exact
environment, a scratch home with SWBPIPE's `config.toml`, and a loopback stub
for its provider; no account, no model, no network.

- The launch: `sandbox-exec` runs Codex in its own place (one process, no
  child), and stdio works through it.
- Remote control: Codex sends `remoteControl/status/changed` once, with
  `status: "disabled"` (and `serverName`, `installationId`,
  `environmentId`), as it answers `initialize`, before any other request is
  answered; `remoteControl/status/read` agrees. That is so with
  `CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED=1` and without it, with
  no ChatGPT sign-in. Started with `--remote-control`, Codex says `"errored"`
  with the variable and without it. The variable is still in the binary, but
  its effect could not be seen without a ChatGPT sign-in (not made). A
  `requirements.toml` with `allow_remote_control = false` in the home did not
  change `"errored"` either. `remoteControl/enable` is refused (`remote control
  requires ChatGPT authentication`), and `remoteControl/pairing/start` too
  (`requires remote control to be enabled`).
- SWBPIPE's settings: every key parses (`--strict-config
  --print-startup-config` accepts the file; a misspelled feature fails it).
  Codex warns that `[features].multi_agent` is deprecated. With a model Codex
  has no metadata for, the model is offered only the application's namespace:
  `[features]` and `web_search = "disabled"` remove `exec_command`,
  `write_stdin`, `view_image`, the goal tools, the multi-agent v1 tools and
  `web_search` that the same provider without them offers, and
  `request_user_input` goes too. Plugins off, no git fetch of
  `openai/plugins` starts (with them on, one does as Codex starts).
  The provider's `env_key` is sent as the bearer token;
  `requires_openai_auth = false` leaves `account/read` with no account and
  `requiresOpenaiAuth: false`. `model/list` answers from Codex's bundled
  catalogue and asks the provider nothing.
- But a model Codex has metadata for (its bundled `gpt-6.2-sol`) chooses its
  own tools: with SWBPIPE's settings it is offered a code-mode `exec` (whose
  nested tools are `apply_patch`, the MCP resource tools, `clock` and the
  application's), `wait`, `send_message_to_user_async`, `clock.sleep`, and
  multi-agent v2 (`collaboration.spawn_agent` and the rest). `[agents]
  enabled = false` removes the multi-agent tools. Code mode fails closed
  without its host (Codex warns so), so the application's tools, reachable
  only inside `exec`, cannot be called. A model's catalogue entry decides
  this before `[features]` does.
- `thread/start` takes `ephemeral`, `baseInstructions`, `environments: []`,
  `sandbox: "read-only"`, `approvalPolicy: "never"`, `modelProvider` and
  `dynamicTools` (`environments` and `dynamicTools` need the experimental
  API, which the engine declares). It answers `approvalPolicy: "never"`, `sandbox: {type:
  "readOnly", networkAccess: false}`, `thread.ephemeral: true` and
  `thread.environments: []`. The base instructions go to the model in place
  of Codex's own; the skills Codex installs in the home are still listed to
  it.
- An ephemeral thread: `thread/read` with turns and `thread/turns/list` are
  refused (`ephemeral threads do not support ...`), and `thread/list` leaves
  it out; its turn runs and is told of as any other.
- A `turn/start` naming no approval policy or sandbox keeps the thread's
  (Codex: they apply "for this turn and subsequent turns").
- Closing Codex's input ends it at once (exit 0) under these settings, with
  nothing left. Under the default settings, the plugins fetch Codex started
  (git and its helpers) went on after Codex had ended.

**Not observed.** Whether the remote-control variable is honoured with a
ChatGPT sign-in, and whether a catalogue (`model_catalog_json`) can give a
known model direct tools: both need what this verification did not use (a
sign-in, a real model's behaviour). How `imageView`, `sleep` and
`imageGeneration` items look on the wire was read from the protocol's
schema, not seen.

## Known limits

- Assignment: a thread started before the engine had its tools (or by another client) cannot assign, message or read; Codex tells its agent the call is unsupported. A turn whose start the engine did not see (another client's) reports only if its thread is assigned. A subagent's task is not in its history, so a read of one starts at its work. A `read` cursor whose turn is gone (the thread reverted under it) is refused. A report not made is said in the turn only until the thread is read again (Codex does not keep it). A project removed and added back while a report's recipient is being resumed for it can still receive that report. A mentioned thread's title and project are those the engine knew when the message was sent. A delivery waiting when the engine stops is not made, and a turn that ends because Codex stopped reports nothing. The sender's title in a mark is the one the engine knew when it sent it (none for a thread Codex has not yet titled); the interface shows the sender's current title. A message the person writes that begins with a mark is shown as from that thread; its turn reports only if that thread assigned this one. After a restart, a close is found by reading the assigner's history, once per report.
- A legacy-history exchange can stay doubled in a view the person switches away from while its read is pending, and can show doubled briefly before the completion read returns.
- Text or a plan that streamed before a successful read whose history does not yet hold it (a thread opened mid-reply, a follow-up sent before a completion read returns) is missing from the view until its item completes, and stays missing if the person stops the turn first.
- A Stop sent after a Stop that timed out and a send that went to Codex as `turn/start` can name the old turn.
- Threads in Codex's older (legacy) history format are not supported.
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
