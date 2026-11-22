# Embedding under containment

Part of the engine's design (`SPEC.md`). Designed 2026-11-22 for SWBPIPE,
revised after four rounds of review; not yet built.

SWBPIPE runs one Codex per session, with one ephemeral thread, no built-in
capability, and remote control off; any built-in activity ends the session.
Its host spawns, frames, observes and reaps Codex. The engine speaks JSON-RPC
over the host's transport.

**A separate engine.** `EmbeddedEngine` lives in `crates/engine/src/embedded/`.
It shares none of the ordinary engine's structure (`Inner`, the store,
`turns`, `calls`, the broadcast, `AppTools`, `Snapshot`, `Event`), nor
`protocol.rs`, whose types build `Value`s and tagged-enum buffers; it decodes
with its own borrowed readers. The ordinary engine and `packages/ui` are
untouched, so nothing is gated.

```rust
impl EmbeddedEngine {
    pub fn new(config: EmbeddedConfig) -> Result<(EmbeddedEngine, EmbeddedEvents), EmbeddedError>;
    pub const fn max_bytes(limits: &EmbeddedLimits) -> u64; // the host reserves this before `new`
    pub async fn connect(&self) -> Result<(), EmbeddedError>;
    pub async fn open_thread(&self, options: &ThreadOptions) -> Result<OpenedThread, EmbeddedError>;
    pub async fn send_text(&self, text: &str) -> Result<TurnId, EmbeddedError>;
    pub async fn interrupt(&self) -> Result<(), EmbeddedError>;
    pub fn answer(&self, call: &CallRef, success: bool, text: &str) -> Result<(), EmbeddedError>;
    pub fn remote_control(&self) -> Option<RemoteControl>;
    pub fn finished(&self) -> Finished; // Future<Output = ()> + Clone + Send + 'static
    pub fn close(&self);                 // also on drop
}
impl EmbeddedEvents {
    pub async fn recv(&mut self) -> Option<EmbeddedEvent>; // None after `Closed`
    pub fn try_recv(&mut self) -> Result<EmbeddedEvent, tokio::sync::mpsc::error::TryRecvError>;
}
pub struct EmbeddedConfig {
    pub transport: HostTransport,
    pub work_dir: PathBuf,
    pub namespace: String, // at most 64 bytes
    pub tools: String,     // the namespace's `tools` JSON, sent as is; up to 64 names read from it
    pub limits: EmbeddedLimits,
}
pub struct HostTransport { // one message per frame, no newline
    pub from_codex: tokio::sync::mpsc::Receiver<String>,
    pub to_codex: tokio::sync::mpsc::Sender<String>,
}
pub struct EmbeddedLimits {   // `new` refuses values outside these ranges
    pub pending_calls: u32,   // 1..=16
    pub calls_per_turn: u32,  // 1..=1,024 requests from Codex, refused ones included
    pub argument_bytes: u32,  // 1..=65,536
    pub result_bytes: u32,    // 1..=65,536 of an answer's `result`
    pub event_bytes: u32,     // 256 KiB..=16 MiB of events not yet received
}
pub enum EmbeddedError {
    Invalid { reason: &'static str },     // nothing was written
    Codex { code: i64, message: String }, // a message over 1 KiB faults
    Closed { fault: Option<String> },
}
```

**Frames.** The host owns line framing, duplicate keys and numeric fidelity;
its 128 KiB includes the newline both ways, so a frame's text is at most
131,071 bytes. A valid frame is one JSON object: a request (an integer or
identity `id`, a string `method`, optional object `params`, no `result` or
`error`), a notification (no `id`, `result` or `error`), or a response (an
integer `id` in flight, no `method` or `params`, and exactly one of `result`
or an `error` with an integer `code` and a string `message`). Other members
are ignored; anything else faults. An identity is a non-empty string of at
most 256 bytes without control characters. The engine serializes what it
writes into one writer that refuses past 131,071 bytes before allocating.

**One consumer.** The engine's one task, spawned in `connect`, takes each
frame from `from_codex` and handles it before the next, never awaiting.
Methods run in their caller's task; a caller that gives up leaves its RPC id
pending until the reply, then discarded (at most four, one per method).
- A reply's effect is applied before its caller completes: the thread
  `thread/start` returned, the turn `turn/start` returned. At the pin each
  reply comes just before its `thread/started` or `turn/started`.
- A turn runs from its reply to its `turn/completed`, and is not admitted if
  either that or `thread/closed` came first. A turn notification naming any
  turn but the running one, the last ended one, or (with a `turn/start` in
  flight) the first new one faults, as does a reply naming another.
- `send_text` is `Invalid` unless a thread is held and open, no turn runs,
  no call is pending and no RPC is in flight: no steering, no queue.
  `interrupt` is `Invalid` unless a turn runs, and returns on Codex's reply.
  `open_thread` is `Invalid` after its first call, even a cancelled one.

**Calls go through the stream**, and the host answers them; there is no
handler. Every request counts toward `calls_per_turn`, its RPC id and
`callId` (if any) recorded before dispatch; a repeat, or a request past the
count, faults, even outside a turn. A newly admitted turn clears the record.
- An `item/tool/call` becomes `Call` only if its `callId` is an identity, it
  names the namespace and one of its tools, it is in the running turn on the
  held thread, its `arguments` are present and within `argument_bytes`
  (checked borrowed), and fewer than `pending_calls` are pending.
- Any other request, including a call without a valid `callId`, becomes
  `Refused`, then `{"id": …, "error": {"code": -32601, "message": "not
  supported by this client"}}` is written.
- `answer` writes `{"id": …, "result": {"contentItems": [{"type":
  "inputText", "text": …}], "success": …}}`, refusing a `result` over
  `result_bytes`. For a retired call it is `Invalid` and writes nothing. A
  call is retired by its answer, its `item/completed`, its turn's
  `turn/completed`, `thread/closed`, or close. On interrupt, Codex completes
  an unanswered call's item as `failed` before it replies, and ignores a late
  answer.

Since a call is an event in wire order, the host never answers one it reads
after an event that ends its session.

**Events.**

```rust
// All `Serialize`, `rename_all = "camelCase"`; `EmbeddedEvent` is tagged `kind`
// (`rename_all_fields = "camelCase"`), `RpcId` untagged.
pub enum EmbeddedEvent {
    RemoteControl { status: RemoteControl },
    Notification { method: Method, turn: Option<TurnId>, item: Option<ItemRef>, status: Option<String>, json: String },
    Call { call: CallRef, tool: String, arguments: String },
    Refused { method: String, thread: Option<String>, json: String },
    Unheld { method: String, thread: String, json: String },
    Unmapped { method: String, thread: Option<String>, json: String },
    Closed { fault: Option<String> },
}
pub struct CallRef { pub rpc_id: RpcId, pub call_id: String, pub turn: TurnId }
pub enum RpcId { Integer(i64), Text(String) }
pub struct ItemRef { pub id: Option<String>, pub item_type: String }
pub enum RemoteControl { Disabled, Connecting, Connected, Errored, Other }
pub enum Method {
    ThreadStarted, ThreadStatus, ThreadClosed, TokenUsage, RateLimits,
    TurnStarted, TurnCompleted, ItemStarted, ItemCompleted,
    AgentMessageDelta, ReasoningSummaryDelta, ReasoningSummaryPart, ReasoningDelta,
    Retrying, Error, Warning, ConfigWarning, RawResponseItem, RawResponse,
}
```

`json` is the frame's `params` as sent and `arguments` their raw text; other
strings are identities, or at most 64 bytes for a type or status. Only
SWBPIPE's Rust host consumes these, so they have no ts-rs bindings.

Each frame from Codex gets the first of these that applies, and only it:
1. **A fault:** an invalid frame, a repeated RPC id or `callId`, or a request
   past the count.
2. **A response:** its caller.
3. **An admitted `item/tool/call`:** `Call`.
4. **Any other request:** `Refused`.
5. **A notification whose `threadId`, `thread.id` or `conversationId` is not
   the held thread** (or any, before one is held): `Unheld`.
6. **`remoteControl/status/changed` with a string `status`:** `RemoteControl`
   (`Other` for one it does not know).
7. **A method below, whose facts decode:** `Notification`.
8. **Anything else:** `Unmapped`.

| Method | From (facts that must decode) |
| --- | --- |
| `ThreadStarted`, `ThreadClosed` | `thread/started`, `thread/closed` (which revokes the thread) |
| `ThreadStatus` | `thread/status/changed` (`status.type`), its only route; `idle` does not end the turn |
| `TokenUsage`, `RateLimits` | `thread/tokenUsage/updated` (`turnId`), `account/rateLimits/updated` |
| `TurnStarted`, `TurnCompleted` | `turn/started`, `turn/completed` (`turn.id`, `turn.status`) |
| `ItemStarted`, `ItemCompleted` | `item/started`, `item/completed` (`turnId`, `item.id`, `item.type`) |
| the deltas | `item/agentMessage/delta`, `item/reasoning/{summaryTextDelta, summaryPartAdded, textDelta}` (`turnId`, `itemId`) |
| `Retrying`, `Error` | `error`, by `willRetry` (`turnId`) |
| `Warning`, `ConfigWarning` | `warning`, `configWarning` |
| `RawResponseItem`, `RawResponse` | `rawResponseItem/completed` (`turnId`, `item.type`), `rawResponse/completed` (`turnId`) |

**SWBPIPE's rule**, which the host applies.
- **Not ending:** `RemoteControl` `Disabled`; `ThreadStarted`, `TurnStarted`,
  `TurnCompleted`; `ThreadStatus` `active` or `idle`; `ItemStarted` and
  `ItemCompleted` of `userMessage`, `agentMessage`, `reasoning` or
  `dynamicToolCall`, and the deltas; `Call`; `TokenUsage`, `RateLimits`,
  `Retrying`; `RawResponseItem` of `message`, `reasoning`, `function_call` or
  `function_call_output`; `RawResponse`.
- **Ending, exactly:** any other `RemoteControl` or `ThreadStatus` status;
  `ThreadClosed`; `Error`; `Warning` and `ConfigWarning` (the recorded closed
  set is empty; the code-mode-host warning is a `Warning`); any other item or
  raw item type; `Refused`, `Unheld`, `Unmapped`, `Closed`.

**The thread.**

```rust
pub struct ThreadOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub base_instructions: Option<String>,
    pub policy: ThreadPolicy,
    pub raw_response_items: bool, // `experimentalRawEvents`
}
pub struct ThreadPolicy { pub sandbox: SandboxMode, pub approval: ApprovalPolicy }
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }
pub enum ApprovalPolicy { Untrusted, OnRequest, Never }

/// Each as Codex reported it in the `thread/start` reply.
pub struct OpenedThread {
    pub thread: ThreadId,                        // result.thread.id, an identity, else a fault
    pub approval_policy: Reported<String>,       // result.approvalPolicy
    pub sandbox_type: Reported<String>,          // result.sandbox.type
    pub network_access: Reported<bool>,          // result.sandbox.networkAccess
    pub ephemeral: Reported<bool>,               // result.thread.ephemeral
    pub environments: Reported<u32>,             // result.thread.environments, counted, not kept
    pub model: Reported<String>,                 // result.model
    pub model_provider: Reported<String>,        // result.modelProvider
    pub cwd: Reported<String>,                   // result.cwd
}
/// `Other`: present but not of this type, or a string over 256 bytes (1,024 for `cwd`).
pub enum Reported<T> { Absent, Value(T), Other }
```

`Absent` is missing or `null`. The last three also appear under
`result.thread`. `thread/start` carries `cwd` (`work_dir`), `ephemeral:
true`, `environments: []`, the namespace with its `tools`, the policy and the
options; nothing else in the reply is kept (`runtimeWorkspaceRoots` is
skipped unread).

**Close and finished.** On the wire, close is the end of `to_codex`: the
engine drops its only sender and Codex exits at the end of its input. No RPC
comes first: at the pin `thread/archive` refuses an ephemeral thread and
`thread/unsubscribe` leaves it loaded. Closing is synchronous and awaits
nothing. Under the writer and queue locks it marks the engine closed (nothing
more is published or written), takes `to_codex` (before `connect`, the whole
`HostTransport`), fails pending callers with `Closed`, retires every call,
drops the unread events, aborts the consumer, and puts `Closed { fault }` in
its own slot. A fault closes too: an invalid frame, a charge past the budget,
the end of `from_codex`, or a full or closed `to_codex` at a `try_send` (so
an idle loss is noticed at the next write).
- `finished()` resolves once the consumer has ended and every charge but the
  fixed row is released. Since close drops unread events itself, the host
  can await it while holding an undrained `EmbeddedEvents`; `Closed` stays
  readable without holding completion. The fixed row goes when the last of
  `EmbeddedEngine`, `EmbeddedEvents` and `Finished` drops.
- Frames left in `from_codex` are freed with it. Frames in `to_codex` are
  the host's: after `Closed` it discards them, closes Codex's stdin, then
  waits for and reaps Codex, which the engine's completion does not cover.

**Memory.** Every byte the engine holds is charged before it is allocated, at
SWBPIPE's prices (each allocation at its class, the next power of two of at
least 16 bytes; old plus new while growing); a charge past
`max_bytes(&limits)` faults. Payers:
- A frame is the host's until `recv` returns it, then the engine's at its
  capacity's class (over 128 KiB faults) until decoding ends.
- Decoding reads the frame borrowed and builds no vector or map: arrays are
  counted or skipped, opaque parts stay `&RawValue`. It owns at most six
  identities or short strings (owned only when escaped, after a length check
  on the raw text, so serde_json's scratch stays within 1.5 KiB) and one
  exact-capacity copy of `json` or `arguments`, which moves into the event.
- An event costs its strings' classes plus three 128-byte slots (its share of
  the queue's buffer, old plus new); it is the engine's until `recv` returns
  it.
- What the engine writes goes through the one 128 KiB writer buffer; the
  exact copy sent is the engine's until `try_send` succeeds.

| Charge | Bound | SWBPIPE |
| --- | --- | --- |
| The frame in hand, and its decoded facts | 128 KiB + 4 KiB | 132 KiB |
| Events not yet received | `event_bytes` | 1,024 KiB (theirs to set) |
| The record: a 56-byte entry and two identities per request | class(`calls_per_turn` × 56) + `calls_per_turn` × 512 B | 576 KiB |
| Pending replies, four at most | 16 KiB | 16 KiB |
| The writer buffer and one copy being sent (answers within `result_bytes`) | 2 × 128 KiB | 256 KiB |
| The tools text, until written | 128 KiB | 128 KiB |
| Fixed: control block, consumer task, terminal slot | 64 KiB | 64 KiB |
| **Sum** | | **2,196 KiB** (2,248,704 bytes) |

Step 1's counting-allocator test drives adversarial frames down every path,
counts the engine's allocations at classes and old plus new, and fails above
`max_bytes`; another checks an event is at most 128 bytes.

**Established at the pin (0.171.0).** Scratch homes, plugins off, a loopback
stub provider, `timeout`, a dummy relay variable, no credential, nothing left
running.

- **The exact launch** is SWBPIPE's, rebuilt from its source with
  `--strict-config` (Seatbelt: `deny default`, `deny process-fork`, TCP only
  to the stub). Codex ran turns and exited 0. Seatbelt logged no denied fork
  or exec, which shows none was attempted, not that one would be harmless.
- **The recorded configuration** is SWBPIPE's file with a one-entry
  `model_catalog_json` (replacing the bundled list), `[agents] enabled =
  false` for `multi_agent = false`, and the two skills tables off. An entry
  needs `slug`, `display_name`, `supported_reasoning_levels`, `shell_type`,
  `visibility`, `supported_in_api`, `priority`, `support_verbosity`,
  `truncation_policy`, `experimental_supported_tools`, and an instruction
  source (`base_instructions`, which SWBPIPE uses, or
  `model_messages.instructions_template`); without one, Codex stops before
  initialising. The recorded entry adds `tool_mode: "direct"` and
  `apply_patch_tool_type: null`.
- **It offers the namespace only:** for `gpt-6.2-sol` (bundled) and
  `swbpipe-relay-model` (not), every request's only tool was `swbpipe` with
  SWBPIPE's seven, with no warning and no skills listed.
  `apply_patch_tool_type: "freeform"` adds `apply_patch`; `tool_mode:
  "code_mode"` adds the code-mode-host warning; SWBPIPE's file as it is warns
  that `[features].multi_agent` is deprecated. Recorded in
  `crates/engine/fixtures/codex/swbpipe-namespace-offer.json` (sha256
  `d35430e127d172f9daf2f0b600d1930bccaac39b65fb2f9e5b90c4d896b32868`), with
  `builtIns: []` and `warnings: []`; the relay compares a request's other
  tools with `builtIns` and the namespace's names and descriptions with its
  catalogue. The code-mode results are in this PR's history.
- **Codex reduces the schemas** it sends: it drops length, range, pattern and
  item-count bounds, turns `const` into `enum`, and replaces some nested
  schemas with `{}` (`swbpipe_propose_operations`: 6,454 to 1,706 bytes of
  compact JSON). The host's own check of the arguments is what holds them.
- **A raw-mode turn** sends `rawResponse/completed` after each response, the
  first before the call's item and `item/tool/call`, and `idle` before `turn/completed`. A
  stream cut short adds `error` with `willRetry: true`, with no status
  change.
- **Close:** ending Codex's stdin mid-turn exited 0 in 2.9–3.4 ms over five
  runs, for this setup.
- **Remote control: the variable's effect is unverified.** Without sign-in,
  Codex reports `"disabled"` after its `initialize` reply, with the variable
  or without.
- `--strict-config` exits 1 on an unknown key or malformed TOML.

**Not observed:**
- the variable's effect, which needs a sign-in;
- a `thread/closed` from Codex.
