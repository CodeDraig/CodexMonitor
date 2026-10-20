# Multi-agent runtime implementation plan

Introduce a shared runtime interface, preserve Codex behind its own adapter, and add OpenCode through ACP. Durable runs and comparisons identify the runtime and execution explicitly rather than assuming every result has a Codex thread and turn ID.

Code baseline: `d4dbad7` — “Decompose runtime and UI code and unify remote settings.”

This plan is separate from `IMPLEMENTATION_PLAN.md`. Preserve that file byte-for-byte; its SHA-256 is:

```text
a21b70485abcffb966f5375db31087bafb90c28db1fdde969967442d23775250
```

## Current implementation

The existing separation between shared cores, desktop adapters, and daemon handlers is suitable for this work, but the execution boundary remains Codex-specific:

- `src-tauri/src/backend/app_server/session.rs`: `WorkspaceSession` owns Codex transport, pending requests, activity tracking, workspace routing, and run-recorder queues.
- `src-tauri/src/shared/runs_rpc.rs` accepts a map of Codex `WorkspaceSession` objects. Capture and reconciliation call Codex history directly.
- `src-tauri/src/shared/runs_core/recording.rs` identifies execution through Codex methods, thread IDs, and turn IDs.
- `src-tauri/src/shared/workspaces_core/connect.rs` shares an existing live Codex process between workspaces. Runtime identity is not part of that lookup.
- Codex operations reside in `shared/codex_core/*`, desktop commands in `codex/*`, and daemon handlers in `rpc/threads.rs`, `rpc/turns.rs`, and other modules. Use these current locations.
- Remote selection uses `remoteBackends` and `activeRemoteBackendId`. Runtime selection remains a separate choice within the selected backend.
- `src-tauri/src/shared/codex_core/history.rs` requires paginated full-item reads. Preserve that behavior.
- Follow-up routing supports queue and steer. ACP support needs capability-aware routing without restoring removed settings.

## Scope and decisions

This milestone delivers Codex and OpenCode in ordinary conversations and recorded runs, through both the desktop backend and daemon. A comparison may contain captures from different runtimes.

“Multi-agent runtime support” means supporting multiple agent implementations. It does not add an automatic coordinator that delegates tasks among them.

Use native Rust integration with the existing Tokio and Serde stack. OpenCode documents `opencode acp` as its stdio ACP entry point. No JavaScript bridge is needed. [OpenCode ACP documentation](https://opencode.ai/docs/acp/)

Retain Codex-specific capabilities—such as its account, collaboration, review, and steering features—as adapter extensions. The common interface covers shared behavior without inventing equivalents for unsupported features.

Start with ACP v1 and capabilities actually negotiated with OpenCode. The installed executable is `/home/cadwaladyr/.opencode/bin/opencode`; its version and live compatibility are not yet verified.

## 1. Introduce the shared runtime contract and wrap Codex

Add `src-tauri/src/shared/agent_runtime.rs` and an `agent_runtime/` subtree containing contracts, registry, and adapters. Keep process transport under `backend/` and domain orchestration under `shared/`.

The following are proposed contract shapes, not existing types:

```rust
enum RuntimeKind {
    Codex,
    OpenCodeAcp,
}

struct RuntimeRef {
    profile_id: String,
    profile_revision: u64,
    kind: RuntimeKind,
}

struct RuntimeSessionRef {
    store_id: String,
    runtime: RuntimeRef,
    session_id: String,        // Monitor-owned durable identity
    native_session_id: String,
}

struct SubmitExecution {
    request_id: String,        // Idempotency key
    session: RuntimeSessionRef,
    prompt: Vec<PromptContent>,
    run_context: Option<RunContext>,
    options: RuntimeOptions,   // Tagged per runtime; validated by adapter
}

struct ExecutionTicket {
    execution_id: String,      // Allocated before external submission
    submission: SubmissionOutcome,
    execution: ExecutionOutcome,
    native_turn_id: Option<String>,
}
```

Provide a `RuntimeHandle` enum with Codex and OpenCode implementations and these common operations:

| Operation | Contract |
|---|---|
| `connect` | Return runtime identity, version, and negotiated capabilities. |
| `create_session` / `load_session` | Return a durable session reference and configuration state. |
| `submit` | Return a persisted execution ticket promptly; process output asynchronously. |
| `cancel` | Request cancellation; completion remains event-driven. |
| `respond_permission` | Answer a specific pending request using its advertised option ID. |
| `set_session_option` | Apply an advertised option and return confirmed configuration. |
| `read_history` | Return available history with explicit completeness and execution-boundary information. |

Capabilities describe at least history loading, session discovery, images, configuration options, cancellation, steering, and permission interaction. “Unsupported” is distinguishable from failure.

Initially wrap existing Codex transport and shared operations. Do not move its account, MCP, skill, or review implementations into ACP abstractions.

Replace runtime selection by workspace alone with an explicit registry lookup. Sharing a process is permitted only when the adapter and effective launch configuration match. Preserve current Codex sharing and busy-session replacement guards.

### Acceptance

- Existing Codex command payloads and responses remain compatible, including omitted versus explicit-null options.
- Existing history pagination and activity/replacement tests pass.
- Two runtimes using the same native session ID remain distinct.
- A profile change cannot terminate an active session or an unrelated runtime.
- Unsupported operations return a typed error before sending a protocol request.

## 2. Persist runtime identity and execution ownership in the shared store

Extend `src-tauri/src/shared/runs_core/store.rs` with an additive schema migration. Keep WAL, `synchronous=FULL`, the read-only open fast path, and the contention handling already implemented.

Add four narrowly scoped tables:

| Table | Required responsibility |
|---|---|
| `runtime_profiles` | Backend-owned executable/argument configuration, revision, and defaults. |
| `runtime_sessions` | Workspace binding, profile snapshot, native session ID, and connection ownership. |
| `runtime_executions` | Request identity, execution identity, optional run instance, submission state, terminal result. |
| `runtime_events` | Ordered durable events with execution association, source, normalized payload, and raw protocol evidence. |

Runtime profiles belong to the executing backend. A daemon profile is managed through daemon RPC; storing it only in the desktop’s local `AppSettings` would configure the wrong machine.

Expose the existing Codex executable/argument/home settings as the default Codex profile rather than creating a second competing configuration source. Snapshot effective configuration into each execution.

Use unique request IDs and transactional ownership claims. Two processes sharing the database must not submit the same request twice or independently take ownership of the same live ACP session. An uncertain owner failure must not trigger automatic resubmission.

Add `runtime`, `sessionId`, and `initialExecutionId` to new run instances. Keep existing Codex fields readable. Introduce a versioned capture representation with an execution ID and optional native turn ID; do not manufacture Codex-style turn IDs for ACP.

Preserve old capture payloads, capture IDs, comparison references, notes, and store identity. Interpret legacy records as Codex when reading them.

Before migration, create a recoverable SQLite-consistent backup. Upgrade desktop and daemon together: the current schema reader rejects versions above 1, so mixed-version use must produce a clear incompatibility error.

### Acceptance

- Migrate a populated schema-1 fixture; old capture payloads and comparison notes remain byte-for-byte unchanged.
- Reopening or concurrently opening an upgraded store does not rerun migration or acquire unnecessary write locks.
- Duplicate submission requests return the same execution ticket.
- Competing ownership claims produce one owner and an explicit conflict.
- Invalid or newer schemas are reported as errors, never empty history.

## 3. Implement the OpenCode ACP adapter

Add an ACP transport and an OpenCode adapter. Launch the configured executable directly with an argument vector containing `acp`; avoid shell interpolation.

Implement initialization, session creation/loading, prompt submission, updates, permission requests, cancellation, and advertised session configuration. Record the negotiated protocol version and agent version. Advertise only client capabilities actually implemented. [ACP initialization](https://agentclientprotocol.com/protocol/v1/initialization)

Initially let OpenCode execute its own tools; advertise client filesystem and terminal callbacks as unavailable. Verify this with the installed OpenCode build. If that build requires callbacks for the intended workflow, implement the required bounded callbacks before claiming acceptance.

The key lifecycle distinction is that ACP’s `session/prompt` response signals the end of the prompt, while updates and permission requests can arrive before it. Therefore:

- Allocate and persist the execution ID before sending.
- Register event routing before writing the request.
- Run the prompt exchange in a backend task.
- Return the execution ticket without waiting for the complete prompt.
- Treat a successful pipe write as dispatch, not proof of acceptance.
- Preserve uncertainty after a connection failure; do not silently retry the prompt.

This also avoids holding the existing desktop-to-daemon RPC open for the whole generation. [ACP prompt lifecycle](https://agentclientprotocol.com/protocol/v1/prompt-turn)

Allow one active prompt per ACP session. Queue subsequent messages. Expose steering only if a supported implementation can provide its actual semantics.

For permissions, preserve the supplied option IDs, labels, and scopes. Cancellation must settle outstanding permission requests as cancelled. Do not translate Codex sandbox settings into supposedly equivalent ACP permissions. [ACP tool calls and permissions](https://agentclientprotocol.com/protocol/v1/tool-calls)

Prefer advertised `configOptions`; support legacy mode selection only when needed. Store requested and confirmed settings separately. [ACP session configuration](https://agentclientprotocol.com/protocol/v1/session-config-options)

### Acceptance

Use a deterministic fake ACP subprocess to verify:

- Interleaved responses, notifications, and agent-to-client requests.
- Output arriving before the prompt response.
- Tool-call updates merging partial fields correctly.
- Permission selection, rejection, cancellation, and stale-response rejection.
- Long-running prompts returning an IPC ticket promptly.
- Unsupported protocol versions and authentication-required responses producing actionable errors.
- Process exit during submission preserving `unknown` rather than causing resubmission.

Then capture a sanitized handshake from the actual OpenCode installation and run one real prompt, tool action, permission interaction, cancellation, and session reload. Record the exact executable/version and capabilities.

## 4. Make run recording and capture runtime-aware

Refactor `src-tauri/src/shared/runs_core/recording.rs` and `src-tauri/src/shared/runs_rpc.rs` to consume runtime executions rather than directly requiring Codex sessions.

Use an event envelope containing:

```text
schemaVersion, storeId, runtimeRef, sessionId, executionId?,
sequence, source(live|history), kind, payload, raw
```

Normalize the events needed by existing UI and evidence: message content, tool changes, permission requests, configuration, usage, and execution completion. Retain raw evidence alongside the normalized representation.

Persist events before publishing their durable sequence. Keep protocol reading independent of SQLite retry waits. Retain pending writes across contention and report persistence failures explicitly.

Maintain the original result rule: **the first submitted execution is the initial result; later captures are separate records.**

ACP session loading can replay history through updates. Mark replay explicitly and never attach replayed content to a new live execution. Where replay does not establish an exact historical execution boundary, retain the evidence as incomplete rather than selecting a later answer as the initial result. [ACP session loading](https://agentclientprotocol.com/protocol/v1/session-setup)

Usage carries its scope—execution, session, or unavailable. Session totals must not appear as turn totals. Missing metrics remain unavailable rather than zero.

Decouple recorded-worktree preparation from automatic Codex startup. OpenCode runs must not launch Codex for session creation or metadata generation; use deterministic title/branch defaults where necessary.

### Acceptance

- Codex and ACP fixtures representing equivalent visible output produce comparable captures while retaining different raw evidence.
- Follow-up messages and session reloads leave the initial capture hash unchanged.
- Ambiguous recovery never promotes a later response to the initial result.
- Existing contention tests remain passing, including queued events, accepted responses, and capture request identity beyond the SQLite busy timeout.
- An OpenCode-only launch test observes no Codex subprocess.
- Comparisons between legacy Codex and new OpenCode captures save and reload correctly.

## 5. Wire desktop and daemon surfaces through the same core

Add a shared `runtime_rpc.rs`, a thin desktop adapter, and a daemon runtime handler. Update:

- `src-tauri/src/lib.rs`
- `src-tauri/src/state.rs`
- `src-tauri/src/backend/events.rs`
- `src-tauri/src/bin/codex_monitor_daemon/rpc/dispatcher.rs`
- `src-tauri/src/bin/codex_monitor_daemon/events.rs`
- `src-tauri/src/remote_backend/transport.rs`
- `src/services/tauri.ts`
- `src/services/events.ts`

Expose explicit commands for profile listing/updating, connection, session creation/loading, execution submission/lookup/cancellation, permission responses, configuration, and event replay after a sequence cursor.

Profile updates use expected revisions. New errors have stable codes such as `unsupported`, `auth_required`, `runtime_busy`, `revision_conflict`, and `transport_lost`.

Add a `runtime-event` channel beside the existing Codex event channel. Reconnect by replaying persisted events after the last seen sequence. Do not add execution submission to the remote transport’s automatic retry allowlist.

### Acceptance

- Identical requests against desktop and daemon adapters return equivalent contract shapes.
- Remote profile operations affect the selected daemon.
- Disconnect/reconnect restores committed events without duplicate messages or prompt submission.
- Switching between `remoteBackends` entries cannot leak sessions, permissions, or run caches between stores.

## 6. Add runtime selection and capability-aware UI

Update the current orchestration points:

- `src/features/app/hooks/useMainAppComposerWorkspaceState.ts`
- `src/features/workspaces/hooks/useWorkspaceHome.ts`
- `src/features/threads/hooks/useThreadMessaging.ts`
- `src/features/threads/hooks/useQueuedSend.ts`
- `src/features/runs/hooks/useRunRecords.ts`
- `src/features/runs/components/RunWorkbench.tsx`

Add runtime selection for new conversations and each recorded-run instance. Existing conversations retain their runtime binding.

Render advertised model/mode options and permission choices. Keep unsupported controls unavailable with a concrete explanation. Preserve Codex queue/steer behavior; show queue behavior for OpenCode when steering is unavailable.

Introduce a runtime-event projection into the existing conversation state and reuse the current `MessageRow`, `ToolRow`, and other decomposed components. Do not synthesize fake Codex protocol messages. Each conversation has one authoritative event path.

Scope caches by selected backend, returned store identity, runtime profile, and connection generation. Display runtime/version and requested/observed configuration in comparisons.

### Acceptance

- Existing Codex conversation and workbench tests pass.
- OpenCode selection prevents Codex-only account, model, and collaboration calls.
- A queued follow-up starts exactly once after the active ACP execution finishes.
- A stale permission dialog cannot answer a new process’s request.
- Switching runtimes or backends cannot display stale output.
- Mixed-runtime comparison notes survive reload and revision conflicts remain visible.

## 7. Complete sustained concurrency and native acceptance

Extend the existing store/recorder contention tests with **two OS processes sharing one store**, representing desktop and daemon. Exercise Codex and ACP event streams while comparison notes are updated.

Run sustained writes for at least 60 seconds and include a write lock held beyond the configured five-second SQLite busy timeout. Track expected request IDs, event sequences, capture hashes, and acknowledged comparison revisions.

The test establishes:

- No duplicate external submissions.
- No missing committed events or captures.
- No silent overwrite of conflicting comparison edits.
- Every acknowledged note revision remains recoverable.
- Events retained during contention are committed after the lock is released.
- Store integrity and foreign-key checks pass.

Complete native acceptance with isolated data directories and test repositories:

1. Run Codex and OpenCode locally; capture and compare their initial results.
2. Repeat through the managed daemon.
3. Run local and daemon-backed executions concurrently against the same store while editing comparison notes.
4. Restart the desktop while a daemon execution continues; reconnect and recover its output.
5. Restart both after completion; verify capture hashes, runtime metadata, and notes.
6. Interrupt a backend during execution; verify explicit uncertainty or supported recovery without automatic resubmission.
7. Add follow-ups, reopen sessions, and save later captures; verify the original comparisons remain unchanged.

Use native window interaction and inspection. If computer-use binding fails, use an available native accessibility/window-control route and capture the resulting UI evidence. Headless IPC tests do not replace these checks.

Run focused tests during each slice, then the repository’s required checks:

```bash
npm run typecheck
npm run test
cd src-tauri
cargo check
cargo test
```

Update canonical setup and acceptance documentation with supported versions, capabilities, reproduction commands, and evidence paths. Claude Code and Antigravity adapters follow this milestone; support is not established merely because the interface can accommodate them.

The next concrete uncertainty is the installed OpenCode build’s negotiated capabilities and history behavior; slice 3 resolves it with an actual ACP exchange.
