# Embedding under containment

Part of the engine's design (`SPEC.md`). Designed 2026-11-22 for SWBPIPE,
revised after five rounds of review. Step 1 (the core) is built; calls
(`calls.rs`) are not yet.

SWBPIPE runs one Codex per session, with one ephemeral thread, no built-in
capability, and remote control off; any built-in activity ends the session.
Its host spawns, frames, observes and reaps Codex. The engine speaks JSON-RPC
over the host's transport.

**A separate engine.** `EmbeddedEngine` lives in `crates/engine/src/embedded/`:
- `mod.rs`: the handle and its methods;
- `types.rs`: the public types;
- `budget.rs`: charges, guards and `max_bytes`;
- `wire.rs`: the pre-scan, the visitors and the writer;
- `consumer.rs`: routes and turn rules;
- `calls.rs`: the record, admission and answers.

Each file stays under 600 lines. The engine shares none of the ordinary
engine's structure (`Inner`, the store, `turns`, `calls`, the broadcast,
`AppTools`, `Snapshot`, `Event`). It also does not use `protocol.rs`, whose
types build `Value`s and tagged-enum buffers. The ordinary engine and
`packages/ui` are untouched.

```rust
impl EmbeddedEngine { // not Clone; dropping it closes
    pub fn new(config: EmbeddedConfig) -> Result<(EmbeddedEngine, EmbeddedEvents), EmbeddedError>;
    pub const fn max_bytes(limits: &EmbeddedLimits) -> u64; // the host reserves this before `new`
    pub async fn connect(&self) -> Result<(), EmbeddedError>;
    pub async fn open_thread(&self, options: &ThreadOptions) -> Result<OpenedThread, EmbeddedError>;
    pub async fn send_text(&self, text: &str) -> Result<TurnId, EmbeddedError>;
    pub async fn interrupt(&self) -> Result<(), EmbeddedError>;
    pub fn call_live(&self, call: &CallRef) -> bool;
    pub fn answer(&self, call: &CallRef, success: bool, text: &str) -> Result<(), EmbeddedError>;
    pub fn remote_control(&self) -> Option<RemoteControl>;
    pub fn finished(&self) -> Finished; // Future<Output = ()> + Clone + Send + 'static
    pub fn close(&self);                 // idempotent; the first fault wins
}
impl EmbeddedEvents {
    pub async fn recv(&mut self) -> Option<EmbeddedEvent>; // None after `Closed`
    pub fn try_recv(&mut self) -> Result<EmbeddedEvent, tokio::sync::mpsc::error::TryRecvError>;
}
pub struct EmbeddedConfig {
    pub transport: HostTransport,
    pub work_dir: PathBuf,
    pub namespace: String,
    pub namespace_description: String, // SWBPIPE's: "SWBPIPE tools."
    pub tools: String,                 // the namespace's `tools` array as JSON, sent as is
    pub limits: EmbeddedLimits,
}
pub struct HostTransport { // one message per frame, no newline
    pub from_codex: tokio::sync::mpsc::Receiver<String>,
    pub to_codex: tokio::sync::mpsc::Sender<String>,
}
pub struct EmbeddedLimits {   // `new` refuses values outside these ranges
    pub pending_calls: u32,   // 1..=16
    pub calls_per_turn: u32,  // 1..=1,024 requests from Codex, refused ones included
    pub argument_bytes: u32,  // 1..=65,536 of a call's raw arguments
    pub result_bytes: u32,    // 1..=65,536 of an answer's text, in UTF-8
    pub event_bytes: u32,     // 256 KiB..=16 MiB of unread events' strings
}
pub enum EmbeddedError {
    Invalid { reason: &'static str },     // nothing was written, and nothing changed
    Codex { code: i64, message: String }, // a message over 1 KiB faults
    Closed { fault: Option<Fault> },
}
pub enum Fault { InvalidFrame, RepeatedId, TooManyRequests, WrongTurn, OverBudget, InputEnded, OutputClosed, OutputFull }
```

`new` validates its input and keeps nothing it refuses:
- `namespace` matches `^[a-zA-Z0-9_-]+$`, is at most 64 bytes, and is not
  `mcp`, `mcp__…` or one of Codex's fourteen reserved names.
- `namespace_description` is at most 1,024 characters.
- `tools` is a JSON array of 1 to 64 objects. Each `name` matches the same
  pattern, is at most 128 bytes, is unique, and is not `mcp…`. The array is
  read with the visitors below.
- `work_dir` is absolute, UTF-8, non-empty and at most 1,024 bytes.
- Every string's capacity fits its bound (`tools` 128 KiB, the description
  4 KiB), so a short string with a large capacity is refused.

**Frames.** The host owns line framing, duplicate keys and numeric fidelity.
Its 128 KiB includes the newline both ways, so a frame's text is at most
131,071 bytes.

A valid frame is one JSON object, nested at most 127 deep, that is exactly
one of these:
- a request: an `id` that is an integer or an identity, a `method` that is an
  identity, optional object `params`, and no `result` or `error`;
- a notification: a `method` that is an identity, optional object `params`,
  and no `id`, `result` or `error`;
- a response: an integer `id` in flight, no `method` or `params`, and exactly
  one of `result` or an `error` with an integer `code` and a string
  `message`.

Other members are ignored, and anything else faults. An identity is a
non-empty string of at most 256 bytes, unescaped, with no control
characters, as SWBPIPE defines it. RPC ids, `callId`s, and thread, turn and
item ids are identities: Codex passes a provider's `callId` through, and one
with `"` or `\` arrives escaped.

**Decoding** never unescapes through serde_json, and never uses
`#[derive(Deserialize)]` on a frame.
1. A pre-scan, allocation-free, checks the length and the depth. `new` runs
   it on `tools` too.
2. Hand-written visitors read every key and value as `&RawValue`
   (`serde_json`'s `raw_value` feature, enabled in step 1).
3. A key is matched on its raw text, so an escaped key matches nothing.
4. An identity, `model`, `modelProvider`, `cwd` and an error's `message` are
   unescaped by the engine's own decoder into a fixed stack buffer (256
   bytes; 1,024 for `cwd` and `message`). The decoder handles `\"`, `\\`,
   `\/`, the five control escapes and `\u` pairs, and stops at the buffer's
   end. Only then is the string owned. A `message` may hold control
   characters; an identity may not.
5. Codex's own constants are taken only without escapes, since none
   contains `"`, `\` or a control character. These are `method`, `item.type`,
   `status.type`, `turn.status`, `approvalPolicy`, `sandbox.type` and remote
   control's `status`; an escaped or unknown one is `Other`.
6. Integers (`id`, `error.code`) are parsed from the raw text by the engine,
   not by serde_json, whose `float_roundtrip` feature (enabled in SWBPIPE's
   workspace) puts long digit runs in its scratch.
7. A string out of bound faults when it is a frame's, reply's or turn's
   required identity or an error's `message`, and is `Other` for a reported
   fact. A `callId` out of bound makes its call `Refused`, as below.

So serde_json's scratch holds only `ignore_value`'s nesting stack, at most
127 bytes. The engine writes through one writer: a buffer of 131,071 bytes,
allocated once, that refuses a frame past it without allocating.

**One consumer.** The engine's one task, spawned in `connect`, takes each
frame from `from_codex` and handles it before the next, never awaiting.
Methods run in their caller's task.
- Each method has one slot in the engine's state. A call holds the slot from
  its request until its future takes the reply, or until the future is
  dropped. The reply waits in the slot, not in a channel. So a method is in
  flight while its reply is uncollected, and a second call is `Invalid`.
- A caller that drops its future after the reply arrived frees the slot. One
  that drops it before keeps the method in flight: the slot holds the RPC id
  until the reply arrives, which is then discarded and frees the slot. So at
  most four RPC ids are ever pending, one per slot.
- **`connect`** writes `initialize` with `clientInfo` and `capabilities:
  {experimentalApi: true}`, which `thread/start`'s `environments` needs. The
  consumer writes `initialized` when it applies the reply. `connect` can be
  called once; a second call is `Invalid`, even after a cancelled first one.
  The same holds for `open_thread`.
- **Replies.** A reply's effect is applied before its caller completes:
  - the thread `thread/start` returned;
  - the turn `turn/start` returned, which must have an identity `turn.id`
    and `status: "inProgress"`, or the frame faults.
- **A turn** runs from its reply to its `turn/completed`. It is not admitted
  if `turn/completed` or `thread/closed` came first. A `turn/completed` still
  `inProgress` faults.
- **`send_text`** is `Invalid` unless all of these hold:
  - the latest `RemoteControl` is `Disabled`;
  - a thread is held and open;
  - no turn runs;
  - no call is pending;
  - no RPC is in flight.

  So there is no steering and no queue. A deadline for remote control is the
  host's; `remote_control()` shows the state.
- **`interrupt`** is `Invalid` unless a turn runs and no `turn/interrupt` is
  in flight. It returns on Codex's reply.

**Calls go through the stream.** The host answers them; there is no handler.
- **The record.** Every request counts toward `calls_per_turn`. Before
  dispatch, its RPC id and any valid `callId` go in the record, which is
  preallocated. A repeat, or a request past the count, faults, even outside a
  turn. A newly admitted turn clears the record.
- **Admission.** An `item/tool/call` becomes `Call` only if all of these
  hold:
  - its `callId` is an identity;
  - it names the namespace and one of its tools;
  - it is in the running turn on the held thread;
  - its `arguments` are present and within `argument_bytes`;
  - fewer than `pending_calls` are pending.
- **Refusal.** Any other request, including a call without a valid `callId`,
  becomes `Refused`. Then `{"id": …, "error": {"code": -32601, "message":
  "not supported by this client"}}` is written.
- **Answers.** `answer` writes `{"id": …, "result": {"contentItems":
  [{"type": "inputText", "text": …}], "success": …}}`.
  - It is `Invalid` if the text is over `result_bytes`, or the encoded frame
    is over 131,071 bytes; the call stays pending. A 65,515-byte text, for
    example, makes a 65,710-byte line and is written.
  - It is `Invalid` for a retired call, and writes nothing then.
- **Retirement.** A call is retired by its answer, or by any of these:
  - an `item/completed` of type `dynamicToolCall` whose `item.id` is its
    `callId`, in the running turn;
  - its turn's `turn/completed`;
  - `thread/closed`;
  - close.

  On interrupt, Codex completes the call's item as `failed` before it
  replies, and ignores a late answer.
- **The host's check.** It calls `call_live` when it dequeues a `Call`,
  before starting work. A call retired by then, for example by a Stop, is not
  live, so the host never starts work on it. Work already started may end in
  an `Invalid` answer.

Calls are events in wire order, so the host never answers one it reads after
an event that ends its session.

**Events.**

```rust
// All `Serialize` with `rename_all = "camelCase"`; `EmbeddedEvent` is tagged
// `kind` (`rename_all_fields = "camelCase"`), `RpcId` untagged.
pub enum EmbeddedEvent {
    RemoteControl { status: RemoteControl },
    Notification { method: Method, turn: Option<TurnId>, item: Option<ItemRef>, status: Option<String>, json: String },
    Call { call: CallRef, tool: String, arguments: String },
    Refused { method: String, thread: Option<String>, json: String },
    Unheld { method: String, thread: String, json: String },
    Unmapped { method: String, thread: Option<String>, json: String },
    Closed { fault: Option<Fault> },
}
pub struct CallRef { pub rpc_id: RpcId, pub call_id: String, pub turn: TurnId }
pub enum RpcId { Integer(i64), Text(String) }
pub struct ItemRef { pub id: String, pub item_type: Option<String> } // deltas carry no type
pub enum RemoteControl { Disabled, Connecting, Connected, Errored, Other }
pub enum Method {
    ThreadStarted, ThreadStatus, ThreadClosed, TokenUsage, RateLimits,
    TurnStarted, TurnCompleted, ItemStarted, ItemCompleted,
    AgentMessageDelta, ReasoningSummaryDelta, ReasoningSummaryPart, ReasoningDelta,
    Retrying, Error, Warning, ConfigWarning,
}
```

- `json` is `params` as sent, or `{}` when they are absent.
- `arguments` is their raw text.
- `method`, ids and `thread` are identities. `status` and `item_type` are at
  most 64 bytes.
- Only SWBPIPE's Rust host consumes these, so they have no ts-rs bindings.

Each frame from Codex gets the first of these that applies, and only it:
1. **A fault:** an invalid frame, a repeated RPC id or `callId`, or a request
   past the count.
2. **A response:** its caller.
3. **An admitted `item/tool/call`:** `Call`.
4. **Any other request:** `Refused`.
5. **A notification whose `threadId`, `thread.id` or `conversationId` is an
   identity other than the held thread's** (or any, before one is held):
   `Unheld`. A `null` is no thread.
6. **`remoteControl/status/changed` with a string `status`:** `RemoteControl`
   (`Other` for one the engine does not know).
7. **A turn notification on the held thread naming a turn the turn rule
   forbids:** a fault (`WrongTurn`). The turn rule admits only the running
   turn, the last ended one, or, with a `turn/start` in flight, the first new
   one.
8. **A method below whose fields decode, with the thread as required:**
   `Notification`.
9. **Anything else:** `Unmapped`.

| Method | From | Required besides `params` |
| --- | --- | --- |
| `ThreadStarted` | `thread/started` | `thread.id` = held |
| `ThreadClosed` | `thread/closed` (revokes the thread) | `threadId` = held |
| `ThreadStatus` | `thread/status/changed`, its only route (`idle` does not end the turn) | `threadId` = held, `status.type` |
| `TokenUsage` | `thread/tokenUsage/updated` | `threadId` = held, `turnId` |
| `RateLimits` | `account/rateLimits/updated` | none |
| `TurnStarted`, `TurnCompleted` | `turn/started`, `turn/completed` | `threadId` = held, `turn.id`, `turn.status` |
| `ItemStarted`, `ItemCompleted` | `item/started`, `item/completed` | `threadId` = held, `turnId`, `item.id`, `item.type` |
| the deltas | `item/agentMessage/delta`, `item/reasoning/{summaryTextDelta, summaryPartAdded, textDelta}` | `threadId` = held, `turnId`, `itemId` |
| `Retrying`, `Error` | `error`, by `willRetry` | `threadId` = held, `turnId` |
| `Warning`, `ConfigWarning` | `warning`, `configWarning` | `threadId` held, `null` or absent |

**SWBPIPE's rule**, which the host applies:
- **These don't end the session:**
  - `RemoteControl` `Disabled`;
  - `ThreadStarted`, `TurnStarted`, `TurnCompleted`;
  - `ThreadStatus` `active` or `idle`;
  - `ItemStarted` and `ItemCompleted` of `userMessage`, `agentMessage`,
    `reasoning` or `dynamicToolCall`;
  - the deltas;
  - `Call`;
  - `TokenUsage`, `RateLimits`, `Retrying`.
- **Exactly these end it:**
  - any other `RemoteControl` or `ThreadStatus` status;
  - `ThreadClosed`;
  - `Error`;
  - `Warning` and `ConfigWarning` (the recorded closed set is empty; the
    code-mode-host warning is a `Warning`);
  - any other item type;
  - `Refused`, `Unheld`, `Unmapped`, `Closed`.

**The thread.**

```rust
pub struct ThreadOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub base_instructions: Option<String>,
    pub policy: ThreadPolicy,
}
pub struct ThreadPolicy { pub sandbox: SandboxMode, pub approval: ApprovalPolicy }
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }
pub enum ApprovalPolicy { Untrusted, OnRequest, Never }

/// Each as Codex reported it in the `thread/start` reply.
pub struct OpenedThread {
    pub thread: ThreadId,                  // result.thread.id, an identity, else a fault
    pub approval_policy: Reported<String>, // result.approvalPolicy
    pub sandbox_type: Reported<String>,    // result.sandbox.type
    pub network_access: Reported<bool>,    // result.sandbox.networkAccess
    pub ephemeral: Reported<bool>,         // result.thread.ephemeral
    pub environments: Reported<u32>,       // result.thread.environments, counted, not kept
    pub model: Reported<String>,           // result.model
    pub model_provider: Reported<String>,  // result.modelProvider
    pub cwd: Reported<String>,             // result.cwd
}
/// `Absent`: missing or `null`. `Other`: present but not of this type, a string
/// over 256 bytes once unescaped (1,024 for `cwd`), or a constant with an escape.
pub enum Reported<T> { Absent, Value(T), Other }
```

- When `result.sandbox` is missing or `null`, both sandbox facts are
  `Absent`; when it is not an object, both are `Other`.
- `model`, `modelProvider` and `cwd` also appear under `result.thread`.
- `thread/start` carries:
  - `model`, `modelProvider` and `baseInstructions` when given;
  - `cwd` (`work_dir`) and `ephemeral: true`;
  - the policy's `sandbox` and `approvalPolicy`;
  - `environments: []`;
  - `dynamicTools: [{"type": "namespace", "name", "description", "tools"}]`.
- Nothing else in the reply is kept.

**Close and finished.** On the wire, close is the end of `to_codex`: the
engine drops its only sender, and Codex exits at the end of its input. No RPC
comes first. At the pin, `thread/archive` refuses an ephemeral thread, and
`thread/unsubscribe` leaves it loaded.

The engine has two locks, always taken writer first, then state. Close, a
fault, or a failed `try_send` inside `answer` or the consumer, runs the same
close step with both locks held, so it never locks re-entrantly. The close
step, synchronous and awaiting nothing:
- marks the engine closed, so nothing more is published or written;
- takes `to_codex` (before `connect`, the whole `HostTransport`);
- fails pending callers with `Closed`, and retires every call;
- drops these guards: the configuration, the writer buffer, the record, the
  unread events and the event ring;
- aborts the consumer;
- puts `Closed { fault }` in its own slot.

Faults are fixed codes, never supplier text. A full or closed `to_codex`
faults at the next `try_send`, so an idle loss is noticed at the next write.

- **`finished()`** resolves once the consumer task has ended and every charge
  outside the fixed row is released. The consumer drops the frame in hand
  before it signals its end.
  - The host can await `finished()` while holding an undrained
    `EmbeddedEvents`. `Closed` stays readable without holding completion.
  - The fixed row goes when the last of `EmbeddedEngine`, `EmbeddedEvents`
    and `Finished` drops.
- **Replies stay in their slots,** at most four of at most 4 KiB, until
  their futures take them. Close empties the slots, so an unpolled caller
  never holds `finished()`. Such a caller then gets `Closed`.
- **Frames left in `from_codex`** are freed with the receiver.
- **Frames in `to_codex`** are the host's. After `Closed` it discards them,
  closes Codex's stdin, then waits for and reaps Codex. The engine's
  completion does not cover that.

**Memory.** Every byte the engine holds is charged before it is allocated,
at SWBPIPE's prices. Each allocation costs its class, the next power of two
of at least 16 bytes, with old plus new counted while it grows. A charge past
`max_bytes(&limits)` faults. Who pays:
- **A frame** is the host's until `recv` returns it. It is then the engine's,
  at its capacity's class, until decoding ends; a capacity over 128 KiB
  faults.
- **Decoding** owns the nesting stack and its facts. Facts are at most
  2,304 bytes of strings, for `open_thread`'s reply, and they move into the
  event or reply.
- **An event** is the engine's until `recv` returns it. It costs its strings'
  classes against `event_bytes`, plus a slot in the event ring. The ring is a
  preallocated buffer of `event_bytes / 512` slots of 128 bytes, charged once
  at `new`, which never grows. A full ring faults.
- **What the engine writes** is built in the writer buffer. The exact copy
  sent is the engine's until `try_send` succeeds.

| Charge | Bound | SWBPIPE |
| --- | --- | --- |
| The frame in hand | 128 KiB | 128 KiB |
| Decoding: nesting stack and facts | 4 KiB | 4 KiB |
| The event ring | class(`event_bytes` / 512 × 128) | 256 KiB |
| Unread events' strings | `event_bytes` | 1,024 KiB (theirs to set) |
| The record: preallocated 56-byte entries, and two identities per request | class(`calls_per_turn` × 56) + `calls_per_turn` × 512 | 576 KiB |
| Replies until collected, one slot per method | 4 × 4 KiB | 16 KiB |
| The writer buffer and one copy being sent | 2 × 128 KiB | 256 KiB |
| Configuration until `open_thread` writes it: tools, description, `work_dir` | 128 + 4 + 1 KiB | 133 KiB |
| Fixed: control block, consumer task, terminal slot, namespace, tool names | 64 KiB | 64 KiB |
| **Sum** | | **2,457 KiB** (2,515,968 bytes) |

Step 1's counting-allocator test drives adversarial frames down every path:
- depth 127 and 128;
- escaped keys, identities and constants;
- replies left uncollected across repeated calls;
- a ring filled, drained and refilled with different payload sizes;
- close on every path, before and after `connect`.

It counts the engine's allocations at classes, old plus new, and fails above
`max_bytes`. Another test checks that an event is at most 128 bytes.

**Established at the pin (0.171.0).** These runs used scratch homes, plugins
off, a loopback stub provider, `timeout` and a dummy relay variable. They used
no credential and left nothing running.

- **The exact launch** is SWBPIPE's, rebuilt from its source with
  `--strict-config` (Seatbelt: `deny default`, `deny process-fork`, TCP only
  to the stub). Codex ran turns and exited 0. Seatbelt logged no denied fork
  or exec. That shows none was attempted, not that one would be harmless.
- **`thread/start` needs two things** (review probes). Without
  `experimentalApi`, it is refused with −32600 ("thread/start.environments
  requires experimentalApi capability"). Without the namespace's
  `description`, it is refused with −32600 ("missing field `description`").
- **The recorded configuration** is SWBPIPE's file with three changes:
  - a one-entry `model_catalog_json`, which replaces the bundled list;
  - `[agents] enabled = false` in place of `multi_agent = false`;
  - the two skills tables, off.

  An entry needs `slug`, `display_name`, `supported_reasoning_levels`,
  `shell_type`, `visibility`, `supported_in_api`, `priority`,
  `support_verbosity`, `truncation_policy` and
  `experimental_supported_tools`. It also needs an instruction source:
  `base_instructions`, which SWBPIPE uses, or
  `model_messages.instructions_template`. Without one, Codex stops before
  initialising. The recorded entry adds `tool_mode: "direct"` and
  `apply_patch_tool_type: null`.
- **It offers the namespace only.** For `gpt-6.2-sol` (bundled) and
  `swbpipe-relay-model` (not), every request's only tool was `swbpipe` with
  SWBPIPE's seven, with no warning and no skills listed.
  - `apply_patch_tool_type: "freeform"` adds `apply_patch`.
  - `tool_mode: "code_mode"` adds the code-mode-host warning.
  - SWBPIPE's file as it is warns that `[features].multi_agent` is
    deprecated.

  This is recorded in
  `crates/engine/fixtures/codex/swbpipe-namespace-offer.json` (sha256
  `d35430e127d172f9daf2f0b600d1930bccaac39b65fb2f9e5b90c4d896b32868`), with
  `builtIns: []` and `warnings: []`. The relay compares a request's other
  tools with `builtIns`, and the namespace's names and descriptions with its
  catalogue.
- **Codex reduces the schemas it sends.** It drops length, range, pattern and
  item-count bounds, turns `const` into `enum`, and replaces some nested
  schemas with `{}`. `swbpipe_propose_operations` goes from 6,454 to 1,706
  bytes of compact JSON. The host's own check of the arguments is what holds
  them.
- **A turn** goes in this order:
  - `active` and `turn/started`;
  - the items, and any call;
  - token usage and rate limits;
  - `idle`, then `turn/completed`.

  A stream cut short adds `error` with `willRetry: true`, with no status
  change. A failed stream gives `systemError`, then `error` with `willRetry:
  false`.
- **Close.** Ending Codex's stdin mid-turn exited 0 in 2.9–3.4 ms over five
  runs, for this setup.
- **Remote control: the variable's effect is unverified.** Without sign-in,
  Codex reports `"disabled"` after its `initialize` reply, with the variable
  or without.
- **`--strict-config`** exits 1 on an unknown key or malformed TOML.

**Not observed:**
- the variable's effect, which needs a sign-in;
- a `thread/closed` from Codex.
