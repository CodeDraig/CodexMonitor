# Agent runtimes

CodexMonitor uses the Codex app-server adapter and a shared ACP v1 adapter in both the desktop and daemon. OpenCode is an ACP launch preset. Every configured ACP profile uses the same discovery, replay, projection, ownership, and composer contracts. A profile launches on the selected backend's machine; backend selection and agent selection are independent. The protocol target is ACP v1 [session discovery](https://agentclientprotocol.com/protocol/v1/session-list) and [session loading/replay](https://agentclientprotocol.com/protocol/v1/session-setup); v2 migration is separate.

## Profiles and conversations

Choose **Browse agent sessions** in the normal sidebar. **ACP profiles** configures a profile ID, display name, executable, exact JSON argument vector, and optional absolute launch directory. The **OpenCode launch preset** uses `opencode` with `["acp"]`. Generic ACP launchers receive exactly the configured arguments. Existing `open_code_acp` profiles retain their original behavior: their saved arguments have `acp` appended by the compatibility adapter.

A profile's connection scope depends on its ID and effective executable, argument vector, and configured launch directory. Renaming its display name preserves the scope. Changing its launch configuration creates a different scope. Existing Monitor UUIDs, run references, and profile snapshots remain intact.

The default Codex profile mirrors the executing backend's Codex settings and is read-only through the profile API. Workspace-specific Codex arguments and home settings remain effective. Agents own their provider configuration, credentials, and tools. Monitor advertises ACP client filesystem and terminal callbacks as unavailable. Session compaction is advertised because its patches, summary content, and visible history limitations are handled.

The global browser lists cached sessions, discovers sessions across directories, searches titles and paths, filters by profile, and pages each source independently. A failed source retains its cached rows. Native cursors stay in memory on their discovery connection; stale or differently filtered cursors return an explicit error. Discovery connections never reload a foreground session. Object-valued capability markers, including `{}`, count as advertised capabilities.

Opening a session reuses its matching workspace or offers **Add workspace and open**. The executing backend validates the reported directory. New ACP workspace registrations set `codexAutoConnect: false`, so registration and subsequent workspace restoration do not launch Codex. Registration merges the current workspace file under the shared store lock, preserves unrecognized fields, and saves a recovery copy before an atomic file replacement. Both backends refresh their workspace maps from storage. Missing directories remain visible in the catalog with an error when opened. Agents without listing expose known sessions; **Open session by ID** requires a native ID and absolute agent directory, and an independent initialization probe checks loading support before any catalog or workspace registration.

ACP conversations appear in the ordinary sidebar, `Messages` view, and `Composer`. Rows and conversation controls show profile identity. Title overrides, pinning, and hiding are revision-checked Monitor metadata; hiding never deletes the native session. Backend/store/session-scoped UI keys remain distinct from native session IDs. ACP keys cannot enter Codex thread commands. Existing Codex runtime bindings coalesce with their native thread rows; committed runtime-owned Codex notifications feed the existing thread UI through one event path.

Choose **New agent** from a workspace's sidebar add menu (or the project menu in threads-only mode) to choose **Codex** or a configured **ACP** profile. The choice is independent of the currently selected conversation. **Manage agent profiles** opens profile setup when another agent needs configuring. ACP creation opens the new conversation in the main view; Codex retains its existing new-agent workflow. Workspace Home also offers **Conversation agent** and **New runtime conversation** and retains profile configuration, recorded-run selection, and comparisons; run-history links open the main conversation.

ACP follow-ups queue until the active execution finishes. Stop cancels the active prompt and clears its queued follow-ups. Permission controls retain the original agent choices and connection-generation identity. Configuration selectors and slash suggestions come from advertised agent updates. The ACP composer does not offer Codex collaboration, access, model, or reasoning controls. Images, audio, and embedded input are gated by negotiated prompt capabilities. Uncertain submissions require receipt inspection and are never resent automatically.

The ACP model, effort, and mode selectors accept pointer input within the composer overlay. Automatic replay waits while an ACP control has focus so a history refresh cannot close an open dropdown. **Attach files (optional)** opens a file chooser only when explicitly requested; it is a Monitor attachment control, not a request from the agent. Messages do not require attachments. The agent receives the workspace directory for its normal workspace tools; optional attachments add content to the next message.

## Conversation history and freshness

`session/load` replays into a new durable generation. The protocol reader tags the target session before sending the request, queues the response barrier before resolving it, and classifies immediately following notifications as live. The recorder commits all earlier updates and the raw response before publishing the generation. A successful reload replaces the previous snapshot. Repeated text is never used as a deduplication key. Supplied message IDs are preserved; otherwise generation-scoped identities retain received chunk order. Tool and compaction patches retain omitted fields and update their original timeline position.

The main view combines the committed replay with later live updates. Complete history means **the conversation supplied by a successful agent replay**. Compaction summaries identify unavailable original content; Monitor cannot recover material the agent does not supply. A failed refresh retains the previous successful generation and reports stale cached history. An incomplete first load reports its failure rather than presenting a complete conversation.

Text, reasoning, media, resources, tool content/diffs, plans, configuration, annotations, and unknown update data are retained. Binary blocks are stored as conversation attachments and fetched on demand. Supported images/audio and text resources render in the main view. Other content has a descriptive fallback with inspectable supplied data. Updates for a different native session never appear in the foreground conversation: attached identities route to their own journal, and unresolved identities retain unattributed raw evidence without guessing their directories.

Open, focus, explicit refresh, and session metadata notifications request refresh. A visible conversation checks freshness every 30 seconds. Replay waits while submitting, executing, draining a queue, or handling permissions. Drafts are scoped by backend/store/session and survive selection changes and refresh. The existing message view retains its scroll behavior during background updates.

Cached reads never acquire execution ownership. The read contract reports `connectionReady` separately from cached coverage; an unrestored connection cannot accept a prompt, and a failed initial load can still be disconnected. A different live owner produces a read-only state; Monitor does not steal its session or resubmit its prompts. A stopped owner can be reclaimed only after the ownership checks permit it. Uncertain executions remain reserved. Replayed permissions are historical evidence, never actionable approvals. Replay creates no execution receipts or run captures and cannot change an immutable initial result.

## Recorded runs and evidence

Select **Agents for recorded runs**, enter a prompt in Workspace Home, and send. Local mode accepts one instance; worktree mode permits multiple selected agents and expands selected Codex models/counts. An empty agent selection retains the existing Codex composer workflow. ACP worktrees do not start Codex for preparation or metadata.

Monitor allocates a durable execution UUID before submission. Repeating its request UUID with identical input returns the existing receipt; conflicting input is rejected. Native ACP sessions need no Codex-style turn ID. Dispatch to stdin is not confirmed acceptance. Correlated live events and prompt responses determine execution state.

A run instance's first submitted execution remains its primary result. Its immutable capture commits before terminal state releases the session for follow-ups. Later executions receive separately requested captures. A capture-write error retains the reservation and reports incomplete evidence. Schema-2 capture payloads retain runtime/version, requested and observed configuration, raw evidence, and optional native turn identity. Existing schema-1 captures remain readable without rewriting their payloads.

Usage retains its reported scope. Session totals are not substituted for execution metrics. Missing metrics and command exit codes remain unavailable. Git evidence observes the workspace, including other-process edits; it does not establish exclusive attribution. See [run records](run-records.md).

## Store and recovery

The executing backend stores `runs.sqlite3` beside `workspaces.json`. Schema 3 adds `session_catalog`, `conversation_aliases`, `history_loads`, `conversation_snapshots`, `conversation_items`, and `conversation_attachments` to the run/runtime tables. Catalog rows include the associated workspace UUID when one exists. Catalog identity combines the store, profile connection scope, and native session ID. Backfill preserves Monitor UUIDs and aliases ambiguous legacy bindings without deleting referenced records.

Upgrading schema 1 or 2 first creates a SQLite-consistent `runs.schema<version>-backup-<uuid>.sqlite3` through `VACUUM INTO`, including committed WAL pages. Existing run, instance, operation, capture, attachment, comparison, execution, and event payloads are preserved. Current-schema reads have no writer reservation. Conversation pagination reads the generation pointer, items, and load status in one deferred read transaction so publication cannot produce a mixed snapshot. WAL, `synchronous=FULL`, ordered contention retries, ownership checks, and comparison revision checks remain in use. Desktop and daemon may share a local-filesystem store on one host. Upgrade executables together; older versions reject newer schemas.

History generations publish only after their writes commit. Failed/uncommitted generations remain separate from the last successful snapshot. Owner death before a load commits is reported as failed/stale and permits a new load when no uncertain execution remains. Pending protocol queues are process memory, not a crash-proof disk spool.

Comparison writes retain every acknowledged revision in `comparison_revisions`. A stale revision cannot overwrite a newer edit. The workbench keeps its unsaved draft after an error.

## Runtime command contract

Desktop Tauri and daemon JSON-RPC expose `runtime_request` with `{method, input}` through `src-tauri/src/shared/runtime_rpc.rs`.

| Method | Input and result |
| --- | --- |
| `profiles` | Backend/store identity, schema version, and profiles, including virtual Codex. |
| `save_profile` | `{profile, expectedRevision}` → revision-checked profile. |
| `catalog` | Cached discovered and Monitor-bound session summaries, scopes, metadata revisions, and presentation. |
| `discover_sessions` | `{profileId, cwd?, cursor?}` → summaries, negotiated capabilities, source status, next cursor. Omitted `cwd` requests global discovery. |
| `open_session_by_id` | `{profileId, nativeSessionId, cwd}` → catalog identity for a known native ID. |
| `attach_session` | `{catalogId, workspaceAssociation}` → existing/new Monitor binding. Association is `{kind:"reuse", workspaceId}` or `{kind:"add"}` for the reported directory. |
| `sessions` / `session` | `{workspaceId}` → bindings; `{sessionId}` → one cached binding without connecting. |
| `create_session` | `{workspaceId, profileId, expectedProfileRevision?}` → new session/capabilities. |
| `load_session` | `{sessionId}` → reconnect known binding when ownership permits. ACP replay commits before this compatibility call returns. |
| `load_conversation_history` | `{sessionId, requestId}` → durable asynchronous load ticket. Repeated request IDs return the same load, including while a later execution is waiting for permission. |
| `read_conversation` | `{sessionId, generation?, cursor?, limit?}` → ordered materialized items, generation, load state, coverage, freshness, `readOnly` ownership and `connectionReady` state, next cursor. Limit is 1–500. A changed generation rejects continued pagination. |
| `read_conversation_attachment` | `{sessionId, attachmentId}` → MIME type and base64 content, scoped to that conversation. |
| `update_conversation_metadata` | `{sessionId, expectedRevision, titleOverride?, pinned?, hidden?}` → updated catalog row. Omitted fields remain unchanged. |
| `submit` | `{requestId, sessionId, prompt, options?, runContext?}` → execution ticket promptly. |
| `execution` | `{executionId}` or `{requestId}` → receipt without resubmission. |
| `history` | `{sessionId}` → Monitor-recorded executions; this is not complete conversation history. |
| `events` | `{sessionId, after?}` → up to 1,000 durable ordered events. |
| `configure` | `{sessionId, optionId, value}` → confirmed advertised configuration. |
| `permission` | `{sessionId, requestId, optionId?}` → advertised choice, or cancellation. |
| `steer` | `{sessionId, executionId, prompt}` → supported expected-execution steering. |
| `cancel` / `disconnect` | `{sessionId}` → cancellation / idle connection release. |
| `capture` | `{instanceId, executionId, requestId, label?}` → separate immutable manual capture. |

ACP prompt blocks follow negotiated ACP v1 content shapes. Codex images use `url`; ACP images/audio use base64 `data` and `mimeType`. Embedded resources retain their resource object. Unsupported types/options fail before dispatch. Error categories include `unsupported`, `auth_required`, `runtime_busy`, `revision_conflict`, `transport_lost`, and `invalid_input`.

`runtime-event` notifications carry durable store/runtime/session/sequence identity, optional execution and history-load identities, normalized payload, and raw evidence. They wake durable readers rather than advancing a cursor across a possible gap. The remote transport never automatically retries `runtime_request`; lost submission replies require looking up the original request UUID.

## Verification

```sh
npm run typecheck
npm run test
npm run build
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo test --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
python3 scripts/test-runtime-store-contention.py
```

The sustained runner uses separate desktop-library and daemon test executables for at least 62 seconds, mixes live execution/captures, repeated history generations, and comparison edits, and holds a writer lock beyond SQLite's busy timeout. It verifies acknowledged receipts, replay generations, immutable captures, comparison revisions, restart reads, foreign keys, and SQLite integrity.

Real-agent and native-window acceptance is documented separately in [native runtime acceptance](native-runtime-acceptance.md). Generic ACP fixture conformance establishes shared protocol behavior, not acceptance of every agent, provider, operating system, or package.
