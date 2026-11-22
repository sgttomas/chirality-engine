# Embedding under containment

Part of the engine's design (`SPEC.md`). Designed 2026-11-22 for SWBPIPE, revised after three rounds of review; not yet
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
