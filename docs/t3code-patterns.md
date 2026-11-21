# How T3 Code wraps the Codex app-server, and what to take from it

Prior-art notes for the Rust engine (Codex app-server over JSON-RPC stdio) and the thin Tauri + React wrapper.
Source: T3 Code (`github.com/pingdotgg/t3code`, MIT licence, Copyright (c) 2026 T3 Tools Inc.; see [Attribution](#attribution)), read at commit `be9176761ce2f6597013678b4538c772ae507400`. Protocol at our pin: generated from the pinned Codex `rust-v0.171.0` (`codex app-server generate-ts` and `generate-json-schema`).
Paths below are relative to the T3 Code root unless they start with `pin:` (which means `rust-v0.171.0/schema-ts/`).
This is read-only research: T3 Code and Codex were not run.

Where the code lives:
- `packages/effect-codex-app-server/`: the protocol package. It holds the transport (`src/protocol.ts`), the typed client (`src/client.ts`), the replay client (`src/replay.ts`), the errors (`src/errors.ts`) and the generated schemas (`src/_generated/*`, built by `scripts/generate.ts` from upstream JSON Schema at a pinned upstream commit).
- `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts` (9,111 lines): everything that is specific to Codex. It maps the protocol to T3's own event model.
- `apps/server/src/provider/Codex*.ts`, `codex*.ts`, `Drivers/Codex*.ts`: installing the binary, probing it, the model catalogue, auth, the home layout and the launch arguments.
- `apps/server/scripts/record-codex-app-server-replay-fixture.ts` and `codexReplayRecordingRecords.ts`: the fixture recorder.
- `apps/server/src/orchestration-v2/testkit/`: the replay harness and the fixtures (`fixtures/<scenario>/codex_transcript.ndjson`, `input.ts`, `codex_output.ts`).
- `packages/contracts/src/orchestrationV2.ts`, `providerPolicy.ts`: the event and state model. `packages/provider-core/src/server/ProviderAdapter.ts` holds the adapter interface and its event union.

---

## 1. Process: how Codex is found, launched, initialised, kept healthy, restarted and stopped

**Finding the binary** (`apps/server/src/provider/CodexInstallation.ts`):
- `resolve()` prefers a local binary and falls back to a managed one.
- **Local binary.** Taken from the `binaryPath` setting, default `codex`. T3 checks that it exists and that `codex-code-mode-host` sits beside it. It runs `codex --version` and expects `^codex-cli X.Y.Z$`, then runs `codex app-server --help`, with a 5 s timeout. The result is cached per file fingerprint (realpath, size, mtime, inode). The binary is accepted only if the compatibility advisory says `supported`.
- **Managed binary.** T3 downloads the OpenAI release tarball into `<baseDir>/tools/codex/`, verifies its sha256, writes `codex-package.json` and `active.json`, and checks that `bin/codex`, `bin/codex-code-mode-host` and `codex-path/rg` exist and are executable. It checks npm `@openai/codex/latest` at most once an hour for updates.
- **Compatibility** comes from `apps/server/src/provider/model-manifest.json`. For example, codex `>=0.156.0` is supported, `0.149–0.156` is unsupported, `<0.149` is broken, and `>=0.159.0` is recommended. The same check runs again at `initialize`, using the version parsed from `userAgent`, and T3 refuses broken versions (`brokenRuntimeError`).

**Launching** (`CodexAdapterV2.ts` `layerAppServerClientFactory`, `makeCodexAppServerSpawnCommand`; `provider/codexLaunchArgs.ts`; `provider/CodexProvider.ts` `codexProcessEnvironment`):
- argv: `codex app-server -c thread_unload_delay_secs=0 <user launchArgs>`. T3 unsubscribes threads itself, so it turns Codex's own unload delay off. `T3CODE_CODEX_LAUNCH_ARGS` can override the launch args.
- env: the inherited environment plus `CODEX_INTERNAL_ORIGINATOR_OVERRIDE=<client name>`. The originator selects the backend's model catalogue, and setting it at spawn stops the pre-initialize refresh from writing the plain-CLI catalogue into the shared `models_cache.json`.
- `CODEX_HOME` is set only when the user configures a home path, and T3 expands `~` itself because Codex rejects a literal `~`.
- The default is the user's real `~/.codex`.
- An optional **shadow home** ("authOverlay", `Drivers/CodexHomeLayout.ts`) is a per-account directory. It keeps `auth.json`, `auth.lock` and `models_cache.json` private and symlinks the shared state into the shared home: `sessions`, `archived_sessions`, `sqlite`, `skills`, `plugins`, `cache`, `logs`, `thread-writer-locks` and others. It also adds `-c marketplaces.openai-bundled.source=...`.
- cwd is the thread's cwd. `forceKillAfter: 2s` after SIGTERM.
- The spawn goes through an `AgentScope.wrap` process-tree wrapper, so that tool subprocesses can be attributed and cleaned up.

**One process for many threads.** A single app-server per provider instance serves every thread. Per-thread differences go in `thread/start` `config` overrides rather than in the process env: `shell_environment_policy.set.T3CODE_THREAD_ID`, and `mcp_servers.<name>.env.T3CODE_THREAD_ID` for each stdio MCP server listed by `config/read`.

**Initialising** (`CodexAdapterV2.ts` around line 2030, `ensureInitialized`):
- `initialize` sends `{clientInfo:{name,title,version}, capabilities:{experimentalApi:true, optOutNotificationMethods:["turn/diff/updated"], extensions:{"io.modelcontextprotocol/ui":{mimeTypes:["text/html;profile=mcp-app"]}}}}`. Then the `initialized` notification follows.
- A semaphore guards it because Codex rejects a second initialize. `-32600 "Already initialized"` is treated as success.
- The timeout is 30 s. On timeout T3 fails the session's event stream, which retires the process, so the next turn spawns a fresh one.
- Our pin's `InitializeCapabilities` has `experimentalApi: boolean` and `requestAttestation: boolean`, both non-optional in the generated TS. It also has optional `requiredHumanApproval`, `explicitGatewayOauth`, `mcpServerOpenaiFormElicitation`, `optOutNotificationMethods` and `extensions`.
- T3 omits `requestAttestation`. We should send it explicitly.
- `requiredHumanApproval` (the `humanCommand` approval kind) is new at our pin, and T3 does not use it. It is relevant to acts reserved to a person.

**Health.** There is no ping or heartbeat. Health is inferred from three signals:
- the stdout stream ending,
- the exit code (read with a 250 ms grace, because Node ends stdout before it reports the exit),
- EPIPE on write.

All of them go through one idempotent `handleTermination`: fail every pending request, end the outgoing queue, close the request-handler scope and call `onTermination`. The error carries an excerpt of the stderr tail (an 8 KiB ring buffer, and the last line that looks like `error:`, at most 512 chars): `_internal/stdio.ts`.

**Restarting.** A dead process retires its session (`awaitTermination` leads to `Queue.fail(events)`). The next turn opens a new session and runs `thread/resume {threadId, excludeTurns:true, cwd, model, config}`.
- Resume has known retries in `resumeNativeThread`:
  - "session X is archived" leads to `thread/unarchive`, then resume again.
  - "thread X is closing" leads to waiting up to 10 s for `thread/closed`, then resume again.
- Errors that matter:
  - `-32600 "...already has an active writer"`: another client (the Codex app or CLI) holds the per-thread writer lock on the same CODEX_HOME. The user is shown a "quit that client" message.
  - `-32600 "thread not found:"`.
- A fresh server reports threads as `notLoaded` in `thread/read`. T3 reads first, then resumes, before `thread/revert`.

**Stopping.**
- Closing the scope sends SIGTERM, then a force kill after 2 s.
- `thread/unsubscribe` unloads a single thread (and its MCP servers). `thread/closed` confirms it.
- `thread/archive` also unloads, inline.
- Short-lived "probe" processes (status, model and account discovery) are not killed straight away. T3 waits up to 1 min while `pgrep -g <pid>` shows child work, because Codex runs git or ssh for plugin marketplaces at launch and leaves ssh stranded if killed mid-fetch (`CodexProvider.ts` `stopCodexAfterStartupWork`).

Recommendation: one long-lived app-server per CODEX_HOME. It gets explicit argv `app-server -c thread_unload_delay_secs=0`, an originator env and an explicit `CODEX_HOME`. Track health from EOF, exit and EPIPE with a stderr tail, bound initialize at 30 s, and on death respawn and `thread/resume {excludeTurns:true}` lazily on the next turn.

## 2. The JSON-RPC client

Transport (`packages/effect-codex-app-server/src/protocol.ts`):
- **Framing.** Newline-delimited JSON (JSONL). There is no `"jsonrpc":"2.0"` field on outgoing frames; Codex does not require it. CR is stripped. A line is capped at 128 MiB before parsing, and exceeding the cap is fatal.
- **Routing**, in this order:
  - `method` plus a valid `id` is a server-to-client request.
  - `method` with no `id` is a notification.
  - `id` with `result` or `error` is a response.
  - Anything else is an unroutable parse error.
- **Correlation.** Integer ids from a counter starting at 1. The pending map is keyed by `String(id)`, so a numeric 1 and a string "1" collide; that is fine because only we allocate ids. Each entry is removed in the acquire/release finaliser, so a cancelled caller does not leak its slot.
- **Errors.** A response error becomes `CodexAppServerRequestError{code,errorMessage,method,requestId}` (`errors.ts`). The codes used are `-32601` method not found, `-32602` invalid params, `-32603` internal, `-32001` overloaded, and `-32600`, which Codex uses for semantic refusals (already initialized, thread not found, active writer). Callers match on `code` plus the message prefix.
- **Typed layer** (`client.ts`). Each method looks up its params and response schema in the generated `meta.gen.ts`. Requests are encoded before sending and responses decoded after.
  - A notification that fails to decode is dropped, and logged once per distinct method and issue signature, because a drifted Codex repeats the same bad shape on every frame.
  - An unknown notification goes to `handleUnknownServerNotification`. An unknown request goes to `handleUnknownServerRequest`, or else gets `-32601`.
- **Server-to-client requests.**
  - Each one runs in a `FiberMap` keyed by request id, at most 32 at a time. Beyond that T3 replies `-32001 overloaded`.
  - A handler defect becomes `-32603`. Codex always waits for an answer, so one is always sent.
  - `serverRequest/resolved {threadId, requestId}` cancels the handler still waiting, because Codex settled the request itself (for example, the turn ended). This matters: an approval card must disappear when it arrives.
- **Timeouts.** The client sets none per request. Callers add them: initialize 30 s, interrupt confirmation 10 s, config probes, inject_items 10 s, and a discovery deadline on probes.
- **Backpressure.**
  - Notifications are dispatched inline in the read loop, so a slow handler slows reading stdout (natural backpressure).
  - Raw tap streams are sliding queues of 32 (oldest dropped), used for logging only.
  - The outgoing queue is unbounded.
  - Above the client, assistant and reasoning deltas are coalesced (`CODEX_ASSISTANT_DELTA_FLUSH_INTERVAL_MS = 50`, plan deltas 400 ms) before they become persisted events.
- **Logging.** An optional protocol logger receives raw and decoded frames in both directions. T3 routes each frame to a per-thread NDJSON log (`makeCodexProtocolThreadRouter` uses the `config` env key, `params.threadId` or the parent subagent thread) and redacts keys ending in authorization, apikey, token, password or secret, plus `Bearer` strings (`redactCodexProtocolValue`). The same logger hook is what the fixture recorder uses (section 8).
- **Schema drift.** The generated schema misses fields that the wire accepts, so T3 patches them in by hand:
  - `turn/start` gets `collaborationMode`, `additionalContext` and `cyberAccessProgram` (`client.ts` `V2TurnStartParams`).
  - The method `thread/settings/update` is added by hand (`scripts/generate.ts` `ManualClientRequests`).
  - All of these are **absent from our pin's generated TS** (pin:`v2/TurnStartParams.ts`, pin:`ClientRequest.ts`), as are `thread/sample` and `thread/backgroundTerminals/list|terminate`. They are experimental and behind `experimentalApi`, and our generation leaves them out.

Recommendation: a single reader task that routes by shape, a `HashMap<i64, oneshot::Sender>` for pending requests with guard-based removal, and inline notification dispatch onto a bounded channel. Server requests should be spawned as tasks keyed by id, capped (-32001), always answered, and cancelled on `serverRequest/resolved`. Log every frame through one redacting hook that the recorder reuses.

## 3. Threads and turns

T3 keeps its **own** thread store (SQLite, event-sourced projections) and binds each app thread to a Codex `nativeThreadRef`. **It never calls `thread/list`**. Codex threads are identified by id only. Method usage is in `CodexAdapterV2.ts`:

- **Start.** `thread/start {cwd, model, config:{"tools.update_plan.enabled":true, "mcp_optional_startup_grace_ms":10000, "features.concurrent_reasoning_summaries":true, ...per-thread env and MCP overrides}}` (`CODEX_THREAD_CONFIG`, `codexThreadRuntimeParams`). Before each start T3 calls `config/read {cwd, includeLayers:false}` to find the user's stdio MCP servers.
- **Resume.** `thread/resume {threadId, excludeTurns:true, cwd, model, config}`. History is paged with `thread/turns/list {threadId, cursor, limit, sortDirection:"desc", itemsView:"summary"}`. `thread/read {threadId, includeTurns:false|true}` is used for snapshots and loaded state.
- **Fork.** `thread/fork {threadId, excludeTurns:true, lastTurnId?, ...runtime params}` (`forkThread`, around line 9000). `lastTurnId` is inclusive, and it must not be an in-progress turn. Forking from an archived thread triggers an unarchive-and-retry.
- **Archive and unarchive.** `thread/archive` and `thread/unarchive`. `thread/unsubscribe` is the plain unload.
- **Rename.** `thread/name/set {threadId, name}`. T3 generates the title itself. Codex then echoes `thread/name/updated`.
- **Revert / rollback.**
  - Conversation: `provider/CodexThreadRevert.ts` pages `thread/turns/list` descending to find the N-th newest turn, then calls `thread/revert {threadId, beforeTurnId}`. That turn and every later one are dropped. Codex emits `thread/reverted`. Our pin has `thread/revert`, not "rollback".
  - Files: reverted separately by T3's own git checkpoints (`orchestration-v2/CheckpointService.ts`, refs under `refs/t3/orchestration-v2/checkpoints`).
- **Send.** `turn/start {threadId, input, cwd, model, effort?, summary, approvalPolicy, sandboxPolicy, approvalsReviewer, serviceTier?, collaborationMode:{mode:"default"|"plan", settings:{model, reasoning_effort}}, additionalContext?}` (`buildCodexTurnStartParams`, around line 803).
  - `summary` is the user's `model_reasoning_summary` from `config/read`, or else `"auto"`, because some providers reject `"detailed"`.
  - `collaborationMode` is sent on every turn so that leaving plan mode clears it. It is absent from our pin's generated TS.
  - The response is `{turn:{id}}`. `turn/started` and `turn/completed` arrive as notifications.
  - A queued turn acknowledges with null timestamps, so T3 waits for `turn/started` before allowing an interrupt.
- **Steer.** `turn/steer {threadId, expectedTurnId, input}`. `expectedTurnId` is a precondition: the request fails if it is not the active turn. It takes no `additionalContext`, so any context not yet sent goes in as quoted text input.
- **Interrupt.** `turn/interrupt {threadId, turnId}` on each active root and child turn, plus `thread/backgroundTerminals/terminate` for running commands (not in our pin). T3 then waits up to 10 s for `turn/completed`. If none arrives, the UI shows "unresponsive" and offers **Force stop**, which kills the process.
- **Per-thread or per-turn settings.** Codex treats every turn/start override as sticky for later turns too. T3 sends model, effort, approvals and sandbox on **every** turn from its "runtime mode" (`codexRuntimeModeTurnDefaults`):

  | T3 runtime mode | `approvalPolicy` | `sandboxPolicy` | `approvalsReviewer` |
  | --- | --- | --- | --- |
  | `approval-required` ("Supervised") | `untrusted` | `{type:"readOnly"}` | `user` |
  | `auto-accept-edits` | `on-request` | `{type:"workspaceWrite"}` | `user` |
  | `auto` | `on-request` | `workspaceWrite` | `auto_review` |
  | `full-access` | `{granular:{mcp_elicitations:true, request_permissions:false, rules:false, sandbox_approval:false, skill_approval:false}}` | `{type:"dangerFullAccess"}` | `user` |

  In full-access mode T3 uses `granular` instead of `never` because `never` also auto-rejects MCP and browser prompts.
  At our pin, `SandboxPolicy.workspaceWrite` lists `writableRoots`, `networkAccess`, `excludeTmpdirEnvVar` and `excludeSlashTmp` as non-optional in the TS. T3 sends only `type`, so we should send the full shape.
  `thread/settings/update {threadId, serviceTier}` is used only for the service tier.
  The **reasoning effort** comes from the model selection (`ReasoningEffort`). There is one default override: gpt-6-astra uses "medium".
- **Attachments and file references** (`toCodexInput`, around line 3741; `packages/provider-core/src/server/attachmentPrompt.ts`):
  - Images go in as `{type:"image", url:"data:<mime>;base64,..."}`. Note that T3 does not use `localImage`.
  - Other files are saved under T3's attachments directory and referenced **in the text**: `[Attached file "name" is saved at: /abs/path (text extracted to: ...)]`.
  - Skill mentions are rewritten in the text. T3 does not use `{type:"mention"}` or `{type:"skill"}`.
  - At our pin, `UserInput.text` has a required `text_elements: TextElement[]`; T3 omits it. We should send `[]`.
- **Compaction.** `thread/compact/start`. Codex drops client developer messages when it compacts, so after `turn/completed` T3 re-injects its additional context with `thread/inject_items`.

Recommendation: our app owns the thread list and maps app threads to Codex thread ids. The engine sends the full policy (model, effort, approval, sandbox, summary, collaborationMode) on every `turn/start`. Implement revert as `thread/turns/list` plus `thread/revert {beforeTurnId}`, and fork as `thread/fork {lastTurnId, excludeTurns}`.

## 4. Events: from Codex notifications to T3's own model

**The model** (`packages/contracts/src/orchestrationV2.ts`, `packages/provider-core/src/server/ProviderAdapter.ts`):
- App thread, then Run (one user message and its outcome), then ProviderSession, ProviderThread and ProviderTurn (native ids kept as `nativeThreadRef`, `nativeTurnId` and `nativeItemRef`), then ExecutionNode (a tree: root_turn, assistant_message, tool, subagent and so on), then **TurnItem** (the timeline row), plus ConversationMessage, RuntimeRequest and PlanArtifact.
- IDs are derived deterministically from native ids (`idAllocator.derive.turnItemFromProviderItem({driver, nativeItemId})`). Replays therefore produce the same projection, and an update upserts the same row.
- The adapter emits `ProviderAdapterV2Event`, a union of `*.updated` upserts of whole objects plus a few special events:
  - `app_thread.created`, `provider_session|thread|turn.updated`, `node.updated`, `subagent.updated`, `message.updated`, `turn_item.updated`, `steer.updated` (pending, completed or cancelled), `background_work.updated`, `runtime_request.updated`, `plan.updated`;
  - `command_output.delta` (live only, never persisted);
  - `prompt_suggestion.updated`;
  - `turn.terminal` (completed, interrupted, cancelled or failed, with failure, retry and `threadDisposition: reusable|broken`).
- A central `ProviderEventIngestor` and `EventSink` persist events in batches. `ThreadLiveEventCoalescer` merges repeated updates of the same item before they go over the WebSocket.
- **TurnItem types:** `user_message`, `assistant_message{text, streaming, phase}`, `reasoning{text, streaming}`, `proposed_plan{markdown, streaming}`, `todo_list{steps}`, `user_input_request`, `file_change{fileName, additions, deletions, diffStr, changes}`, `command_execution{input, output, exitCode, outputIndicatesFailure}`, `file_search`, `web_search{patterns, results}`, `approval_request{requestKind, prompt, options}`, `checkpoint`, `run_interrupt_request|result`, `system_notice`, `error{failure, retry}`, `compaction`, `handoff`, `fork`, `thread_created`, `subagent`, `dynamic_tool` (MCP and dynamic tools), `notification`.

**How each notification is mapped** (handlers from around line 4895 in `CodexAdapterV2.ts`):

| Codex notification | Becomes |
| --- | --- |
| `item/agentMessage/delta {turnId,itemId,delta}` | An append to a coalescer (50 ms), then an `assistant_message` item with `streaming:true`. `item/completed` finalises it. `phase` (`final_answer` and others) comes from `item/started`. |
| `item/reasoning/summaryTextDelta` (by `summaryIndex`), `item/reasoning/textDelta` (by `contentIndex`) | A single `reasoning` item. Summary parts are joined. |
| `item/plan/delta` (400 ms) | `proposed_plan` markdown, streaming. Plan mode asks the model to wrap its plan in `<proposed_plan>`. |
| `turn/plan/updated {plan:[{step,status}]}` | A `todo_list` item plus a PlanArtifact. |
| `item/started` / `item/completed` | Switches on `item.type`: `commandExecution` becomes `command_execution` (with `processId` tracked for termination); `fileChange` becomes `file_change` (kinds recorded per path at start, `diffStr` from `changes[].diff` at completion); `mcpToolCall` and `dynamicToolCall` become `dynamic_tool` (presentation in `provider/CodexToolPresentation.ts`, MCP-app resources read with `mcpServer/resource/read`); `webSearch` becomes `web_search`; `contextCompaction` becomes `compaction`; `subAgentActivity` and `collabAgentToolCall` become subagents and child threads; `userMessage` on a child thread becomes the subagent's prompt. |
| `item/commandExecution/outputDelta` | `command_output.delta`, live only. The final output arrives on completion. |
| `turn/started` / `turn/completed` | `provider_turn.updated` / `turn.terminal`. |
| `error`, `model/rerouted`, `thread/tokenUsage/updated`, `account/rateLimits/updated`, `thread/settings/updated`, `mcpServer/startupStatus/updated`, `hook/completed`, `thread/goal/*`, `thread/closed` | Failure and retry, the model change, token usage, usage limits, MCP startup failures as notices, and so on. |

- `error` carries `codexErrorInfo`. T3 classifies it into usage-limit, capacity and HTTP status, and retries capacity errors up to 5 times.
- **Diffs.** T3 opts **out** of `turn/diff/updated` (`optOutNotificationMethods`). Per-tool diffs come from `fileChange.changes[].diff`. Per-turn and branch diffs come from T3's own git checkpoints (`checkpointDiff.ts`), not from Codex.
- Notifications T3 ignores include `thread/status/changed`, `thread/started`, `remoteControl/status/changed`, `configWarning` and `account/updated`. They appear in the fixtures but have no handler.

Recommendation: define a small engine event model (thread, turn, item with a stable id derived from Codex item ids, request) as **upserts of the whole item** plus live-only deltas. Coalesce deltas at about 50 ms. Take tool diffs from `fileChange.changes[].diff`, opt out of `turn/diff/updated` unless the UI needs a turn diff, and treat `turn/completed` as the only end of a turn.

## 5. The person's part: approvals, permissions, questions, elicitation

Every server request follows the same path. The handler registers a Deferred in `pendingRuntimeRequests`, then emits `node.updated` + `turn_item.updated{approval_request|user_input_request}` + `runtime_request.updated{status:"pending"}`. It awaits the person's decision, maps it to the Codex reply, and removes the request on exit (`CodexAdapterV2.ts`, around lines 5919–6400).
- `runtimeRequest.responseCapability:{type:"live", providerSessionId}` records that only that process can answer. Codex requests do not survive a restart (`pendingRequestsSurviveRestart:false`).
- T3's decisions are `accept | acceptForSession | acceptAlways | decline | cancel` (`contracts/src/providerPolicy.ts`).

| Request | How it is shown and answered |
| --- | --- |
| `item/commandExecution/requestApproval` | The prompt is `reason ?? command`. The reply is `{decision}`. `acceptAlways` is mapped to `acceptForSession`. Codex's acceptForSession covers only that exact command, so T3 also remembers a per-thread allowance by kind and auto-replies `acceptForSession` later. T3 does not use the pin's `acceptWithExecpolicyAmendment` or `applyNetworkPolicyAmendment`, nor the new `kind: command|humanCommand|writeStdin`. |
| `item/fileChange/requestApproval` | The prompt is "add/edit/delete <path>", built from the kinds recorded at item start (`codexFileChangeApprovalPrompt`). The reply is `{decision}`. |
| `item/permissions/requestApproval` | The kind is classified as file-change, file-read or command from `permissions.fileSystem` and `entries`. Accept is answered `{permissions: <as requested>, scope: "turn"|"session"}`. A denial is answered `{permissions:{}, scope:"turn"}`. |
| `execCommandApproval`, `applyPatchApproval` (legacy) | The legacy `ReviewDecision` is returned: `approved`, `approved_for_session`, `{denied:{rejection}}` or `abort`. |
| `item/tool/requestUserInput {questions:[{id,header,question,options,isOther,isSecret}]}` | A `user_input_request` item with structured questions. The answer is `{answers:{<qid>:{answers:[strings]}}}`. Our pin adds `isBlocking` and `autoResolutionMs`. |
| `mcpServer/elicitation/request` | Shown as an *approval*, not a form (`provider/CodexMcpElicitation.ts`). Options are Cancel, Decline, optional "Always allow this session" or "Always allow" (when the `_meta` or form indicates persistence) and Approve. The form `content` is filled automatically from enum or option values and defaults. It is cancelled when a required field cannot be filled, and `url` mode is always cancelled. Full-access mode auto-accepts. |
| `item/tool/call` (dynamic tools), `account/chatgptAuthTokens/refresh`, `attestation/generate` | The adapter registers no handler, so they get `-32601`. The recorder answers `item/tool/call` with `success:false`. |

- When `serverRequest/resolved` arrives, the handler fiber is removed and the request settles.
- At `turn.terminal`, open requests are dismissed unless `retainedRuntimeRequestIds` keeps them.
- Approvals from subagent child threads are routed to the owning root turn (`approvalOwnerCodexTurn`).

Recommendation: treat every server request as a persisted "pending request" record with a live-only reply channel. Clear it on the reply, on `serverRequest/resolved` or at the end of the turn. Map decisions per method exactly as above, and consider `requiredHumanApproval` / `humanCommand` for the acts that only a person may take.

## 6. Models: the catalogue, reasoning levels and defaults

- **Discovery.** A short-lived probe app-server (`CodexProvider.ts` `probeCodexAppServerProvider`) calls these in parallel under one deadline:
  - `account/read {}` first;
  - `model/list` paged by `nextCursor`;
  - `skills/list {cwds}`;
  - `config/read {includeLayers:false}`;
  - `account/rateLimits/read` (optional).
- For a ChatGPT account, the catalogue is listed again after `models_cache.json` in CODEX_HOME has a `fetched_at` newer than the spawn, because the app-server refreshes it online at startup.
- **Mapping** (`mapCodexModelCapabilities`): each `Model` becomes `{slug, name, isDefault, capabilities.optionDescriptors}`.
  - Reasoning is a "select" option fed from `supportedReasoningEfforts[].reasoningEffort`, with `defaultReasoningEffort` marked `isDefault` (gpt-6-astra overrides it to "medium") and labels from a fixed map.
  - The service tier is a select fed from `serviceTiers` / `additionalSpeedTiers`, with `defaultServiceTier` or else "default".
  - `cyberAccessProgram` is a select.
- **Bundled overlay.** `model-manifest.json` plus `codexModelCatalog.ts` add `current`/`legacy` status and ordering, and an "update required" entry for a current model whose `minVersion` is newer than the installed CLI.
- **The picker** (`apps/web/src/components/chat/ProviderModelPicker.tsx`, `ModelPickerContent.tsx`, `ComposerModelEffort.tsx`, `EffortGlyph.tsx`) is fed from the server snapshot. Effort is a small glyph next to the model name in the composer. A model switch inside a thread is allowed and recorded on the next user message (`modelSwitch`).
- At our pin, `Model` has `isDefault`, `defaultServiceTier`, `availableAccessPrograms`, and `model/list` takes `includeHidden`.

Recommendation: fill the picker from `model/list` (paged, `isDefault`, `supportedReasoningEfforts`, `defaultReasoningEffort`) when the app starts and whenever the account changes. Store the choice per thread and send `model` and `effort` on each turn. Skip T3's manifest overlay.

## 7. Sign-in and account status

- **Direct mode** (the user's own Codex). T3 **does not drive `account/login/start`**.
  - Status comes from `account/read` → `{account: apiKey|chatgpt{email,planType,workspaceName}|amazonBedrock|gcp|null, requiresOpenaiAuth}`.
  - With no account and `requiresOpenaiAuth`, it shows "Codex CLI is not authenticated. Run `codex login` (with CODEX_HOME=...)" (`CodexProvider.ts`, around line 1000).
  - It watches `auth.json` in the home directory (debounced 250 ms) and probes again when it changes (`Drivers/CodexDriver.ts`, around line 406).
  - Plan labels come from `planType`, and from the rate-limit snapshot's planType when known (`codexPlanLabel`).
- **Managed mode** (`CodexChatGptAuth.ts`, `CodexManagedRuntime.ts`, `CodexManagedTokenBroker.ts`). This is T3's own OAuth with PKCE against auth.openai.com: a client registration (`oaiapp_...`), the scope `chatgpt.tokens.use.direct`, and a loopback `http://127.0.0.1:<port>/auth/callback`. Codex is launched with `model_provider="openai_token_sharing"` and an `auth={command=..., args=...}` provider whose command fetches a token over local IPC from T3's secret store. This is T3-specific token sharing, which we would not copy.
- **What our pin offers** (pin:`v2/LoginAccountParams.ts`, `LoginAccountResponse.ts`, `AccountLoginCompletedNotification.ts`):
  - `account/login/start {type:"chatgpt"}` returns `{loginId, authUrl}`;
  - `{type:"chatgptDeviceCode"}` returns `{verificationUrl, userCode}`;
  - the notification `account/login/completed {loginId, success, error}`;
  - `account/login/cancel` and `account/logout`;
  - `account/updated {authMode, planType}`.
- T3 calls `"logout"`, an old name. The pin method is `account/logout`.

Recommendation: drive sign-in through the app-server itself: `account/login/start {type:"chatgpt"}`, open `authUrl` in the system browser, wait for `account/login/completed`, then `account/read`. Keep status current from `account/updated`. The engine never sees the token.

## 8. Testing: replay fixtures recorded from the real binary (most important)

**Format** (`packages/effect-codex-app-server/src/replay.ts`, `CodexAppServerReplayTranscript`). One NDJSON file per scenario:
- Line 1 is the header: `{"type":"transcript_start","provider":"codex","protocol":"codex.app-server","version":"0.156.1","scenario":"simple","metadata":{source,fileName,description,model,recorderThreadConfigKeys?}}`.
- Each following line is one of:
  - `{"type":"expect_outbound","label":"<method>","frame":<exact JSON frame the client must send>}`
  - `{"type":"emit_inbound","label":"<method>","frame":<frame Codex sent>,"afterMs"?:n}`
  - `{"type":"runtime_exit","status":"success"|"error"|"cancelled","error"?}`. Mid-file this marks a process restart, as in the resume-after-restart scenarios.
- The label is the method. For a response it is the method of the correlated request, so the reply to a server request is labelled with the request method (for example `item/fileChange/requestApproval`).
- Example: `apps/server/src/orchestration-v2/testkit/fixtures/simple/codex_transcript.ndjson`, 37 lines.

**Recording** (`apps/server/scripts/record-codex-app-server-replay-fixture.ts`; `bun scripts/record-codex-app-server-replay-fixture.ts --scenario <name|all|tool_call> [--model m] [--out f | --out-dir d]`):
1. It spawns the **real** `codex app-server` (`T3_CODEX_BIN`, `CODEX_BIN` or `codex`) with the developer's environment and CODEX_HOME. The cwd is the recorder's cwd. It removes `VP_TOOL_RECURSION`, which breaks node calls in the shell. Live model turns are used, so this costs money, needs a signed-in account and is run by hand.
2. It builds the normal typed client with `logIncoming/logOutgoing:true` and a **logger hook**. Every *decoded* frame is appended as `expect_outbound` (outgoing) or `emit_inbound` (incoming), in the order seen. The version is inferred from the `initialize` `userAgent` or from `thread.cliVersion`.
3. It sends exactly the frames the production adapter sends. It uses the same `CODEX_CLIENT_CAPABILITIES`, `buildCodexInitializeParams().clientInfo` and the adapter's exported `CODEX_THREAD_CONFIG`, and the same `revertCodexThread` helper ("Match V2 adapter frames so recordings replay unchanged").
4. Scenarios are data. Each is `{name, model?, runs:[{turnDefaults?, threadConfig?, interactionMode?, steps}]}` with steps `turn | steeredTurn | interruptedTurn{interruptAfterMs|interruptAfterCommandExecutionStarted} | rollback{numTurns} | fork{from,as}`. Prompts are shared constants (`testkit/fixtures/shared.ts`), so the assertions can match them.
   - Scenarios: simple, tool_call_{read_only_on_request, workspace_never, restricted_granular}, subagent(+v2, nested, approval, continue), multi_turn, queued_turn, provider_thread_resume, todo_list, web_search, plan_questions, proposed_plan, message_steering, turn_interrupt(+mid_tool), command_output_streaming, thread_rollback(+after_restart, to_stopped_turn), thread_merge_back_siblings.
   - The model is pinned per scenario, because model behaviour differs (for example, "gpt-6-luna declines the write; gpt-6-sol tries it").
5. Deterministic answers to the person's part:
   - command, fileChange and permissions approvals: `accept`, permissions with `scope:"turn"`;
   - `requestUserInput`: the first option, or "ok";
   - elicitation: `accept`;
   - `item/tool/call`: `success:false`;
   - legacy approvals: `approved`;
   - any unknown server request: the recorder dies.
6. Timing is driven by notifications. The recorder waits for `turn/started` and `turn/completed` through Deferred registries. A steer goes out after `turn/started`, with an approval gate held so the steer lands mid-turn. An interrupt goes out after a fixed delay or after the first `item/started` of a command; the recorder then terminates its background terminals.
7. **Restart scenarios** run a second client session in the same recording, preceded by `runtime_exit success`. They use `thread/read` and then `thread/resume {excludeTurns:true}`.
8. **Sanitising before writing** (`codexReplayRecordingRecords.ts`):
   - the workspace path becomes `<workspace>` (outbound frames only);
   - the home directory becomes `/home/replay-user`, the checkout becomes `/home/replay-user/t3code`, and the hostname becomes `replay-host`;
   - `installationId` becomes a zero UUID and `planType` becomes `unknown`;
   - recorder-only `threadConfig` (for example `skills.include_instructions:false`, which keeps the recording user's own skills out of the fixture) is named in `metadata.recorderThreadConfigKeys`.

**Replaying** (`replay.ts` `makeReplayClientWithState`). A fake `Stdio` stands in for the child process, and the **real production client and adapter** run on top of it:
- **Matching.** A cursor walks the entries. All consecutive `emit_inbound` entries are pushed into the client's stdin, each after its `afterMs` and an optional `beforeEmitInbound(entry)` hook, until the next `expect_outbound`. Each outgoing line the client writes is parsed and compared with the next `expect_outbound`.
  - The comparison is a stable stringify with sorted keys after normalisation.
  - It is strictly ordered. Any mismatch, extra frame, early `runtime_exit` or exhausted transcript fails the replay with a typed error that carries cursor, label, expected and actual.
  - So the test checks both what we send (including our replies to approvals) and how we handle what Codex sends.
- **Normalisation on outgoing frames** (`normalizeReplayFrame`, `normalizeThreadRequest`). These fields are ignored, because they vary by machine or run or have legacy forms:
  - `initialize.clientInfo.version`;
  - `turn/start` `approvalPolicy` / `sandboxPolicy` when they mean full access;
  - `collaborationMode.settings.developer_instructions`;
  - `cwd` on `config/read`;
  - `cwd`, `model` and `config.mcp_servers*`, plus the thread-id env key and recorder-only keys, on `thread/start|resume|fork`;
  - context-handoff summaries.
- **Upgrades on incoming frames** (`normalizeLegacyInboundFrame`). Old recordings gain the fields newer schemas require: `startedAtMs` / `completedAtMs = 0`, `sessionId` / `projectId`, `isBlocking = true`. Old fixtures keep decoding (the fixtures span 0.120.0 to 0.157.0).
- **Harness** (`apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.testkit.ts`, `testkit/ProviderReplayHarness.ts`, `ProviderReplayFixtures.ts`, `CodexReplayFixtures.integration.test.ts`):
  - The real client factory is swapped for one that builds the replay client (`CodexAppServerClientFactory`).
  - `<workspace>` is materialised back into a temporary directory (`materializeReplayTranscriptWorkspace`).
  - The real orchestrator runs the fixture's `input.ts` steps, for example `[{type:"message", text: TOOL_CALL_WRITE_PROMPT}, {type:"approve_next_runtime_request"}]`, against it. The UI action is a step, and the replay then checks the outgoing `{"id":0,"result":{"decision":"accept"}}`.
  - `codex_output.ts` asserts on the **projection** (T3's state), not on frames: run count and statuses, turn item types in order (`["user_message","file_change","approval_request","assistant_message"]`), runtime request counts and kinds, the assistant text includes "fixture simple ok", the approval prompt text.
  - `ProviderReplayGate` (`packages/provider-testing/src/replayGate.ts`) holds an inbound label until the scenario releases it, for races such as queue or steer during an active turn.
  - The same input and assertions are shared across providers; one scenario folder holds the claude, codex and other transcripts.
- Lower-level tests: `packages/effect-codex-app-server/src/protocol.test.ts` and `client.test.ts` drive an in-memory stdio (`makeInMemoryStdio`). `test/fixtures/codex-app-server-mock-peer.ts` is a scripted fake process for spawn, stderr and exit paths.
- `scripts/dev-replay-agent.ts` replays transcripts as a free fake agent with simulated streaming, so the UI can be developed without a paid model. It does not assert frames.

Recommendation: copy this design nearly as it is.
- A `ReplayTransport` that implements the same `Transport` trait as the stdio child: JSONL `expect_outbound` / `emit_inbound` / `runtime_exit`, cursor, strict order, sorted-key comparison after a small named normalisation list.
- A recorder binary that reuses the engine's own request builders and captures through the engine's logging hook, with scripted scenarios, deterministic answers and sanitisation of home, host, workspace and ids.
- Fixtures recorded at 0.171.0 with the version in the header.
- Assertions on the engine's event and state output, with UI actions as steps.
- Record by hand (live model, our own sign-in); replay in CI offline.

## 9. The interface (apps/web): what makes it pleasant

- **Thread list status** (`components/Sidebar.logic.ts` `resolveSidebarThreadStatus`). One status per row, in priority order: `approval` > `input` > `request` > `waiting` > `preparing` > `working` > `failed` / `limited` > `ready`. There is also an unread watermark.
  - A failure the user has opened settles to ready.
  - Rows that are idle and read recede visually. Rows that need you never recede.
  - Collapsed folders show the most urgent dot (attention > failed > unread > working).
  - The window title states the open thread's status.
- **Composer** (`components/chat/ChatComposer.tsx`, `useComposerSend.ts`, `client-runtime/src/state/composerDispatch.ts`).
  - While a turn runs, Enter **steers** by default and a modifier **queues** instead (configurable; `restart` interrupts and resends). Queued messages are shown and editable (`QueuedRunsControl.tsx`). Steering falls back to queue during `/compact`.
  - ArrowUp/Down walks prompt history.
  - The composer also has slash and mention menus, drag-and-drop or paste of images and files (attachment strip), a plan-mode toggle, a Tab prompt suggestion (from a `thread/sample`-style next-message prediction), drafts kept per thread, and a stash.
  - The model, effort glyph and runtime mode (Supervised, Auto-accept edits, Auto, Full access, `runtimeModeConfig.ts`) sit in the composer footer.
- **Approvals.**
  - They render **in the composer area** (`ComposerPendingApprovalActions.tsx`): primary Approve and Decline buttons, with the other options (Always allow this session, Cancel, provider-authored options) in a "More" menu. A timeline row marks them too (`TimelinePendingApprovalRow.tsx`).
  - Child-thread approvals surface on the parent (`ChildThreadApprovals.tsx`).
- **Questions** (`ComposerPendingUserInputPanel.tsx`). One question at a time, with keyboard selection, single or multiple choice, a custom "Other" answer, auto-advance after a single choice, and progress across the questions.
- **Diffs and the side panel.**
  - A right panel with tabs (`RightPanelTabs.tsx`). The diff panel (`DiffPanel.tsx`, `@pierre/diffs`, worker pool) has scopes for branch versus base, unstaged, commit, **turn** (from checkpoints) and **tool-call** (a single file change).
  - Clicking a file_change row opens its diff. `ChangedFilesTree.tsx` shows the files per turn.
  - Commands expand into an output panel that streams live output (`CommandOutputPanel.tsx`).
- **Plans.** A `ProposedPlanCard.tsx` with a collapsed preview, download as .md, and implement. Todo lists render as a checklist row.
- **Notifications** (`ThreadNotificationCoordinator.tsx`, `threadNotifications.ts`). Status transitions in any thread become a sound, an in-app toast or a desktop `Notification` (mode: off, notifications, sound or both), plus a dock or favicon badge count of unread threads. Snooze is supported. The notification tag is a short hash, because Windows drops long tags.
- **Background threads.** Every thread keeps running server-side whatever the UI shows. Hidden threads stay mounted but quiet (`backgroundThread.ts` contexts), and `BackgroundThreadAnnouncer` and `ThreadActivityAnnouncer` handle accessibility announcements. Work that runs after a turn (background commands, subagents) holds the "working" state (`background_work.updated`).
- **Forks and rewind.** Fork from any response (`useTimelineRewind.ts` `forkFromRun`, titled "<title> fork"). Rewind or edit a past message, which reverts the conversation (`thread/revert`) and files (checkpoint). The thread's lineage and relationships are shown (`ThreadRelationshipsControl.tsx`).
- **Why it feels good.** Streaming markdown is rendered incrementally (`markdown-incremental.ts`), steering never makes you wait, the person's part always appears in the same place (the composer), status marks are unambiguous and prioritised, a stop that hangs offers Force stop, and errors are worded for people (`UserFacingErrors.ts`, for example the "open in another Codex client" message).

Recommendation: copy the status priority, approvals and questions in the composer, steer-by-default with queue on a modifier, diffs per tool and per turn in a side panel, and desktop notifications plus a dock badge on transitions. Leave out the rest at first.

## 10. What not to copy

- **Abstractions for many providers.** `ProviderAdapterV2` capabilities matrices, the `ProviderInstanceRegistry`, cross-provider handoff and switching, the ACP/Claude/Cursor/Grok code paths, shared multi-provider fixtures and the model manifest overlay. We have one provider, so the engine event model can sit close to Codex's thread, turn and item.
- **Effect machinery.** Layers and Context services, Scope, FiberMap, Deferred registries, Schema codecs everywhere, `Effect.fn` spans. In Rust: tokio tasks, `oneshot` and `mpsc`, a `JoinSet` or `HashMap<RequestId, AbortHandle>`, `serde` types generated from the pin's JSON Schema, and `tracing`. Generate types once from `schema-json`, not at runtime.
- **The event-sourced orchestrator** (EventStore, Projection*, Outbox, EffectWorker, receipts, and the 9,111-line adapter with its state maps). It is far beyond what a single-user desktop needs at first. Keep a single in-memory state per thread plus a small store.
- **Managed install and update of the Codex binary**, npm update checks, and shadow-home symlink farms. We ship a pinned binary.
- **T3's own ChatGPT OAuth and token-sharing provider**, managed token broker IPC, and rate-limit reset credits. Use `account/login/start`.
- **Experimental or off-pin methods** that T3 relies on: `thread/backgroundTerminals/*`, `thread/sample`, `thread/settings/update`, and the `turn/start` extras `collaborationMode`, `additionalContext` and `cyberAccessProgram`. Use them only after confirming them against 0.171.0 with `experimentalApi`, through a recorded fixture.
- **T3-specific features.** The in-process MCP server injected into every thread (`mcp_servers.t3-code`, `t3-code-direct`), the port scanner, the agent-scope process wrapper, guest share, PR watchers, workflows, the `additionalContext` re-injection after compaction, and goal tracking.
- **Hand-tuned heuristics** that exist because of the many providers or legacy recordings, for example the regexes that guess elicitation persistence and legacy inbound upgrades. Record fresh fixtures at our pin instead.

Recommendation: copy the **protocol client, the replay transport and recorder, the turn policy mapping and the UI patterns**. Build the engine as a lean Rust core: one process manager, one JSON-RPC client, a thread, turn and item state machine, and a pending-request registry. Expose a narrow event stream to the Tauri wrapper.

## Attribution

The file names, identifiers, settings and argument shapes quoted in these notes
come from T3 Code (`github.com/pingdotgg/t3code`), used under its licence:

> MIT License
>
> Copyright (c) 2026 T3 Tools Inc.
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.
